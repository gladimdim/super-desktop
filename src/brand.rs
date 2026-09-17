//! Company/product logos for the toolbar agent buttons.
//!
//! SVG files live in `assets/logos/` (vendored, see ATTRIBUTION.md there).
//! Monochrome marks ship black/white variants selected by theme mode so they
//! stay visible on both dark and light Omarchy themes. When a logo file is
//! missing, callers fall back to the old emoji label.

use std::path::PathBuf;

/// Toolbar logo size in px.
pub const BRAND_ICON_SIZE: i32 = 18;

/// Map an agent key to its vendored logo file for the given theme mode.
pub fn logo_filename(agent: &str, light_theme: bool) -> Option<&'static str> {
    match agent {
        // Antigravity ships only a wide wordmark lockup (unreadable at 18px),
        // so the Google company mark is used instead.
        "antigravity" | "agy" => Some("google.svg"),
        "claude" => Some(if light_theme {
            "anthropic-black.svg"
        } else {
            "anthropic-white.svg"
        }),
        "codex" => Some(if light_theme {
            "openai-black.svg"
        } else {
            "openai-white.svg"
        }),
        "opencode" => Some(if light_theme {
            "opencode-light.svg"
        } else {
            "opencode-dark.svg"
        }),
        "grok" => Some(if light_theme {
            "grok-black.svg"
        } else {
            "grok-white.svg"
        }),
        // No vendored mark for Reasonix yet, so the HUD falls back to the
        // emoji label (see the agent table in window.rs).
        "reasonix" => None,
        "shell" | "bash" | "terminal" => Some(if light_theme {
            "shell-black.svg"
        } else {
            "shell-white.svg"
        }),
        _ => None,
    }
}

/// Locate the vendored logos directory: installed copy under
/// `~/.config/super-desktop/assets/logos` first, then the `assets/logos`
/// folder of the repo checkout relative to the executable
/// (`<repo>/target/{release,debug}/super-desktop`).
pub fn find_logos_dir() -> Option<PathBuf> {
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
    let dir = find_logos_dir()?;
    let p = dir.join(logo_filename(agent, light_theme)?);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logo_filename_mapping() {
        assert_eq!(logo_filename("claude", false), Some("anthropic-white.svg"));
        assert_eq!(logo_filename("claude", true), Some("anthropic-black.svg"));
        assert_eq!(logo_filename("codex", false), Some("openai-white.svg"));
        assert_eq!(logo_filename("opencode", false), Some("opencode-dark.svg"));
        assert_eq!(logo_filename("opencode", true), Some("opencode-light.svg"));
        assert_eq!(logo_filename("grok", true), Some("grok-black.svg"));
        assert_eq!(logo_filename("shell", false), Some("shell-white.svg"));
        // Antigravity uses the Google company mark.
        assert_eq!(logo_filename("antigravity", false), Some("google.svg"));
        assert_eq!(logo_filename("unknown-agent", false), None);
    }

    #[test]
    fn test_vendored_logos_exist_and_parse() {
        // Only checks files that are actually vendored next to this checkout;
        // skipped silently when running from an installed location without them.
        let Some(dir) = find_logos_dir() else {
            return;
        };
        for agent in ["antigravity", "claude", "codex", "opencode", "grok", "shell"] {
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
