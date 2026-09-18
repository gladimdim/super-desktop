//! ⌨ Configurable toggle shortcut: record a key combination, remember it, and
//! make Hyprland use it for `super-desktop toggle`.
//!
//! The show/hide key is not a GTK accelerator. It is a Hyprland bind — the
//! `o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle")` line
//! `install.sh` puts in `~/.config/hypr/bindings.lua` — so a shortcut picked in
//! the overlay has to end up in that file, not only in `state.json`. Three
//! parts, in dependency order:
//!
//! 1. **Interpret** ([`interpret`]) — a GTK key event becomes a Hyprland combo
//!    string: a real modifier (SUPER/CTRL/ALT) plus a key, or a function key
//!    F1–F12 on its own. Pure: the spelling rules are unit-tested without a
//!    display.
//! 2. **Guard** ([`begin_capture`] / [`CaptureGuard`]) — Hyprland handles its
//!    own binds *before* it forwards a key to the focused surface, so a plain
//!    listener can never see a combination Hyprland already owns — starting
//!    with the very shortcut being replaced. While the listener is armed the
//!    overlay parks Hyprland in a throw-away submap: no global bind is
//!    processed inside a submap, so every key reaches the overlay. Best effort
//!    — without `hyprctl` (or on a Hyprland whose config is not Lua) capture
//!    still works for every combination Hyprland does not bind.
//! 3. **Apply** ([`apply_combo_at`]) — rewrite only the managed block of
//!    `bindings.lua`, then `hyprctl reload` and `hyprctl configerrors`.
//!
//! Everything that touches the machine takes its file/binary as a parameter
//! (`*_at`), so the tests drive the real code against a temp file and a stub
//! `hyprctl` instead of the user's live session — the same shape as
//! `main::socket_path_in` for the IPC socket.

use gtk4::gdk::{self, ModifierType};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The binding SUPER DESKTOP ships with, and the fallback whenever `state.json`
/// holds nothing usable (fresh install, hand-edited file, empty string).
pub const DEFAULT_COMBO: &str = "SUPER + SHIFT + Q";

/// Fence around the block of `bindings.lua` this app owns.
///
/// `install.sh` writes the very same pair — change one without the other and
/// the app can no longer find (or replace) the block an earlier run wrote.
/// `tests::test_install_sh_writes_the_managed_block_we_read` keeps them honest.
pub const MANAGED_BEGIN: &str =
    "-- >>> super-desktop shortcut (managed by the overlay settings) >>>";
pub const MANAGED_END: &str = "-- <<< super-desktop shortcut <<<";

/// Description every bind of ours carries. Also how our own (just applied) bind
/// is told apart from the user's binds when looking for conflicts.
const BIND_DESCRIPTION: &str = "Super Desktop";
/// The command our bind runs; the only thing that identifies a line as ours.
const TOGGLE_COMMAND: &str = "super-desktop toggle";
/// Hyprland `bindr`: fire once on key *release*. A press-bind repeats for as
/// long as the key is held, which is what made the overlay strobe unless we
/// ignored toggles — and ignoring them made a second tap during the slide-in
/// do nothing. Release does not repeat.
const BIND_RELEASE: &str = "{ release = true }";

/// Combinations older `install.sh` runs claimed for the toggle.
///
/// A migration has to clear these out, unbinds included: those `hl.unbind(...)`
/// lines can outlive the bind they were written for (Omarchy's config-sync, or
/// a hand edit, removes the bind but not the unbind), so matching the unbinds by
/// their bind alone is not enough — the file on disk has exactly that shape.
/// The managed block does the same job for every future combination.
const LEGACY_COMBOS: [&str; 4] = [
    "SUPER + SHIFT + Q",
    "SUPER + SHIFT + Cyrillic_shorti",
    "SUPER + SHIFT + Cyrillic_SHORTI",
    "SUPER + SHIFT + code:24",
];

/// Runtime-only submap used while recording. Registered through `hyprctl eval`
/// and gone again at the next config reload, so it never has to exist in the
/// user's config for capture to work.
const RECORD_SUBMAP: &str = "super_desktop_recording";

/// Hyprland refuses to enter a submap that defines no bind ("submap doesn't
/// exist (wasn't registered!)"), so the recording submap carries one bind for a
/// key nobody presses. Everything else stays unbound → falls through to us.
const SUBMAP_DEFINE: &str = r#"hl.define_submap("super_desktop_recording", function() hl.bind("XF86Launch9", hl.dsp.no_op()) end)"#;
const SUBMAP_ENTER: &str = r#"hl.dsp.submap("super_desktop_recording")"#;
const SUBMAP_RESET: &str = r#"hl.dsp.submap("reset")"#;

/// What a single key event means to the recorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capture {
    /// Keep waiting: a bare modifier, or a key GDK has no name for.
    Waiting,
    /// A real key, but no modifier was held — and it is not one of the
    /// function keys that may stand alone. Refused on purpose: a bare letter
    /// would swallow that key everywhere, with nothing on screen saying why.
    NeedsModifier,
    /// Ready to apply — already spelled the way Hyprland writes bindings.
    Combo(String),
}

/// Function keys that may be bound with no modifier at all.
///
/// They type nothing in any application, so a bare `F5` costs the user nothing
/// — unlike a bare letter, which would be swallowed everywhere it is typed.
const STANDALONE_KEYS: [gdk::Key; 12] = [
    gdk::Key::F1,
    gdk::Key::F2,
    gdk::Key::F3,
    gdk::Key::F4,
    gdk::Key::F5,
    gdk::Key::F6,
    gdk::Key::F7,
    gdk::Key::F8,
    gdk::Key::F9,
    gdk::Key::F10,
    gdk::Key::F11,
    gdk::Key::F12,
];

/// Turn one key press into a Hyprland combo.
pub fn interpret(key: gdk::Key, mods: ModifierType) -> Capture {
    if is_modifier(key) {
        return Capture::Waiting;
    }
    let Some(name) = key_name(key) else {
        return Capture::Waiting;
    };
    let mut parts = match modifier_labels(mods) {
        Some(labels) => labels,
        // No SUPER/CTRL/ALT. SHIFT on its own is not a shortcut either —
        // "SHIFT + Q" would swallow capital Q in every application, and
        // "SHIFT + F5" reloads things in enough of them — so the only
        // modifier-less combination allowed is a function key.
        None if STANDALONE_KEYS.contains(&key) && !mods.contains(ModifierType::SHIFT_MASK) => {
            Vec::new()
        }
        None => return Capture::NeedsModifier,
    };
    parts.push(name);
    Capture::Combo(parts.join(" + "))
}

/// The combo to display and to fall back on: whatever is stored, or the shipped
/// default when there is nothing usable.
pub fn current_combo(stored: Option<&str>) -> String {
    match stored.map(str::trim) {
        Some(combo) if !combo.is_empty() => combo.to_string(),
        _ => DEFAULT_COMBO.to_string(),
    }
}

/// `~/.config/hypr/bindings.lua` — the file `install.sh` appends to and the
/// README tells contributors to edit and reload.
pub fn bindings_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/hypr/bindings.lua")
}

/// The block install.sh's counterpart, for `combo` alone (no physical key
/// known): the shipped default.
pub fn managed_block(combo: &str, keycode: Option<u32>) -> String {
    let mut out = String::new();
    out.push_str(MANAGED_BEGIN);
    out.push('\n');
    out.push_str("-- Set in the overlay: ⚙ Settings → Keyboard shortcut.\n");
    out.push_str("-- Rewritten there on every change; edits inside this block are lost.\n");
    // Unbind first: Hyprland keeps the first bind it registers for a
    // combination and drops later duplicates, so an Omarchy default bound
    // before this file would win over the bind below.
    for variant in combo_variants(combo, keycode) {
        out.push_str(&format!("hl.unbind(\"{variant}\")\n"));
        out.push_str(&format!("{}\n", toggle_bind_lua(&variant)));
    }
    out.push_str(MANAGED_END);
    out.push('\n');
    out
}

/// The combo spelled as a keysym, plus — when the physical key is known — its
/// `code:` form, so the shortcut keeps working after a layout switch. That is
/// the trick `install.sh` uses for the shipped SUPER + SHIFT + Q (`code:24` is
/// Q): the UK and Cyrillic layouts put a different keysym on that key.
fn combo_variants(combo: &str, keycode: Option<u32>) -> Vec<String> {
    let mut out = vec![combo.to_string()];
    if let (Some(keycode), Some((mods, _key))) = (keycode, combo.rsplit_once(" + ")) {
        // GDK reports X11/xkb keycodes (evdev + 8) on Wayland, which is exactly
        // the numbering Hyprland's `code:` uses — no conversion needed.
        out.push(format!("{mods} + code:{keycode}"));
    }
    out
}

/// Put `combo` in the managed block of `existing`, leaving every other line
/// alone — including other super-desktop lines such as the daemon autostart.
///
/// Also migrates what older installs wrote: a loose
/// `o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle")` line
/// plus the `hl.unbind(...)` lines that existed only to clear the way for it.
/// Leaving those behind would keep an unused key dead — the Omarchy default it
/// shadowed would never come back.
pub fn rewrite_bindings(existing: &str, combo: &str, keycode: Option<u32>) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut replaced: Vec<String> = Vec::new();
    let mut inside_managed = false;

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == MANAGED_BEGIN {
            inside_managed = true;
            continue;
        }
        if trimmed == MANAGED_END {
            inside_managed = false;
            continue;
        }
        if inside_managed {
            continue;
        }
        if is_toggle_bind(trimmed) {
            if let Some(old) = first_string_literal(trimmed) {
                replaced.push(normalize_combo(&old));
            }
            continue;
        }
        kept.push(line.to_string());
    }

    kept.retain(|line| match unbind_target(line.trim()) {
        Some(target) => {
            let target = normalize_combo(&target);
            // Keep unbinds that belong to someone else's binding, drop the ones
            // that only ever existed for a bind we just removed — including a
            // duplicate of the combo being applied, which our block unbinds.
            let ours = replaced.contains(&target)
                || LEGACY_COMBOS.iter().any(|c| normalize_combo(c) == target);
            !ours && target != normalize_combo(combo)
        }
        None => true,
    });

    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }

    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&managed_block(combo, keycode));
    out
}

/// The first bind in `hyprctl binds` already sitting on `combo` that is not
/// ours, described the way the user would recognise it ("Close window").
///
/// Applying a shortcut unbinds whatever held the combination, so this is what
/// lets the panel say *what* it just replaced instead of silently breaking a
/// binding the user forgot about.
pub fn conflicting_bind(dump: &str, combo: &str) -> Option<String> {
    let want_mask = modifier_mask(combo);
    let want_key = combo.rsplit(" + ").next()?.trim().to_string();

    let mut mask: Option<u64> = None;
    let mut key: Option<String> = None;
    let mut submap: Option<String> = None;
    let mut description: Option<String> = None;

    // Binds are separated by blank lines; the sentinel flushes the last block.
    for line in dump.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.is_empty() {
            let same_mods = mask == Some(want_mask);
            let same_key = key
                .as_deref()
                .is_some_and(|k| k.eq_ignore_ascii_case(&want_key));
            let global = submap.as_deref().unwrap_or("").is_empty();
            let ours = description.as_deref() == Some(BIND_DESCRIPTION);
            if same_mods && same_key && global && !ours {
                return Some(
                    description
                        .clone()
                        .unwrap_or_else(|| "an unnamed Hyprland binding".to_string()),
                );
            }
            mask = None;
            key = None;
            submap = None;
            description = None;
            continue;
        }
        if let Some(v) = line.strip_prefix("modmask:") {
            mask = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("key:") {
            key = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("submap:") {
            submap = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        }
    }
    None
}

/// Outcome of a successful apply: the binding is on disk. `conflict`/`warning`
/// are the two things the user needs to hear about afterwards.
pub struct Applied {
    pub combo: String,
    /// Another bind the combo used to belong to, which applying it removed.
    pub conflict: Option<String>,
    /// Written, but Hyprland did not pick it up (no session, config error).
    pub warning: Option<String>,
}

/// Write `combo` into `bindings_path`'s managed block and reload Hyprland.
///
/// `Err` means nothing was written; `Ok(Applied { warning: Some(_), .. })` means
/// the binding is saved but not live yet.
pub fn apply_combo_at(
    bindings_path: &Path,
    hyprctl_bin: &str,
    combo: &str,
    keycode: Option<u32>,
) -> Result<Applied, String> {
    // Read the conflict before writing: afterwards our own bind is in the list.
    let conflict = hyprctl_run(hyprctl_bin, &["binds"])
        .ok()
        .and_then(|dump| conflicting_bind(&dump, combo));

    let existing = fs::read_to_string(bindings_path).unwrap_or_default();
    let updated = rewrite_bindings(&existing, combo, keycode);
    if updated != existing {
        write_atomic(bindings_path, &updated)?;
    }

    let warning = match hyprctl_run(hyprctl_bin, &["reload"]) {
        Err(e) => Some(format!(
            "the config could not be reloaded ({e}) — it applies on the next reload"
        )),
        Ok(_) => {
            let errors = hyprctl_run(hyprctl_bin, &["configerrors"]).unwrap_or_default();
            if errors.is_empty() || errors.eq_ignore_ascii_case("ok") {
                None
            } else {
                Some(format!("Hyprland reported a config error: {errors}"))
            }
        }
    };

    Ok(Applied {
        combo: combo.to_string(),
        conflict,
        warning,
    })
}

/// [`apply_combo_at`] against this machine's `bindings.lua` and `hyprctl`.
pub fn apply_combo(combo: &str, keycode: Option<u32>) -> Result<Applied, String> {
    apply_combo_at(&bindings_path(), "hyprctl", combo, keycode)
}

/// One `o.bind` line: description, toggle command, fire on key-release.
fn toggle_bind_lua(variant: &str) -> String {
    format!(
        "o.bind(\"{variant}\", \"{BIND_DESCRIPTION}\", \"{TOGGLE_COMMAND}\", {BIND_RELEASE})"
    )
}

/// True when every overlay-toggle bind in `lua` already fires on release.
fn toggle_binds_use_release(lua: &str) -> bool {
    let mut saw = false;
    for line in lua.lines() {
        let trimmed = line.trim();
        if !is_toggle_bind(trimmed) {
            continue;
        }
        saw = true;
        if !trimmed.contains("release") {
            return false;
        }
    }
    saw
}

/// `code:N` from a managed toggle bind, if the file still has one.
fn parse_bind_keycode(lua: &str) -> Option<u32> {
    for line in lua.lines() {
        if !is_toggle_bind(line.trim()) {
            continue;
        }
        let Some(rest) = line.split("code:").nth(1) else {
            continue;
        };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u32>() {
            return Some(n);
        }
    }
    None
}

/// Rewrite the live `bindings.lua` so the overlay toggle is a release-bind.
///
/// Older installs bound on press (and the daemon then had to ignore repeats).
/// Idempotent: if the file already uses `{ release = true }`, this is a no-op
/// and does not reload Hyprland.
pub fn ensure_release_toggle() {
    let path = bindings_path();
    let Ok(existing) = fs::read_to_string(&path) else {
        return;
    };
    if !existing.contains(TOGGLE_COMMAND) || toggle_binds_use_release(&existing) {
        return;
    }
    let combo = current_combo(
        crate::state::load_state()
            .toggle_shortcut
            .as_deref(),
    );
    let keycode = parse_bind_keycode(&existing);
    let _ = apply_combo(&combo, keycode);
}

/// Arms the keymap guard for one recording and restores it however the
/// recording ends. Holding it in the panel is what makes "click Record, press
/// the combination, get it recorded" work for combinations Hyprland owns.
#[must_use = "dropping the guard restores the keymap immediately, so it must be held while recording"]
pub struct CaptureGuard {
    armed: bool,
    hyprctl: String,
}

impl CaptureGuard {
    /// Whether Hyprland is actually parked in the recording submap. `false`
    /// means capture still works, just not for combinations Hyprland binds.
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Release the guard now instead of at the end of the scope.
    pub fn end(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        // Harmless when no submap is active, which also makes this the way back
        // for a recorder that died mid-capture: it leaves the keymap normal.
        let _ = hyprctl_run(&self.hyprctl, &["dispatch", SUBMAP_RESET]);
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// Park Hyprland in the recording submap. Best effort: if anything here fails
/// the guard comes back unarmed and the recorder still catches every
/// combination Hyprland does not bind itself.
pub fn begin_capture_at(hyprctl_bin: &str) -> CaptureGuard {
    // A submap left behind by a recorder that died would already be hiding
    // every shortcut from the user, so clear it before arming a new one.
    let _ = hyprctl_run(hyprctl_bin, &["dispatch", SUBMAP_RESET]);
    let _ = hyprctl_run(hyprctl_bin, &["eval", SUBMAP_DEFINE]);
    let _ = hyprctl_run(hyprctl_bin, &["dispatch", SUBMAP_ENTER]);
    let armed = hyprctl_run(hyprctl_bin, &["submap"]).ok().as_deref() == Some(RECORD_SUBMAP);
    CaptureGuard {
        armed,
        hyprctl: hyprctl_bin.to_string(),
    }
}

/// [`begin_capture_at`] against this machine's `hyprctl`.
pub fn begin_capture() -> CaptureGuard {
    begin_capture_at("hyprctl")
}

// ─── spelling helpers ─────────────────────────────────────────────────────────

/// Whether a key is a modifier on its own. Pressing one only ever means the
/// user is still holding the combination, so it must never be recorded.
fn is_modifier(key: gdk::Key) -> bool {
    let Some(name) = key.name() else {
        return false;
    };
    let name = name.as_str();
    name.ends_with("_Lock")
        || matches!(
            name,
            "ISO_Level3_Shift" | "ISO_Level5_Shift" | "Mode_switch"
        )
        || [
            "Shift_", "Control_", "Alt_", "Super_", "Meta_", "Hyper_", "Caps_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// GDK keysym name → the spelling Hyprland matches.
fn key_name(key: gdk::Key) -> Option<String> {
    let name = key.name()?.to_string();
    if name.is_empty() {
        return None;
    }
    // Every Omarchy binding writes single letters upper-case, and Hyprland
    // folds the case when it matches, so normalise instead of storing whatever
    // shift happened to be held.
    let mut chars = name.chars();
    if name.chars().count() == 1 && name.chars().all(|c| c.is_ascii_alphabetic()) {
        return chars.next().map(|c| c.to_ascii_uppercase().to_string());
    }
    Some(name)
}

/// Modifier labels in the order Hyprland configs are written
/// ("SUPER + SHIFT + Q", "CTRL + ALT + T"). `None` means the combination has no
/// modifier that is safe to bind — SHIFT on its own is not enough.
fn modifier_labels(mods: ModifierType) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut has_real_modifier = false;
    // SUPER and META are the same key on Wayland; GDK reports either.
    if mods.contains(ModifierType::SUPER_MASK) || mods.contains(ModifierType::META_MASK) {
        out.push("SUPER".to_string());
        has_real_modifier = true;
    }
    if mods.contains(ModifierType::CONTROL_MASK) {
        out.push("CTRL".to_string());
        has_real_modifier = true;
    }
    if mods.contains(ModifierType::ALT_MASK) {
        out.push("ALT".to_string());
        has_real_modifier = true;
    }
    if mods.contains(ModifierType::SHIFT_MASK) {
        out.push("SHIFT".to_string());
    }
    if has_real_modifier {
        Some(out)
    } else {
        None
    }
}

/// Hyprland's modifier bits, as `hyprctl binds` prints them in `modmask`.
/// Zero for a combination that has no modifier at all (a bare function key).
fn modifier_mask(combo: &str) -> u64 {
    let mut mask = 0u64;
    for part in combo.split('+').map(|p| p.trim().to_ascii_uppercase()) {
        match part.as_str() {
            "SHIFT" => mask |= 1,
            "CAPS" | "CAPS_LOCK" | "CAPSLOCK" => mask |= 2,
            "CTRL" | "CONTROL" => mask |= 4,
            "ALT" | "MOD1" => mask |= 8,
            "SUPER" | "MOD4" | "META" | "WIN" => mask |= 64,
            _ => {}
        }
    }
    mask
}

/// Case- and space-insensitive spelling, so `"SUPER + SHIFT + q"` (install.sh's
/// unbind) and `"SUPER + SHIFT + Q"` (the bind it clears) compare equal.
fn normalize_combo(combo: &str) -> String {
    combo
        .split('+')
        .map(|part| part.trim().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// A line that binds the overlay toggle.
fn is_toggle_bind(trimmed: &str) -> bool {
    (trimmed.starts_with("o.bind(")
        || trimmed.starts_with("o.rebind(")
        || trimmed.starts_with("hl.bind("))
        && trimmed.contains(TOGGLE_COMMAND)
}

/// The combination a `hl.unbind("…")` line clears.
fn unbind_target(trimmed: &str) -> Option<String> {
    if trimmed.starts_with("hl.unbind(") || trimmed.starts_with("o.unbind(") {
        first_string_literal(trimmed)
    } else {
        None
    }
}

/// The first double-quoted run in a config line.
fn first_string_literal(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ─── machine helpers ──────────────────────────────────────────────────────────

/// Run `hyprctl`, returning trimmed stdout. `Err` carries whatever the tool
/// said, so the panel can show it verbatim.
fn hyprctl_run(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            format!("{bin} failed")
        } else {
            detail
        });
    }
    Ok(stdout)
}

/// Replace a file without widening its mode: `bindings.lua` is `0600` on a
/// default Omarchy setup, and a fresh 0644 file would quietly relax that.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.sd-tmp", path.display()));
    fs::write(&tmp, contents).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, path).map_err(|e| format!("could not replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A file only this test process uses, like `ipc_tests::private_runtime_dir`.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sd-shortcut-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A stand-in for `hyprctl` that logs its arguments and answers the few
    /// calls the apply path makes.
    fn stub_hyprctl(dir: &Path, tag: &str, script: &str) -> String {
        let path = dir.join(format!("hyprctl-{tag}"));
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
        path.to_string_lossy().to_string()
    }

    /// A realistic slice of `bindings.lua` as install.sh + the Omarchy defaults
    /// leave it today.
    const LEGACY_LUA: &str = "\
-- Keep only your personal keybinding overrides here.
o.bind(\"SUPER + Q\", \"Close window\", hl.dsp.window.close())

-- SUPER DESKTOP: Sticky notes and AI agent terminal overlay
hl.unbind(\"SUPER + SHIFT + q\")
hl.unbind(\"SUPER + SHIFT + Cyrillic_SHORTI\")
hl.unbind(\"SUPER + SHIFT + code:24\")
o.bind(\"SUPER + SHIFT + Q\", \"Super Desktop\", \"super-desktop toggle\")
o.bind(\"SUPER + SHIFT + Cyrillic_shorti\", \"Super Desktop\", \"super-desktop toggle\")
o.exec_on_start(\"super-desktop daemon\")
";

    #[test]
    fn test_interpret_spells_combos_the_way_hyprland_writes_them() {
        let key = |name: &str| gdk::Key::from_name(name).unwrap_or_else(|| panic!("keysym {name}"));
        let m = ModifierType::SUPER_MASK | ModifierType::SHIFT_MASK;

        // A shifted letter already arrives upper-case; an unshifted one is
        // normalised, because Hyprland folds the case anyway.
        assert_eq!(
            interpret(key("q"), m),
            Capture::Combo("SUPER + SHIFT + Q".to_string())
        );
        assert_eq!(
            interpret(key("k"), ModifierType::SUPER_MASK),
            Capture::Combo("SUPER + K".to_string())
        );
        // Named keys keep their keysym name, modifiers keep config order.
        assert_eq!(
            interpret(
                key("Return"),
                ModifierType::CONTROL_MASK | ModifierType::ALT_MASK
            ),
            Capture::Combo("CTRL + ALT + Return".to_string())
        );
        assert_eq!(
            interpret(key("F5"), ModifierType::ALT_MASK),
            Capture::Combo("ALT + F5".to_string())
        );
    }

    #[test]
    fn test_interpret_waits_on_modifiers_and_refuses_bare_keys() {
        let key = |name: &str| gdk::Key::from_name(name).unwrap();
        assert_eq!(
            interpret(key("Shift_L"), ModifierType::SHIFT_MASK),
            Capture::Waiting
        );
        assert_eq!(
            interpret(key("Super_L"), ModifierType::SUPER_MASK),
            Capture::Waiting
        );
        assert_eq!(
            interpret(key("ISO_Level3_Shift"), ModifierType::empty()),
            Capture::Waiting
        );
        // A bare key would swallow that key everywhere: refused, not recorded.
        assert_eq!(
            interpret(key("k"), ModifierType::empty()),
            Capture::NeedsModifier
        );
        assert_eq!(
            interpret(key("Escape"), ModifierType::empty()),
            Capture::NeedsModifier
        );
        // …and holding only SHIFT is still a bare key.
        assert_eq!(
            interpret(key("q"), ModifierType::SHIFT_MASK),
            Capture::NeedsModifier
        );
    }

    #[test]
    fn test_interpret_accepts_f1_to_f12_without_a_modifier() {
        let key = |name: &str| gdk::Key::from_name(name).unwrap();

        // The point of the exemption: a function key types nothing, so it can
        // stand alone.
        for name in ["F1", "F5", "F12"] {
            assert_eq!(
                interpret(key(name), ModifierType::empty()),
                Capture::Combo(name.to_string()),
                "{name} must be recordable on its own"
            );
        }
        // With a modifier they are ordinary combinations.
        assert_eq!(
            interpret(key("F5"), ModifierType::SUPER_MASK),
            Capture::Combo("SUPER + F5".to_string())
        );
        // A locked modifier (CapsLock on) is not "the user held something".
        assert_eq!(
            interpret(key("F5"), ModifierType::LOCK_MASK),
            Capture::Combo("F5".to_string())
        );

        // SHIFT is not a licence to stand alone either: Shift+F5 reloads things
        // in enough applications to be as rude as Shift+Q.
        assert_eq!(
            interpret(key("F5"), ModifierType::SHIFT_MASK),
            Capture::NeedsModifier
        );
        // The window is F1-F12: F13+ and every other bare key stay refused.
        assert_eq!(
            interpret(key("F13"), ModifierType::empty()),
            Capture::NeedsModifier
        );
        assert_eq!(
            interpret(key("space"), ModifierType::empty()),
            Capture::NeedsModifier
        );
        assert_eq!(
            interpret(key("Return"), ModifierType::empty()),
            Capture::NeedsModifier
        );
    }

    #[test]
    fn test_bare_function_key_gets_no_code_variant() {
        // A `code:` form disambiguates a keysym the layout can move (Q on the UK
        // and Cyrillic layouts). F5 is F5 everywhere, so there is nothing to add.
        let updated = rewrite_bindings(LEGACY_LUA, "F5", Some(71));
        assert!(
            updated.contains(&toggle_bind_lua("F5")),
            "got:\n{updated}"
        );
        assert!(updated.contains(r#"hl.unbind("F5")"#), "got:\n{updated}");
        assert!(!updated.contains("code:71"), "got:\n{updated}");
        // Still idempotent, and the migration is untouched by the new shape.
        assert_eq!(rewrite_bindings(&updated, "F5", Some(71)), updated);
        assert!(!updated.contains("Cyrillic_shorti"));
    }

    #[test]
    fn test_current_combo_falls_back_to_the_shipped_default() {
        assert_eq!(current_combo(None), DEFAULT_COMBO);
        assert_eq!(current_combo(Some("  ")), DEFAULT_COMBO);
        assert_eq!(
            current_combo(Some(" SUPER + SHIFT + K ")),
            "SUPER + SHIFT + K"
        );
    }

    #[test]
    fn test_rewrite_migrates_what_install_sh_used_to_append() {
        let updated = rewrite_bindings(LEGACY_LUA, "SUPER + SHIFT + K", Some(45));

        // The stale binds and their clear-the-way unbinds are gone.
        assert!(!updated.contains("Cyrillic_shorti"), "got:\n{updated}");
        assert_eq!(
            updated.matches(TOGGLE_COMMAND).count(),
            2,
            "got:\n{updated}"
        );
        assert_eq!(updated.matches("hl.unbind(").count(), 2, "got:\n{updated}");
        assert!(
            !updated.contains("hl.unbind(\"SUPER + SHIFT + q\")"),
            "got:\n{updated}"
        );

        // Everything that is not ours survives untouched.
        assert!(updated.contains("o.bind(\"SUPER + Q\", \"Close window\", hl.dsp.window.close())"));
        assert!(updated.contains("o.exec_on_start(\"super-desktop daemon\")"));

        // The physical key is bound too, so a layout switch keeps working.
        assert!(updated.contains(&toggle_bind_lua("SUPER + SHIFT + K")));
        assert!(updated.contains(&toggle_bind_lua("SUPER + SHIFT + code:45")));
        assert!(
            updated.ends_with(&format!("{MANAGED_END}\n")),
            "got:\n{updated}"
        );
    }

    #[test]
    fn test_rewrite_is_idempotent() {
        let once = rewrite_bindings(LEGACY_LUA, "SUPER + SHIFT + K", Some(45));
        let twice = rewrite_bindings(&once, "SUPER + SHIFT + K", Some(45));
        assert_eq!(
            once, twice,
            "applying the same combo twice must not grow the file"
        );
        // And swapping to another combo replaces the block instead of stacking.
        let other = rewrite_bindings(&once, "CTRL + ALT + T", None);
        assert_eq!(other.matches(TOGGLE_COMMAND).count(), 1, "got:\n{other}");
        assert!(other
            .contains(&toggle_bind_lua("CTRL + ALT + T")));
        assert!(!other.contains("SUPER + SHIFT + K"));
        assert_eq!(rewrite_bindings(&other, "CTRL + ALT + T", None), other);
    }

    #[test]
    fn test_rewrite_keeps_bindings_that_merely_mention_super_desktop() {
        // Only the toggle command is ours: a status/daemon bind must survive.
        let foreign = "\
o.bind(\"SUPER + SHIFT + S\", \"Super Desktop status\", \"super-desktop status\")
hl.unbind(\"SUPER + SHIFT + X\")
";
        let updated = rewrite_bindings(foreign, "SUPER + SHIFT + K", None);
        assert!(
            updated.contains("\"super-desktop status\""),
            "got:\n{updated}"
        );
        assert!(
            updated.contains("hl.unbind(\"SUPER + SHIFT + X\")"),
            "got:\n{updated}"
        );
    }

    #[test]
    fn test_rewrite_creates_the_block_in_a_file_that_has_none() {
        let updated = rewrite_bindings("", "SUPER + SHIFT + K", None);
        assert!(updated.starts_with(MANAGED_BEGIN), "got:\n{updated}");
        assert!(updated.contains(&toggle_bind_lua("SUPER + SHIFT + K")));
        assert!(!updated.contains("code:"));
    }

    #[test]
    fn test_conflicting_bind_finds_what_the_combo_used_to_do() {
        // Trimmed `hyprctl binds` output: SUPER + Q is Omarchy's Close window.
        let dump = "\
bindd
\tmodmask: 64
\tsubmap: 
\tkey: Q
\tkeycode: 0
\tcatchall: false
\tdescription: Close window
\tdispatcher: __lua
\targ: 17

bindd
\tmodmask: 65
\tsubmap: 
\tkey: Q
\tkeycode: 0
\tcatchall: false
\tdescription: Super Desktop
\tdispatcher: __lua
\targ: 178
";
        assert_eq!(
            conflicting_bind(dump, "SUPER + Q").as_deref(),
            Some("Close window")
        );
        // Our own bind is not a conflict, and SHIFT is part of the mask.
        assert_eq!(conflicting_bind(dump, "SUPER + SHIFT + Q"), None);
        assert_eq!(conflicting_bind(dump, "SUPER + SHIFT + K"), None);
        // Submap binds do not run in the normal keymap.
        let in_submap = dump.replace("\tsubmap: \n", "\tsubmap: resize\n");
        assert_eq!(conflicting_bind(&in_submap, "SUPER + Q"), None);
        // modmask 0 (a bare key) must be matched, not read as "no mask at all".
        let bare = dump.replace(
            "\tmodmask: 64\n\tsubmap: \n\tkey: Q",
            "\tmodmask: 0\n\tsubmap: \n\tkey: F5",
        );
        assert_eq!(
            conflicting_bind(&bare, "F5").as_deref(),
            Some("Close window"),
            "a modifier-less conflict must still be reported"
        );
        assert_eq!(conflicting_bind(&bare, "F6"), None);
    }

    #[test]
    fn test_apply_writes_the_block_and_reloads_hyprland() {
        let dir = temp_dir("apply");
        let binds = dir.join("binds.txt");
        let calls = dir.join("calls.log");
        let bindings = dir.join("bindings.lua");
        fs::write(
            &binds,
            // `hyprctl binds` as the recorder sees it: SHIFT + SUPER + K is
            // already taken by something else the user forgot about.
            "bindd\n\tmodmask: 65\n\tsubmap: \n\tkey: K\n\tkeycode: 0\n\tdescription: Close window\n\n",
        )
        .expect("seed binds dump");
        fs::write(&bindings, LEGACY_LUA).expect("seed bindings");
        fs::set_permissions(&bindings, fs::Permissions::from_mode(0o600)).expect("chmod bindings");

        let stub = stub_hyprctl(
            &dir,
            "ok",
            &format!(
                "case \"$1\" in\n  binds) cat {binds} ;;\n  configerrors) echo ok ;;\nesac\necho \"$@\" >> {calls}\nexit 0",
                binds = binds.display(),
                calls = calls.display(),
            ),
        );

        let applied = apply_combo_at(&bindings, &stub, "SUPER + SHIFT + K", Some(45))
            .expect("apply must succeed");
        assert_eq!(applied.combo, "SUPER + SHIFT + K");
        assert_eq!(applied.conflict.as_deref(), Some("Close window"));
        assert_eq!(applied.warning, None);

        let written = fs::read_to_string(&bindings).expect("read bindings");
        assert!(written.contains(&toggle_bind_lua("SUPER + SHIFT + K")));
        assert!(!written.contains("Cyrillic_shorti"));
        // The mode the user's file had is kept.
        assert_eq!(
            fs::metadata(&bindings)
                .expect("stat bindings")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let calls = fs::read_to_string(&calls).expect("read calls");
        assert!(calls.contains("reload"), "got:\n{calls}");
        assert!(calls.contains("configerrors"), "got:\n{calls}");
        assert!(
            !dir.join("bindings.lua.sd-tmp").exists(),
            "temp file must be renamed away"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_keeps_the_binding_when_hyprland_cannot_reload() {
        let dir = temp_dir("apply-fail");
        let bindings = dir.join("bindings.lua");
        let stub = stub_hyprctl(
            &dir,
            "fail",
            "case \"$1\" in\n  reload) echo \"no session\" >&2; exit 1 ;;\nesac\nexit 0",
        );

        let applied = apply_combo_at(&bindings, &stub, "SUPER + SHIFT + K", None)
            .expect("the file write itself must succeed");
        assert!(
            applied
                .warning
                .as_deref()
                .is_some_and(|w| w.contains("reload")),
            "got: {:?}",
            applied.warning
        );
        assert!(fs::read_to_string(&bindings)
            .expect("read bindings")
            .contains("SUPER + SHIFT + K"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_capture_guard_parks_and_restores_the_keymap() {
        let dir = temp_dir("guard");
        let calls = dir.join("calls.log");
        let submap = dir.join("submap");
        fs::write(&submap, "default\n").expect("seed submap");
        let stub = stub_hyprctl(
            &dir,
            "submap",
            &format!(
                "echo \"$*\" >> {calls}\nif [ \"$1\" = submap ]; then cat {submap}; fi\nexit 0",
                calls = calls.display(),
                submap = submap.display(),
            ),
        );

        // Hyprland did not park us (no Lua config / no session): the recorder
        // carries on, it just cannot see combinations Hyprland owns.
        let guard = begin_capture_at(&stub);
        assert!(!guard.armed());
        guard.end();

        fs::write(&submap, format!("{RECORD_SUBMAP}\n")).expect("seed submap");
        let guard = begin_capture_at(&stub);
        assert!(guard.armed());
        // Dropping (not just `end`) restores the keymap: every cancel path in
        // the panel can therefore never leave the desktop without shortcuts.
        drop(guard);

        let log = fs::read_to_string(&calls).expect("read calls");
        assert!(
            log.contains(&format!("eval {SUBMAP_DEFINE}")),
            "got:\n{log}"
        );
        assert!(
            log.contains(&format!("dispatch {SUBMAP_ENTER}")),
            "got:\n{log}"
        );
        assert!(
            log.matches(&format!("dispatch {SUBMAP_RESET}")).count() >= 2,
            "reset must run before arming and on release, got:\n{log}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_install_sh_writes_the_managed_block_we_read() {
        // install.sh and this module must agree on the markers: the app only
        // ever finds (and replaces) a block install.sh wrote with the same pair
        // of lines. Changing one side alone silently orphans the other.
        let installer = include_str!("../install.sh");
        assert!(
            installer.contains(MANAGED_BEGIN),
            "install.sh must write {MANAGED_BEGIN}"
        );
        assert!(
            installer.contains(MANAGED_END),
            "install.sh must write {MANAGED_END}"
        );
        assert!(installer.contains(TOGGLE_COMMAND));
        assert!(installer.contains(DEFAULT_COMBO));
        assert!(
            installer.contains("release = true"),
            "install.sh must bind on key-release so holding the shortcut does not repeat"
        );
    }

    #[test]
    fn test_toggle_bind_is_a_release_bind() {
        let block = managed_block("SUPER + SHIFT + K", Some(45));
        assert!(toggle_binds_use_release(&block));
        assert!(block.contains(BIND_RELEASE));
        assert_eq!(parse_bind_keycode(&block), Some(45));

        let press = r#"o.bind("SUPER + SHIFT + Q", "Super Desktop", "super-desktop toggle")"#;
        assert!(!toggle_binds_use_release(press));
        assert_eq!(parse_bind_keycode(press), None);
    }

    #[test]
    fn test_rewrite_upgrades_a_press_bind_to_release() {
        let press = format!(
            "{MANAGED_BEGIN}\no.bind(\"SUPER + SHIFT + Q\", \"Super Desktop\", \"super-desktop toggle\")\n{MANAGED_END}\n"
        );
        let updated = rewrite_bindings(&press, "SUPER + SHIFT + Q", Some(24));
        assert!(toggle_binds_use_release(&updated));
        assert!(updated.contains(&toggle_bind_lua("SUPER + SHIFT + Q")));
        assert_eq!(
            rewrite_bindings(&updated, "SUPER + SHIFT + Q", Some(24)),
            updated,
            "a release-bind rewrite must be idempotent"
        );
    }
}
