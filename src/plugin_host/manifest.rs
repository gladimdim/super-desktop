//! `super-desktop-plugin.json`: parsing and validation.
//!
//! The normative contract is `skills/super-desktop-plugin/schemas/manifest.schema.json`.
//! This module accepts exactly what that schema accepts and adds the rules a
//! JSON schema cannot express (listed at the end of
//! `skills/super-desktop-plugin/references/manifest.md`). The shared fixture
//! corpus in `tests/fixtures/plugins/manifests/` is checked against both.
//!
//! Every problem names where it is, what is wrong, how to fix it, and which
//! reference section explains it, so a coding agent can fix its own mistake.
use super::version_range::Range;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "super-desktop-plugin.json";

pub const PERMISSIONS: [&str; 16] = [
    "ui.toolbar",
    "ui.cardButtons",
    "ui.cardControls",
    "ui.titles",
    "ui.popup",
    "ui.notify",
    "shortcuts.global",
    "shortcuts.overlay",
    "cards.read",
    "cards.control",
    "terminal.read",
    "terminal.write",
    "harness.provide",
    "harness.launch",
    "layout.renderer",
    "llm",
];

/// Contribution points this build draws. The others validate but are not
/// active yet; `plugin describe` lists both so agents do not guess.
pub const SUPPORTED_CONTRIBUTIONS: [&str; 5] = ["commands", "shortcuts", "toolbar", "settings", "views"];
pub const ALL_CONTRIBUTIONS: [&str; 11] = [
    "commands",
    "shortcuts",
    "toolbar",
    "toolbarHide",
    "cardButtons",
    "cardControls",
    "titles",
    "renderer",
    "harnesses",
    "settings",
    "views",
];

const INTERPRETERS: [&str; 7] = ["python3", "python", "node", "bun", "deno", "bash", "sh"];
const BUILTIN_CONTROLS: [&str; 4] = ["builtin:iconify", "builtin:restore", "builtin:expand", "builtin:close"];
const MAX_ICON_BYTES: u64 = 256 * 1024;
const MAX_WASM_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    pub engines: Engines,
    #[serde(default)]
    pub main: Option<MainSpec>,
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
    #[serde(default)]
    pub activation: Vec<String>,
    pub permissions: Vec<String>,
    /// `"none"` or a sandbox profile; parsed by `sandbox()`.
    #[serde(default)]
    pub sandbox: Option<Value>,
    pub contributes: Contributes,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Engines {
    pub super_desktop: String,
    pub plugin_api: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MainSpec {
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub arch: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SandboxSpec {
    None,
    Profile(Sandbox),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Contributes {
    #[serde(default)]
    pub commands: Vec<Command>,
    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default)]
    pub toolbar: Vec<ToolbarItem>,
    #[serde(default)]
    pub toolbar_hide: Vec<String>,
    #[serde(default)]
    pub card_buttons: Vec<CardButton>,
    #[serde(default)]
    pub card_controls: Option<CardControls>,
    #[serde(default)]
    pub titles: Option<Value>,
    #[serde(default)]
    pub renderer: Option<Renderer>,
    #[serde(default)]
    pub harnesses: Vec<Harness>,
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default)]
    pub views: Vec<View>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub id: String,
    pub command: String,
    pub default: String,
    pub scope: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolbarItem {
    pub id: String,
    pub icon: String,
    #[serde(default)]
    pub label: Option<String>,
    pub tooltip: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub view: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardButton {
    pub id: String,
    pub icon: String,
    pub tooltip: String,
    pub command: String,
    #[serde(default)]
    pub show_in_icon: bool,
    #[serde(default)]
    pub when: Option<When>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    #[serde(default)]
    pub agents: Option<Vec<String>>,
    #[serde(default)]
    pub status: Option<Vec<String>>,
    #[serde(default)]
    pub iconified: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardControls {
    pub id: String,
    /// Entries are `"builtin:…"` strings or `{id, icon, tooltip, command}`.
    pub controls: Vec<Value>,
    #[serde(default)]
    pub icon_controls: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Renderer {
    pub id: String,
    pub wasm: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Harness {
    pub id: String,
    pub name: String,
    pub glyph: String,
    #[serde(default)]
    pub logo: Option<String>,
    pub binaries: Vec<String>,
    #[serde(default)]
    pub launch: Option<HarnessLaunch>,
    #[serde(default)]
    pub resume: Option<Vec<String>>,
    pub adapter: HarnessAdapter,
    #[serde(default)]
    pub status: Option<HarnessStatus>,
    #[serde(default)]
    pub prompts: Option<HarnessPrompts>,
    #[serde(default)]
    pub completion: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessLaunch {
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessAdapter {
    pub kind: String,
    #[serde(default)]
    pub setup: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessStatus {
    #[serde(default)]
    pub work: Vec<String>,
    #[serde(default)]
    pub shell_fallback: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessPrompts {
    #[serde(default)]
    pub injected_prefixes: Vec<String>,
    #[serde(default)]
    pub placeholder_titles: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub values: Option<Vec<String>>,
    /// `file` or `directory`, for `path` and `paths`.
    #[serde(default, rename = "kind")]
    pub path_kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub id: String,
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl Manifest {
    pub fn sandbox(&self) -> SandboxSpec {
        match &self.sandbox {
            Some(Value::String(text)) if text == "none" => SandboxSpec::None,
            Some(value) => SandboxSpec::Profile(serde_json::from_value(value.clone()).unwrap_or_default()),
            None => SandboxSpec::Profile(Sandbox::default()),
        }
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    pub fn setting(&self, key: &str) -> Option<&Setting> {
        self.contributes.settings.iter().find(|s| s.key == key)
    }

    /// Contribution points this manifest uses.
    pub fn used_contributions(&self) -> Vec<&'static str> {
        let c = &self.contributes;
        let mut used = Vec::new();
        for (name, present) in [
            ("commands", !c.commands.is_empty()),
            ("shortcuts", !c.shortcuts.is_empty()),
            ("toolbar", !c.toolbar.is_empty()),
            ("toolbarHide", !c.toolbar_hide.is_empty()),
            ("cardButtons", !c.card_buttons.is_empty()),
            ("cardControls", c.card_controls.is_some()),
            ("titles", c.titles.is_some()),
            ("renderer", c.renderer.is_some()),
            ("harnesses", !c.harnesses.is_empty()),
            ("settings", !c.settings.is_empty()),
            ("views", !c.views.is_empty()),
        ] {
            if present {
                used.push(name);
            }
        }
        used
    }

    /// Whether anything needs the process component.
    pub fn needs_process(&self) -> bool {
        let c = &self.contributes;
        !c.commands.is_empty() || !c.views.is_empty() || c.titles.is_some() || !c.settings.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Problem {
    pub path: String,
    pub message: String,
    pub hint: String,
    pub docs: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub ok: bool,
    pub errors: Vec<Problem>,
    pub warnings: Vec<Problem>,
}

/// Validated manifest plus the problems found. `manifest` is `None` when the
/// JSON did not even have the right shape.
pub struct Loaded {
    pub manifest: Option<Manifest>,
    pub report: Report,
}

pub fn load_dir(dir: &Path) -> Loaded {
    let path = dir.join(FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => validate_text(&text, Some(dir)),
        Err(error) => {
            let mut checker = Checker::default();
            checker.error(
                "",
                format!("cannot read {}: {error}", path.display()),
                format!("Put {FILE_NAME} at the root of the plugin directory."),
                "references/manifest.md",
            );
            checker.finish(None)
        }
    }
}

/// Validate manifest text. With `dir`, files the manifest names are checked too.
pub fn validate_text(text: &str, dir: Option<&Path>) -> Loaded {
    let mut checker = Checker::default();
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            checker.error("", format!("not valid JSON: {error}"), "Fix the JSON syntax first.", "references/manifest.md");
            return checker.finish(None);
        }
    };
    let manifest: Manifest = match serde_json::from_value(value) {
        Ok(manifest) => manifest,
        Err(error) => {
            checker.error(
                "",
                format!("wrong shape: {error}"),
                "Compare with schemas/manifest.schema.json: a field is missing, misspelled, unknown, or has the wrong type.",
                "references/manifest.md#top-level-fields",
            );
            return checker.finish(None);
        }
    };
    checker.check(&manifest, dir);
    checker.finish(Some(manifest))
}

#[derive(Default)]
struct Checker {
    errors: Vec<Problem>,
    warnings: Vec<Problem>,
}

impl Checker {
    fn error(&mut self, path: &str, message: impl Into<String>, hint: impl Into<String>, docs: &str) {
        self.errors.push(Problem { path: path.into(), message: message.into(), hint: hint.into(), docs: docs.into() });
    }

    fn warn(&mut self, path: &str, message: impl Into<String>, hint: impl Into<String>, docs: &str) {
        self.warnings.push(Problem { path: path.into(), message: message.into(), hint: hint.into(), docs: docs.into() });
    }

    fn finish(self, manifest: Option<Manifest>) -> Loaded {
        Loaded { report: Report { ok: self.errors.is_empty(), errors: self.errors, warnings: self.warnings }, manifest }
    }

    fn len(&mut self, path: &str, text: &str, min: usize, max: usize) {
        let n = text.chars().count();
        if n < min || n > max {
            self.error(path, format!("must be {min}–{max} characters, is {n}"), "Shorten or fill in the text.", "references/manifest.md");
        }
    }

    fn count<T>(&mut self, path: &str, items: &[T], max: usize) {
        if items.len() > max {
            self.error(path, format!("at most {max} entries, has {}", items.len()), "Remove entries; see the limits table.", "references/manifest.md#contribution-points");
        }
    }

    fn contribution_id(&mut self, path: &str, id: &str, plugin: &str, seen: &mut BTreeSet<String>) {
        if !is_contribution_id(id) || id.split_once('.').map(|(p, _)| p) != Some(plugin) {
            self.error(
                path,
                format!("contribution id `{id}` must be `{plugin}.<name>` (name: letters, digits, `_`, `-`, up to 48)"),
                format!("Rename it to `{plugin}.{}`.", id.rsplit('.').next().unwrap_or("name")),
                "references/manifest.md#contribution-points",
            );
        }
        if !seen.insert(id.to_string()) {
            self.error(path, format!("duplicate contribution id `{id}`"), "Give every contribution its own id.", "references/manifest.md#contribution-points");
        }
    }

    fn icon(&mut self, path: &str, icon: &str, dir: Option<&Path>) {
        if is_image_path(icon) {
            self.relative_file(path, icon, dir, MAX_ICON_BYTES);
        } else if icon.is_empty() || icon.len() > 32 || icon.chars().any(|c| c.is_ascii_alphabetic() || c.is_whitespace()) {
            self.error(path, format!("icon `{icon}` is neither one emoji nor a .svg/.png path"), "Use one emoji such as \"🌿\", or a file like \"icons/git.svg\".", "references/manifest.md#contribution-points");
        }
    }

    /// A path inside the plugin directory: relative, no `..`, and (with `dir`)
    /// an existing file of at most `max` bytes that stays inside after symlinks.
    fn relative_file(&mut self, path: &str, file: &str, dir: Option<&Path>, max: u64) {
        if !is_relative_path(file) {
            self.error(path, format!("`{file}` must be a path inside the plugin: no leading `/`, no `..`, only letters, digits, `_`, `.`, `-`, `/`"), "Use a path relative to the manifest, e.g. \"icons/app.svg\".", "references/manifest.md#rules-the-validator-adds-to-the-schema");
            return;
        }
        let Some(dir) = dir else { return };
        let full = dir.join(file);
        let inside = match (full.canonicalize(), dir.canonicalize()) {
            (Ok(real), Ok(root)) => real.starts_with(&root).then_some(real),
            (Err(_), _) => {
                self.error(path, format!("`{file}` does not exist"), "Add the file or fix the path.", "references/manifest.md#rules-the-validator-adds-to-the-schema");
                return;
            }
            _ => None,
        };
        let Some(real) = inside else {
            self.error(path, format!("`{file}` resolves outside the plugin directory"), "Do not use symlinks that point outside the repository.", "references/manifest.md#rules-the-validator-adds-to-the-schema");
            return;
        };
        match std::fs::metadata(&real) {
            Ok(meta) if !meta.is_file() => self.error(path, format!("`{file}` is not a file"), "Point at a file, not a directory.", "references/manifest.md#rules-the-validator-adds-to-the-schema"),
            Ok(meta) if meta.len() > max => self.error(path, format!("`{file}` is {} bytes; the limit is {max}", meta.len()), "Make the file smaller.", "references/manifest.md#rules-the-validator-adds-to-the-schema"),
            _ => {}
        }
    }

    fn check(&mut self, m: &Manifest, dir: Option<&Path>) {
        const TOP: &str = "references/manifest.md#top-level-fields";
        if m.manifest_version != 1 {
            self.error("manifestVersion", "must be 1", "Set \"manifestVersion\": 1.", TOP);
        }
        if !is_plugin_id(&m.id) {
            self.error("id", format!("`{}` is not a valid plugin id", m.id), "Use 3–40 lowercase letters, digits and hyphens, starting with a letter and not ending with a hyphen, e.g. \"git-flush\".", TOP);
        }
        self.len("name", &m.name, 1, 48);
        self.len("description", &m.description, 1, 200);
        if let Some(author) = &m.author {
            self.len("author", author, 0, 100);
        }
        if let Some(license) = &m.license {
            self.len("license", license, 0, 64);
        }
        if !is_semver(&m.version) {
            self.error("version", format!("`{}` is not a semantic version", m.version), "Use MAJOR.MINOR.PATCH, e.g. \"0.1.0\".", TOP);
        }
        if let Some(repository) = &m.repository {
            if !is_github_repository(repository) {
                self.error("repository", "must be https://github.com/<owner>/<repo>", "Use the repository's GitHub URL.", TOP);
            }
        }
        if m.engines.plugin_api != "1" {
            self.error("engines.pluginApi", format!("plugin API `{}` is not supported; this build speaks API 1", m.engines.plugin_api), "Set \"pluginApi\": \"1\".", TOP);
        }
        match Range::parse(&m.engines.super_desktop) {
            None => self.error("engines.superDesktop", format!("`{}` is not a version range", m.engines.super_desktop), "Use a range like \">=1.2.0\".", TOP),
            Some(range) if !range.contains(crate::updates::running()) => self.warn(
                "engines.superDesktop",
                format!("this build ({}) is outside `{}`; it will not activate the plugin", crate::updates::running(), m.engines.super_desktop),
                "Lower the range if the plugin works with this build, or update SUPER DESKTOP.",
                TOP,
            ),
            _ => {}
        }

        self.check_main(m, dir);

        for (key, asset) in &m.assets {
            let path = format!("assets.{key}");
            if !is_relative_path(key) {
                self.error(&path, "asset names are paths inside the plugin", "Use e.g. \"bin/tool\".", TOP);
            }
            if !asset.url.starts_with("https://") {
                self.error(&path, "url must be https://", "Attach the file to a GitHub Release and use its URL.", "references/publishing.md");
            }
            if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                self.error(&path, "sha256 must be 64 lowercase hex digits", "Run `sha256sum <file>`.", "references/publishing.md");
            }
            if asset.arch.as_deref().is_some_and(|a| a != "x86_64" && a != "aarch64") {
                self.error(&path, "arch must be x86_64 or aarch64", "Remove arch for platform-independent files.", TOP);
            }
        }
        self.count("assets", &m.assets.keys().collect::<Vec<_>>(), 16);

        let mut permissions = BTreeSet::new();
        for (i, permission) in m.permissions.iter().enumerate() {
            if !PERMISSIONS.contains(&permission.as_str()) {
                self.error(&format!("permissions[{i}]"), format!("unknown permission `{permission}`"), format!("Use one of: {}.", PERMISSIONS.join(", ")), "references/manifest.md#permissions");
            }
            if !permissions.insert(permission.as_str()) {
                self.error(&format!("permissions[{i}]"), format!("`{permission}` is listed twice"), "Remove the duplicate.", "references/manifest.md#permissions");
            }
        }

        self.check_sandbox(m);
        self.check_contributions(m, dir);
    }

    fn check_main(&mut self, m: &Manifest, dir: Option<&Path>) {
        const DOCS: &str = "references/manifest.md#top-level-fields";
        let Some(main) = &m.main else {
            if m.needs_process() {
                self.error("main", "commands, views, titles and settings need a process component", "Add \"main\": {\"command\": [\"python3\", \"main.py\"]}.", DOCS);
            }
            return;
        };
        if main.command.is_empty() || main.command.len() > 32 || main.command.iter().any(|a| a.is_empty() || a.len() > 4096) {
            self.error("main.command", "needs 1–32 non-empty arguments", "E.g. [\"python3\", \"main.py\"].", DOCS);
            return;
        }
        let program = &main.command[0];
        if INTERPRETERS.contains(&program.as_str()) {
            if let Some(script) = main.command.get(1) {
                let looks_like_file = [".py", ".js", ".mjs", ".cjs", ".ts", ".sh"].iter().any(|ext| script.ends_with(ext));
                if looks_like_file {
                    self.relative_file("main.command[1]", script.trim_start_matches("./"), dir, u64::MAX);
                }
            }
        } else if program.contains('/') {
            let file = program.trim_start_matches("./");
            self.relative_file("main.command[0]", file, dir, u64::MAX);
            if let Some(dir) = dir {
                use std::os::unix::fs::PermissionsExt;
                if std::fs::metadata(dir.join(file)).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 == 0)
                    && !m.assets.contains_key(file)
                {
                    self.error("main.command[0]", format!("`{program}` is not executable"), format!("Run `chmod +x {file}` and commit the mode."), DOCS);
                }
            }
        } else {
            self.error(
                "main.command[0]",
                format!("`{program}` is neither an allowed interpreter nor a path inside the plugin"),
                format!("Use one of {} or a path such as \"./bin/tool\".", INTERPRETERS.join(", ")),
                DOCS,
            );
        }
        if main.env.len() > 32 {
            self.error("main.env", "at most 32 variables", "Remove some.", DOCS);
        }
        for key in main.env.keys() {
            if !is_env_name(key) {
                self.error(&format!("main.env.{key}"), "variable names are uppercase letters, digits and `_`", "Rename the variable.", DOCS);
            }
        }
    }

    fn check_sandbox(&mut self, m: &Manifest) {
        const DOCS: &str = "references/manifest.md#sandbox";
        let Some(value) = &m.sandbox else { return };
        if let Value::String(text) = value {
            if text != "none" {
                self.error("sandbox", "a string sandbox can only be \"none\"", "Use an object like {\"network\": false}, or \"none\".", DOCS);
            }
            return;
        }
        let sandbox: Sandbox = match serde_json::from_value(value.clone()) {
            Ok(sandbox) => sandbox,
            Err(error) => {
                self.error("sandbox", format!("wrong shape: {error}"), "Allowed fields: network, read, write, env.", DOCS);
                return;
            }
        };
        for (name, paths) in [("read", &sandbox.read), ("write", &sandbox.write)] {
            self.count(&format!("sandbox.{name}"), paths, 64);
            for (i, path) in paths.iter().enumerate() {
                if !is_sandbox_path(path) {
                    self.error(&format!("sandbox.{name}[{i}]"), format!("`{path}` must start with /, ~/, ${{settings.<key>}} or ${{env:NAME}}"), "Use an absolute or home path, or a placeholder.", DOCS);
                } else if let Some(key) = path.strip_prefix("${settings.").and_then(|rest| rest.split_once('}')).map(|(key, _)| key) {
                    match m.setting(key) {
                        Some(setting) if setting.kind == "path" || setting.kind == "paths" => {}
                        _ => self.error(&format!("sandbox.{name}[{i}]"), format!("`{path}` must name a path or paths setting"), format!("Declare a setting with key `{key}` and type `paths`."), DOCS),
                    }
                }
            }
        }
        self.count("sandbox.env", &sandbox.env, 32);
        for (i, name) in sandbox.env.iter().enumerate() {
            if !is_env_name(name) {
                self.error(&format!("sandbox.env[{i}]"), format!("`{name}` is not a variable name"), "Use uppercase names like SSH_AUTH_SOCK.", DOCS);
            }
        }
    }

    fn check_contributions(&mut self, m: &Manifest, dir: Option<&Path>) {
        const DOCS: &str = "references/manifest.md#contribution-points";
        const RULES: &str = "references/manifest.md#rules-the-validator-adds-to-the-schema";
        let c = &m.contributes;
        let id = m.id.as_str();
        let mut seen = BTreeSet::new();
        let commands: BTreeSet<&str> = c.commands.iter().map(|c| c.id.as_str()).collect();
        let views: BTreeSet<&str> = c.views.iter().map(|v| v.id.as_str()).collect();
        let mut needed: BTreeMap<&'static str, String> = BTreeMap::new();

        let command_ref = |checker: &mut Checker, path: &str, command: &str| {
            if !commands.contains(command) {
                checker.error(path, format!("command `{command}` is not declared in contributes.commands"), format!("Add {{\"id\": \"{command}\", \"title\": \"…\"}} to contributes.commands."), RULES);
            }
        };

        self.count("contributes.commands", &c.commands, 64);
        for (i, command) in c.commands.iter().enumerate() {
            let path = format!("contributes.commands[{i}]");
            self.contribution_id(&format!("{path}.id"), &command.id, id, &mut seen);
            self.len(&format!("{path}.title"), &command.title, 1, 64);
            if let Some(icon) = &command.icon {
                self.icon(&format!("{path}.icon"), icon, dir);
            }
        }

        self.count("contributes.shortcuts", &c.shortcuts, 8);
        for (i, shortcut) in c.shortcuts.iter().enumerate() {
            let path = format!("contributes.shortcuts[{i}]");
            self.contribution_id(&format!("{path}.id"), &shortcut.id, id, &mut seen);
            command_ref(self, &format!("{path}.command"), &shortcut.command);
            if !is_combo(&shortcut.default) {
                self.error(&format!("{path}.default"), format!("`{}` is not a key combination", shortcut.default), "Write it like Hyprland: \"SUPER + SHIFT + G\", or \"F7\".", DOCS);
            } else if shortcut.scope == "global" && !shortcut.default.contains(" + ") && !is_function_key(&shortcut.default) {
                self.error(&format!("{path}.default"), "a global shortcut needs a modifier (SUPER, CTRL, ALT) unless it is F1–F12", "Add a modifier, e.g. \"SUPER + SHIFT + G\".", DOCS);
            }
            match shortcut.scope.as_str() {
                "global" => {
                    needed.insert("shortcuts.global", path);
                }
                "overlay" => {
                    needed.insert("shortcuts.overlay", path);
                }
                other => self.error(&format!("{path}.scope"), format!("scope `{other}` is not global or overlay"), "Use \"global\" (works everywhere) or \"overlay\" (inside SUPER DESKTOP).", DOCS),
            }
        }

        self.count("contributes.toolbar", &c.toolbar, 4);
        for (i, item) in c.toolbar.iter().enumerate() {
            let path = format!("contributes.toolbar[{i}]");
            self.contribution_id(&format!("{path}.id"), &item.id, id, &mut seen);
            self.icon(&format!("{path}.icon"), &item.icon, dir);
            self.len(&format!("{path}.tooltip"), &item.tooltip, 0, 120);
            if let Some(label) = &item.label {
                self.len(&format!("{path}.label"), label, 0, 24);
            }
            match (&item.command, &item.view) {
                (Some(command), None) => command_ref(self, &format!("{path}.command"), command),
                (None, Some(view)) => {
                    if !views.contains(view.as_str()) {
                        self.error(&format!("{path}.view"), format!("view `{view}` is not declared in contributes.views"), "Declare the view or use a command.", RULES);
                    }
                }
                _ => self.error(&path, "needs exactly one of `command` or `view`", "Keep one of the two.", DOCS),
            }
            needed.insert("ui.toolbar", path);
        }

        for (i, hide) in c.toolbar_hide.iter().enumerate() {
            let path = format!("contributes.toolbarHide[{i}]");
            let ok = matches!(hide.as_str(), "brand" | "shortcutHint" | "newNote" | "usage")
                || hide.strip_prefix("launcher:").is_some_and(|key| !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'));
            if !ok {
                self.error(&path, format!("`{hide}` cannot be hidden"), "Allowed: brand, shortcutHint, newNote, usage, launcher:<key>. Arrange, Settings, Hide and the PC selector always stay.", DOCS);
            }
            needed.insert("ui.toolbar", path);
        }
        if c.toolbar_hide.iter().collect::<BTreeSet<_>>().len() != c.toolbar_hide.len() {
            self.error("contributes.toolbarHide", "lists an item twice", "Remove the duplicate.", DOCS);
        }

        self.count("contributes.cardButtons", &c.card_buttons, 2);
        for (i, button) in c.card_buttons.iter().enumerate() {
            let path = format!("contributes.cardButtons[{i}]");
            self.contribution_id(&format!("{path}.id"), &button.id, id, &mut seen);
            self.icon(&format!("{path}.icon"), &button.icon, dir);
            self.len(&format!("{path}.tooltip"), &button.tooltip, 0, 120);
            command_ref(self, &format!("{path}.command"), &button.command);
            if let Some(statuses) = button.when.as_ref().and_then(|w| w.status.as_ref()) {
                for status in statuses {
                    if !["working", "completed", "idle", "waiting", "error", "unknown"].contains(&status.as_str()) {
                        self.error(&format!("{path}.when.status"), format!("unknown status `{status}`"), "Use working, completed, idle, waiting, error or unknown.", DOCS);
                    }
                }
            }
            needed.insert("ui.cardButtons", path);
        }

        if let Some(controls) = &c.card_controls {
            let path = "contributes.cardControls".to_string();
            self.contribution_id(&format!("{path}.id"), &controls.id, id, &mut seen);
            let lists = [("controls", Some(&controls.controls)), ("iconControls", controls.icon_controls.as_ref())];
            for (name, list) in lists {
                let Some(list) = list else { continue };
                if list.is_empty() || list.len() > 6 {
                    self.error(&format!("{path}.{name}"), "needs 1–6 controls", "Keep between one and six.", DOCS);
                }
                for (j, control) in list.iter().enumerate() {
                    let at = format!("{path}.{name}[{j}]");
                    match control {
                        Value::String(builtin) if BUILTIN_CONTROLS.contains(&builtin.as_str()) => {}
                        Value::String(other) => self.error(&at, format!("unknown built-in control `{other}`"), format!("Use one of {}.", BUILTIN_CONTROLS.join(", ")), DOCS),
                        Value::Object(_) => match serde_json::from_value::<CardButtonLike>(control.clone()) {
                            Ok(custom) => {
                                self.contribution_id(&format!("{at}.id"), &custom.id, id, &mut seen);
                                self.icon(&format!("{at}.icon"), &custom.icon, dir);
                                self.len(&format!("{at}.tooltip"), &custom.tooltip, 0, 120);
                                command_ref(self, &format!("{at}.command"), &custom.command);
                            }
                            Err(error) => self.error(&at, format!("wrong shape: {error}"), "Use {\"id\", \"icon\", \"tooltip\", \"command\"}.", DOCS),
                        },
                        _ => self.error(&at, "must be a built-in name or an object", "See the cardControls row.", DOCS),
                    }
                }
            }
            needed.insert("ui.cardControls", path);
        }

        if let Some(titles) = &c.titles {
            if titles != &Value::Bool(true) {
                self.error("contributes.titles", "must be true", "Write \"titles\": true.", DOCS);
            }
            if !m.activation.iter().any(|a| a == "onStartup") {
                self.error("activation", "a plugin that sets titles must start with the desktop", "Add \"onStartup\" to activation.", RULES);
            }
            needed.insert("ui.titles", "contributes.titles".into());
        }

        if let Some(renderer) = &c.renderer {
            let path = "contributes.renderer".to_string();
            self.contribution_id(&format!("{path}.id"), &renderer.id, id, &mut seen);
            self.relative_file(&format!("{path}.wasm"), &renderer.wasm, dir.filter(|_| !m.assets.contains_key(&renderer.wasm)), MAX_WASM_BYTES);
            if let Some(dir) = dir {
                if !m.assets.contains_key(&renderer.wasm) {
                    if let Ok(bytes) = std::fs::read(dir.join(&renderer.wasm)) {
                        if !bytes.starts_with(b"\0asm") {
                            self.error(&format!("{path}.wasm"), "is not a WebAssembly module", "Build it for wasm32-unknown-unknown.", "references/renderer-abi.md");
                        }
                    }
                }
            }
            needed.insert("layout.renderer", path);
        }

        self.count("contributes.harnesses", &c.harnesses, 4);
        let mut harness_ids = BTreeSet::new();
        for (i, harness) in c.harnesses.iter().enumerate() {
            let path = format!("contributes.harnesses[{i}]");
            const HDOCS: &str = "references/harnesses.md";
            if !is_harness_id(&harness.id) || !harness_ids.insert(harness.id.as_str()) {
                self.error(&format!("{path}.id"), format!("`{}` must be unique, 1–32 lowercase letters, digits or `-`, starting with a letter", harness.id), "E.g. \"acme\".", HDOCS);
            }
            self.len(&format!("{path}.name"), &harness.name, 1, 32);
            self.len(&format!("{path}.glyph"), &harness.glyph, 1, 16);
            if let Some(logo) = &harness.logo {
                self.relative_file(&format!("{path}.logo"), logo, dir, MAX_ICON_BYTES);
            }
            if harness.binaries.is_empty() || harness.binaries.len() > 8 {
                self.error(&format!("{path}.binaries"), "needs 1–8 entries", "List the executable names to look for.", HDOCS);
            }
            if !["generic-report", "none"].contains(&harness.adapter.kind.as_str()) {
                self.error(&format!("{path}.adapter.kind"), format!("unknown adapter `{}`", harness.adapter.kind), "Use \"generic-report\" or \"none\".", HDOCS);
            }
            for pattern in harness.status.iter().flat_map(|s| &s.work) {
                if pattern.is_empty() || pattern.chars().count() > 200 {
                    self.error(&format!("{path}.status.work"), "patterns are 1–200 characters", "Shorten the pattern.", HDOCS);
                }
            }
            needed.insert("harness.provide", path);
        }

        self.count("contributes.settings", &c.settings, 32);
        let mut keys = BTreeSet::new();
        for (i, setting) in c.settings.iter().enumerate() {
            self.check_setting(&format!("contributes.settings[{i}]"), setting, &mut keys);
        }

        self.count("contributes.views", &c.views, 8);
        for (i, view) in c.views.iter().enumerate() {
            let path = format!("contributes.views[{i}]");
            self.contribution_id(&format!("{path}.id"), &view.id, id, &mut seen);
            self.len(&format!("{path}.title"), &view.title, 1, 64);
            if view.kind != "panel" && view.kind != "popover" {
                self.error(&format!("{path}.kind"), format!("kind `{}` is not panel or popover", view.kind), "Use \"panel\" or \"popover\".", "references/ui.md");
            }
            if view.width.is_some_and(|w| !(240..=1600).contains(&w)) || view.height.is_some_and(|h| !(160..=1200).contains(&h)) {
                self.error(&path, "width must be 240–1600 and height 160–1200", "Pick a size in range.", "references/ui.md");
            }
            needed.insert("ui.popup", path);
        }

        let mut activations = BTreeSet::new();
        for (i, activation) in m.activation.iter().enumerate() {
            let path = format!("activation[{i}]");
            if !activations.insert(activation.as_str()) {
                self.error(&path, "listed twice", "Remove the duplicate.", RULES);
            }
            match activation.strip_prefix("onCommand:") {
                Some(command) => command_ref(self, &path, command),
                None if activation == "onStartup" || activation == "onOverlayShown" => {}
                None => self.error(&path, format!("unknown activation `{activation}`"), "Use onStartup, onOverlayShown or onCommand:<command id>.", RULES),
            }
        }

        for (permission, path) in &needed {
            if !m.has_permission(permission) {
                self.error(path, format!("needs the `{permission}` permission"), format!("Add \"{permission}\" to permissions."), "references/manifest.md#permissions");
            }
        }
        for permission in &m.permissions {
            let implied_by_contribution = [
                "ui.toolbar",
                "ui.cardButtons",
                "ui.cardControls",
                "ui.titles",
                "ui.popup",
                "shortcuts.global",
                "shortcuts.overlay",
                "layout.renderer",
                "harness.provide",
            ];
            if implied_by_contribution.contains(&permission.as_str()) && !needed.contains_key(permission.as_str()) {
                self.warn("permissions", format!("`{permission}` is listed but nothing in contributes uses it"), "Remove it: users see every permission at install.", "references/manifest.md#permissions");
            }
        }
        for name in m.used_contributions() {
            if !SUPPORTED_CONTRIBUTIONS.contains(&name) {
                self.warn(
                    &format!("contributes.{name}"),
                    format!("this build ({}) does not run `{name}` yet; it is ignored", crate::updates::running()),
                    "Check `super-desktop plugin describe --json`; raise engines.superDesktop once a build supports it.",
                    "references/testing.md#commands",
                );
            }
        }
    }

    fn check_setting(&mut self, path: &str, setting: &Setting, keys: &mut BTreeSet<String>) {
        const DOCS: &str = "references/manifest.md#contribution-points";
        if !is_setting_key(&setting.key) {
            self.error(&format!("{path}.key"), format!("`{}` is not a valid key", setting.key), "Start with a letter; letters, digits and `_`; at most 48.", DOCS);
        }
        if !keys.insert(setting.key.clone()) {
            self.error(&format!("{path}.key"), format!("duplicate setting `{}`", setting.key), "Keys must be unique.", DOCS);
        }
        self.len(&format!("{path}.title"), &setting.title, 1, 64);
        if let Some(description) = &setting.description {
            self.len(&format!("{path}.description"), description, 0, 300);
        }
        let kind = setting.kind.as_str();
        if !["string", "secret", "bool", "number", "enum", "path", "paths", "color"].contains(&kind) {
            self.error(&format!("{path}.type"), format!("unknown type `{kind}`"), "Use string, secret, bool, number, enum, path, paths or color.", DOCS);
            return;
        }
        if kind == "enum" {
            match &setting.values {
                Some(values) if !values.is_empty() && values.len() <= 32 => {}
                _ => self.error(&format!("{path}.values"), "an enum needs 1–32 values", "Add \"values\": [\"a\", \"b\"].", DOCS),
            }
        }
        if (kind == "path" || kind == "paths") && !matches!(setting.path_kind.as_deref(), Some("file" | "directory")) {
            self.error(&format!("{path}.kind"), "a path setting needs kind file or directory", "Add \"kind\": \"directory\".", DOCS);
        }
        if let (Some(min), Some(max)) = (setting.min, setting.max) {
            if min > max {
                self.error(path, "min is greater than max", "Swap them.", DOCS);
            }
        }
        if let Some(default) = &setting.default {
            if let Err(why) = check_value(setting, default) {
                self.error(&format!("{path}.default"), format!("default does not fit: {why}"), "Make the default match the type and range.", DOCS);
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CardButtonLike {
    id: String,
    icon: String,
    tooltip: String,
    command: String,
}

/// Whether `value` fits `setting` (type, range, enum values). Used for
/// defaults here and for values set from the settings page or `settings.set`.
pub fn check_value(setting: &Setting, value: &Value) -> Result<(), String> {
    match (setting.kind.as_str(), value) {
        ("string" | "secret", Value::String(text)) if text.len() <= 4096 => Ok(()),
        ("color", Value::String(text)) if is_color(text) => Ok(()),
        ("bool", Value::Bool(_)) => Ok(()),
        ("number", Value::Number(n)) => {
            let n = n.as_f64().unwrap_or(f64::NAN);
            if setting.min.is_some_and(|min| n < min) || setting.max.is_some_and(|max| n > max) {
                Err(format!("{n} is outside {:?}–{:?}", setting.min, setting.max))
            } else {
                Ok(())
            }
        }
        ("enum", Value::String(text)) if setting.values.as_ref().is_some_and(|v| v.contains(text)) => Ok(()),
        ("path", Value::String(text)) if text.is_empty() || text.starts_with('/') || text.starts_with("~/") => Ok(()),
        ("paths", Value::Array(items))
            if items.len() <= 256 && items.iter().all(|i| i.as_str().is_some_and(|t| t.starts_with('/') || t.starts_with("~/"))) =>
        {
            Ok(())
        }
        (kind, value) => Err(format!("{value} is not a valid {kind}")),
    }
}

fn is_plugin_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (3..=40).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes[bytes.len() - 1] != b'-'
}

fn is_contribution_id(id: &str) -> bool {
    let Some((plugin, name)) = id.split_once('.') else { return false };
    is_plugin_id(plugin)
        && (1..=48).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn is_harness_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len()) && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn is_setting_key(key: &str) -> bool {
    let b = key.as_bytes();
    (1..=48).contains(&b.len()) && b[0].is_ascii_alphabetic() && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

fn is_env_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

fn is_semver(version: &str) -> bool {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers.iter().all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && (n.len() == 1 || !n.starts_with('0')))
        && pre.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
}

fn is_github_repository(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else { return false };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
}

pub fn is_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && !path.starts_with('/')
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-/".contains(&b))
        && !path.split('/').any(|part| part == "..")
}

fn is_image_path(icon: &str) -> bool {
    icon.ends_with(".svg") || icon.ends_with(".png")
}

fn is_sandbox_path(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with("~/") {
        return true;
    }
    if let Some(rest) = path.strip_prefix("${settings.") {
        return rest.split_once('}').is_some_and(|(key, _)| !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    }
    if let Some(rest) = path.strip_prefix("${env:") {
        return rest.split_once('}').is_some_and(|(name, _)| is_env_name(name));
    }
    false
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=12).contains(&n)) && !key[1..].starts_with('0')
}

/// Hyprland spelling: `MOD + MOD + KEY` (SUPER, CTRL, ALT, SHIFT), or F1–F12.
pub fn is_combo(combo: &str) -> bool {
    if is_function_key(combo) {
        return true;
    }
    let parts: Vec<&str> = combo.split(" + ").collect();
    let (key, modifiers) = parts.split_last().expect("split yields at least one part");
    !key.is_empty()
        && key.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && modifiers.iter().all(|m| matches!(*m, "SUPER" | "CTRL" | "ALT" | "SHIFT"))
}

fn is_color(text: &str) -> bool {
    let hex = text.strip_prefix('#').unwrap_or("");
    matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The skills folder that ships with this build's source, if present.
pub fn skills_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let clone = crate::updates::clone_of(&exe.canonicalize().ok()?)
        .or_else(|| Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))))?;
    let dir = clone.join("skills/super-desktop-plugin");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/manifests")
    }

    fn examples() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills/super-desktop-plugin/examples")
    }

    /// The same corpus is checked against the JSON schema by
    /// `tests/plugin_spec_check.py`; both must agree.
    #[test]
    fn plugin_spec_valid_corpus_is_accepted() {
        let mut count = 0;
        for entry in std::fs::read_dir(corpus().join("valid")).unwrap() {
            let path = entry.unwrap().path();
            let loaded = validate_text(&std::fs::read_to_string(&path).unwrap(), None);
            assert!(loaded.report.ok, "{}: {:#?}", path.display(), loaded.report.errors);
            count += 1;
        }
        assert!(count >= 3);
    }

    #[test]
    fn plugin_spec_invalid_corpus_is_rejected() {
        let mut count = 0;
        for entry in std::fs::read_dir(corpus().join("invalid")).unwrap() {
            let path = entry.unwrap().path();
            let loaded = validate_text(&std::fs::read_to_string(&path).unwrap(), None);
            assert!(!loaded.report.ok, "{} was accepted", path.display());
            for problem in &loaded.report.errors {
                assert!(!problem.hint.is_empty() && problem.docs.starts_with("references/"), "{problem:?}");
            }
            count += 1;
        }
        assert!(count >= 11);
    }

    #[test]
    fn plugin_spec_examples_validate_with_their_files() {
        for name in ["git-flush", "window-controls"] {
            let loaded = load_dir(&examples().join(name));
            assert!(loaded.report.ok, "{name}: {:#?}", loaded.report.errors);
        }
        // The renderer example ships its source; renderer.wasm is built by build.sh.
        let loaded = load_dir(&examples().join("center-magnify"));
        let errors: Vec<_> = loaded.report.errors.iter().map(|e| e.path.as_str()).collect();
        assert!(errors.iter().all(|p| *p == "contributes.renderer.wasm"), "{:#?}", loaded.report.errors);
    }

    #[test]
    fn plugin_spec_rules_beyond_the_schema() {
        let base: Value = serde_json::from_str(&std::fs::read_to_string(examples().join("git-flush").join(FILE_NAME)).unwrap()).unwrap();
        let with = |edit: &dyn Fn(&mut Value)| {
            let mut value = base.clone();
            edit(&mut value);
            validate_text(&value.to_string(), None).report
        };
        let messages = |report: Report| report.errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join(" | ");
        let r = with(&|m| m["contributes"]["toolbar"][0]["command"] = "git-flush.missing".into());
        assert!(messages(r).contains("not declared"));
        let r = with(&|m| m["contributes"]["commands"][0]["id"] = "other.open".into());
        assert!(messages(r).contains("must be `git-flush.<name>`"));
        let r = with(&|m| m["permissions"] = serde_json::json!(["ui.popup", "ui.notify", "shortcuts.global", "llm"]));
        assert!(messages(r).contains("needs the `ui.toolbar` permission"));
        let r = with(&|m| m["contributes"]["shortcuts"][0]["default"] = "G".into());
        assert!(messages(r).contains("needs a modifier"));
        let r = with(&|m| m["sandbox"]["write"] = serde_json::json!(["${settings.nope}"]));
        assert!(messages(r).contains("path or paths setting"));
        let r = with(&|m| m["main"]["command"] = serde_json::json!(["ruby", "main.rb"]));
        assert!(messages(r).contains("neither an allowed interpreter"));
        let r = with(&|m| m["contributes"]["settings"][3]["default"] = 5.into());
        assert!(messages(r).contains("default does not fit"));
        let r = with(&|m| {
            m["contributes"]["titles"] = true.into();
            m["permissions"].as_array_mut().unwrap().push("ui.titles".into());
            m["activation"] = serde_json::json!([]);
        });
        assert!(messages(r).contains("must start with the desktop"));
        let r = with(&|m| m["permissions"].as_array_mut().unwrap().push("layout.renderer".into()));
        assert!(r.ok && r.warnings.iter().any(|w| w.message.contains("nothing in contributes uses it")));
    }

    #[test]
    fn plugin_spec_files_must_stay_inside_the_plugin() {
        let dir = std::env::temp_dir().join(format!("sd-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("icons")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", dir.join("icons/escape.svg")).unwrap();
        std::fs::write(dir.join("main.py"), "").unwrap();
        let manifest = serde_json::json!({
            "manifestVersion": 1, "id": "files", "name": "Files", "version": "0.1.0", "description": "d",
            "engines": {"superDesktop": ">=1.0.0", "pluginApi": "1"},
            "main": {"command": ["python3", "main.py"]},
            "permissions": ["ui.toolbar"],
            "contributes": {
                "commands": [{"id": "files.go", "title": "Go"}],
                "toolbar": [
                    {"id": "files.a", "icon": "icons/escape.svg", "tooltip": "t", "command": "files.go"},
                    {"id": "files.b", "icon": "icons/missing.svg", "tooltip": "t", "command": "files.go"}
                ]
            }
        });
        std::fs::write(dir.join(FILE_NAME), manifest.to_string()).unwrap();
        let report = load_dir(&dir).report;
        let text = report.errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join(" | ");
        assert!(text.contains("resolves outside"), "{text}");
        assert!(text.contains("does not exist"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
