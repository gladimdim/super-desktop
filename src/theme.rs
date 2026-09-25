use gtk4::gdk;
use std::env;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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
static LAST_THEME: RwLock<Option<SeenTheme>> = RwLock::new(None);

/// What the theme files say. A change of theme is a change of these contents.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeSignature {
    colors_path: PathBuf,
    theme_name: String,
    colors: Vec<u8>,
}

impl ThemeSignature {
    fn read(colors_path: PathBuf, name_path: &Path) -> Self {
        Self {
            colors: fs::read(&colors_path).unwrap_or_default(),
            colors_path,
            theme_name: fs::read_to_string(name_path).unwrap_or_default(),
        }
    }
}

/// One file as `stat` sees it, `None` when it is missing. Writing, replacing
/// or renaming the file changes at least one field — `ctime` included, which
/// neither `cp -p` nor `touch` can set back — so an equal stamp means the file
/// was not touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        })
    }
}

/// Both theme files as `stat` sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeStamp {
    colors_path: PathBuf,
    colors: Option<FileStamp>,
    theme_name: Option<FileStamp>,
}

impl ThemeStamp {
    fn of(colors_path: &Path, name_path: &Path) -> Self {
        Self {
            colors_path: colors_path.to_path_buf(),
            colors: FileStamp::of(colors_path),
            theme_name: FileStamp::of(name_path),
        }
    }
}

/// The theme the daemon last loaded. The contents decide whether the theme
/// changed; the stamp only lets an untouched pair of files skip being read —
/// the overlay checks every second while it is visible.
#[derive(Debug, Clone)]
struct SeenTheme {
    stamp: ThemeStamp,
    signature: ThemeSignature,
}

impl SeenTheme {
    /// Stamp first, then read: the stamp kept is never newer than the
    /// contents, so a write racing the load is noticed by the next check.
    fn load(colors_path: PathBuf, name_path: &Path) -> Self {
        let stamp = ThemeStamp::of(&colors_path, name_path);
        Self { stamp, signature: ThemeSignature::read(colors_path, name_path) }
    }

    /// Whether the files now say something else than when they were seen.
    /// Untouched files are only `stat`ed; touched ones are read and compared,
    /// and a rewrite with the same bytes just refreshes the stamp.
    fn changed(&mut self, colors_path: PathBuf, name_path: &Path) -> bool {
        let stamp = ThemeStamp::of(&colors_path, name_path);
        if stamp == self.stamp {
            return false;
        }
        if ThemeSignature::read(colors_path, name_path) != self.signature {
            return true;
        }
        self.stamp = stamp;
        false
    }
}

fn theme_name_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local/state/omarchy/current/theme.name")
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

    let seen = SeenTheme::load(get_theme_colors_path(), &name_path);

    // The bytes the signature holds, so the palette and what later checks
    // compare against are one read of the file.
    if let Ok(content) = std::str::from_utf8(&seen.signature.colors) {
        parse_colors_into(content, &mut theme);
    }

    if let Ok(mut last) = LAST_THEME.write() {
        *last = Some(seen);
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
    LAST_THEME
        .write()
        .map(|mut last| match last.as_mut() {
            Some(seen) => seen.changed(get_theme_colors_path(), &theme_name_path()),
            None => true,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A private copy of Omarchy's two theme files.
    struct Files {
        dir: PathBuf,
        colors: PathBuf,
        name: PathBuf,
    }

    impl Files {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("sd-theme-{test}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let files = Self { colors: dir.join("colors.toml"), name: dir.join("theme.name"), dir };
            files.replace(&files.colors, "accent = \"#aabbcc\"\n");
            files.replace(&files.name, "Tokyo Night\n");
            files
        }

        /// Write through a new file renamed over the old one, as a theme switch
        /// does. The new file exists before the old one goes, so it never gets
        /// the old inode back.
        fn replace(&self, path: &Path, text: &str) {
            let next = self.dir.join("next");
            fs::write(&next, text).unwrap();
            fs::rename(&next, path).unwrap();
        }

        fn load(&self) -> SeenTheme {
            SeenTheme::load(self.colors.clone(), &self.name)
        }

        fn changed(&self, seen: &mut SeenTheme) -> bool {
            seen.changed(self.colors.clone(), &self.name)
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn theme_check_trusts_an_untouched_stamp_without_reading() {
        let files = Files::new("untouched");
        let mut seen = files.load();
        assert!(!files.changed(&mut seen));
        // Were the files read, these contents would differ from them: an
        // unchanged stamp must answer without reading.
        seen.signature.colors = b"something else".to_vec();
        seen.signature.theme_name = "Another".into();
        assert!(!files.changed(&mut seen));
    }

    #[test]
    fn theme_check_ignores_a_rewrite_with_the_same_bytes() {
        let files = Files::new("same-bytes");
        let mut seen = files.load();
        files.replace(&files.colors, "accent = \"#aabbcc\"\n");
        files.replace(&files.name, "Tokyo Night\n");
        assert_ne!(seen.stamp, ThemeStamp::of(&files.colors, &files.name));
        assert!(!files.changed(&mut seen), "same contents are the same theme");
        // …and the new stamp is kept, so the next check is `stat` only again.
        assert_eq!(seen.stamp, ThemeStamp::of(&files.colors, &files.name));
        seen.signature.colors.clear();
        assert!(!files.changed(&mut seen));
    }

    #[test]
    fn theme_check_sees_every_real_change() {
        let files = Files::new("changes");
        // Same length, different palette.
        let mut seen = files.load();
        files.replace(&files.colors, "accent = \"#ccbbaa\"\n");
        assert!(files.changed(&mut seen));
        // A change is reported until the theme is loaded again.
        assert!(files.changed(&mut seen));
        let mut seen = files.load();
        assert!(!files.changed(&mut seen));

        // Only the name moves.
        files.replace(&files.name, "Catppuccin\n");
        assert!(files.changed(&mut seen));

        // A missing file coming back is a change; so is it going away.
        let mut seen = files.load();
        fs::remove_file(&files.name).unwrap();
        assert!(files.changed(&mut seen));
        let mut seen = files.load();
        assert!(!files.changed(&mut seen));
        files.replace(&files.name, "Catppuccin\n");
        assert!(files.changed(&mut seen));

        // The colors path switching (state dir appearing over config dir).
        let mut seen = files.load();
        let other = files.dir.join("other.toml");
        fs::write(&other, "accent = \"#000000\"\n").unwrap();
        assert!(seen.changed(other, &files.name));
    }
}
