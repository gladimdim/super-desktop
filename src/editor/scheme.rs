//! SD Editor's colors: a GtkSourceView style scheme made from the Omarchy
//! theme, written to the cache and loaded again when the theme changes.
use crate::theme::OmarchyTheme;
use std::cell::Cell;
use std::path::PathBuf;

pub const ID: &str = "sd-omarchy";

thread_local! {
    static SEARCH_PATH_ADDED: Cell<bool> = const { Cell::new(false) };
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

/// The scheme for `theme`. Comments are the text faded toward the
/// background, not the theme's `muted` (a border color that some themes
/// make nearly invisible on the background).
pub fn xml(theme: &OmarchyTheme) -> String {
    let t = theme;
    let colors = [
        ("bg", t.background.clone()),
        ("fg", t.foreground.clone()),
        ("accent", t.accent.clone()),
        ("selection", t.selection.clone()),
        ("line", mix(&t.background, &t.foreground, 0.06)),
        ("gutter", mix(&t.background, &t.foreground, 0.35)),
        ("comment", mix(&t.foreground, &t.background, 0.42)),
        ("red", t.red.clone()),
        ("green", t.green.clone()),
        ("yellow", t.yellow.clone()),
        ("orange", t.orange.clone()),
        ("blue", t.blue.clone()),
        ("magenta", t.magenta.clone()),
        ("cyan", t.cyan.clone()),
    ];
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<style-scheme id=\"{ID}\" name=\"SUPER DESKTOP ({})\" version=\"1.0\">\n",
        escape(&t.name)
    );
    for (name, value) in &colors {
        out.push_str(&format!("  <color name=\"{name}\" value=\"{}\"/>\n", escape(value)));
    }
    let styles: &[(&str, &str)] = &[
        ("text", r#"foreground="fg" background="bg""#),
        ("selection", r#"foreground="fg" background="selection""#),
        ("selection-unfocused", r#"foreground="fg" background="selection""#),
        ("cursor", r#"foreground="accent""#),
        ("current-line", r#"background="line""#),
        ("line-numbers", r#"foreground="gutter" background="bg""#),
        ("current-line-number", r#"foreground="fg" background="line""#),
        ("bracket-match", r#"foreground="bg" background="accent" bold="true""#),
        ("bracket-mismatch", r#"foreground="bg" background="red""#),
        ("search-match", r#"foreground="bg" background="yellow""#),
        ("def:comment", r#"foreground="comment" italic="true""#),
        ("def:doc-comment-element", r#"foreground="comment" bold="true""#),
        ("def:string", r#"foreground="green""#),
        ("def:character", r#"foreground="green""#),
        ("def:keyword", r#"foreground="magenta" bold="true""#),
        ("def:statement", r#"foreground="magenta""#),
        ("def:type", r#"foreground="yellow""#),
        ("def:number", r#"foreground="orange""#),
        ("def:boolean", r#"foreground="orange""#),
        ("def:constant", r#"foreground="orange""#),
        ("def:special-constant", r#"foreground="orange""#),
        ("def:function", r#"foreground="blue""#),
        ("def:builtin", r#"foreground="cyan""#),
        ("def:preprocessor", r#"foreground="cyan""#),
        ("def:special-char", r#"foreground="cyan""#),
        ("def:identifier", r#"foreground="fg""#),
        ("def:error", r#"foreground="red" underline="error""#),
        ("def:warning", r#"foreground="yellow""#),
        ("def:note", r#"foreground="yellow" bold="true""#),
        ("def:heading", r#"foreground="blue" bold="true""#),
        ("def:emphasis", r#"italic="true""#),
        ("def:strong-emphasis", r#"bold="true""#),
        ("def:link-destination", r#"foreground="cyan" underline="single""#),
        ("diff:added-line", r#"foreground="green""#),
        ("diff:removed-line", r#"foreground="red""#),
        ("diff:location", r#"foreground="blue""#),
    ];
    for (name, attributes) in styles {
        out.push_str(&format!("  <style name=\"{name}\" {attributes}/>\n"));
    }
    out.push_str("</style-scheme>\n");
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
        .join("super-desktop/sourceview")
}

/// The scheme for the current theme, written and loaded; `None` when it
/// cannot be written (GtkSourceView's own default is used then).
pub fn current() -> Option<sourceview5::StyleScheme> {
    let dir = dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{ID}.xml"));
    let text = xml(&crate::theme::current_theme());
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&path, text).ok()?;
    }
    let manager = sourceview5::StyleSchemeManager::default();
    if !SEARCH_PATH_ADDED.with(|added| added.replace(true)) {
        manager.append_search_path(&dir.to_string_lossy());
    }
    manager.force_rescan();
    manager.scheme(ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_scheme_follows_the_theme() {
        let theme = OmarchyTheme { background: "#000000".into(), foreground: "#ffffff".into(), name: "A & B".into(), ..OmarchyTheme::default() };
        let xml = xml(&theme);
        assert!(xml.contains(r##"<color name="bg" value="#000000"/>"##), "{xml}");
        // Comments: the text faded 42% toward the background.
        assert!(xml.contains(r##"<color name="comment" value="#949494"/>"##), "{xml}");
        assert!(xml.contains("name=\"SUPER DESKTOP (A &amp; B)\""));
        assert!(xml.contains(r#"<style name="def:keyword" foreground="magenta" bold="true"/>"#));
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080");
        assert_eq!(rgb("nonsense"), (128, 128, 128));
    }
}
