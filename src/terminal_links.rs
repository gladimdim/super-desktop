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
    if candidate.len() > 8192
        || candidate
            .chars()
            .any(|c| c.is_whitespace() || (c as u32) < 0x20 || c as u32 == 0x7f)
    {
        return None;
    }
    if let Some(after_scheme) = candidate
        .split_once("://")
        .map(|(_, rest)| rest)
    {
        let authority = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        if authority.contains('\\') {
            return None;
        }
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

/// Find an HTTP(S) link at (`target_row`, `target_col`) across contiguous
/// hard-wrapped lines of `cols` width, returning `clean_link` on the token.
pub fn wrapped_link_at(rows: &[&str], target_row: usize, target_col: usize, cols: usize) -> Option<String> {
    if target_row >= rows.len() || cols < 16 {
        return None;
    }
    fn trim_cr(s: &str) -> &str {
        s.trim_end_matches('\r')
    }
    let is_wrap_join = |idx: usize| -> bool {
        let cur = trim_cr(rows[idx]);
        let next = trim_cr(rows[idx + 1]);
        let len = cur.chars().count();
        let ends_non_ws = cur.chars().next_back().is_some_and(|c| !c.is_whitespace());
        let next_starts_non_ws = next.chars().next().is_some_and(|c| !c.is_whitespace());
        len >= cols.saturating_sub(1) && ends_non_ws && next_starts_non_ws
    };

    let mut start = target_row;
    while start > 0 && is_wrap_join(start - 1) {
        start -= 1;
    }
    let mut end = target_row;
    while end + 1 < rows.len() && is_wrap_join(end) {
        end += 1;
    }

    let mut combined = String::new();
    let mut click_char_idx = 0usize;
    for (offset_row, raw) in rows[start..=end].iter().enumerate() {
        let line = trim_cr(raw);
        let row_idx = start + offset_row;
        let line_chars = line.chars().count();
        if row_idx < target_row {
            click_char_idx += line_chars;
        } else if row_idx == target_row {
            click_char_idx += target_col.min(line_chars);
        }
        combined.push_str(line);
    }

    let chars: Vec<char> = combined.chars().collect();
    if chars.is_empty() || click_char_idx >= chars.len() {
        return None;
    }
    let is_url_char = |c: char| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | '`');
    let mut left = click_char_idx;
    if !is_url_char(chars[left]) {
        return None;
    }
    while left > 0 && is_url_char(chars[left - 1]) {
        left -= 1;
    }
    let mut right = click_char_idx + 1;
    while right < chars.len() && is_url_char(chars[right]) {
        right += 1;
    }
    let token: String = chars[left..right].iter().collect();
    clean_link(&token)
}

fn wrapped_link_in_term(term: &vte4::Terminal, x: f64, y: f64) -> Option<String> {
    let cw = term.char_width() as f64;
    let ch = term.char_height() as f64;
    if cw <= 0.0 || ch <= 0.0 || x < 0.0 || y < 0.0 {
        return None;
    }
    let col = (x / cw).floor() as usize;
    let view_row = (y / ch).floor() as usize;
    let cols = term.column_count().max(1) as usize;
    let visible_rows = term.row_count().max(1) as usize;
    let text = term.text_format(vte4::Format::Text)?;
    let rows: Vec<&str> = text.split('\n').collect();
    let top_row = if rows.len() <= visible_rows {
        0
    } else {
        term.vadjustment()
            .map(|adj| adj.value().round().max(0.0) as usize)
            .unwrap_or(0)
            .min(rows.len().saturating_sub(visible_rows))
    };
    wrapped_link_at(&rows, top_row + view_row, col, cols)
}

/// The link under view position (`x`, `y`), if Ctrl highlighting is active.
fn link_at(term: &vte4::Terminal, x: f64, y: f64) -> Option<String> {
    let single = clean_link(term.check_match_at(x, y).0?.as_str());
    match (single, wrapped_link_in_term(term, x, y)) {
        (Some(s), Some(w)) if w.len() > s.len() => Some(w),
        (s, w) => s.or(w),
    }
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
        if let Some(uri) = link_at(&term, x, y).or_else(|| wrapped_link_in_term(&term, x, y)) {
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
        assert!(clean_link("https://evil.com\\@good.com").is_none());
        assert!(clean_link("https://example.org/a\nb").is_none());
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

    #[test]
    fn wrapped_link_joins_soft_wrapped_urls_across_rows_without_joining_short_lines() {
        // 2-row wrapped URL clicked on row 1 or row 2:
        let rows2 = ["open https://example.org/a/b", "cdef/ghij?k=1. done"];
        let r0_len = rows2[0].chars().count();
        assert_eq!(
            wrapped_link_at(&rows2, 0, 10, r0_len).as_deref(),
            Some("https://example.org/a/bcdef/ghij?k=1")
        );
        assert_eq!(
            wrapped_link_at(&rows2, 1, 3, r0_len).as_deref(),
            Some("https://example.org/a/bcdef/ghij?k=1")
        );

        // 3-row wrapped URL with trailing sentence period:
        let rows3 = [
            "https://example.org/",
            "very/long/path/segmt",
            "end/page?q=42).",
        ];
        let cols = rows3[0].chars().count();
        assert_eq!(
            wrapped_link_at(&rows3, 2, 2, cols).as_deref(),
            Some("https://example.org/very/long/path/segmtend/page?q=42")
        );

        // Short line must not join with the line below it.
        let short = ["see https://example.org/a", "more/text"];
        assert_eq!(
            wrapped_link_at(&short, 0, 10, 40).as_deref(),
            Some("https://example.org/a")
        );
        assert_eq!(wrapped_link_at(&short, 1, 2, 40), None);
    }
}
