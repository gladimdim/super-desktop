//! SD Editor's colors, made from the Omarchy theme: the text view's own
//! (background, text, selection, cursor) as CSS, and one text tag per kind of
//! syntax, the current line, matching brackets and search matches.
use crate::theme::OmarchyTheme;
use gtk4::prelude::*;
use gtk4::{gdk, pango};

/// A kind of text the syntax colors tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Comment,
    String,
    Keyword,
    Type,
    Number,
    Constant,
    Function,
    Builtin,
    Special,
    Error,
    Heading,
    Emphasis,
    Strong,
    Link,
    Added,
    Removed,
    Location,
}

impl Kind {
    pub const ALL: [Kind; 17] = [
        Kind::Comment, Kind::String, Kind::Keyword, Kind::Type, Kind::Number, Kind::Constant,
        Kind::Function, Kind::Builtin, Kind::Special, Kind::Error, Kind::Heading, Kind::Emphasis,
        Kind::Strong, Kind::Link, Kind::Added, Kind::Removed, Kind::Location,
    ];

    /// The name of its text tag.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Comment => "sd-syntax-comment",
            Kind::String => "sd-syntax-string",
            Kind::Keyword => "sd-syntax-keyword",
            Kind::Type => "sd-syntax-type",
            Kind::Number => "sd-syntax-number",
            Kind::Constant => "sd-syntax-constant",
            Kind::Function => "sd-syntax-function",
            Kind::Builtin => "sd-syntax-builtin",
            Kind::Special => "sd-syntax-special",
            Kind::Error => "sd-syntax-error",
            Kind::Heading => "sd-syntax-heading",
            Kind::Emphasis => "sd-syntax-emphasis",
            Kind::Strong => "sd-syntax-strong",
            Kind::Link => "sd-syntax-link",
            Kind::Added => "sd-syntax-added",
            Kind::Removed => "sd-syntax-removed",
            Kind::Location => "sd-syntax-location",
        }
    }
}

/// The cursor's line.
pub const CURRENT_LINE: &str = "sd-current-line";
/// A bracket next to the cursor and its partner.
pub const BRACKET: &str = "sd-bracket";
/// A match of the find bar's text.
pub const SEARCH_MATCH: &str = "sd-search-match";

/// The editor's colors for one theme, as `#rrggbb`.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub selection: String,
    /// The cursor's line: the background, slightly toward the text.
    pub line: String,
    /// Line numbers.
    pub gutter: String,
    /// Comments: the text faded toward the background, not the theme's
    /// `muted` (a border color that some themes make nearly invisible).
    pub comment: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub orange: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
}

/// `#rrggbb` → (r, g, b); anything else is gray.
fn rgb(color: &str) -> (u8, u8, u8) {
    let hex = color.trim().trim_start_matches('#');
    let channel = |at: usize| hex.get(at..at + 2).and_then(|c| u8::from_str_radix(c, 16).ok()).unwrap_or(128);
    (channel(0), channel(2), channel(4))
}

/// `a` moved `t` of the way toward `b`.
fn mix(a: &str, b: &str, t: f64) -> String {
    let ((ar, ag, ab), (br, bg, bb)) = (rgb(a), rgb(b));
    let step = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
    format!("#{:02x}{:02x}{:02x}", step(ar, br), step(ag, bg), step(ab, bb))
}

/// A color GTK can use; an unreadable one is gray.
pub fn rgba(color: &str) -> gdk::RGBA {
    let (r, g, b) = rgb(color);
    gdk::RGBA::new(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, 1.0)
}

/// `color` as `#rrggbb`, whatever the theme file wrote.
fn hex(color: &str) -> String {
    let (r, g, b) = rgb(color);
    format!("#{r:02x}{g:02x}{b:02x}")
}

impl Palette {
    pub fn new(theme: &OmarchyTheme) -> Self {
        let t = theme;
        Self {
            background: hex(&t.background),
            foreground: hex(&t.foreground),
            accent: hex(&t.accent),
            selection: hex(&t.selection),
            line: mix(&t.background, &t.foreground, 0.06),
            gutter: mix(&t.background, &t.foreground, 0.35),
            comment: mix(&t.foreground, &t.background, 0.42),
            red: hex(&t.red),
            green: hex(&t.green),
            yellow: hex(&t.yellow),
            orange: hex(&t.orange),
            blue: hex(&t.blue),
            magenta: hex(&t.magenta),
            cyan: hex(&t.cyan),
        }
    }

    /// The current Omarchy theme's.
    pub fn current() -> Self {
        Self::new(&crate::theme::current_theme())
    }

    /// How a kind of syntax is drawn: color, bold, italic, underlined.
    pub fn look(&self, kind: Kind) -> (&str, bool, bool, bool) {
        match kind {
            Kind::Comment => (&self.comment, false, true, false),
            Kind::String => (&self.green, false, false, false),
            Kind::Keyword => (&self.magenta, true, false, false),
            Kind::Type => (&self.yellow, false, false, false),
            Kind::Number | Kind::Constant => (&self.orange, false, false, false),
            Kind::Function => (&self.blue, false, false, false),
            Kind::Builtin | Kind::Special => (&self.cyan, false, false, false),
            Kind::Error => (&self.red, false, false, true),
            Kind::Heading => (&self.blue, true, false, false),
            Kind::Emphasis => (&self.foreground, false, true, false),
            Kind::Strong => (&self.foreground, true, false, false),
            Kind::Link => (&self.cyan, false, false, true),
            Kind::Added => (&self.green, false, false, false),
            Kind::Removed => (&self.red, false, false, false),
            Kind::Location => (&self.blue, false, false, false),
        }
    }

    /// The text view's own colors.
    pub fn css(&self) -> String {
        format!(
            "textview.sd-editor-text, textview.sd-editor-text > text {{ background-color: {bg}; color: {fg}; caret-color: {accent}; }}\n\
             textview.sd-editor-text > border {{ background-color: {bg}; }}\n\
             textview.sd-editor-text > text > selection {{ background-color: {selection}; color: {fg}; }}\n",
            bg = self.background,
            fg = self.foreground,
            accent = self.accent,
            selection = self.selection,
        )
    }
}

/// The tag `name` of `buffer`, made when it is not there yet.
fn tag(buffer: &gtk4::TextBuffer, name: &str) -> gtk4::TextTag {
    let table = buffer.tag_table();
    table.lookup(name).unwrap_or_else(|| {
        let tag = gtk4::TextTag::new(Some(name));
        table.add(&tag);
        tag
    })
}

/// Gives `buffer` its tags in `palette`'s colors (again after a theme change).
/// Later tags win: the current line is under the syntax colors, and brackets
/// and search matches are over them.
pub fn style_buffer(buffer: &gtk4::TextBuffer, palette: &Palette) {
    let line = tag(buffer, CURRENT_LINE);
    line.set_paragraph_background_rgba(Some(&rgba(&palette.line)));
    for kind in Kind::ALL {
        let (color, bold, italic, underlined) = palette.look(kind);
        let tag = tag(buffer, kind.tag());
        tag.set_foreground_rgba(Some(&rgba(color)));
        tag.set_weight(if bold { 700 } else { 400 });
        tag.set_style(if italic { pango::Style::Italic } else { pango::Style::Normal });
        tag.set_underline(match (underlined, kind) {
            (true, Kind::Error) => pango::Underline::Error,
            (true, _) => pango::Underline::Single,
            _ => pango::Underline::None,
        });
    }
    let found = tag(buffer, SEARCH_MATCH);
    found.set_foreground_rgba(Some(&rgba(&palette.background)));
    found.set_background_rgba(Some(&rgba(&palette.yellow)));
    let bracket = tag(buffer, BRACKET);
    bracket.set_foreground_rgba(Some(&rgba(&palette.background)));
    bracket.set_background_rgba(Some(&rgba(&palette.accent)));
    bracket.set_weight(700);
}

thread_local! {
    static PROVIDER: std::cell::OnceCell<gtk4::CssProvider> = const { std::cell::OnceCell::new() };
}

/// Loads `palette`'s text view colors for every editor on this display.
pub fn apply_css(palette: &Palette) {
    let Some(display) = gdk::Display::default() else { return };
    PROVIDER.with(|cell| {
        let provider = cell.get_or_init(|| {
            let provider = gtk4::CssProvider::new();
            gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
            provider
        });
        provider.load_from_string(&palette.css());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_scheme_follows_the_theme() {
        let theme = OmarchyTheme { background: "#000000".into(), foreground: "#FFFFFF".into(), magenta: "#ff00ff".into(), ..OmarchyTheme::default() };
        let palette = Palette::new(&theme);
        assert_eq!(palette.background, "#000000");
        assert_eq!(palette.foreground, "#ffffff");
        // Comments: the text faded 42% toward the background.
        assert_eq!(palette.comment, "#949494");
        assert_eq!(palette.look(Kind::Keyword), ("#ff00ff", true, false, false));
        assert_eq!(palette.look(Kind::Comment), ("#949494", false, true, false));
        assert!(palette.css().contains("background-color: #000000; color: #ffffff;"), "{}", palette.css());
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080");
        assert_eq!(rgb("nonsense"), (128, 128, 128));
        // Every kind has its own tag.
        let names: std::collections::HashSet<_> = Kind::ALL.iter().map(|k| k.tag()).collect();
        assert_eq!(names.len(), Kind::ALL.len());
    }
}
