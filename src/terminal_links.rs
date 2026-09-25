//! Ctrl+click links in terminal cards.
//!
//! While Ctrl is held, an http(s) URL under the pointer is underlined with a
//! hand cursor, and Ctrl+left-click opens it in the default browser. Without
//! Ctrl the terminal behaves as before: clicks, drags and selections go to
//! tmux and the harness, and nothing is underlined.
//!
//! VTE does the matching and the underline: the URL regex is registered only
//! while Ctrl is down (VTE highlights registered matches on hover even while
//! tmux has mouse tracking on). The click is caught in the capture phase and
//! claimed, so tmux never sees it. Only plain http(s) links with a host and no
//! credentials open (`clean_link`, shared with the Files & links list).
//!
//! Links are matched per terminal row: tmux redraws a wrapped URL as separate
//! rows, so only the part on one row is found.

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use std::cell::Cell;
use std::rc::Rc;
use vte4::prelude::*;

/// PCRE2 compile flags VTE expects for match regexes.
const PCRE2_CASELESS: u32 = 0x0000_0008;
const PCRE2_MULTILINE: u32 = 0x0000_0400;
const PCRE2_UTF: u32 = 0x0008_0000;

/// A URL up to whitespace or a quote, not ending in sentence punctuation.
const URL_PATTERN: &str = r#"https?://[^\s<>"'`]*[^\s<>"'`.,;:!?)\]}]"#;

/// The http(s) link a candidate token names, trimmed of trailing punctuation
/// and unbalanced closing brackets; `None` for anything that is not a plain
/// web link (other schemes, no host, embedded credentials, oversized).
pub fn clean_link(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let start = [lower.find("https://"), lower.find("http://")]
        .into_iter()
        .flatten()
        .min()?;
    let mut candidate = &token[start..];
    if candidate.len() > 8192 {
        return None;
    }
    loop {
        let old = candidate;
        candidate = candidate.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        for (close, open) in [(')', '('), (']', '['), ('}', '{')] {
            if candidate.ends_with(close)
                && candidate.matches(close).count() > candidate.matches(open).count()
            {
                candidate = &candidate[..candidate.len() - 1];
            }
        }
        if old == candidate {
            break;
        }
    }
    let url = reqwest::Url::parse(candidate).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(candidate.to_string())
}

struct Links {
    regex: Option<vte4::Regex>,
    active: Cell<bool>,
}

impl Links {
    /// Register the URL regex while Ctrl is held, and drop it (clearing any
    /// underline) when Ctrl is released.
    fn set_active(&self, term: &vte4::Terminal, on: bool) {
        if self.active.get() == on {
            return;
        }
        let Some(regex) = &self.regex else { return };
        if on {
            let tag = term.match_add_regex(regex, 0);
            term.match_set_cursor_name(tag, "pointer");
        } else {
            term.match_remove_all();
        }
        self.active.set(on);
    }
}

/// The link under view position (`x`, `y`), if Ctrl highlighting is active.
fn link_at(term: &vte4::Terminal, x: f64, y: f64) -> Option<String> {
    clean_link(term.check_match_at(x, y).0?.as_str())
}

fn open(uri: String) {
    glib::spawn_future_local(async move {
        if let Err(error) =
            gio::AppInfo::launch_default_for_uri_future(&uri, None::<&gio::AppLaunchContext>).await
        {
            eprintln!("super-desktop: could not open {uri}: {error}");
        }
    });
}

fn has_ctrl(state: gdk::ModifierType) -> bool {
    state.contains(gdk::ModifierType::CONTROL_MASK)
}

/// Enable Ctrl+hover highlighting and Ctrl+click opening on a card terminal.
pub fn install(term: &vte4::Terminal) {
    let flags = PCRE2_CASELESS | PCRE2_MULTILINE | PCRE2_UTF;
    let regex = match vte4::Regex::for_match(URL_PATTERN, flags) {
        Ok(regex) => Some(regex),
        Err(error) => {
            eprintln!("super-desktop: link matching disabled: {error}");
            None
        }
    };
    let links = Rc::new(Links { regex, active: Cell::new(false) });

    // Ctrl pressed or released while the terminal has the keyboard.
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let (l, t) = (Rc::clone(&links), term.downgrade());
    keys.connect_modifiers(move |_, state| {
        if let Some(term) = t.upgrade() {
            l.set_active(&term, has_ctrl(state));
        }
        glib::Propagation::Proceed
    });
    term.add_controller(keys);

    // The pointer's own modifier state covers Ctrl held while another widget
    // has the keyboard; leaving the terminal ends highlighting.
    let motion = gtk4::EventControllerMotion::new();
    let (l, t) = (Rc::clone(&links), term.downgrade());
    motion.connect_motion(move |controller, _, _| {
        if let Some(term) = t.upgrade() {
            l.set_active(&term, has_ctrl(controller.current_event_state()));
        }
    });
    let (l, t) = (Rc::clone(&links), term.downgrade());
    motion.connect_leave(move |_| {
        if let Some(term) = t.upgrade() {
            l.set_active(&term, false);
        }
    });
    term.add_controller(motion);

    let focus = gtk4::EventControllerFocus::new();
    let (l, t) = (Rc::clone(&links), term.downgrade());
    focus.connect_leave(move |_| {
        if let Some(term) = t.upgrade() {
            l.set_active(&term, false);
        }
    });
    term.add_controller(focus);

    // Ctrl+left-click on a link: open it and keep the click from tmux.
    let click = gtk4::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    click.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let (l, t) = (Rc::clone(&links), term.downgrade());
    click.connect_pressed(move |gesture, presses, x, y| {
        let Some(term) = t.upgrade() else { return };
        if presses != 1 || !has_ctrl(gesture.current_event_state()) {
            return;
        }
        l.set_active(&term, true);
        if let Some(uri) = link_at(&term, x, y) {
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            open(uri);
        }
    });
    term.add_controller(click);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_link_keeps_web_links_and_trims_sentence_punctuation() {
        assert_eq!(clean_link("https://example.org/a.").as_deref(), Some("https://example.org/a"));
        assert_eq!(
            clean_link("(see https://en.wikipedia.org/wiki/Rust_(language))").as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
        assert_eq!(clean_link("HTTP://Example.org/x?y=1,").as_deref(), Some("HTTP://Example.org/x?y=1"));
        assert!(clean_link("file:///etc/passwd").is_none());
        assert!(clean_link("javascript:alert(1)").is_none());
        assert!(clean_link("https://user:secret@example.org").is_none());
        assert!(clean_link("https://").is_none());
        assert!(clean_link(&format!("https://example.org/{}", "a".repeat(9000))).is_none());
    }

    #[test]
    fn url_pattern_compiles_for_vte() {
        crate::gtk_test::run_in_child_process("terminal_links::tests::ctrl_hover_finds_links_child");
    }

    /// A real VTE: text fed as terminal output, the link found only while the
    /// Ctrl highlighting is active, and at the link's own cells only.
    #[test]
    fn ctrl_hover_finds_links_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();
        let term = vte4::Terminal::new();
        install(&term);
        let window = gtk4::Window::new();
        window.set_default_size(800, 200);
        window.set_child(Some(&term));
        window.present();
        let pump = || {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
            }
        };
        pump();
        term.feed(b"open https://example.org/docs?q=1. now\r\n");
        pump();
        let (w, h) = (term.char_width() as f64, term.char_height() as f64);
        let at = |col: f64| (col * w + w / 2.0, h / 2.0);
        let links = Links {
            regex: vte4::Regex::for_match(URL_PATTERN, PCRE2_CASELESS | PCRE2_MULTILINE | PCRE2_UTF).ok(),
            active: Cell::new(false),
        };
        assert!(links.regex.is_some(), "the URL pattern must compile as a VTE match regex");
        let (x, y) = at(10.0);
        assert_eq!(link_at(&term, x, y), None, "no Ctrl, no link");
        links.set_active(&term, true);
        assert_eq!(link_at(&term, x, y).as_deref(), Some("https://example.org/docs?q=1"));
        let (x, y) = at(1.0);
        assert_eq!(link_at(&term, x, y), None, "text before the link is not a link");
        links.set_active(&term, false);
        let (x, y) = at(10.0);
        assert_eq!(link_at(&term, x, y), None, "releasing Ctrl ends highlighting");
        window.close();
    }
}
