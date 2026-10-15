//! `plugin check`'s second look: QML that loads fine but is wrong in SUPER
//! DESKTOP (an error, which fails the check) or usually unwanted (advice).
//! Each rule comes from a mistake plugins have really made.
use super::manifest::{Manifest, Slot};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Fails the check.
    Error,
    /// Reported; the check still passes.
    Advice,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub level: Level,
    pub file: PathBuf,
    pub line: usize,
    pub message: String,
    pub hint: String,
}

/// The QML of a plugin, every rule applied.
pub fn lint(manifest: &Manifest) -> Vec<Finding> {
    let mut findings = Vec::new();
    let files = qml_files(&manifest.dir);
    let sources: Vec<(PathBuf, Source)> = files.iter().filter_map(|f| Some((f.clone(), Source::read(f)?))).collect();
    for (file, source) in &sources {
        file_rules(file, source, &mut findings);
    }
    // Rules about where a file is shown: the surface's file and the
    // components it uses.
    let mut bar = BTreeSet::new();
    let mut always = BTreeSet::new();
    for surface in &manifest.surfaces {
        let reached = reachable(&manifest.qml_path(surface));
        if matches!(surface.slot, Slot::DockLeft | Slot::DockRight | Slot::ToolbarItem) {
            bar.extend(reached.iter().cloned());
        }
        if matches!(surface.slot, Slot::DockLeft | Slot::DockRight | Slot::ToolbarItem | Slot::Card) {
            always.extend(reached);
        }
    }
    for (file, source) in &sources {
        let canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
        if bar.contains(&canonical) {
            for at in source.find_word("ToolTip") {
                findings.push(Finding {
                    rule: "bar-tooltip", level: Level::Error, file: file.clone(), line: source.line(at),
                    message: "a ToolTip in a top-bar item or dock: it covers the button and swallows its clicks".into(),
                    hint: "Remove it and set \"tooltip\" on the surface in plugin.json; SUPER DESKTOP shows it on hover.".into(),
                });
                break;
            }
        }
        if always.contains(&canonical) {
            for frame in source.frames.iter().filter(|f| f.kind == Kind::Type && f.name == "Timer") {
                let body = &source.stripped[frame.start..frame.end];
                // `running: true`: a condition there means the author chose when.
                if contains_re(body, r"repeat\s*:\s*true") && contains_re(body, r"running\s*:\s*true") {
                    findings.push(Finding {
                        rule: "hidden-timer", level: Level::Advice, file: file.clone(), line: source.line(frame.start),
                        message: "a repeating Timer that keeps running while SUPER DESKTOP is hidden".into(),
                        hint: "If it only updates what is drawn (a clock, a list), add `running: sd.visible`. Keep it running only for work that must go on (alarms, checks).".into(),
                    });
                }
            }
        }
    }
    // `// sd-lint: ignore hidden-timer` on a line, or the line above it,
    // keeps advice its author has weighed out of the report.
    findings.retain(|f| {
        f.level == Level::Error || !std::fs::read_to_string(&f.file).is_ok_and(|text| {
            let lines: Vec<&str> = text.lines().collect();
            let near = |n: usize| lines.get(n).is_some_and(|l| l.contains(&format!("sd-lint: ignore {}", f.rule)));
            near(f.line.saturating_sub(1)) || (f.line >= 2 && near(f.line - 2))
        })
    });
    findings.sort_by(|a, b| (a.level != Level::Error, &a.file, a.line).cmp(&(b.level != Level::Error, &b.file, b.line)));
    findings
}

fn file_rules(file: &Path, source: &Source, findings: &mut Vec<Finding>) {
    let mut add = |rule, level, at: usize, message: String, hint: &str| {
        findings.push(Finding { rule, level, file: file.to_path_buf(), line: source.line(at), message, hint: hint.into() });
    };
    // Software rendering: no shaders, no 3D.
    for word in ["ShaderEffect", "ShaderEffectSource", "MultiEffect"] {
        if let Some(at) = source.find_word(word).next() {
            add("software-rendering", Level::Error, at, format!("{word} is not available: SUPER DESKTOP draws plugins in software"),
                "Use plain items, gradients, opacity, Canvas or Image instead.");
        }
    }
    if let Some(at) = source.stripped.find("layer.effect") {
        add("software-rendering", Level::Error, at, "layer.effect is not available: SUPER DESKTOP draws plugins in software".into(),
            "Use plain items, gradients, opacity, Canvas or Image instead.");
    }
    for (at, import) in source.imports() {
        let (bad, why) = match import.as_str() {
            m if m.starts_with("QtQuick3D") || m.starts_with("Qt5Compat.GraphicalEffects") || m == "QtQuick.Effects" =>
                (true, "is not available: SUPER DESKTOP draws plugins in software"),
            m if m.starts_with("Qt.labs.platform") || m == "QtQuick.Dialogs" =>
                (true, "opens windows and system dialogs, which plugins cannot have"),
            _ => (false, ""),
        };
        if bad {
            add("import", Level::Error, at, format!("import {import} {why}"),
                "Remove the import; draw what you need inside the surface (a Popup or Menu from QtQuick.Controls works).");
        }
    }
    for frame in &source.frames {
        let nested_window = frame.depth > 0 && matches!(frame.name.as_str(), "Window" | "ApplicationWindow");
        let dialog = matches!(frame.name.as_str(), "FileDialog" | "FolderDialog" | "MessageDialog" | "ColorDialog" | "FontDialog");
        if frame.kind == Kind::Type && (nested_window || dialog) {
            add("window", Level::Error, frame.start, format!("{} opens a window, which plugins cannot have", frame.name),
                "Draw it inside the surface: a Popup, a Rectangle shown on top, or another surface (panel, popup).");
        }
    }
    // Acting on the user's agents only follows a click or a key.
    for method in ["cards.send", "harness.launch", "cards.close"] {
        for quote in ['"', '\''] {
            let needle = format!("{quote}{method}{quote}");
            for (at, _) in source.text.match_indices(&needle) {
                let Some(handler) = source.handler_at(at) else { continue };
                if is_automatic(&handler) {
                    add("act-without-click", Level::Error, at,
                        format!("{method} in {handler}: it runs without the user asking"),
                        "Call it only from a click or key handler (onClicked, Keys.onPressed), after showing what it will do.");
                }
            }
        }
    }
    // An open panel or card stays open while the overlay is hidden: opening
    // it again from a show, a timer or a change undoes the user's ✕. Opening
    // it once on load, or on a click, is fine. One helper function deep:
    // `onVisibleChanged: reopen()` with `function reopen() { sd.call(…) }`.
    for method in ["panel.open", "panel.toggle", "card.open", "card.toggle", "card.new", "fullscreen.open", "fullscreen.toggle", "editor.open"] {
        for quote in ['"', '\''] {
            for (at, _) in source.text.match_indices(&format!("{quote}{method}{quote}")) {
                let mut triggers: Vec<(usize, String)> = source.handler_at(at).filter(|h| reopens(h)).map(|h| (at, h)).into_iter().collect();
                let helper = source.frames_at(at).into_iter()
                    .find(|f| f.kind == Kind::Function && !f.name.starts_with("on"))
                    .map(|f| f.name.clone());
                if let Some(name) = helper {
                    for call in find_word(&source.stripped, &name) {
                        let called = source.stripped[call + name.len()..].trim_start().starts_with('(');
                        let definition = source.stripped[..call].trim_end().ends_with("function");
                        if let Some(handler) = (called && !definition).then(|| source.handler_at(call)).flatten().filter(|h| reopens(h)) {
                            triggers.push((call, handler));
                        }
                    }
                }
                for (at, handler) in triggers {
                    add("reopen", Level::Advice, at, format!("{method} from {handler}: it opens again what the user closed with ✕"),
                        "Panels and cards stay open while SUPER DESKTOP is hidden. Open one on a click (or once in Component.onCompleted), never from sd.visibleChanged, a Timer or a change.");
                }
            }
        }
    }
    // `muted` is the theme's border color: text in it is hard to read.
    for (at, _) in source.stripped.match_indices("sd.theme.muted") {
        let in_text = source.frames_at(at).into_iter().find(|f| f.kind == Kind::Type)
            .is_some_and(|f| matches!(f.name.as_str(), "Text" | "Label" | "TextField" | "TextInput" | "TextEdit" | "TextArea"));
        if in_text && source.property_at(at).as_deref() == Some("color") {
            add("muted-text", Level::Advice, at, "text in sd.theme.muted, the theme's border color: in several themes it can hardly be read".into(),
                "Use sd.theme.foreground (with opacity: 0.7 for secondary text) or another theme color that keeps a 4.5:1 contrast with the background.");
        }
    }
    // Colors that stay the same in every theme.
    let colors = source.hard_colors();
    if let Some(&(at, ref first)) = colors.first() {
        let more = if colors.len() > 1 { format!(" and {} more", colors.len() - 1) } else { String::new() };
        add("theme-colors", Level::Advice, at, format!("a fixed color ({first}){more}: it does not follow the user's theme"),
            "Use sd.theme colors (background, foreground, accent, surface, red, green…), or Qt.alpha(sd.theme.x, 0.3) for a tint. Fixed colors are fine for art that must not change.");
    }
    // Clickable areas without the hand pointer.
    let plain = source.frames.iter()
        .filter(|f| f.kind == Kind::Type && f.name == "MouseArea")
        .filter(|f| {
            let body = &source.stripped[f.start..f.end];
            contains_re(body, r"\bon(Clicked|Pressed|DoubleClicked)\b") && !body.contains("cursorShape")
        })
        .collect::<Vec<_>>();
    if let Some(first) = plain.first() {
        let more = if plain.len() > 1 { format!(" ({} of them)", plain.len()) } else { String::new() };
        add("cursor", Level::Advice, first.start, format!("a clickable MouseArea without a pointer shape{more}"),
            "Add `cursorShape: Qt.PointingHandCursor` so people see it can be clicked.");
    }
    // A sound through a shell chain of players.
    for player in ["pw-play", "paplay", "aplay"] {
        if let Some((at, _)) = source.text.match_indices(player).next() {
            add("sound", Level::Advice, at, format!("plays a sound with {player}"),
                "Use sd.call(\"sound.play\", {file: \"ding.wav\"}) (or {name: \"complete\"}): SUPER DESKTOP finds a player and keeps snapshots quiet.");
            break;
        }
    }
}

/// Handlers that run on their own: on load, on a timer, on any change.
fn is_automatic(handler: &str) -> bool {
    let last = handler.rsplit('.').next().unwrap_or(handler);
    matches!(last, "onCompleted" | "onTriggered" | "onOutput" | "onExited" | "onFinished")
        || (last.starts_with("on") && last.ends_with("Changed"))
}

/// A handler that runs on its own, again and again: a change (the overlay
/// shown, saved values) or a timer. Not a click, and not the one-time load.
fn reopens(handler: &str) -> bool {
    let last = handler.rsplit('.').next().unwrap_or(handler);
    last == "onTriggered" || (last.starts_with("on") && last.ends_with("Changed"))
}

fn contains_re(text: &str, pattern: &str) -> bool {
    // A tiny matcher for the few patterns used here, no regex crate.
    match pattern {
        r"repeat\s*:\s*true" | r"running\s*:\s*true" => {
            let name = &pattern[..pattern.find('\\').unwrap_or(0)];
            find_word(text, name).any(|at| {
                let rest = text[at + name.len()..].trim_start();
                rest.strip_prefix(':').is_some_and(|r| {
                    let r = r.trim_start();
                    r.starts_with("true") && !r[4..].trim_start().starts_with(['&', '|', '?'])
                })
            })
        }
        r"\bon(Clicked|Pressed|DoubleClicked)\b" => ["onClicked", "onPressed", "onDoubleClicked"].iter().any(|w| find_word(text, w).next().is_some()),
        _ => text.contains(pattern),
    }
}

/// Positions of `word` not inside a longer identifier.
fn find_word<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).map(|(at, _)| at).filter(move |&at| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// Every .qml file of the plugin (hidden folders skipped).
fn qml_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if hidden {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "qml") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A surface's file and the plugin's own components it uses, transitively.
fn reachable(root: &Path) -> BTreeSet<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(file) = stack.pop() {
        let Ok(canonical) = file.canonicalize() else { continue };
        if !seen.insert(canonical) {
            continue;
        }
        let Some(source) = Source::read(&file) else { continue };
        let dir = file.parent().unwrap_or(Path::new("."));
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(source.imports().into_iter().filter_map(|(_, i)| {
            i.strip_prefix('"').and_then(|i| i.split('"').next()).map(|sub| dir.join(sub))
        }));
        for frame in source.frames.iter().filter(|f| f.kind == Kind::Type) {
            for d in &dirs {
                let candidate = d.join(format!("{}.qml", frame.name));
                if candidate.is_file() {
                    stack.push(candidate);
                }
            }
        }
    }
    seen
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `Rectangle {`, `delegate: Item {`, `component X: Text {`
    Type,
    /// `onClicked: {`, `function onVisibleChanged() {`, `property var x: {`
    Binding,
    /// `function save() {`
    Function,
    /// `if (…) {`, an object literal…
    Block,
}

struct Frame {
    kind: Kind,
    name: String,
    /// The `{` and one past the matching `}`.
    start: usize,
    end: usize,
    depth: usize,
}

struct Source {
    text: String,
    /// `text` with comments and string contents blanked (same positions).
    stripped: String,
    frames: Vec<Frame>,
    lines: Vec<usize>,
}

impl Source {
    fn read(path: &Path) -> Option<Self> {
        Some(Self::parse(std::fs::read_to_string(path).ok()?))
    }

    fn parse(text: String) -> Self {
        let stripped = strip(&text);
        let frames = frames(&stripped);
        let lines = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        Self { text, stripped, frames, lines }
    }

    fn line(&self, at: usize) -> usize {
        self.lines.partition_point(|&start| start <= at)
    }

    fn find_word<'a>(&'a self, word: &'a str) -> impl Iterator<Item = usize> + 'a {
        find_word(&self.stripped, word)
    }

    /// Frames around `at`, innermost first.
    fn frames_at(&self, at: usize) -> Vec<&Frame> {
        let mut around: Vec<&Frame> = self.frames.iter().filter(|f| f.start < at && at < f.end).collect();
        around.sort_by_key(|f| std::cmp::Reverse(f.depth));
        around
    }

    /// The start of the statement `at` is in (after a new line, `;`, `{` or `}`).
    fn statement_start(&self, at: usize) -> usize {
        self.stripped[..at].rfind(['\n', ';', '{', '}']).map_or(0, |i| i + 1)
    }

    /// The property a value at `at` is bound to: `color` for `color: x`.
    fn property_at(&self, at: usize) -> Option<String> {
        let head = &self.stripped[self.statement_start(at)..at];
        let (name, _) = head.split_once(':')?;
        let name = name.trim();
        (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')).then(|| name.to_owned())
    }

    /// The handler (or binding) `at` runs in: `onClicked`, `Component.onCompleted`…
    /// `None` inside a plain function, where the caller is unknown.
    fn handler_at(&self, at: usize) -> Option<String> {
        // `onTriggered: sd.call(...)` on one line.
        if let Some(name) = self.property_at(at) {
            return Some(name);
        }
        match self.frames_at(at).into_iter().find(|f| f.kind != Kind::Block) {
            Some(f) if f.kind == Kind::Binding => Some(f.name.clone()),
            Some(f) if f.kind == Kind::Function && f.name.starts_with("on") => Some(f.name.clone()),
            _ => None,
        }
    }

    /// `(position, module)` of each import line; a folder keeps its quotes.
    fn imports(&self) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut at = 0;
        for line in self.text.split_inclusive('\n') {
            if let Some(rest) = line.trim_start().strip_prefix("import ") {
                let module = rest.split_whitespace().next().unwrap_or("").to_owned();
                out.push((at, module));
            }
            at += line.len();
        }
        out
    }

    /// `(position, "color: \"#fff\"")` of each fixed color in a color property.
    fn hard_colors(&self) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for (at, _) in self.stripped.match_indices(':') {
            let name_start = self.stripped[..at].trim_end().rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')).map_or(0, |i| i + 1);
            let name = self.stripped[name_start..at].trim();
            if !name.to_ascii_lowercase().ends_with("color") {
                continue;
            }
            let value = self.text[at + 1..].trim_start();
            let Some(rest) = value.strip_prefix('"') else { continue };
            let Some(literal) = rest.split('"').next() else { continue };
            let named = !literal.is_empty() && literal.chars().all(|c| c.is_ascii_alphabetic());
            let hex = literal.starts_with('#') && (4..=9).contains(&literal.len()) && literal[1..].chars().all(|c| c.is_ascii_hexdigit());
            if (hex || named) && literal != "transparent" {
                out.push((at, format!("{name}: \"{literal}\"")));
            }
        }
        out
    }
}

/// Blanks comments and the inside of strings, keeping every position.
fn strip(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in &mut out[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + 2 + n + 2);
                blank(&mut out, i, end);
                i = end;
            }
            quote @ (b'"' | b'\'' | b'`') => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != quote {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                let end = j.min(bytes.len());
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            _ => i += 1,
        }
    }
    // Only ASCII bytes were replaced (by spaces), so this stays UTF-8 except
    // where a multibyte character was blanked byte by byte: also spaces.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Every `{ … }` block with what it is.
fn frames(stripped: &str) -> Vec<Frame> {
    let mut done = Vec::new();
    let mut open: Vec<Frame> = Vec::new();
    for (at, c) in stripped.char_indices() {
        match c {
            '{' => {
                let head_start = stripped[..at].rfind(['\n', ';', '{', '}']).map_or(0, |i| i + 1);
                let (kind, name) = classify(stripped[head_start..at].trim());
                open.push(Frame { kind, name, start: at, end: stripped.len(), depth: open.len() });
            }
            '}' => {
                if let Some(mut frame) = open.pop() {
                    frame.end = at + 1;
                    done.push(frame);
                }
            }
            _ => {}
        }
    }
    done.extend(open);
    done.sort_by_key(|f| f.start);
    done
}

fn classify(head: &str) -> (Kind, String) {
    let is_type = |t: &str| t.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
    let type_name = |t: &str| t.rsplit('.').next().unwrap_or(t).to_owned();
    if is_type(head) {
        return (Kind::Type, type_name(head));
    }
    if let Some(rest) = head.strip_prefix("function") {
        let name = rest.trim_start().split(|c: char| c == '(' || c.is_whitespace()).next().unwrap_or("").to_owned();
        return (Kind::Function, name);
    }
    if let Some((name, rest)) = head.split_once(':') {
        let rest = rest.trim();
        if is_type(rest) {
            return (Kind::Type, type_name(rest));
        }
        let name = name.trim();
        let name = name.rsplit(' ').next().unwrap_or(name);
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
            return (Kind::Binding, name.to_owned());
        }
    }
    (Kind::Block, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plugin(name: &str, files: &[(&str, &str)], surfaces: serde_json::Value) -> Manifest {
        let dir = std::env::temp_dir().join(format!("sd-plugins-lint-{}", std::process::id())).join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        super::super::manifest::validate(&dir, &json!({"id": name, "api": 1, "surfaces": surfaces})).unwrap()
    }

    fn rules(findings: &[Finding]) -> Vec<(&'static str, usize)> {
        findings.iter().map(|f| (f.rule, f.line)).collect()
    }

    #[test]
    fn plugins_lint_finds_the_known_mistakes() {
        let bar = r##"import QtQuick
import QtQuick.Controls
Rectangle {
    color: "#202020"   // fixed
    border.color: "transparent"
    Timer { interval: 1000; repeat: true; running: true; onTriggered: sd.call("cards.send", {id: "x", text: "hi"}) }
    Component.onCompleted: {
        if (true) { sd.call('harness.launch', {harness: "claude"}) }
    }
    Pill { }
    MouseArea { anchors.fill: parent; onClicked: sd.call("cards.send", {id: "x", text: "ok"}) }
    Text { text: "x"; color: sd.theme.muted }
    Rectangle { border.color: sd.theme.muted }
}
"##;
        let pill = "import QtQuick\nimport QtQuick.Controls\nItem {\n    ToolTip.visible: true\n    ShaderEffect { }\n}\n";
        let panel = r#"import QtQuick
import Qt.labs.platform
Item {
    Timer { repeat: true; running: true }
    FileDialog { }
    Connections { target: sd; function onValuesChanged() { sd.call("cards.send", {}) } }
    function send() { sd.call("cards.send", {}) }
    Canvas { onPaint: { sd.exec(["sh", "-c", "pw-play ding.wav"]) } }
    Connections { target: sd; function onCardButtonClicked(button, card) { sd.call("cards.close", {id: card.id}) } }
    Connections { target: sd; function onVisibleChanged(v) { sd.call("cards.close", {id: "x"}) } }
}
"#;
        let m = plugin("lint", &[("Bar.qml", bar), ("Pill.qml", pill), ("Panel.qml", panel)], json!([
            {"id": "main", "slot": "toolbar.item", "qml": "Bar.qml"},
            {"id": "win", "slot": "panel", "qml": "Panel.qml"},
        ]));
        let found = lint(&m);
        let by_file = |name: &str| -> Vec<(&'static str, usize)> {
            rules(&found.iter().filter(|f| f.file.ends_with(name)).cloned().collect::<Vec<_>>())
        };
        // Bar.qml: cards.send in a timer and harness.launch on load (the
        // click is fine), muted text (not a muted border), fixed colors, a
        // hidden timer, a MouseArea without a hand.
        let bar_rules = by_file("Bar.qml");
        assert!(bar_rules.contains(&("act-without-click", 6)), "{bar_rules:?}");
        assert!(bar_rules.contains(&("act-without-click", 8)), "{bar_rules:?}");
        assert_eq!(bar_rules.iter().filter(|r| r.0 == "act-without-click").count(), 2, "{bar_rules:?}");
        assert!(bar_rules.contains(&("muted-text", 12)), "{bar_rules:?}");
        assert_eq!(bar_rules.iter().filter(|r| r.0 == "muted-text").count(), 1);
        assert!(bar_rules.contains(&("theme-colors", 4)), "{bar_rules:?}");
        assert!(bar_rules.contains(&("hidden-timer", 6)), "{bar_rules:?}");
        assert!(bar_rules.contains(&("cursor", 11)), "{bar_rules:?}");
        // Pill.qml is used by the bar: its ToolTip eats clicks; no shaders.
        assert_eq!(by_file("Pill.qml"), [("bar-tooltip", 4), ("software-rendering", 5)]);
        // Panel.qml: a dialog and its import, on*Changed, a sound; a panel's
        // timer runs only while open; a plain function is not judged.
        let panel_rules = by_file("Panel.qml");
        assert!(panel_rules.contains(&("import", 2)), "{panel_rules:?}");
        assert!(panel_rules.contains(&("window", 5)), "{panel_rules:?}");
        assert!(panel_rules.contains(&("act-without-click", 6)), "{panel_rules:?}");
        assert!(!panel_rules.iter().any(|r| r == &("act-without-click", 7)), "{panel_rules:?}");
        assert!(panel_rules.contains(&("sound", 8)), "{panel_rules:?}");
        // A card button is a click; closing a card when the overlay shows is not.
        assert!(!panel_rules.iter().any(|r| r == &("act-without-click", 9)), "{panel_rules:?}");
        assert!(panel_rules.contains(&("act-without-click", 10)), "{panel_rules:?}");
        assert!(!panel_rules.iter().any(|r| r.0 == "hidden-timer"), "{panel_rules:?}");
        // Errors first.
        let first_advice = found.iter().position(|f| f.level == Level::Advice).unwrap();
        assert!(found[first_advice..].iter().all(|f| f.level == Level::Advice));
    }

    #[test]
    fn plugins_lint_finds_reopening_what_the_user_closed() {
        let bar = r#"import QtQuick
Rectangle {
    id: root
    function showPanel() { sd.call("panel.open", {surface: "game"}) }
    Component.onCompleted: showPanel()
    Connections { target: sd; function onVisibleChanged(v) { if (v) root.showPanel() } }
    MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: root.showPanel() }
    Timer { interval: 60000; running: sd.visible; repeat: true; onTriggered: sd.call('card.open', {surface: "board"}) }
    function onValuesChanged() { sd.call("fullscreen.toggle", {surface: "f"}) }
    MouseArea { cursorShape: Qt.PointingHandCursor; onClicked: sd.call("card.toggle", {surface: "board"}) }
}
"#;
        let m = plugin("reopen", &[("Bar.qml", bar)], json!([{"id": "main", "slot": "toolbar.item", "qml": "Bar.qml"}]));
        let found = lint(&m);
        let reopen: Vec<usize> = found.iter().filter(|f| f.rule == "reopen").map(|f| f.line).collect();
        // The show (through the helper), the timer and the change; not the
        // load or the clicks.
        assert_eq!(reopen, [6, 8, 9], "{found:?}");
        assert!(found.iter().filter(|f| f.rule == "reopen").all(|f| f.level == Level::Advice));
    }

    #[test]
    fn plugins_lint_passes_clean_qml() {
        let clean = r##"import QtQuick
// "#fff" in a comment, color: "red" too
Rectangle {
    color: sd.theme.background
    property string hint: "color: #ffffff"
    Timer { interval: 1000; repeat: true; running: sd.visible }
    Timer { interval: 1000; repeat: true; running: true && sd.visible }
    // An alarm: it must ring while hidden. sd-lint: ignore hidden-timer
    Timer { interval: 5000; repeat: true; running: true }
    MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sd.call("cards.send", {id: "x", text: "y"}) }
    Text { text: "{ not a block"; color: sd.theme.foreground }
}
"##;
        let m = plugin("clean", &[("Dock.qml", clean)], json!([{"id": "main", "slot": "dock.left", "qml": "Dock.qml"}]));
        let found = lint(&m);
        assert!(found.is_empty(), "{:?}", rules(&found));
    }
}
