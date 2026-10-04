//! Bounded path hints from terminal prose; callers verify type and workspace access.
use std::collections::HashSet;

const MAX_TEXT: usize = 128 * 1024;
const MAX_PATH: usize = 4096;
const MAX_CANDIDATES: usize = 256;

pub fn candidates(text: &str, supported: impl Fn(&str) -> bool) -> Vec<String> {
    let text: String = text.chars().take(MAX_TEXT).collect();
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |path: &str| {
        let path = path.trim();
        if path.len() <= MAX_PATH
            && !path.chars().any(char::is_control)
            && !path.contains("://")
            && supported(path)
            && found.len() < MAX_CANDIDATES
            && seen.insert(path.to_owned())
        {
            found.push(path.to_owned());
        }
    };
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
                    add(path);
                    if path.contains('\n') && path.lines().count() <= 8 {
                        let joined: String = path.lines().map(str::trim).collect();
                        add(&joined);
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
    // A slash at a line break is a strong continuation hint even without quotes.
    let mut joined = String::with_capacity(text.len());
    for line in text.lines() {
        let line = line.trim_end();
        if joined.ends_with('/') {
            joined.push_str(line.trim_start());
        } else {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(line);
        }
    }
    for word in joined.split_whitespace() {
        let path = word
            .trim_start_matches(|c: char| "()[]{}<>\"'`,;*".contains(c))
            .trim_end_matches(|c: char| "()[]{}<>\"'`,;.!?*".contains(c));
        add(path);
        // Source references often include a line and optionally a column.
        let mut without_location = path;
        for _ in 0..2 {
            let Some((prefix, suffix)) = without_location.rsplit_once(':') else {
                break;
            };
            if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
                break;
            }
            without_location = prefix;
            add(without_location);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(text: &str) -> Vec<String> {
        candidates(text, |p| {
            ["png", "json", "wav", "rs", "md"]
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
