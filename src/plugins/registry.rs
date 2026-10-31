//! The public list of plugins (github.com/gladimdim/super-desktop-plugins,
//! `registry.json`, registryVersion 2): fetched with curl, kept in the state
//! folder for offline use, searched by text and category (its tags). Each
//! entry installs from `https://github.com/<repo>` with `install::install_as`,
//! which checks the repository's plugin.json has the id the list gives.
//! Listing is not a review: an installed plugin still waits for approval.
use super::{manifest, paths};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/gladimdim/super-desktop-plugins/main/registry.json";
/// The registry format this SUPER DESKTOP reads.
pub const REGISTRY_VERSION: u64 = 2;
const MAX_BYTES: usize = 1 << 20;
/// A preview.png larger than this is not shown.
pub const MAX_PREVIEW_BYTES: usize = 3 << 19;

/// One plugin in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    /// GitHub `owner/name`.
    pub repo: String,
    /// Its categories.
    pub tags: Vec<String>,
    pub author: String,
    pub api: u64,
    pub min_host: String,
    pub slots: Vec<String>,
}

impl Entry {
    /// Where it installs from (`$SD_PLUGIN_REPO_BASE` replaces
    /// `https://github.com` in tests).
    pub fn git_url(&self) -> String {
        let base = std::env::var("SD_PLUGIN_REPO_BASE").unwrap_or_else(|_| "https://github.com".into());
        format!("{}/{}", base.trim_end_matches('/'), self.repo)
    }

    pub fn web_url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// Its `preview.png`, at the top of its repository (`$SD_PLUGIN_RAW_BASE`
    /// replaces `https://raw.githubusercontent.com` in tests).
    pub fn preview_url(&self) -> String {
        let base = std::env::var("SD_PLUGIN_RAW_BASE").unwrap_or_else(|_| "https://raw.githubusercontent.com".into());
        format!("{}/{}/HEAD/preview.png", base.trim_end_matches('/'), self.repo)
    }

    /// Why this SUPER DESKTOP may not run it, or `None`. The version check
    /// is advice (a build from source can have what a release lacks); a
    /// newer plugin API is not.
    pub fn compatibility(&self) -> Compatibility {
        if self.api > manifest::API_VERSION {
            Compatibility::NeedsApi(self.api)
        } else if manifest::version_older(manifest::HOST_VERSION, &self.min_host) {
            Compatibility::MadeForNewer(self.min_host.clone())
        } else {
            Compatibility::Ok
        }
    }

    /// Whether `query` (words, any case) matches its name, id, description,
    /// author or categories: every word somewhere.
    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!("{} {} {} {} {}", self.name, self.id, self.description, self.author, self.tags.join(" ")).to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(&word.to_lowercase()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Compatibility {
    Ok,
    /// Its `minHost` is newer than this SUPER DESKTOP.
    MadeForNewer(String),
    /// It needs a newer plugin API.
    NeedsApi(u64),
}

/// Whether version `a` is older than `b` ("1.5.0" < "1.6.0").
fn valid_repo(repo: &str) -> bool {
    let mut halves = repo.split('/');
    let ok = |part: Option<&str>| {
        part.is_some_and(|p| !p.is_empty() && p != "." && p != ".." && !p.starts_with('-')
            && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
    };
    ok(halves.next()) && ok(halves.next()) && halves.next().is_none()
}

/// Reads registry.json. Entries that are not well formed are left out (a bad
/// entry must not hide the others), and named in the second list.
pub fn parse(text: &str) -> Result<(Vec<Entry>, Vec<String>), String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("the plugin list is not valid JSON: {e}"))?;
    match value["registryVersion"].as_u64() {
        Some(REGISTRY_VERSION) => {}
        Some(v) if v > REGISTRY_VERSION => return Err(format!("the plugin list has format {v}; update SUPER DESKTOP to read it")),
        _ => return Err("the plugin list has no registryVersion this SUPER DESKTOP reads".into()),
    }
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for item in value["plugins"].as_array().ok_or("the plugin list has no \"plugins\"")? {
        let text = |name: &str| item[name].as_str().unwrap_or("").trim().to_owned();
        let list = |name: &str| item[name].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
        let entry = Entry {
            id: text("id"),
            name: text("name"),
            description: text("description"),
            repo: text("repo"),
            tags: list("tags"),
            author: text("author"),
            api: item["api"].as_u64().unwrap_or(0),
            min_host: text("minHost"),
            slots: list("slots"),
        };
        if manifest::valid_plugin_id(&entry.id) && valid_repo(&entry.repo) && !entry.name.is_empty() && entry.api >= 1 {
            entries.push(entry);
        } else {
            skipped.push(if entry.id.is_empty() { "an entry without an id".into() } else { entry.id });
        }
    }
    Ok((entries, skipped))
}

/// Where the list comes from: `$SD_PLUGIN_REGISTRY` (a URL or a file) or
/// the public registry.
pub fn source() -> String {
    std::env::var("SD_PLUGIN_REGISTRY").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_URL.into())
}

/// The last list fetched, kept for when there is no network.
pub fn cache_path() -> PathBuf {
    paths::state_dir().join("registry.json")
}

/// The list as last fetched, and when (seconds since the epoch).
pub fn cached() -> Option<(Vec<Entry>, u64)> {
    let path = cache_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let when = std::fs::metadata(&path).ok()?.modified().ok()?
        .duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    parse(&text).ok().map(|(entries, _)| (entries, when))
}

/// Fetches the list now (blocking: run it off the GTK thread), keeps a copy,
/// and returns it with the names of entries left out.
pub fn fetch() -> Result<(Vec<Entry>, Vec<String>), String> {
    let from = source();
    let text = if from.starts_with("https://") || from.starts_with("http://") {
        let out = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--max-time", "20", "--max-filesize", &MAX_BYTES.to_string(), "--", &from])
            .output()
            .map_err(|e| format!("cannot run curl ({e}); install curl to see the plugin list"))?;
        if !out.status.success() {
            return Err(format!("could not get the plugin list: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        String::from_utf8(out.stdout).map_err(|_| "the plugin list is not text".to_owned())?
    } else {
        std::fs::read_to_string(&from).map_err(|e| format!("cannot read {from}: {e}"))?
    };
    let parsed = parse(&text)?;
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &text);
    Ok(parsed)
}

fn previews_dir() -> PathBuf {
    paths::state_dir().join("previews")
}

/// A PNG file of at most MAX_PREVIEW_BYTES.
pub fn is_preview(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.len() == 0 || meta.len() as usize > MAX_PREVIEW_BYTES {
        return false;
    }
    let mut head = [0u8; 8];
    use std::io::Read;
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == *b"\x89PNG\r\n\x1a\n"
}

/// The entry's preview picture, downloaded once and kept (blocking: run it
/// off the GTK thread). `None` when it has none; that is remembered too,
/// until `again` asks for both again.
pub fn preview(entry: &Entry, again: bool) -> Option<PathBuf> {
    let dir = previews_dir();
    let path = dir.join(format!("{}.png", entry.id));
    let missing = dir.join(format!("{}.none", entry.id));
    if !again {
        if is_preview(&path) {
            return Some(path);
        }
        if missing.exists() {
            return None;
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    let part = dir.join(format!("{}.part", entry.id));
    let fetched = Command::new("curl")
        .args(["--fail", "--silent", "--location", "--max-time", "20", "--max-filesize", &MAX_PREVIEW_BYTES.to_string(), "--output"])
        .arg(&part)
        .args(["--", &entry.preview_url()])
        .status()
        .is_ok_and(|s| s.success());
    if fetched && is_preview(&part) && std::fs::rename(&part, &path).is_ok() {
        let _ = std::fs::remove_file(&missing);
        return Some(path);
    }
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::write(&missing, "");
    None
}

/// Every category (tag) in the list, sorted.
pub fn categories(entries: &[Entry]) -> Vec<String> {
    let mut tags: Vec<String> = entries.iter().flat_map(|e| e.tags.iter().cloned()).collect();
    tags.sort();
    tags.dedup();
    tags
}

/// How a list is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Name,
    /// Grouped under each entry's first category, groups by name.
    Category,
}

/// The entries matching `query` and `category` (`None`: all), ordered; with
/// `Order::Category`, each with the category it is shown under.
pub fn select<'a>(entries: &'a [Entry], query: &str, category: Option<&str>, order: Order) -> Vec<(&'a Entry, Option<String>)> {
    let mut found: Vec<(&Entry, Option<String>)> = entries
        .iter()
        .filter(|e| e.matches(query) && category.is_none_or(|c| e.tags.iter().any(|t| t == c)))
        .map(|e| {
            let group = match order {
                Order::Name => None,
                // Under the category picked, else its first one.
                Order::Category => Some(category.map(str::to_owned).or_else(|| e.tags.first().cloned()).unwrap_or_else(|| "other".into())),
            };
            (e, group)
        })
        .collect();
    found.sort_by(|(a, ga), (b, gb)| ga.cmp(gb).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    found
}

/// What `plugin install <source>` means: a folder or URL as given, else a
/// plugin id from the list (fetched, or the cached one offline).
pub fn resolve(source: &str) -> Result<(String, Option<Entry>), String> {
    if std::path::Path::new(source).exists() || source.contains("://") || source.contains('@') || !manifest::valid_plugin_id(source) {
        return Ok((source.to_owned(), None));
    }
    let entries = match fetch() {
        Ok((entries, _)) => entries,
        Err(e) => cached().map(|(entries, _)| entries).ok_or(e)?,
    };
    let entry = entries.into_iter().find(|e| e.id == source)
        .ok_or_else(|| format!("no plugin {source:?} in the plugin list; `super-desktop plugin search` shows them"))?;
    Ok((entry.git_url(), Some(entry)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"registryVersion": 2, "plugins": [
        {"id": "reminder", "name": "Reminder", "description": "A bell for reminders", "repo": "me/sd-reminder",
         "tags": ["time", "productivity"], "author": "me", "api": 1, "minHost": "1.6.0", "slots": ["popup", "toolbar.item"]},
        {"id": "clock", "name": "Clock", "description": "A dock clock", "repo": "you/clock", "tags": ["time"],
         "author": "you", "api": 1, "minHost": "1.0.0"},
        {"id": "snake", "name": "snake", "description": "A game", "repo": "x/snake", "tags": ["games"], "author": "x", "api": 9, "minHost": "1.0.0"},
        {"id": "Bad Id", "name": "Bad", "repo": "a/b", "api": 1, "minHost": "1.0.0"},
        {"id": "evil", "name": "Evil", "repo": "--upload-pack=x/y", "api": 1, "minHost": "1.0.0"},
        {"id": "dots", "name": "Dots", "repo": "../etc", "api": 1, "minHost": "1.0.0"}
    ]}"#;

    #[test]
    fn plugins_registry_parses_and_leaves_out_bad_entries() {
        let (entries, skipped) = parse(LIST).unwrap();
        assert_eq!(entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["reminder", "clock", "snake"]);
        assert_eq!(skipped, ["Bad Id", "evil", "dots"]);
        assert_eq!(entries[0].git_url(), "https://github.com/me/sd-reminder");
        assert_eq!(entries[0].slots, ["popup", "toolbar.item"]);
        assert!(parse(r#"{"registryVersion": 3, "plugins": []}"#).unwrap_err().contains("update SUPER DESKTOP"));
        assert!(parse(r#"{"registryVersion": 1, "plugins": []}"#).is_err());
        assert!(parse("nope").is_err());
    }

    #[test]
    fn plugins_registry_compatibility() {
        let (entries, _) = parse(LIST).unwrap();
        assert!(manifest::version_older("1.5.0", "1.6.0") && manifest::version_older("1.9.0", "1.10.0") && !manifest::version_older("1.6.0", "1.6.0") && !manifest::version_older("2.0.0", "1.6.0"));
        assert_eq!(entries[1].compatibility(), Compatibility::Ok);
        assert_eq!(entries[2].compatibility(), Compatibility::NeedsApi(9));
        let newer = Entry { min_host: "99.0.0".into(), ..entries[1].clone() };
        assert_eq!(newer.compatibility(), Compatibility::MadeForNewer("99.0.0".into()));
    }

    #[test]
    fn plugins_registry_search_categories_and_order() {
        let (entries, _) = parse(LIST).unwrap();
        assert_eq!(categories(&entries), ["games", "productivity", "time"]);
        let ids = |found: Vec<(&Entry, Option<String>)>| found.iter().map(|(e, g)| format!("{}{}", e.id, g.as_ref().map(|g| format!("@{g}")).unwrap_or_default())).collect::<Vec<_>>();
        // By name, any case; every word must match somewhere.
        assert_eq!(ids(select(&entries, "", None, Order::Name)), ["clock", "reminder", "snake"]);
        assert_eq!(ids(select(&entries, "BELL", None, Order::Name)), ["reminder"]);
        assert_eq!(ids(select(&entries, "a game", None, Order::Name)), ["snake"]);
        assert_eq!(ids(select(&entries, "you", None, Order::Name)), ["clock"]);
        // A category filters; by category groups under the first tag.
        assert_eq!(ids(select(&entries, "", Some("time"), Order::Name)), ["clock", "reminder"]);
        assert_eq!(ids(select(&entries, "", None, Order::Category)), ["snake@games", "clock@time", "reminder@time"]);
        assert_eq!(ids(select(&entries, "", Some("productivity"), Order::Category)), ["reminder@productivity"]);
    }

    #[test]
    fn plugins_registry_fetches_a_file_and_keeps_a_copy() {
        let dir = std::env::temp_dir().join(format!("sd-plugins-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("registry.json");
        std::fs::write(&file, LIST).unwrap();
        // Library tests run without the private test root: point both here.
        std::env::set_var("SD_PLUGIN_REGISTRY", &file);
        std::env::set_var("SD_PLUGINS_STATE_DIR", dir.join("state"));
        let (entries, _) = fetch().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(cached().unwrap().0, entries);
        // An id installs from its repository; a URL or folder is kept.
        assert_eq!(resolve("clock").unwrap().0, "https://github.com/you/clock");
        assert!(resolve("missing").unwrap_err().contains("plugin search"));
        assert_eq!(resolve("https://example.com/x.git").unwrap(), ("https://example.com/x.git".into(), None));
        assert_eq!(resolve(dir.to_str().unwrap()).unwrap().1, None);
        // preview.png from the repository, kept; a missing one is remembered.
        let raw = dir.join("raw");
        std::fs::create_dir_all(raw.join("you/clock/HEAD")).unwrap();
        std::fs::write(raw.join("you/clock/HEAD/preview.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        std::env::set_var("SD_PLUGIN_RAW_BASE", format!("file://{}", raw.display()));
        let clock = &entries[1];
        let shown = preview(clock, false).unwrap();
        assert!(is_preview(&shown));
        std::fs::remove_dir_all(raw.join("you")).unwrap();
        assert_eq!(preview(clock, false), Some(shown.clone()), "kept");
        assert_eq!(preview(&entries[0], false), None);
        assert!(dir.join("state/previews/reminder.none").exists());
        // Not a PNG: not shown.
        std::fs::create_dir_all(raw.join("me/sd-reminder/HEAD")).unwrap();
        std::fs::write(raw.join("me/sd-reminder/HEAD/preview.png"), b"<html>").unwrap();
        assert_eq!(preview(&entries[0], true), None);
        std::env::remove_var("SD_PLUGIN_RAW_BASE");
        std::env::remove_var("SD_PLUGIN_REGISTRY");
        std::env::remove_var("SD_PLUGINS_STATE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
