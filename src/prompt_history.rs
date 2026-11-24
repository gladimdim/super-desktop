//! Submitted input belongs to the tmux session, not to its rendered output.
//! Session options are shared by the desktop and bridge, survive UI restarts,
//! and disappear with the session. Unknown editor operations invalidate a draft
//! rather than publishing a plausible but incorrect fragment.
use std::process::Command;

const OPTION: &str = "@super_desktop_last_prompt";
const MAX_CHARS: usize = 16_384;

/// Also adds the full text to the terminal's prompt history (`prompt_log`).
pub fn record(session: &str, text: &str) {
    crate::prompt_log::append(session, text);
    record_with(Command::new(crate::tmux::tmux_bin()), session, text);
}

fn record_with(mut command: Command, session: &str, text: &str) {
    if !crate::tmux::is_owned_session(session) {
        return;
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return;
    }
    let title = crate::tmux::truncate_prompt_title(&text);
    let target = format!("={session}:");
    let _ = command
        .args(["set-option", "-t", &target, OPTION, &title])
        .output();
}

pub fn last(session: &str) -> Option<String> {
    last_with(Command::new(crate::tmux::tmux_bin()), session)
}

fn last_with(mut command: Command, session: &str) -> Option<String> {
    if !crate::tmux::is_owned_session(session) {
        return None;
    }
    let target = format!("={session}:");
    let out = command
        .args(["show-options", "-qv", "-t", &target, OPTION])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Filter out harness-injected messages, bare slash commands, and short
/// interactive confirmation replies so typed history keeps real prompts.
pub fn is_substantive_harness_prompt(text: &str) -> bool {
    if !crate::harness_record::is_user_prompt(text) {
        return false;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "y" | "n" | "yes" | "no" | "a" | "all" | "always" | "once" | "ok" | "cancel" | "skip" | "q" | "quit"
    ) {
        return false;
    }
    if lower.len() == 1 && lower.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    true
}

/// Record typed VTE input only when it belongs in prompt history for `agent`.
pub fn record_typed(agent: &str, session: &str, text: &str) {
    if !crate::shell_title::is_regular(agent) && !is_substantive_harness_prompt(text) {
        return;
    }
    record(session, text);
}

/// Record typed remote/PTY input, checking tmux only when a short slash/reply
/// would otherwise pollute an AI harness's card title.
pub fn record_typed_session(session: &str, text: &str) {
    if is_substantive_harness_prompt(text) {
        record(session, text);
        return;
    }
    let info = crate::tmux::tmux_stdout(&[
        "display-message",
        "-p",
        "-t",
        session,
        "#{pane_current_command}\n#{@super_desktop_shell_tracking}",
    ]);
    let is_shell = info.is_some_and(|raw| {
        let mut lines = raw.lines();
        let cmd = lines.next().unwrap_or("").trim();
        let tracked = lines.next().unwrap_or("").trim() == "1";
        tracked || matches!(cmd, "bash" | "zsh" | "fish" | "sh" | "dash")
    });
    if is_shell {
        record(session, text);
    }
}

#[derive(Default)]
pub struct InputTracker {
    text: Vec<char>,
    cursor: usize,
    invalid: bool,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    paste: bool,
}

impl InputTracker {
    fn submit(&mut self, submitted: &mut Vec<String>) {
        if !self.invalid && self.utf8.is_empty() {
            let text: String = self.text.iter().collect();
            if !text.trim().is_empty() {
                submitted.push(text);
            }
        }
        *self = Self::default();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    fn delete_word_backward(&mut self) {
        while self.cursor > 0 && self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
        while self.cursor > 0 && !self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    fn delete_word_forward(&mut self) {
        while self.cursor < self.text.len() && self.text[self.cursor].is_whitespace() {
            self.text.remove(self.cursor);
        }
        while self.cursor < self.text.len() && !self.text[self.cursor].is_whitespace() {
            self.text.remove(self.cursor);
        }
    }

    fn move_word_left(&mut self) {
        while self.cursor > 0 && self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }

    fn move_word_right(&mut self) {
        while self.cursor < self.text.len() && self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < self.text.len() && !self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    fn insert_char(&mut self, ch: char) {
        if self.text.len() >= MAX_CHARS {
            self.invalid = true;
            return;
        }
        self.text.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut submitted = Vec::new();
        for &byte in bytes {
            if !self.escape.is_empty() {
                self.escape.push(byte);
                if self.escape.len() == 2 && matches!(byte, b'[' | b'O') {
                    continue;
                }
                if self.escape.len() > 2 && !(0x40..=0x7e).contains(&byte) {
                    if self.escape.len() > 32 {
                        self.invalid = true;
                        self.escape.clear();
                    }
                    continue;
                }
                match self.escape.as_slice() {
                    b"\x1b[200~" => self.paste = true,
                    b"\x1b[201~" => self.paste = false,
                    b"\x1b[13u" => self.submit(&mut submitted),
                    b"\x1b[13;2u" | b"\x1b[13;5u" | b"\x1b[27;2;13~" | b"\x1b\r" | b"\x1b\n" => {
                        self.insert_char('\n');
                    }
                    b"\x1b[127u" => self.backspace(),
                    b"\x1b[127;3u" | b"\x1b[127;5u" | b"\x1b\x7f" | b"\x1b\x08" => {
                        self.delete_word_backward();
                    }
                    b"\x1bb" | b"\x1b[1;3D" | b"\x1b[1;5D" => self.move_word_left(),
                    b"\x1bf" | b"\x1b[1;3C" | b"\x1b[1;5C" => self.move_word_right(),
                    b"\x1bd" => self.delete_word_forward(),
                    b"\x1b[D" | b"\x1bOD" => self.cursor = self.cursor.saturating_sub(1),
                    b"\x1b[C" | b"\x1bOC" => self.cursor = (self.cursor + 1).min(self.text.len()),
                    b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" => self.cursor = 0,
                    b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" => self.cursor = self.text.len(),
                    b"\x1b[3~" => {
                        if self.cursor < self.text.len() {
                            self.text.remove(self.cursor);
                        }
                    }
                    // History, completion, terminal replies and unknown editor modes
                    // cannot be reconstructed from bytes alone.
                    _ => self.invalid = true,
                }
                self.escape.clear();
                continue;
            }
            if byte == 0x1b {
                self.escape.push(byte);
                continue;
            }
            if self.paste {
                self.insert_byte(byte);
                continue;
            }
            match byte {
                b'\r' => self.submit(&mut submitted),
                b'\n' => self.insert_char('\n'),
                3 => *self = Self::default(), // Ctrl-C cancels the draft
                21 => {
                    // Ctrl-U removes only the part before the cursor.
                    self.text.drain(..self.cursor);
                    self.cursor = 0;
                }
                1 => self.cursor = 0,
                5 => self.cursor = self.text.len(),
                11 => self.text.truncate(self.cursor),
                8 | 127 => self.backspace(),
                23 => self.delete_word_backward(),
                0..=31 => self.invalid = true,
                _ => self.insert_byte(byte),
            }
        }
        submitted
    }

    fn insert_byte(&mut self, byte: u8) {
        if self.text.len() >= MAX_CHARS {
            self.invalid = true;
            return;
        }
        self.utf8.push(byte);
        match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                let chars: Vec<char> = text.chars().collect();
                self.utf8.clear();
                for ch in chars {
                    self.insert_char(ch);
                }
            }
            Err(error) if error.error_len().is_some() => {
                self.invalid = true;
                self.utf8.clear();
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submitted_prompt_is_shared_and_survives_response_output() {
        let server = crate::test_isolation::TmuxServer::new();
        let session = "sd_term_prompt_test";
        assert!(server
            .command()
            .args(["new-session", "-d", "-s", session])
            .status()
            .unwrap()
            .success());
        record_with(server.command(), session, "Fix the bug\nwith Unicode ✓");
        assert_eq!(
            last_with(server.command(), session).as_deref(),
            Some("Fix the bug with Unicode ✓")
        );
        assert!(server
            .command()
            .args([
                "send-keys",
                "-t",
                session,
                "printf 'response # not a prompt'",
                "Enter"
            ])
            .status()
            .unwrap()
            .success());
        record_with(server.command(), session, "  ");
        assert_eq!(
            last_with(server.command(), session).as_deref(),
            Some("Fix the bug with Unicode ✓")
        );
        assert_eq!(last_with(server.command(), "sd_term_other"), None);
        record_with(server.command(), session, "Next task");
        assert_eq!(
            last_with(server.command(), session).as_deref(),
            Some("Next task")
        );
    }

    #[test]
    fn only_submitted_input_changes_history() {
        let mut input = InputTracker::default();
        assert!(input.feed(b"explain the bug").is_empty());
        assert_eq!(input.feed(b"\r"), vec!["explain the bug"]);
        assert!(input.feed(b"\r").is_empty());
        assert!(input.feed(b"discard this\x03\r").is_empty());
    }

    #[test]
    fn editing_and_unicode_across_chunks() {
        let mut input = InputTracker::default();
        assert!(input.feed(b"fix typoX\x7f\x1b[D").is_empty());
        assert_eq!(input.feed(b"!\x1b[C\r"), vec!["fix typ!o"]);
        let word = "Привіт";
        for byte in word.as_bytes() {
            assert!(input.feed(&[*byte]).is_empty());
        }
        assert_eq!(input.feed(b"\r"), vec![word]);
        assert_eq!(input.feed(b"wrong\x15right\r"), vec!["right"]);
    }

    #[test]
    fn multiline_paste_is_one_prompt_and_escape_can_span_chunks() {
        let mut input = InputTracker::default();
        assert!(input.feed(b"\x1b[20").is_empty());
        assert!(input.feed(b"0~first\nsecond\x1b[201~").is_empty());
        assert_eq!(input.feed(b"\r"), vec!["first\nsecond"]);
    }

    #[test]
    fn history_completion_and_unknown_sequences_never_publish_fragments() {
        let mut input = InputTracker::default();
        assert!(input.feed(b"\x1b[A extra\r").is_empty());
        assert!(input.feed(b"comp\t partial\r").is_empty());
        assert_eq!(input.feed(b"next prompt\r"), vec!["next prompt"]);
        assert!(input.feed(&vec![b'a'; MAX_CHARS + 1]).is_empty());
        assert!(input.feed(b"\r").is_empty());
    }

    #[test]
    fn multiline_shift_enter_alt_editing_and_substantive_prompt_filter() {
        let mut input = InputTracker::default();
        assert!(input.feed(b"first line\nsecond").is_empty());
        assert!(input.feed(b"\x1b[13;2uthird").is_empty());
        assert!(input.feed(b"\x1b[27;2;13~fourth").is_empty());
        assert!(input.feed(b"\x1b\rfifth").is_empty());
        assert_eq!(
            input.feed(b"\x1b[13u"),
            vec!["first line\nsecond\nthird\nfourth\nfifth"]
        );

        assert_eq!(
            input.feed(b"alpha beta gamma\x1b\x7f\x1bb\x1bf delta\x1bd epsilon\r"),
            vec!["alpha beta delta epsilon"]
        );
        assert_eq!(
            input.feed(b"one two\x1b[1;5D\x1b[1;5C three\r"),
            vec!["one two three"]
        );

        for ignored in [
            "/clear",
            " /model sonnet",
            "y",
            "N",
            "yes",
            "no",
            "1",
            "<task-notification>done</task-notification>",
        ] {
            assert!(!is_substantive_harness_prompt(ignored), "should filter {ignored:?}");
        }
        assert!(is_substantive_harness_prompt("Fix the parser"));
        assert!(is_substantive_harness_prompt("Explain /etc/hosts routing"));
    }
}
