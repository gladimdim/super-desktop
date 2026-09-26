//! Desktop clipboard shortcuts shared by local and remote VTE terminals.
//!
//! The shortcuts are Omarchy's terminal ones: Ctrl+Shift+C/V, and
//! Ctrl+Insert/Shift+Insert, which Omarchy's Super+C/Super+V send to a
//! terminal. Super+V sends Ctrl+V instead when the window under the overlay is
//! not a terminal, so a harness card pastes text on Ctrl+V too.
use gtk4::{gdk, glib, prelude::*};
use vte4::prelude::*;

#[derive(Debug, PartialEq)]
enum Action {
    Copy,
    Paste,
}

/// `paste_on_control_v`: plain Ctrl+V pastes text. Only harness cards do: a
/// shell card keeps Ctrl+V for its programs (Vim's block selection, the
/// shell's quoted insert). `text_available` is asked only for Ctrl+V.
fn action(
    key: gdk::Key,
    modifiers: gdk::ModifierType,
    paste_on_control_v: bool,
    text_available: impl Fn() -> bool,
) -> Option<Action> {
    // Ignore Caps Lock and pointer-button state, but never steal Alt/Super chords.
    let modifiers = modifiers
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::HYPER_MASK
            | gdk::ModifierType::META_MASK);
    let control = gdk::ModifierType::CONTROL_MASK;
    let shift = gdk::ModifierType::SHIFT_MASK;
    match key {
        gdk::Key::c | gdk::Key::C if modifiers == control | shift => Some(Action::Copy),
        gdk::Key::v | gdk::Key::V if modifiers == control | shift => Some(Action::Paste),
        gdk::Key::Insert | gdk::Key::KP_Insert if modifiers == control => Some(Action::Copy),
        // VTE's own Shift+Insert pastes the primary selection, not what was copied.
        gdk::Key::Insert | gdk::Key::KP_Insert if modifiers == shift || modifiers == control | shift => {
            Some(Action::Paste)
        }
        // Claude Code and Codex read a clipboard image on Ctrl+V themselves, so
        // a clipboard without text still gets the raw key.
        gdk::Key::v | gdk::Key::V
            if modifiers == control && paste_on_control_v && text_available() =>
        {
            Some(Action::Paste)
        }
        _ => None,
    }
}

/// The key a shortcut means in any layout. A non-Latin layout turns Ctrl+V
/// into Ctrl+м (Ukrainian), so a letter outside ASCII stands for the Latin
/// letter on the same physical key in another layout, as in GTK's own
/// shortcuts. `same_key` lists the key's unshifted keyvals in every layout.
fn shortcut_key(key: gdk::Key, same_key: impl FnOnce() -> Vec<gdk::Key>) -> gdk::Key {
    let Some(letter) = key.to_unicode() else {
        return key;
    };
    if letter.is_ascii() {
        return key;
    }
    same_key()
        .into_iter()
        .find(|other| other.to_unicode().is_some_and(|c| c.is_ascii_alphabetic()))
        .unwrap_or(key)
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

pub fn install(terminal: &vte4::Terminal, paste_on_control_v: bool) -> gtk4::EventControllerKey {
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = terminal.downgrade();
    keys.connect_key_pressed(move |_, key, keycode, modifiers| {
        let Some(terminal) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let key = shortcut_key(key, || {
            terminal
                .display()
                .map_keycode(keycode)
                .unwrap_or_default()
                .into_iter()
                .filter(|(position, _)| position.level() == 0)
                .map(|(_, keyval)| keyval)
                .collect()
        });
        // Paste text when Chrome (or another app) offers text, including rich
        // selections. Check formats only: never read or replace the clipboard
        // on key routing.
        let text_available = || has_text(&terminal.clipboard().formats());
        let Some(action) = action(key, modifiers, paste_on_control_v, text_available) else {
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
        let alt = gdk::ModifierType::ALT_MASK;
        let text = || true;
        assert_eq!(action(gdk::Key::C, ctrl | shift, false, text), Some(Action::Copy));
        assert_eq!(action(gdk::Key::v, ctrl | shift, false, text), Some(Action::Paste));
        assert_eq!(
            action(gdk::Key::c, ctrl | shift | gdk::ModifierType::LOCK_MASK, false, text),
            Some(Action::Copy)
        );
        for key in [gdk::Key::c, gdk::Key::v] {
            assert_eq!(action(key, ctrl, false, text), None);
            assert_eq!(action(key, shift, false, text), None);
        }
        for key in [gdk::Key::c, gdk::Key::v, gdk::Key::Insert] {
            assert_eq!(action(key, ctrl | shift | alt, true, text), None);
            assert_eq!(action(key, ctrl | gdk::ModifierType::SUPER_MASK, true, text), None);
        }
        assert_eq!(action(gdk::Key::c, ctrl, true, text), None, "Ctrl+C interrupts");
        assert_eq!(action(gdk::Key::Insert, gdk::ModifierType::empty(), true, text), None);
    }

    /// Omarchy's Super+C and Super+V send Ctrl+Insert and Shift+Insert to a
    /// terminal, and its terminals copy and paste the clipboard on them. VTE's
    /// own Shift+Insert pastes the primary selection instead.
    #[test]
    fn clipboard_insert_shortcuts_follow_omarchy_terminals() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let shift = gdk::ModifierType::SHIFT_MASK;
        for paste_on_control_v in [false, true] {
            for key in [gdk::Key::Insert, gdk::Key::KP_Insert] {
                let asked = std::cell::Cell::new(false);
                let text = || {
                    asked.set(true);
                    false
                };
                assert_eq!(action(key, ctrl, paste_on_control_v, text), Some(Action::Copy));
                assert_eq!(action(key, shift, paste_on_control_v, || false), Some(Action::Paste));
                assert_eq!(action(key, ctrl | shift, paste_on_control_v, || false), Some(Action::Paste));
                assert!(!asked.get(), "only Ctrl+V looks at the clipboard");
            }
        }
    }

    /// Omarchy's Super+V sends Ctrl+V when the window under the overlay is not
    /// a terminal, which is the usual case after copying in a browser.
    #[test]
    fn clipboard_control_v_pastes_text_in_harness_cards_only() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(action(gdk::Key::v, ctrl, true, || true), Some(Action::Paste));
        assert_eq!(
            action(gdk::Key::V, ctrl | gdk::ModifierType::LOCK_MASK, true, || true),
            Some(Action::Paste)
        );
        // An image-only clipboard reaches the harness, which attaches it.
        assert_eq!(action(gdk::Key::v, ctrl, true, || false), None);
        // A shell card keeps Ctrl+V for Vim and the shell.
        assert_eq!(action(gdk::Key::v, ctrl, false, || true), None);
        assert!(!has_text(&gdk::ContentFormats::new(&["image/png"])));
        assert!(has_text(&gdk::ContentFormats::new(&[
            "text/html",
            "text/plain;charset=utf-8",
            "image/png"
        ])));
    }

    /// With the Ukrainian layout active the V key arrives as м, and with Shift
    /// as М. It still pastes, like GTK's own shortcuts.
    #[test]
    fn clipboard_shortcuts_work_in_non_latin_layouts() {
        let v_key = || vec![gdk::Key::v, gdk::Key::Cyrillic_em];
        let c_key = || vec![gdk::Key::c, gdk::Key::Cyrillic_es];
        assert_eq!(shortcut_key(gdk::Key::Cyrillic_em, v_key), gdk::Key::v);
        assert_eq!(shortcut_key(gdk::Key::Cyrillic_EM, v_key), gdk::Key::v);
        assert_eq!(shortcut_key(gdk::Key::Cyrillic_es, c_key), gdk::Key::c);
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(
            action(shortcut_key(gdk::Key::Cyrillic_EM, v_key), ctrl | shift, false, || true),
            Some(Action::Paste)
        );
        // A Latin layout, or a key with no letter, is taken as it is.
        let unused = || -> Vec<gdk::Key> { panic!("a Latin key needs no lookup") };
        assert_eq!(shortcut_key(gdk::Key::v, unused), gdk::Key::v);
        assert_eq!(shortcut_key(gdk::Key::Insert, unused), gdk::Key::Insert);
        assert_eq!(shortcut_key(gdk::Key::period, unused), gdk::Key::period);
        // A non-Latin key with no Latin letter anywhere stays itself.
        assert_eq!(
            shortcut_key(gdk::Key::Cyrillic_em, || vec![gdk::Key::Cyrillic_em]),
            gdk::Key::Cyrillic_em
        );
    }

    /// Copy and paste through a real emulator, on a private Broadway display
    /// whose clipboard this test owns.
    #[test]
    fn clipboard_round_trip() {
        crate::gtk_test::run_in_child_process("terminal_clipboard::tests::clipboard_round_trip_inner");
    }

    #[test]
    fn clipboard_round_trip_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().expect("GTK display");
        let terminal = vte4::Terminal::new();
        let shell = vte4::Terminal::new();
        let cards = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        cards.append(&terminal);
        cards.append(&shell);
        let window = gtk4::Window::new();
        window.set_child(Some(&cards));
        window.present();
        let keys = install(&terminal, true);
        let shell_keys = install(&shell, false);
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
        let press = |keys: &gtk4::EventControllerKey, key: gdk::Key, modifiers: gdk::ModifierType| {
            keys.emit_by_name::<bool>("key-pressed", &[&key, &0u32, &modifiers])
        };
        let shortcut =
            |key| press(&keys, key, gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
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
        // Omarchy's Super+V in a terminal: Shift+Insert pastes the clipboard.
        input.borrow_mut().clear();
        clipboard.set_text("from super v");
        assert!(press(&keys, gdk::Key::Insert, gdk::ModifierType::SHIFT_MASK));
        pump(&|| input.borrow().ends_with(b"\x1b[201~"));
        assert_eq!(&*input.borrow(), b"\x1b[200~from super v\x1b[201~");
        // Image-only paste still reaches the harness's own key handler.
        clipboard
            .set_content(Some(&gdk::ContentProvider::for_bytes(
                "image/png",
                &glib::Bytes::from_static(b"image"),
            )))
            .unwrap();
        assert!(!press(&keys, gdk::Key::v, gdk::ModifierType::CONTROL_MASK));
        // A shell card leaves Ctrl+V to its programs, even with text copied.
        clipboard.set_text("not pasted by ctrl+v");
        assert!(!press(&shell_keys, gdk::Key::v, gdk::ModifierType::CONTROL_MASK));
        assert!(press(&shell_keys, gdk::Key::Insert, gdk::ModifierType::SHIFT_MASK));
        window.close();
    }
}
