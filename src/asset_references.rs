//! Bounded path hints from terminal prose; callers verify type and workspace access.
use std::collections::HashSet;

const MAX_TEXT: usize = 128 * 1024;
const MAX_PATH: usize = 4096;
const MAX_CANDIDATES: usize = 256;
/// Lines one path may span when a harness wraps it with hard newlines.
const MAX_WRAPPED_LINES: usize = 8;

struct Found<F> {
    supported: F,
    found: Vec<String>,
    seen: HashSet<String>,
}

impl<F: Fn(&str) -> bool> Found<F> {
    fn add(&mut self, path: &str) {
        let path = path.trim();
        if path.len() <= MAX_PATH
            && !path.chars().any(char::is_control)
            && !path.contains("://")
            && (self.supported)(path)
            && self.found.len() < MAX_CANDIDATES
            && self.seen.insert(path.to_owned())
        {
            self.found.push(path.to_owned());
        }
    }

    /// Adds a whitespace-free word without prose punctuation or a source location.
    fn add_word(&mut self, word: &str) {
        let path = trim_word(word);
        self.add(path);
        let mut without_location = path;
        while without_location != strip_location(without_location) {
            without_location = strip_location(without_location);
            self.add(without_location);
        }
    }

    fn is_reference(&self, word: &str) -> bool {
        let path = trim_word(word);
        (self.supported)(path) || (self.supported)(strip_location(strip_location(path)))
    }
}

fn trim_word(word: &str) -> &str {
    word.trim_start_matches(|c: char| "()[]{}<>\"'`,;*".contains(c))
        .trim_end_matches(|c: char| "()[]{}<>\"'`,;.!?*".contains(c))
}

/// Source references often include a line and optionally a column.
fn strip_location(path: &str) -> &str {
    match path.rsplit_once(':') {
        Some((prefix, suffix)) if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) => prefix,
        _ => path,
    }
}

/// Box-drawing borders that TUIs draw around wrapped text.
fn is_frame(c: char) -> bool {
    c.is_whitespace() || ('\u{2500}'..='\u{257f}').contains(&c)
}

pub fn candidates(text: &str, supported: impl Fn(&str) -> bool) -> Vec<String> {
    let text: String = text.chars().take(MAX_TEXT).collect();
    let mut found = Found { supported, found: Vec::new(), seen: HashSet::new() };
    // Preserve spaces in quoted paths and Markdown/parenthesized targets.
    // Also try the unwrapped spelling when a harness inserts hard newlines.
    for (open, close) in [('`', '`'), ('"', '"'), ('\'', '\''), ('(', ')'), ('<', '>')] {
        let mut start = None;
        for (index, ch) in text.char_indices() {
            if let Some(begin) = start {
                if index - begin > MAX_PATH {
                    start = None;
                } else if ch == close {
                    let path = &text[begin..index];
                    found.add(path);
                    if path.contains('\n') && path.lines().count() <= MAX_WRAPPED_LINES {
                        let joined: String = path.lines().map(str::trim).collect();
                        found.add(&joined);
                    }
                    start = None;
                    continue;
                }
            }
            if ch == open && start.is_none() {
                start = Some(index + ch.len_utf8());
            }
        }
    }
    let lines: Vec<&str> = text.lines().map(|line| line.trim_matches(is_frame)).collect();
    for word in lines.iter().flat_map(|line| line.split_whitespace()) {
        found.add_word(word);
    }
    // Harnesses such as Claude Code and Codex wrap long paths themselves, at any
    // character and with indented continuation lines, so tmux cannot join them.
    // Try the last word of a line followed by the first word of the next ones.
    // These come after every single-line candidate, so they never crowd one
    // out, and two complete references on adjacent lines are never joined.
    for (index, line) in lines.iter().enumerate() {
        let Some(head) = line.split_whitespace().last() else {
            continue;
        };
        let mut joined = head.to_owned();
        for next in lines.iter().skip(index + 1).take(MAX_WRAPPED_LINES - 1) {
            let Some(tail) = next.split_whitespace().next() else {
                break;
            };
            if joined.ends_with(|c: char| ",;)]}>\"'`!?".contains(c))
                || joined.len() + tail.len() > MAX_PATH
                || (found.is_reference(&joined) && found.is_reference(tail))
            {
                break;
            }
            joined.push_str(tail);
            found.add_word(&joined);
            // A path continues past this line only if it filled the whole line.
            if tail.len() != next.len() {
                break;
            }
        }
    }
    found.found
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(text: &str) -> Vec<String> {
        candidates(text, |p| {
            ["png", "json", "wav", "rs", "md", "js"]
                .iter()
                .any(|ext| p.ends_with(&format!(".{ext}")))
        })
    }
    #[test]
    fn imagegen_wrapped_paths_and_sentence_punctuation() {
        let found = paths("Created with imagegen: image (game/assets/art/menu/\n  prisoners_at_dawn_v1.png) · prompt (game/assets/art/menu/\n  prisoners_at_dawn_v1.prompt.json).");
        assert!(found.contains(&"game/assets/art/menu/prisoners_at_dawn_v1.png".into()));
        assert!(found.contains(&"game/assets/art/menu/prisoners_at_dawn_v1.prompt.json".into()));
    }
    #[test]
    fn wrapped_names_spaces_audio_and_locations() {
        let found = paths("`art/prisoners_at_\n  dawn.png` \"audio/my sound.wav\" [image](art/test.png). src/main.rs:12:3 sounds/\n  dawn.wav");
        for path in [
            "art/prisoners_at_dawn.png",
            "audio/my sound.wav",
            "art/test.png",
            "src/main.rs",
            "sounds/dawn.wav",
        ] {
            assert!(found.contains(&path.into()), "{path}: {found:?}");
        }
    }
    #[test]
    fn harness_hard_wraps_inside_names_extensions_and_locations() {
        // Claude Code tool output breaks at the column, mid-word, and indents
        // the continuation; prose lines are padded with trailing spaces.
        let found = paths(concat!(
            "⏺ Write(game/assets/art/menu/prisoners_at_d\n",
            "       awn_v1.png)\n",
            "  ⎿  Wrote 12 lines to src/very/long/module/asset_refer\n",
            "     ences.rs                                          \n",
            "  The settings live in config/harness/settings.js\n",
            "  on and the error is at src/main.r\n",
            "  s:12:3, see the log.\n",
            "  │ docs/notes/super_long_name_that_spans_more_than_one_li │\n",
            "  │ ne_of_the_box_and_then_some_more_of_it_until_the_end_o │\n",
            "  │ f_it.md and more                                       │\n",
        ));
        for path in [
            "game/assets/art/menu/prisoners_at_dawn_v1.png",
            "src/very/long/module/asset_references.rs",
            "config/harness/settings.json",
            "src/main.rs",
            "docs/notes/super_long_name_that_spans_more_than_one_line_of_the_box_and_then_some_more_of_it_until_the_end_of_it.md",
        ] {
            assert!(found.contains(&path.into()), "{path}: {found:?}");
        }
    }
    #[test]
    fn hard_wrap_joins_stay_behind_single_line_candidates() {
        // Adjacent complete references are separate files, and sentence ends
        // are not continued; a speculative join never takes a direct slot.
        assert_eq!(paths("a/one.png\nb/two.png"), ["a/one.png", "b/two.png"]);
        assert_eq!(paths("  see a/one.png,\n  b/two.png"), ["a/one.png", "b/two.png"]);
        assert_eq!(paths("art/one\n  two.png then"), ["two.png", "art/onetwo.png"]);
        let direct: String = (0..MAX_CANDIDATES).map(|i| format!("{i}.png ")).collect();
        let found = paths(&format!("{direct}\nart/wrapped_na\n  me.png"));
        assert_eq!(found.len(), MAX_CANDIDATES);
        assert!(!found.contains(&"art/wrapped_name.png".into()));
        let long = format!("{}\n", "a".repeat(80)).repeat(MAX_WRAPPED_LINES + 2);
        let found = paths(&format!("{long}b.png"));
        let longest = found.iter().map(String::len).max().unwrap();
        assert_eq!(longest, 80 * (MAX_WRAPPED_LINES - 1) + "b.png".len(), "{found:?}");
    }
    #[test]
    fn bounded_deduplicated_and_does_not_join_unrelated_lines() {
        let found = paths(&format!("{} other.md", "same.png ".repeat(300)));
        assert_eq!(found, ["same.png", "other.md"]);
        assert_eq!(paths("a/one.png\nb/two.png"), ["a/one.png", "b/two.png"]);
        assert!(paths("https://example.org/test.png").is_empty());
        assert_eq!(
            paths(".hidden.png ../outside.png"),
            [".hidden.png", "../outside.png"]
        );
        assert_eq!(
            paths(&(0..400).map(|i| format!("{i}.png ")).collect::<String>()).len(),
            MAX_CANDIDATES
        );
        assert!(paths(&format!("{}last.png", " ".repeat(MAX_TEXT))).is_empty());
    }
}
