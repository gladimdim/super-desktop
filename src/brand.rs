//! Company/product logos for the toolbar agent buttons.
//!
//! SVG files live in `assets/logos/` (vendored, see ATTRIBUTION.md there).
//! Derived SVG templates use the active accent/background palette. Original
//! brand variants remain available as a fallback if the cache is not writable. When a logo file is
//! missing, callers fall back to the old emoji label.

use std::path::PathBuf;

/// Toolbar logo size in px.
pub const BRAND_ICON_SIZE: i32 = 18;

/// Map an agent key to its vendored logo file for the given theme mode.
pub fn logo_filename(agent: &str, light_theme: bool) -> Option<&'static str> {
    let logos = logo_manifest();
    let agent = match agent {
        "agy" => "antigravity",
        "bash" | "terminal" => "shell",
        _ => agent,
    };
    logos[agent][if light_theme { "light" } else { "dark" }].as_str()
}

fn logo_manifest() -> &'static serde_json::Value {
    static LOGOS: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    LOGOS.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/logos/harness-logos.json"))
            .expect("vendored logo manifest")
    })
}

fn paint_template(svg: &str, ink: &str, surface: &str) -> String {
    svg.replace("#123456", ink).replace("#fedcba", surface)
}

fn themed_logo_path(agent: &str) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let agent = match agent {
        "agy" => "antigravity",
        "bash" | "terminal" => "shell",
        _ => agent,
    };
    let name = logo_manifest()[agent]["themed"].as_str()?;
    let source = asset_path(name)?;
    let theme = crate::theme::current_theme();
    let color = |value: &str| -> Option<String> {
        let rgba = gtk4::gdk::RGBA::parse(value).ok()?;
        Some(format!(
            "#{:02x}{:02x}{:02x}",
            (rgba.red() * 255.0).round() as u8,
            (rgba.green() * 255.0).round() as u8,
            (rgba.blue() * 255.0).round() as u8
        ))
    };
    let svg = paint_template(
        &std::fs::read_to_string(source).ok()?,
        &color(&theme.accent)?,
        &color(&theme.background)?,
    );
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))?
        .join("super-desktop/harness-logos");
    let digest = format!("{:x}", Sha256::digest(svg.as_bytes()));
    let path = cache.join(format!("{digest}.svg"));
    if !path.is_file() {
        std::fs::create_dir_all(&cache).ok()?;
        let temporary = cache.join(format!("{digest}-{}.tmp", std::process::id()));
        std::fs::write(&temporary, svg).ok()?;
        std::fs::rename(temporary, &path).ok()?;
    }
    Some(path)
}

fn asset_path(name: &str) -> Option<PathBuf> {
    if let Some(dir) = find_logos_dir() {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/logos")
        .join(name);
    path.is_file().then_some(path)
}

/// Packaged builds use /usr/share/super-desktop/assets. Other builds use the copy under
/// `~/.config/super-desktop/assets/logos` first, then the `assets/logos`
/// folder of the repo checkout relative to the executable
/// (`<repo>/target/{release,debug}/super-desktop`).
pub fn find_logos_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(path) = bundle_asset("logos") { return Some(path); }
    #[cfg(target_os = "linux")]
    if let Some(path) = packaged_assets("logos") {
        return Some(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".config/super-desktop/assets/logos");
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe
            .parent()
            .and_then(|d| d.parent())
            .and_then(|t| t.parent())
        {
            let p = root.join("assets/logos");
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
}

/// Full path to an agent's logo for the given theme mode, if vendored.
pub fn logo_path(agent: &str, light_theme: bool) -> Option<PathBuf> {
    themed_logo_path(agent).or_else(|| asset_path(logo_filename(agent, light_theme)?))
}

/// Locate the vendored icon-theme root (`assets/icons`, containing `hicolor/`).
/// Same resolution order as [`find_logos_dir`]: package assets, installed copy, then the
/// repo checkout relative to the executable.
pub fn find_icons_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(path) = bundle_asset("icons") { return Some(path); }
    #[cfg(target_os = "linux")]
    if let Some(path) = packaged_assets("icons") {
        return Some(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".config/super-desktop/assets/icons");
        if p.join("hicolor").is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe
            .parent()
            .and_then(|d| d.parent())
            .and_then(|t| t.parent())
        {
            let p = root.join("assets/icons");
            if p.join("hicolor").is_dir() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn bundle_asset(kind: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let path = executable.parent()?.parent()?.join("Resources/assets").join(kind);
    path.is_dir().then_some(path)
}

// Package upgrades own these assets. An old installer's per-user copy must
// not shadow the assets matching the packaged executable.
#[cfg(target_os = "linux")]
fn packaged_assets(kind: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if exe != std::path::Path::new("/usr/lib/super-desktop/super-desktop") {
        return None;
    }
    let path = PathBuf::from("/usr/share/super-desktop/assets").join(kind);
    path.is_dir().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themed_templates_use_palette_without_recoloring_masks() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/logos");
        for (_, entry) in logo_manifest().as_object().unwrap() {
            let svg = std::fs::read_to_string(dir.join(entry["themed"].as_str().unwrap())).unwrap();
            assert!(svg.contains("#123456"));
            let colored = paint_template(&svg, "#aabbcc", "#112233");
            assert!(!colored.contains("#123456") && !colored.contains("#fedcba"));
            assert!(colored.contains("#aabbcc"));
        }
        let svg = std::fs::read_to_string(dir.join("kiro-themed.svg")).unwrap();
        assert!(paint_template(&svg, "#aabbcc", "#112233").contains("fill=\"white\""));
    }

    #[test]
    fn test_vendored_icons_exist_and_parse() {
        // Same rule as logos: only checked when running from a checkout that
        // ships the files; an installed location without them is skipped.
        let Some(root) = find_icons_root() else {
            return;
        };
        for name in [
            "sd-gears-symbolic.svg",
            "sd-arrange-symbolic.svg",
            "sd-hide-symbolic.svg",
        ] {
            let p = root.join("hicolor/scalable/actions").join(name);
            assert!(p.is_file(), "missing vendored HUD icon {name}");
            let text = std::fs::read_to_string(&p).unwrap();
            assert!(text.contains("<svg"), "{name} is not an SVG");
        }
    }

    #[test]
    fn test_logo_filename_mapping() {
        assert_eq!(logo_filename("claude", false), Some("claude.svg"));
        assert_eq!(logo_filename("claude", true), Some("claude.svg"));
        assert_eq!(logo_filename("codex", false), Some("openai-white.svg"));
        assert_eq!(logo_filename("opencode", false), Some("opencode-dark.svg"));
        assert_eq!(logo_filename("opencode", true), Some("opencode-light.svg"));
        assert_eq!(logo_filename("grok", true), Some("grok-mark-light.svg"));
        assert_eq!(logo_filename("shell", false), Some("shell-white.svg"));
        // Use the product mark, not the Google company mark.
        assert_eq!(logo_filename("antigravity", false), Some("antigravity.svg"));
        // DeepSeek Harness has no product mark of its own: DeepSeek's.
        assert_eq!(logo_filename("dsh", false), Some("deepseek.svg"));
        assert_eq!(logo_filename("dsh", true), Some("deepseek.svg"));
        assert_eq!(logo_manifest()["dsh"]["themed"].as_str(), Some("dsh-themed.svg"));
        assert_eq!(logo_filename("unknown-agent", false), None);
    }

    #[test]
    fn test_vendored_logos_exist_and_parse() {
        // Always validate checkout assets, never an older installed bundle.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/logos");
        for agent in crate::tmux::HARNESS_KEYS
            .iter()
            .copied()
            .filter(|key| *key != "herder")
        {
            for light in [false, true] {
                let name = logo_filename(agent, light).unwrap();
                assert!(
                    dir.join(name).is_file(),
                    "missing vendored logo {name} for agent {agent}"
                );
            }
        }
    }
}
