use gtk4::gdk;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::RwLock;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OmarchyTheme {
    pub name: String,
    pub mode: String,

    pub accent: String,
    pub selection: String,
    pub muted: String,

    pub background: String,
    pub dark_background: String,
    pub darker_background: String,
    pub lighter_background: String,

    pub foreground: String,
    pub dark_foreground: String,
    pub light_foreground: String,
    pub bright_foreground: String,

    pub red: String,
    pub yellow: String,
    pub orange: String,
    pub green: String,
    pub cyan: String,
    pub blue: String,
    pub magenta: String,
    pub brown: String,

    pub bright_red: String,
    pub bright_yellow: String,
    pub bright_green: String,
    pub bright_cyan: String,
    pub bright_blue: String,
    pub bright_magenta: String,

    pub hyprland_active_border: Option<String>,
    pub hyprland_inactive_border: Option<String>,

    pub font_family: String,
    pub font_size: u32,
}

impl Default for OmarchyTheme {
    fn default() -> Self {
        Self {
            name: "Loca Deserta Dark".to_string(),
            mode: "dark".to_string(),

            accent: "#ece8e5".to_string(),
            selection: "#2e2d2b".to_string(),
            muted: "#524f4c".to_string(),

            background: "#10100f".to_string(),
            dark_background: "#0c0c0b".to_string(),
            darker_background: "#080807".to_string(),
            lighter_background: "#1c1b1a".to_string(),

            foreground: "#d1cdc9".to_string(),
            dark_foreground: "#736f6b".to_string(),
            light_foreground: "#b2aeaa".to_string(),
            bright_foreground: "#ece8e5".to_string(),

            red: "#d95750".to_string(),
            yellow: "#dbb471".to_string(),
            orange: "#d47844".to_string(),
            green: "#88ad7c".to_string(),
            cyan: "#5ca3ad".to_string(),
            blue: "#6b93b8".to_string(),
            magenta: "#a688b5".to_string(),
            brown: "#7a6552".to_string(),

            bright_red: "#ea6861".to_string(),
            bright_yellow: "#e8c484".to_string(),
            bright_green: "#9ec292".to_string(),
            bright_cyan: "#72bac5".to_string(),
            bright_blue: "#84acd2".to_string(),
            bright_magenta: "#ba9dca".to_string(),

            hyprland_active_border: Some("rgba(ece8e5ee) rgba(736f6bee) 45deg".to_string()),
            hyprland_inactive_border: Some("rgba(2e2d2baa)".to_string()),

            font_family: "Adwaita Mono".to_string(),
            font_size: 11,
        }
    }
}

#[allow(dead_code)]
impl OmarchyTheme {
    pub fn hex_to_rgba(hex: &str, alpha: f32) -> String {
        let clean = hex.trim().trim_start_matches('#');
        if clean.len() >= 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&clean[0..2], 16),
                u8::from_str_radix(&clean[2..4], 16),
                u8::from_str_radix(&clean[4..6], 16),
            ) {
                return format!("rgba({}, {}, {}, {:.3})", r, g, b, alpha);
            }
        }
        format!("rgba(20, 20, 20, {:.3})", alpha)
    }

    pub fn rgba_accent(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.accent, alpha)
    }

    pub fn rgba_bg(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.background, alpha)
    }

    pub fn rgba_dark_bg(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.dark_background, alpha)
    }

    pub fn rgba_darker_bg(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.darker_background, alpha)
    }

    pub fn rgba_lighter_bg(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.lighter_background, alpha)
    }

    pub fn rgba_fg(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.foreground, alpha)
    }

    pub fn rgba_muted(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.muted, alpha)
    }

    pub fn rgba_selection(&self, alpha: f32) -> String {
        Self::hex_to_rgba(&self.selection, alpha)
    }

    pub fn to_rgba_color(hex: &str) -> Option<gdk::RGBA> {
        gdk::RGBA::parse(hex).ok()
    }

    pub fn get_ansi_palette(&self) -> Vec<gdk::RGBA> {
        let color_strs = [
            &self.dark_background,   // 0: black
            &self.red,               // 1: red
            &self.green,             // 2: green
            &self.yellow,            // 3: yellow
            &self.blue,              // 4: blue
            &self.magenta,           // 5: magenta
            &self.cyan,              // 6: cyan
            &self.light_foreground,  // 7: white
            &self.dark_foreground,   // 8: bright black
            &self.bright_red,        // 9: bright red
            &self.bright_green,      // 10: bright green
            &self.bright_yellow,     // 11: bright yellow
            &self.bright_blue,       // 12: bright blue
            &self.bright_magenta,    // 13: bright magenta
            &self.bright_cyan,       // 14: bright cyan
            &self.bright_foreground, // 15: bright white
        ];

        color_strs
            .iter()
            .map(|s| gdk::RGBA::parse(*s).unwrap_or_else(|_| gdk::RGBA::new(0.5, 0.5, 0.5, 1.0)))
            .collect()
    }
}

static CURRENT_THEME: RwLock<Option<OmarchyTheme>> = RwLock::new(None);
static LAST_THEME_SIGNATURE: RwLock<Option<ThemeSignature>> = RwLock::new(None);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeSignature {
    colors_path: PathBuf,
    theme_name: String,
    colors: Vec<u8>,
}

fn theme_name_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local/state/omarchy/current/theme.name")
}

fn current_signature() -> ThemeSignature {
    let colors_path = get_theme_colors_path();
    ThemeSignature {
        colors: fs::read(&colors_path).unwrap_or_default(),
        colors_path,
        theme_name: fs::read_to_string(theme_name_path()).unwrap_or_default(),
    }
}

pub fn get_theme_colors_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let state_path = PathBuf::from(&home).join(".local/state/omarchy/current/theme/colors.toml");
    if state_path.exists() {
        return state_path;
    }

    let config_path = PathBuf::from(&home).join(".config/omarchy/current/theme/colors.toml");
    if config_path.exists() {
        return config_path;
    }

    PathBuf::from("/usr/share/omarchy/themes/loca-deserta-dark/colors.toml")
}

pub fn detect_font_family() -> String {
    if let Ok(output) = Command::new("fc-match")
        .args(["monospace", "-f", "%{family}\n"])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            if let Some(first) = s.lines().next() {
                let name = first.split(',').next().unwrap_or("").trim();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    "Adwaita Mono".to_string()
}

pub fn load_current_theme() -> OmarchyTheme {
    let mut theme = OmarchyTheme::default();

    let name_path = theme_name_path();
    if let Ok(name) = fs::read_to_string(&name_path) {
        let trimmed = name.trim().to_string();
        if !trimmed.is_empty() {
            theme.name = trimmed;
        }
    }

    theme.font_family = detect_font_family();

    let signature = current_signature();
    let colors_path = signature.colors_path.clone();

    if let Ok(content) = fs::read_to_string(&colors_path) {
        parse_colors_into(&content, &mut theme);
    }

    if let Ok(mut last) = LAST_THEME_SIGNATURE.write() {
        *last = Some(signature);
    }

    theme
}

fn parse_colors_into(content: &str, theme: &mut OmarchyTheme) {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, val)) = line.split_once('=') {
            let k = key.trim();
            let v = val.trim().trim_matches('"').trim_matches('\'').trim();

            match k {
                "mode" => theme.mode = v.to_string(),
                "accent" => theme.accent = v.to_string(),
                "selection" => theme.selection = v.to_string(),
                "muted" => theme.muted = v.to_string(),
                "background" => theme.background = v.to_string(),
                "dark_background" => theme.dark_background = v.to_string(),
                "darker_background" => theme.darker_background = v.to_string(),
                "lighter_background" => theme.lighter_background = v.to_string(),
                "foreground" => theme.foreground = v.to_string(),
                "dark_foreground" => theme.dark_foreground = v.to_string(),
                "light_foreground" => theme.light_foreground = v.to_string(),
                "bright_foreground" => theme.bright_foreground = v.to_string(),
                "red" => theme.red = v.to_string(),
                "yellow" => theme.yellow = v.to_string(),
                "orange" => theme.orange = v.to_string(),
                "green" => theme.green = v.to_string(),
                "cyan" => theme.cyan = v.to_string(),
                "blue" => theme.blue = v.to_string(),
                "magenta" => theme.magenta = v.to_string(),
                "brown" => theme.brown = v.to_string(),
                "bright_red" => theme.bright_red = v.to_string(),
                "bright_yellow" => theme.bright_yellow = v.to_string(),
                "bright_green" => theme.bright_green = v.to_string(),
                "bright_cyan" => theme.bright_cyan = v.to_string(),
                "bright_blue" => theme.bright_blue = v.to_string(),
                "bright_magenta" => theme.bright_magenta = v.to_string(),
                "hyprland_active_border" => theme.hyprland_active_border = Some(v.to_string()),
                "hyprland_inactive_border" => theme.hyprland_inactive_border = Some(v.to_string()),
                _ => {}
            }
        }
    }
}

pub fn current_theme() -> OmarchyTheme {
    if let Ok(read) = CURRENT_THEME.read() {
        if let Some(t) = read.as_ref() {
            return t.clone();
        }
    }

    let loaded = load_current_theme();
    if let Ok(mut write) = CURRENT_THEME.write() {
        *write = Some(loaded.clone());
    }
    loaded
}

pub fn reload_theme() -> OmarchyTheme {
    let loaded = load_current_theme();
    if let Ok(mut write) = CURRENT_THEME.write() {
        *write = Some(loaded.clone());
    }
    loaded
}

pub fn check_theme_changed() -> bool {
    let current = current_signature();
    LAST_THEME_SIGNATURE
        .read()
        .map(|last| last.as_ref() != Some(&current))
        .unwrap_or(true)
}

/// Return the current palette even when the Omarchy hook was missed.
///
/// The bridge calls this while serving phone clients, which gives both the app
/// and its widgets a polling fallback in addition to the instant `theme-set`
/// hook installed by super-desktop.
pub fn current_theme_fresh() -> OmarchyTheme {
    if check_theme_changed() {
        reload_theme()
    } else {
        current_theme()
    }
}
