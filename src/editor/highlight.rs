//! SD Editor's syntax colors. The grammars (Sublime Text's, the set the `bat`
//! tool ships) are compiled into the binary and read by `syntect`; nothing is
//! loaded from the system. A file is colored a few lines at a time while the
//! desktop is idle, and an edit colors again only the lines it can change:
//! from the edited line down to the first line that ends as it did before.
use super::scheme::Kind;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::OnceLock;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Larger files stay uncolored: every line keeps its parser state.
const MAX_CHARS: i32 = 1_500_000;
const MAX_LINES: i32 = 30_000;
/// A longer line (minified code) is not parsed; it would stall the grammar.
const MAX_LINE_BYTES: usize = 4_000;
/// How long one idle step may color before it gives the desktop back.
const STEP: std::time::Duration = std::time::Duration::from_millis(6);

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// The grammar for a file: by its name (`Makefile`), its extension, then its
/// first line (`#!/bin/sh`). `None` is plain text.
pub fn detect(path: &Path, first_line: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    set.find_syntax_by_extension(name)
        .or_else(|| set.find_syntax_by_extension(extension))
        .or_else(|| set.find_syntax_by_extension(&extension.to_lowercase()))
        .or_else(|| set.find_syntax_by_first_line(first_line))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// A grammar's name the way languages are usually written: `rust`, `markdown`.
#[cfg(test)]
pub fn id(syntax: &SyntaxReference) -> String {
    syntax.name.to_lowercase()
}

/// What a scope on the stack means for the color.
#[derive(Clone, Copy, PartialEq)]
enum Rule {
    Draw(Kind),
    /// Drawn as plain text, whatever the scopes around it say.
    Plain,
    /// Says nothing: the scope around it decides.
    None,
}

/// Scope prefixes and their colors; the first that fits a scope wins.
const RULES: &[(&str, Rule)] = &[
    ("comment", Rule::Draw(Kind::Comment)),
    ("constant.character.escape", Rule::Draw(Kind::Special)),
    ("constant.other.placeholder", Rule::Draw(Kind::Special)),
    ("string", Rule::Draw(Kind::String)),
    ("constant.numeric", Rule::Draw(Kind::Number)),
    ("constant", Rule::Draw(Kind::Constant)),
    ("keyword.operator.word", Rule::Draw(Kind::Keyword)),
    ("keyword.operator", Rule::Plain),
    ("keyword", Rule::Draw(Kind::Keyword)),
    ("storage", Rule::Draw(Kind::Keyword)),
    ("entity.name.function", Rule::Draw(Kind::Function)),
    ("variable.function", Rule::Draw(Kind::Function)),
    ("support.function", Rule::Draw(Kind::Builtin)),
    ("entity.name.tag", Rule::Draw(Kind::Function)),
    ("entity.other.attribute-name", Rule::Draw(Kind::Type)),
    ("entity.name.section", Rule::Draw(Kind::Heading)),
    ("entity.other.inherited-class", Rule::Draw(Kind::Type)),
    ("entity.name", Rule::Draw(Kind::Type)),
    ("support.type", Rule::Draw(Kind::Type)),
    ("support.class", Rule::Draw(Kind::Type)),
    ("support.constant", Rule::Draw(Kind::Constant)),
    ("support", Rule::Draw(Kind::Builtin)),
    ("variable.language", Rule::Draw(Kind::Constant)),
    ("meta.preprocessor", Rule::Draw(Kind::Builtin)),
    ("invalid", Rule::Draw(Kind::Error)),
    ("markup.heading", Rule::Draw(Kind::Heading)),
    ("markup.bold", Rule::Draw(Kind::Strong)),
    ("markup.italic", Rule::Draw(Kind::Emphasis)),
    ("markup.underline.link", Rule::Draw(Kind::Link)),
    ("markup.inserted", Rule::Draw(Kind::Added)),
    ("markup.deleted", Rule::Draw(Kind::Removed)),
    ("markup.raw", Rule::Draw(Kind::String)),
    ("meta.diff.range", Rule::Draw(Kind::Location)),
    ("meta.diff.header", Rule::Draw(Kind::Location)),
    ("meta.diff.index", Rule::Draw(Kind::Location)),
];

/// Turns a scope stack into a color, remembering each scope's answer.
struct Classifier {
    rules: Vec<(Scope, Rule)>,
    known: HashMap<Scope, Rule>,
}

impl Classifier {
    fn new() -> Self {
        let rules = RULES.iter().filter_map(|(prefix, rule)| Some((Scope::new(prefix).ok()?, *rule))).collect();
        Self { rules, known: HashMap::new() }
    }

    fn rule(&mut self, scope: Scope) -> Rule {
        if let Some(rule) = self.known.get(&scope) {
            return *rule;
        }
        let rule = self.rules.iter().find(|(prefix, _)| prefix.is_prefix_of(scope)).map_or(Rule::None, |(_, rule)| *rule);
        self.known.insert(scope, rule);
        rule
    }

    /// The innermost scope that says something decides.
    fn kind(&mut self, stack: &ScopeStack) -> Option<Kind> {
        for scope in stack.as_slice().iter().rev() {
            match self.rule(*scope) {
                Rule::Draw(kind) => return Some(kind),
                Rule::Plain => return None,
                Rule::None => {}
            }
        }
        None
    }
}

/// The parser at the end of a line: what the next line starts from.
#[derive(Clone, PartialEq)]
struct LineEnd {
    parse: ParseState,
    scopes: ScopeStack,
}

impl LineEnd {
    fn start(syntax: &SyntaxReference) -> Self {
        Self { parse: ParseState::new(syntax), scopes: ScopeStack::new() }
    }
}

/// A colored stretch of a line, in bytes.
type Span = (usize, usize, Kind);

/// Parses one line (without its line break) from `state`, which becomes the
/// state at the line's end.
fn color_line(state: &mut LineEnd, line: &str, classifier: &mut Classifier) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    if line.len() > MAX_LINE_BYTES {
        return spans;
    }
    let mut text = String::with_capacity(line.len() + 1);
    text.push_str(line);
    text.push('\n');
    let Ok(ops) = state.parse.parse_line(&text, syntaxes()) else {
        return spans;
    };
    let mut push = |from: usize, to: usize, kind: Option<Kind>| {
        let to = to.min(line.len());
        let Some(kind) = kind.filter(|_| from < to) else { return };
        match spans.last_mut() {
            Some(last) if last.1 == from && last.2 == kind => last.1 = to,
            _ => spans.push((from, to, kind)),
        }
    };
    let mut at = 0;
    for (index, op) in ops {
        if index > at {
            push(at, index, classifier.kind(&state.scopes));
            at = index;
        }
        let _ = state.scopes.apply(&op);
    }
    push(at, line.len(), classifier.kind(&state.scopes));
    spans
}

/// One line of the file: how it ended when it was last colored, and whether
/// it has to be colored again.
#[derive(Clone, Default)]
struct Line {
    end: Option<LineEnd>,
    dirty: bool,
}

/// Which lines need coloring, kept in step with the text's edits.
#[derive(Default)]
struct Lines {
    lines: Vec<Line>,
    /// No dirty line is before this one.
    first_dirty: usize,
}

impl Lines {
    fn reset(&mut self, count: usize) {
        self.lines = vec![Line { end: None, dirty: true }; count.max(1)];
        self.first_dirty = 0;
    }

    fn mark(&mut self, line: usize) {
        if let Some(entry) = self.lines.get_mut(line) {
            entry.dirty = true;
            self.first_dirty = self.first_dirty.min(line);
        }
    }

    /// Text with `breaks` line breaks is about to be inserted in `line`. The
    /// line's old end now belongs to its last piece.
    fn inserted(&mut self, line: usize, breaks: usize) {
        let line = line.min(self.lines.len().saturating_sub(1));
        self.lines.splice(line..line, std::iter::repeat_n(Line::default(), breaks));
        for changed in line..=line + breaks {
            self.mark(changed);
        }
    }

    /// The text from somewhere in line `first` to somewhere in line `last` is
    /// about to be deleted: what is left is one line that ends as `last` did.
    fn deleted(&mut self, first: usize, last: usize) {
        let last = last.min(self.lines.len().saturating_sub(1));
        if first < last {
            self.lines.drain(first..last);
        }
        self.mark(first);
    }

    fn next_dirty(&mut self) -> Option<usize> {
        let found = (self.first_dirty..self.lines.len()).find(|line| self.lines[*line].dirty);
        self.first_dirty = found.unwrap_or(self.lines.len());
        found
    }

    /// The state line `line` starts from; `None` when the line above still
    /// has to be colored first.
    fn start(&mut self, line: usize, syntax: &SyntaxReference) -> Option<LineEnd> {
        if line == 0 {
            return Some(LineEnd::start(syntax));
        }
        let above = self.lines[line - 1].end.clone();
        if above.is_none() {
            self.mark(line - 1);
        }
        above
    }

    /// Line `line` was colored and ended in `end`. When that differs from
    /// last time, the next line starts differently and is colored again.
    fn colored(&mut self, line: usize, end: LineEnd) {
        let same = self.lines[line].end.as_ref() == Some(&end);
        self.lines[line] = Line { end: Some(end), dirty: false };
        if !same {
            self.mark(line + 1);
        }
    }
}

/// Colors one text buffer.
pub struct Highlighter {
    buffer: gtk4::TextBuffer,
    syntax: Cell<Option<&'static SyntaxReference>>,
    lines: RefCell<Lines>,
    classifier: RefCell<Classifier>,
    tags: RefCell<Vec<(Kind, gtk4::TextTag)>>,
    idle: RefCell<Option<glib::SourceId>>,
}

impl Highlighter {
    /// For a buffer whose tags `scheme::style_buffer` already made.
    pub fn new(buffer: &gtk4::TextBuffer) -> Rc<Self> {
        let table = buffer.tag_table();
        let tags = Kind::ALL.iter().filter_map(|kind| Some((*kind, table.lookup(kind.tag())?))).collect();
        let this = Rc::new(Self {
            buffer: buffer.clone(),
            syntax: Cell::new(None),
            lines: RefCell::new(Lines::default()),
            classifier: RefCell::new(Classifier::new()),
            tags: RefCell::new(tags),
            idle: RefCell::new(None),
        });
        // Both run before the text changes: the positions are the old text's.
        buffer.connect_insert_text({
            let weak = Rc::downgrade(&this);
            move |_, at, text| {
                if let Some(this) = weak.upgrade() {
                    if this.syntax.get().is_some() {
                        this.lines.borrow_mut().inserted(at.line() as usize, text.matches('\n').count());
                        this.schedule();
                    }
                }
            }
        });
        buffer.connect_delete_range({
            let weak = Rc::downgrade(&this);
            move |_, start, end| {
                if let Some(this) = weak.upgrade() {
                    if this.syntax.get().is_some() {
                        this.lines.borrow_mut().deleted(start.line() as usize, end.line() as usize);
                        this.schedule();
                    }
                }
            }
        });
        this
    }

    /// The language's name, `None` for plain text.
    #[cfg(test)]
    pub fn language(&self) -> Option<String> {
        self.syntax.get().map(id)
    }

    /// Colors the buffer as `syntax` from now on (`None`: plain text).
    pub fn set_syntax(self: &Rc<Self>, syntax: Option<&'static SyntaxReference>) {
        if self.syntax.get().map(|s| &s.name) == syntax.map(|s| &s.name) {
            return;
        }
        self.syntax.set(syntax);
        self.restart();
    }

    /// Forgets every color and starts over from the first line.
    fn restart(self: &Rc<Self>) {
        let (start, end) = self.buffer.bounds();
        for (_, tag) in self.tags.borrow().iter() {
            self.buffer.remove_tag(tag, &start, &end);
        }
        self.lines.borrow_mut().reset(self.buffer.line_count() as usize);
        if self.syntax.get().is_some() {
            self.schedule();
        }
    }

    fn schedule(self: &Rc<Self>) {
        if self.idle.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let source = glib::idle_add_local_full(glib::Priority::LOW, move || {
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            if this.step() {
                glib::ControlFlow::Continue
            } else {
                this.idle.borrow_mut().take();
                glib::ControlFlow::Break
            }
        });
        *self.idle.borrow_mut() = Some(source);
    }

    /// Colors lines for a few milliseconds; `true` while some are left.
    fn step(&self) -> bool {
        let Some(syntax) = self.syntax.get() else { return false };
        if self.buffer.char_count() > MAX_CHARS || self.buffer.line_count() > MAX_LINES {
            return false;
        }
        let started = std::time::Instant::now();
        loop {
            let count = self.buffer.line_count() as usize;
            let mut lines = self.lines.borrow_mut();
            if lines.lines.len() != count {
                // An edit this did not see: color everything again.
                let (start, end) = self.buffer.bounds();
                for (_, tag) in self.tags.borrow().iter() {
                    self.buffer.remove_tag(tag, &start, &end);
                }
                lines.reset(count);
            }
            let Some(line) = lines.next_dirty() else { return false };
            let Some(mut state) = lines.start(line, syntax) else { continue };
            self.color(line, &mut state);
            lines.colored(line, state);
            if started.elapsed() > STEP {
                return true;
            }
        }
    }

    /// Parses line `line` from `state` and gives its text the tags.
    fn color(&self, line: usize, state: &mut LineEnd) {
        let Some(start) = self.buffer.iter_at_line(line as i32) else { return };
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let text = self.buffer.text(&start, &end, true);
        let spans = color_line(state, &text, &mut self.classifier.borrow_mut());
        let tags = self.tags.borrow();
        for (_, tag) in tags.iter() {
            self.buffer.remove_tag(tag, &start, &end);
        }
        for (from, to, kind) in &spans {
            let Some((_, tag)) = tags.iter().find(|(k, _)| k == kind) else { continue };
            let (Some(from), Some(to)) =
                (self.buffer.iter_at_line_index(line as i32, *from as i32), self.buffer.iter_at_line_index(line as i32, *to as i32))
            else {
                continue;
            };
            self.buffer.apply_tag(tag, &from, &to);
        }
    }

    /// Colors everything that is waiting, at once (tests).
    #[cfg(test)]
    pub fn finish(&self) {
        while self.step() {}
    }
}

impl Drop for Highlighter {
    fn drop(&mut self) {
        if let Some(source) = self.idle.borrow_mut().take() {
            source.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colored(syntax: &SyntaxReference, text: &str) -> Vec<Vec<(String, Kind)>> {
        let mut state = LineEnd::start(syntax);
        let mut classifier = Classifier::new();
        text.lines()
            .map(|line| color_line(&mut state, line, &mut classifier).into_iter().map(|(from, to, kind)| (line[from..to].to_string(), kind)).collect())
            .collect()
    }

    #[test]
    fn editor_syntax_is_found_by_name_extension_and_first_line() {
        let name = |path: &str, first: &str| detect(Path::new(path), first).map(id);
        assert_eq!(name("/p/src/main.rs", "").as_deref(), Some("rust"));
        assert_eq!(name("/p/README.md", "").as_deref(), Some("markdown"));
        assert_eq!(name("/p/Makefile", "").as_deref(), Some("makefile"));
        assert_eq!(name("/p/Cargo.toml", "").as_deref(), Some("toml"));
        assert_eq!(name("/p/app.ts", "").as_deref(), Some("typescript"));
        assert_eq!(name("/p/App.kt", "").as_deref(), Some("kotlin"));
        assert_eq!(name("/p/run", "#!/bin/bash").as_deref(), Some("bourne again shell (bash)"));
        assert_eq!(name("/p/notes.txt", "hello"), None);
        assert_eq!(name("/p/unknown.zzz", "hello"), None);
    }

    #[test]
    fn editor_syntax_colors_code() {
        let rust = detect(Path::new("a.rs"), "").unwrap();
        let lines = colored(rust, "// note\nfn main() {\n    let x = \"hi\\n\"; // end\n    return 42;\n}\n");
        assert_eq!(lines[0], [("// note".to_string(), Kind::Comment)]);
        assert!(lines[1].contains(&("fn".to_string(), Kind::Keyword)), "{:?}", lines[1]);
        assert!(lines[1].contains(&("main".to_string(), Kind::Function)), "{:?}", lines[1]);
        assert!(lines[2].contains(&("let".to_string(), Kind::Keyword)), "{:?}", lines[2]);
        assert!(lines[2].contains(&("\\n".to_string(), Kind::Special)), "{:?}", lines[2]);
        assert!(lines[2].iter().any(|(text, kind)| text.starts_with("\"hi") && *kind == Kind::String), "{:?}", lines[2]);
        assert!(lines[2].contains(&("// end".to_string(), Kind::Comment)), "{:?}", lines[2]);
        assert!(lines[3].contains(&("42".to_string(), Kind::Number)), "{:?}", lines[3]);
        // A block comment carries over lines through the saved state.
        let lines = colored(rust, "/* one\ntwo */ let\n");
        assert_eq!(lines[0], [("/* one".to_string(), Kind::Comment)]);
        assert_eq!(lines[1][0], ("two */".to_string(), Kind::Comment));
        assert!(lines[1].contains(&("let".to_string(), Kind::Keyword)));
        // Markdown.
        let markdown = detect(Path::new("a.md"), "").unwrap();
        let lines = colored(markdown, "# Title\n");
        assert!(lines[0].iter().any(|(_, kind)| *kind == Kind::Heading), "{:?}", lines[0]);
        // A line too long to parse is left plain and does not change the state.
        let long = format!("let s = \"{}\";", "x".repeat(MAX_LINE_BYTES));
        assert!(colored(rust, &long)[0].is_empty());
    }

    #[test]
    fn editor_syntax_recolors_only_what_an_edit_changes() {
        let rust = detect(Path::new("a.rs"), "").unwrap();
        let mut text: Vec<&str> = vec!["fn a() {}", "let x = 1;", "let y = 2;", "let z = 3;"];
        let mut lines = Lines::default();
        lines.reset(text.len());
        let mut classifier = Classifier::new();
        let mut run = |lines: &mut Lines, text: &[&str]| {
            let mut done = Vec::new();
            while let Some(line) = lines.next_dirty() {
                let Some(mut state) = lines.start(line, rust) else { continue };
                color_line(&mut state, text[line], &mut classifier);
                lines.colored(line, state);
                done.push(line);
            }
            done
        };
        assert_eq!(run(&mut lines, &text), [0, 1, 2, 3]);
        // A change inside a line that ends as before: that line only.
        text[1] = "let x = 10;";
        lines.inserted(1, 0);
        assert_eq!(run(&mut lines, &text), [1]);
        // Opening a block comment changes how every later line starts.
        text[1] = "let x = 1; /*";
        lines.inserted(1, 0);
        assert_eq!(run(&mut lines, &text), [1, 2, 3]);
        // Closing it on the next line: lines up to where the state is the old one again.
        text[2] = "*/ let y = 2;";
        lines.inserted(2, 0);
        assert_eq!(run(&mut lines, &text), [2, 3]);
        // Two lines pasted into line 0: the new lines, and no line after them.
        text.splice(0..1, ["fn a() {", "    b();", "}"]);
        lines.inserted(0, 2);
        assert_eq!(lines.lines.len(), text.len());
        assert_eq!(run(&mut lines, &text), [0, 1, 2]);
        // Lines 1 and 2 joined into line 0 by a deletion.
        text.splice(0..3, ["fn a() {}"]);
        lines.deleted(0, 2);
        assert_eq!(lines.lines.len(), text.len());
        assert_eq!(run(&mut lines, &text), [0]);
    }
}
