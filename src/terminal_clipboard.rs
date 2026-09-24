//! Desktop clipboard shortcuts shared by local and remote VTE terminals.
use gtk4::{gdk, glib, prelude::*};
use vte4::prelude::*;

#[derive(Debug, PartialEq)]
enum Action {
    Copy,
    Paste,
}

fn action(
    key: gdk::Key,
    modifiers: gdk::ModifierType,
    codex: bool,
    text_available: bool,
) -> Option<Action> {
    // Ignore Caps Lock and pointer-button state, but never steal Alt/Super chords.
    let modifiers = modifiers
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::HYPER_MASK
            | gdk::ModifierType::META_MASK);
    let control_shift = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
    if modifiers == control_shift {
        match key {
            gdk::Key::c | gdk::Key::C => Some(Action::Copy),
            gdk::Key::v | gdk::Key::V => Some(Action::Paste),
            _ => None,
        }
    } else if codex
        && text_available
        && modifiers == gdk::ModifierType::CONTROL_MASK
        && matches!(key, gdk::Key::v | gdk::Key::V)
    {
        Some(Action::Paste)
    } else {
        None
    }
}

fn has_text(formats: &gdk::ContentFormats) -> bool {
    formats.contains_type(String::static_type())
        || [
            "text/plain;charset=utf-8",
            "text/plain",
            "UTF8_STRING",
            "TEXT",
            "STRING",
        ]
        .iter()
        .any(|mime| formats.contain_mime_type(mime))
}

pub fn install(terminal: &vte4::Terminal, codex: bool) -> gtk4::EventControllerKey {
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = terminal.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(terminal) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        // Codex interprets raw Ctrl+V as image paste. Prefer VTE's text paste
        // when Chrome (or another app) offers text, including rich selections.
        // Check formats only: never read or replace the clipboard on key routing.
        let text_available = codex && has_text(&terminal.clipboard().formats());
        let Some(action) = action(key, modifiers, codex, text_available) else {
            return glib::Propagation::Proceed;
        };
        match action {
            Action::Copy => {
                // An empty selection must not erase the desktop clipboard.
                if terminal.has_selection() {
                    terminal.copy_clipboard_format(vte4::Format::Text);
                }
            }
            // VTE handles newline sanitation and bracketed paste. Its commit
            // signal also delivers the result to the existing remote transport.
            Action::Paste => terminal.paste_clipboard(),
        }
        glib::Propagation::Stop
    });
    terminal.add_controller(keys.clone());
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };

    #[test]
    fn clipboard_shortcuts_preserve_terminal_control_keys() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(
            action(gdk::Key::C, ctrl | shift, false, false),
            Some(Action::Copy)
        );
        assert_eq!(
            action(gdk::Key::v, ctrl | shift, false, false),
            Some(Action::Paste)
        );
        assert_eq!(
            action(
                gdk::Key::c,
                ctrl | shift | gdk::ModifierType::LOCK_MASK,
                false,
                false
            ),
            Some(Action::Copy)
        );
        for key in [gdk::Key::c, gdk::Key::v, gdk::Key::Insert] {
            assert_eq!(action(key, ctrl, false, false), None);
            assert_eq!(action(key, shift, false, false), None);
            assert_eq!(
                action(key, ctrl | shift | gdk::ModifierType::ALT_MASK, true, true),
                None
            );
        }
    }

    #[test]
    fn codex_control_v_prefers_text_but_preserves_image_paste_and_shell_keys() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(action(gdk::Key::v, ctrl, true, true), Some(Action::Paste));
        assert_eq!(
            action(gdk::Key::V, ctrl | gdk::ModifierType::LOCK_MASK, true, true),
            Some(Action::Paste)
        );
        assert_eq!(action(gdk::Key::v, ctrl, true, false), None);
        assert_eq!(action(gdk::Key::v, ctrl, false, true), None);
        assert_eq!(action(gdk::Key::c, ctrl, true, true), None);
        assert!(!has_text(&gdk::ContentFormats::new(&["image/png"])));
        assert!(has_text(&gdk::ContentFormats::new(&[
            "text/html",
            "text/plain;charset=utf-8",
            "image/png"
        ])));
    }

    // Run explicitly on an isolated display; this test owns its clipboard.
    #[test]
    #[ignore = "requires an isolated GTK display and clipboard"]
    fn clipboard_round_trip() {
        gtk4::init().expect("GTK display");
        let terminal = vte4::Terminal::new();
        let window = gtk4::Window::new();
        window.set_child(Some(&terminal));
        window.present();
        let keys = install(&terminal, true);
        let context = glib::MainContext::default();
        let pump = |ready: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !ready() && Instant::now() < deadline {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(ready(), "GTK operation timed out");
        };
        let shortcut = |key| {
            keys.emit_by_name::<bool>(
                "key-pressed",
                &[
                    &key,
                    &0u32,
                    &(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK),
                ],
            )
        };
        let clipboard = terminal.clipboard();
        terminal.feed("copy café Україна\r\n".as_bytes());
        pump(&|| {
            terminal
                .text_format(vte4::Format::Text)
                .is_some_and(|t| t.contains("Україна"))
        });
        terminal.select_all();
        assert!(shortcut(gdk::Key::C));
        let copied = context
            .block_on(clipboard.read_text_future())
            .unwrap()
            .unwrap();
        assert_eq!(copied.trim(), "copy café Україна");
        terminal.unselect_all();
        assert!(shortcut(gdk::Key::C));
        assert_eq!(
            context
                .block_on(clipboard.read_text_future())
                .unwrap()
                .unwrap(),
            copied
        );

        let input = Rc::new(RefCell::new(Vec::new()));
        let received = input.clone();
        crate::card_source::connect_host_input(&terminal, move |bytes| {
            received.borrow_mut().extend_from_slice(bytes)
        });
        terminal.feed(b"\x1b[?2004h");
        // Allow the emulator to consume the mode change before paste.
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        clipboard.set_text("héllo\nworld");
        assert!(shortcut(gdk::Key::V));
        pump(&|| input.borrow().ends_with(b"\x1b[201~"));
        assert_eq!(
            &*input.borrow(),
            "\x1b[200~héllo\rworld\x1b[201~".as_bytes()
        );
        input.borrow_mut().clear();
        let plain = gdk::ContentProvider::for_bytes(
            "text/plain;charset=utf-8",
            &glib::Bytes::from_static("Chrome café\nplain text".as_bytes()),
        );
        let html = gdk::ContentProvider::for_bytes(
            "text/html",
            &glib::Bytes::from_static(b"<b>Chrome text</b>"),
        );
        let image = gdk::ContentProvider::for_bytes(
            "image/png",
            &glib::Bytes::from_static(b"not decoded by text paste"),
        );
        clipboard
            .set_content(Some(&gdk::ContentProvider::new_union(&[
                plain, html, image,
            ])))
            .unwrap();
        assert!(keys.emit_by_name::<bool>(
            "key-pressed",
            &[&gdk::Key::v, &0u32, &gdk::ModifierType::CONTROL_MASK]
        ));
        pump(&|| input.borrow().ends_with(b"\x1b[201~"));
        assert_eq!(
            &*input.borrow(),
            "\x1b[200~Chrome café\rplain text\x1b[201~".as_bytes()
        );
        // Image-only paste still reaches Codex's own key handler.
        clipboard
            .set_content(Some(&gdk::ContentProvider::for_bytes(
                "image/png",
                &glib::Bytes::from_static(b"image"),
            )))
            .unwrap();
        assert!(!keys.emit_by_name::<bool>(
            "key-pressed",
            &[&gdk::Key::v, &0u32, &gdk::ModifierType::CONTROL_MASK]
        ));
        window.close();
    }
}
