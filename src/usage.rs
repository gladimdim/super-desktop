use gtk4::prelude::*;
use gtk4::{Align, Image, Label, Orientation, ProgressBar, Separator};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Everything the hover card needs to render itself as an expansion of the
/// HUD button it hangs from.
#[derive(Clone)]
pub struct UsageCardInfo<'a> {
    pub usage_id: &'a str,
    pub agent_key: &'a str,
    /// Short HUD label ("OpenCode"), so the card reads as the button grown up.
    pub display_name: &'a str,
    /// Emoji fallback when no brand logo is vendored.
    pub emoji: &'a str,
    pub light_theme: bool,
}

/// "Click to launch • opencode --auto" from the agent's real config.
pub fn launch_line(agent_key: &str) -> String {
    let cfg = crate::tmux::get_agent_config(agent_key);
    let cmd = cfg.commands.first().copied().unwrap_or("shell");
    let args = cfg.default_args.join(" ");
    if args.is_empty() {
        format!("Click to launch • {cmd}")
    } else {
        format!("Click to launch • {cmd} {args}")
    }
}

/// Maps a super-desktop HUD agent key to the Omarchy usage-data id stored at
/// `~/.local/state/omarchy/agents/usage/<id>.json`.
/// `None` means Omarchy tracks no quota for that button (plain tooltip only).
pub fn usage_id_for_agent(agent: &str) -> Option<&'static str> {
    match agent {
        "antigravity" | "agy" => Some("agy"),
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        "opencode" => Some("opencode"),
        "hermes" => Some("hermes"),
        "cursor" => Some("cursor"),
        "dsh" => Some("deepseek"),
        "kimi" => Some("kimi"),
        "qwen" => Some("qwen"),
        "copilot" => Some("copilot"),
        _ => None,
    }
}

fn usage_fallback_id(id: &str) -> Option<&'static str> {
    match id {
        "agy" => Some("antigravity"),
        "opencode" => Some("fireworks"),
        _ => None,
    }
}

/// One quota window. `percent_used` is the fraction consumed:
/// 0.0 = fresh, 1.0 = exhausted (matches Omarchy's collector schema).
pub struct UsageLimit {
    pub label: String,
    pub percent_used: f64,
    pub resets_at: String,
    pub used: Option<u64>,
    pub allowance: Option<u64>,
}

impl UsageLimit {
    /// Fraction of quota left, 0.0..=1.0.
    pub fn left(&self) -> f64 {
        (1.0 - self.percent_used).clamp(0.0, 1.0)
    }

    pub fn left_percent_int(&self) -> i64 {
        (self.left() * 100.0).round() as i64
    }

    /// CSS level class for the bar: ok / warn / crit.
    pub fn level_class(&self) -> &'static str {
        let left = self.left();
        if left < 0.05 {
            "crit"
        } else if left < 0.20 {
            "warn"
        } else {
            "ok"
        }
    }
}

pub struct ProviderUsage {
    pub id: String,
    pub name: String,
    pub tier: String,
    pub ready: bool,
    pub limits: Vec<UsageLimit>,
    pub today_prompts: u64,
    pub today_sessions: u64,
    pub today_tokens: u64,
    pub total_prompts: u64,
    pub total_sessions: u64,
    pub model_tokens: u64,
    pub status_text: String,
    pub auth_help: String,
    pub updated_at: String,
}

fn usage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local/state/omarchy/agents/usage")
}

fn read_usage_value(dir: &PathBuf, id: &str) -> Option<(&'static str, Value)> {
    let read = |candidate: &str| {
        let raw = fs::read_to_string(dir.join(format!("{candidate}.json"))).ok()?;
        let value: Value = serde_json::from_str(&raw).ok()?;
        value.is_object().then_some(value)
    };
    if let Some(value) = read(id) {
        let static_id: &'static str = match id {
            "codex" => "codex",
            "claude" => "claude",
            "agy" => "agy",
            "antigravity" => "antigravity",
            "grok" => "grok",
            "opencode" => "opencode",
            "fireworks" => "fireworks",
            "hermes" => "hermes",
            "copilot" => "copilot",
            "cursor" => "cursor",
            "deepseek" => "deepseek",
            "kimi" => "kimi",
            "qwen" => "qwen",
            "ollama" => "ollama",
            "openrouter" => "openrouter",
            "vibe" => "vibe",
            "zai" => "zai",
            "devin" => "devin",
            "minimax" => "minimax",
            "synthetic" => "synthetic",
            "local" => "local",
            _ => return None,
        };
        return Some((static_id, value));
    }
    let fallback = usage_fallback_id(id)?;
    read(fallback).map(|value| (fallback, value))
}

fn has_active_usage(value: &Value) -> bool {
    value.get("ready").and_then(Value::as_bool) == Some(true)
        || value
            .get("limits")
            .and_then(Value::as_array)
            .is_some_and(|limits| !limits.is_empty())
        || opt_u64(value, "todayTotalTokens").is_some_and(|n| n > 0)
        || opt_u64(value, "todayPrompts").is_some_and(|n| n > 0)
        || opt_u64(value, "todaySessions").is_some_and(|n| n > 0)
        || opt_u64(value, "totalPrompts").is_some_and(|n| n > 0)
        || opt_u64(value, "totalSessions").is_some_and(|n| n > 0)
}

const PRIMARY_USAGE_IDS: &[&str] = &["codex", "claude", "agy", "grok", "opencode"];
const EXTRA_USAGE_IDS: &[&str] = &[
    "fireworks",
    "hermes",
    "copilot",
    "cursor",
    "deepseek",
    "kimi",
    "qwen",
    "ollama",
    "openrouter",
    "vibe",
    "zai",
    "devin",
    "minimax",
    "synthetic",
    "local",
];

/// Public, allowlisted usage snapshots for the launcher header. Reads the
/// collector cache only; credentials and arbitrary collector fields stay local.
pub fn launcher_usage() -> Vec<Value> {
    if cfg!(not(test)) {
        crate::antigravity_usage::refresh_async();
    }
    let dir = usage_dir();
    let mut out = launcher_usage_in(&dir);
    if let Some(live) = crate::antigravity_usage::cached_snapshot() {
        apply_agy_live_to_snapshots(&mut out, &live);
    }
    out
}

fn launcher_usage_in(dir: &std::path::Path) -> Vec<Value> {
    let dir = dir.to_path_buf();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &id in PRIMARY_USAGE_IDS {
        if let Some((resolved, value)) = read_usage_value(&dir, id) {
            seen.insert(resolved);
            if let Some(snapshot) = launcher_usage_snapshot(resolved, &value) {
                out.push(snapshot);
            }
        }
    }
    for &id in EXTRA_USAGE_IDS {
        if seen.contains(id) {
            continue;
        }
        let Some((resolved, value)) = read_usage_value(&dir, id) else {
            continue;
        };
        if !has_active_usage(&value) {
            continue;
        }
        seen.insert(resolved);
        if let Some(snapshot) = launcher_usage_snapshot(resolved, &value) {
            out.push(snapshot);
        }
    }
    out
}

/// Read all active local provider usage records (never filtered to the phone bridge subset).
pub fn all_provider_usage() -> Vec<ProviderUsage> {
    if cfg!(not(test)) {
        crate::antigravity_usage::refresh_async();
    }
    let mut out = all_provider_usage_in(&usage_dir());
    if let Some(live) = crate::antigravity_usage::cached_snapshot() {
        if let Some(pos) = out.iter().position(|p| p.id == "agy") {
            let existing = out.remove(pos);
            if let Some(updated) = apply_agy_live(Some(existing), Some(&live)) {
                out.insert(pos, updated);
            }
        } else if let Some(added) = apply_agy_live(None, Some(&live)) {
            out.push(added);
        }
    }
    out
}

fn all_provider_usage_in(dir: &std::path::Path) -> Vec<ProviderUsage> {
    let dir = dir.to_path_buf();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &id in PRIMARY_USAGE_IDS.iter().chain(EXTRA_USAGE_IDS.iter()) {
        if seen.contains(id) {
            continue;
        }
        let Some((resolved, value)) = read_usage_value(&dir, id) else {
            continue;
        };
        if !seen.insert(resolved) || !has_active_usage(&value) {
            continue;
        }
        let parsed = parse_provider_usage(resolved, &value);
        let has_content = !parsed.limits.is_empty()
            || parsed.today_tokens > 0
            || parsed.today_prompts > 0
            || parsed.today_sessions > 0
            || parsed.total_prompts > 0
            || parsed.total_sessions > 0
            || !parsed.status_text.is_empty();
        if has_content {
            out.push(parsed);
        }
    }
    out
}

fn launcher_usage_snapshot(id: &str, value: &Value) -> Option<Value> {
    if !value.is_object() {
        return None;
    }
    let limits: Vec<Value> = value.get("limits").and_then(Value::as_array)
        .into_iter().flatten().take(4).map(|limit| {
            serde_json::json!({
                "label": limit.get("label").or_else(|| limit.get("title"))
                    .and_then(Value::as_str).unwrap_or("Limit"),
                "percentUsed": limit.get("percent").and_then(Value::as_f64)
                    .filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0)),
                "resetsAt": limit.get("resetsAt").and_then(Value::as_str),
                "used": opt_u64(limit, "used"),
                "allowance": opt_u64(limit, "allowance"),
            })
        }).collect();
    Some(serde_json::json!({
        "id": id,
        "name": value.get("name").and_then(Value::as_str).unwrap_or(id),
        "ready": value.get("ready").and_then(Value::as_bool).unwrap_or(false),
        "updatedAt": value.get("updatedAt").and_then(Value::as_str),
        "limits": limits,
        "todayTokens": opt_u64(value, "todayTotalTokens"),
        "todaySessions": opt_u64(value, "todaySessions"),
        "todayPrompts": opt_u64(value, "todayPrompts"),
        "totalSessions": opt_u64(value, "totalSessions"),
        "totalPrompts": opt_u64(value, "totalPrompts"),
    }))
}

fn get_str(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn get_u64(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn opt_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}

/// Sum input+output+cache tokens across every model in `modelUsage`.
fn sum_model_tokens(v: &Value) -> u64 {
    let mut total = 0u64;
    if let Some(map) = v.get("modelUsage").and_then(|m| m.as_object()) {
        for (_, m) in map {
            for k in [
                "inputTokens",
                "outputTokens",
                "cacheReadInputTokens",
                "cacheCreationInputTokens",
            ] {
                total = total.saturating_add(m.get(k).and_then(|x| x.as_u64()).unwrap_or(0));
            }
        }
    }
    total
}

/// Load cached Omarchy usage data for a provider id. Pure file read, no
/// network — safe to call on hover on the UI thread.
pub fn load_usage(id: &str) -> Option<ProviderUsage> {
    let dir = usage_dir();
    if id == "agy" {
        let direct = load_usage_direct(&dir, "agy");
        if let Some(live) = crate::antigravity_usage::cached_snapshot() {
            return apply_agy_live(direct, Some(&live));
        }
        if let Some(usage) = direct {
            return Some(usage);
        }
    }
    load_usage_in(&dir, id)
}

fn load_usage_direct(dir: &std::path::Path, id: &str) -> Option<ProviderUsage> {
    let content = fs::read_to_string(dir.join(format!("{id}.json"))).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    v.is_object().then(|| parse_provider_usage(id, &v))
}

fn live_limits_to_usage(limits: &[crate::antigravity_usage::LiveLimit]) -> Vec<UsageLimit> {
    limits
        .iter()
        .map(|lim| UsageLimit {
            label: lim.label.clone(),
            percent_used: lim.percent_used.clamp(0.0, 1.0),
            resets_at: lim.resets_at.clone(),
            used: None,
            allowance: None,
        })
        .collect()
}

fn is_newer_stamp(candidate: &str, current: &str) -> bool {
    match (parse_iso_to_epoch(candidate), parse_iso_to_epoch(current)) {
        (Some(a), Some(b)) => a >= b,
        (Some(_), None) => true,
        _ => false,
    }
}

fn apply_agy_live(
    usage: Option<ProviderUsage>,
    live: Option<&crate::antigravity_usage::LiveSnapshot>,
) -> Option<ProviderUsage> {
    let Some(snap) = live else {
        return usage;
    };
    let live_limits = live_limits_to_usage(&snap.limits);
    match usage {
        Some(mut u) => {
            u.name = "Antigravity".to_string();
            if !live_limits.is_empty() {
                u.limits = live_limits;
                u.ready = true;
                u.auth_help.clear();
            }
            if !snap.tier.is_empty() {
                u.tier = snap.tier.clone();
            }
            if !snap.updated_at.is_empty() && is_newer_stamp(&snap.updated_at, &u.updated_at) {
                u.updated_at = snap.updated_at.clone();
            }
            Some(u)
        }
        None => Some(ProviderUsage {
            id: "agy".to_string(),
            name: "Antigravity".to_string(),
            tier: snap.tier.clone(),
            ready: true,
            limits: live_limits,
            today_prompts: 0,
            today_sessions: 0,
            today_tokens: 0,
            total_prompts: 0,
            total_sessions: 0,
            model_tokens: 0,
            status_text: String::new(),
            auth_help: String::new(),
            updated_at: snap.updated_at.clone(),
        }),
    }
}

fn apply_agy_live_to_snapshots(
    out: &mut Vec<Value>,
    live: &crate::antigravity_usage::LiveSnapshot,
) {
    let limits_json: Vec<Value> = live
        .limits
        .iter()
        .take(4)
        .map(|lim| {
            serde_json::json!({
                "label": lim.label,
                "percentUsed": lim.percent_used.clamp(0.0, 1.0),
                "resetsAt": (!lim.resets_at.is_empty()).then_some(lim.resets_at.as_str()),
                "used": Value::Null,
                "allowance": Value::Null,
            })
        })
        .collect();
    if let Some(entry) = out
        .iter_mut()
        .find(|v| matches!(v.get("id").and_then(Value::as_str), Some("agy" | "antigravity")))
    {
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("id".into(), Value::String("agy".into()));
            obj.insert("name".into(), Value::String("Antigravity".into()));
            if !limits_json.is_empty() {
                obj.insert("ready".into(), Value::Bool(true));
                obj.insert("limits".into(), Value::Array(limits_json));
            }
            let cur_updated = obj.get("updatedAt").and_then(Value::as_str).unwrap_or("");
            if !live.updated_at.is_empty() && is_newer_stamp(&live.updated_at, cur_updated) {
                obj.insert("updatedAt".into(), Value::String(live.updated_at.clone()));
            }
        }
    } else if !limits_json.is_empty() {
        out.push(serde_json::json!({
            "id": "agy",
            "name": "Antigravity",
            "ready": true,
            "updatedAt": live.updated_at,
            "limits": limits_json,
            "todayTokens": Value::Null,
            "todaySessions": Value::Null,
            "todayPrompts": Value::Null,
            "totalSessions": Value::Null,
            "totalPrompts": Value::Null,
        }));
    }
}

fn load_usage_in(dir: &std::path::Path, id: &str) -> Option<ProviderUsage> {
    let parse = |candidate: &str| -> Option<Value> {
        let content = fs::read_to_string(dir.join(format!("{candidate}.json"))).ok()?;
        let v: Value = serde_json::from_str(&content).ok()?;
        v.is_object().then_some(v)
    };
    let v = parse(id).or_else(|| usage_fallback_id(id).and_then(parse))?;
    Some(parse_provider_usage(id, &v))
}

fn parse_provider_usage(id: &str, v: &Value) -> ProviderUsage {
    let mut limits = Vec::new();
    if let Some(arr) = v.get("limits").and_then(|l| l.as_array()) {
        for item in arr.iter().take(6) {
            let used = opt_u64(item, "used");
            let allowance = opt_u64(item, "allowance");
            let percent = match item
                .get("percent")
                .and_then(|x| x.as_f64())
                .filter(|p| p.is_finite())
            {
                Some(p) => p.clamp(0.0, 1.0),
                None => match (used, allowance) {
                    (Some(u), Some(a)) if a > 0 => (u as f64 / a as f64).clamp(0.0, 1.0),
                    _ => continue,
                },
            };
            let label = item
                .get("label")
                .or_else(|| item.get("title"))
                .and_then(|x| x.as_str())
                .unwrap_or("Limit")
                .to_string();
            limits.push(UsageLimit {
                label,
                percent_used: percent,
                resets_at: item
                    .get("resetsAt")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                used,
                allowance,
            });
        }
    }

    ProviderUsage {
        id: id.to_string(),
        name: {
            let n = get_str(v, "name");
            if n.is_empty() {
                id.to_string()
            } else {
                n
            }
        },
        tier: get_str(v, "tierLabel"),
        ready: v.get("ready").and_then(|x| x.as_bool()).unwrap_or(false),
        limits,
        today_prompts: get_u64(v, "todayPrompts"),
        today_sessions: get_u64(v, "todaySessions"),
        today_tokens: get_u64(v, "todayTotalTokens"),
        total_prompts: get_u64(v, "totalPrompts"),
        total_sessions: get_u64(v, "totalSessions"),
        model_tokens: sum_model_tokens(v),
        status_text: get_str(v, "usageStatusText"),
        auth_help: get_str(v, "authHelpText"),
        updated_at: get_str(v, "updatedAt"),
    }
}

/// 1250 -> "1.2K", 2_400_000 -> "2.4M", 3_100_000_000 -> "3.1B".
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

/// ISO-8601 time (`2026-09-19T08:10:41[.frac]`, `T` or a space between date
/// and time) to unix seconds. The zone is `Z`, `±HH:MM` or `±HHMM`; a time
/// without one is UTC.
fn parse_iso_to_epoch(s: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDateTime};
    let s = s.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(s) {
        return Some(at.timestamp());
    }
    ["T", " "].iter().find_map(|separator| {
        let local = format!("%Y-%m-%d{separator}%H:%M:%S%.f");
        DateTime::parse_from_str(s, &format!("{local}%z"))
            .map(|at| at.timestamp())
            .or_else(|_| NaiveDateTime::parse_from_str(s, &local).map(|at| at.and_utc().timestamp()))
            .ok()
    })
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "resets in 2d 4h" / "resets in 36m" / "resets soon". Empty when unknown.
pub fn format_reset_in(resets_at: &str) -> String {
    let target = match parse_iso_to_epoch(resets_at) {
        Some(t) => t,
        None => return String::new(),
    };
    let diff = target - now_epoch();
    if diff <= 0 {
        return "resets soon".to_string();
    }
    let days = diff / 86400;
    let hours = (diff % 86400) / 3600;
    let mins = (diff % 3600) / 60;
    if days > 0 {
        if hours > 0 {
            format!("resets in {days}d {hours}h")
        } else {
            format!("resets in {days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("resets in {hours}h {mins}m")
        } else {
            format!("resets in {hours}h")
        }
    } else if mins > 0 {
        format!("resets in {mins}m")
    } else {
        "resets soon".to_string()
    }
}

/// "updated 5m ago" / "updated 3h ago" / "updated Sep 3". Empty when unknown.
pub fn format_updated_ago(updated_at: &str) -> String {
    let t = match parse_iso_to_epoch(updated_at) {
        Some(t) => t,
        None => return String::new(),
    };
    let diff = (now_epoch() - t).max(0);
    if diff < 60 {
        "updated just now".to_string()
    } else if diff < 3600 {
        format!("updated {}m ago", diff / 60)
    } else if diff < 86400 {
        format!("updated {}h ago", diff / 3600)
    } else if diff < 86400 * 7 {
        format!("updated {}d ago", diff / 86400)
    } else {
        "updated over a week ago".to_string()
    }
}

fn format_limit_subtitle(lim: &UsageLimit) -> String {
    let mut sub = String::new();
    if let (Some(used), Some(allow)) = (lim.used, lim.allowance) {
        if allow > 0 {
            sub = format!("{used} / {allow} used");
        }
    }
    let reset = format_reset_in(&lim.resets_at);
    // Untouched 100%-left buckets can carry a past window timestamp; omit misleading "resets soon".
    let show_reset = !reset.is_empty() && !(reset == "resets soon" && lim.left_percent_int() >= 100);
    if show_reset {
        if !sub.is_empty() {
            sub.push_str(" • ");
        }
        sub.push_str(&reset);
    }
    sub
}

fn small_label(text: &str, class: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class(class);
    l.set_halign(Align::Start);
    l.set_wrap(true);
    l.set_max_width_chars(42);
    l
}

/// Build the hover card for a provider: a pill header that mirrors the HUD
/// button (logo + short name + tier) with the quota details unfolding below
/// it, like the button itself expanded. Always returns a widget — even when
/// Omarchy has no data yet it shows how to fetch it.
pub fn build_usage_content(info: &UsageCardInfo) -> gtk4::Box {
    let root = gtk4::Box::new(Orientation::Vertical, 8);
    root.add_css_class("usage-pop-box");
    root.set_size_request(300, -1);

    // Header pill: the button, grown up.
    let head = gtk4::Box::new(Orientation::Horizontal, 8);
    head.add_css_class("usage-head");
    if let Some(logo) = crate::brand::logo_path(info.agent_key, info.light_theme) {
        let img = Image::from_file(&logo);
        img.set_pixel_size(20);
        head.append(&img);
    } else {
        head.append(&Label::new(Some(info.emoji)));
    }
    let name = Label::new(Some(info.display_name));
    name.add_css_class("usage-head-name");
    name.set_halign(Align::Start);
    name.set_hexpand(true);
    head.append(&name);

    // Tier lands in the header once data is loaded; placeholder keeps the
    // pill shape stable for the no-data case.
    let tier_holder = gtk4::Box::new(Orientation::Horizontal, 0);
    head.append(&tier_holder);
    root.append(&head);

    let Some(u) = load_usage(info.usage_id) else {
        root.append(&small_label("No usage data yet", "usage-title"));
        root.append(&small_label(
            &format!(
                "Run `omarchy agent usage update {}` to fetch it.",
                info.usage_id
            ),
            "usage-warn",
        ));
        root.append(&footer_box(&launch_line(info.agent_key), ""));
        return root;
    };

    if !u.tier.is_empty() {
        let tier = Label::new(Some(&u.tier));
        tier.add_css_class("usage-tier");
        tier_holder.append(&tier);
    } else if !u.name.is_empty() && u.name != info.display_name {
        let tier = Label::new(Some(&u.name));
        tier.add_css_class("usage-tier");
        tier_holder.append(&tier);
    }

    let sep = Separator::new(Orientation::Horizontal);
    sep.add_css_class("usage-sep");
    root.append(&sep);

    if !u.status_text.is_empty() {
        root.append(&small_label(&u.status_text, "usage-status"));
    }

    // Quota windows with bars.
    if u.limits.is_empty() {
        root.append(&small_label("No quota windows tracked", "usage-meta"));
    }
    for lim in &u.limits {
        let row = gtk4::Box::new(Orientation::Vertical, 3);

        let top = gtk4::Box::new(Orientation::Horizontal, 8);
        let lbl = small_label(&lim.label, "usage-limit-label");
        lbl.set_hexpand(true);
        top.append(&lbl);
        let pct = Label::new(Some(&format!("{}% left", lim.left_percent_int())));
        pct.add_css_class("usage-left");
        pct.add_css_class(lim.level_class());
        top.append(&pct);
        row.append(&top);

        let bar = ProgressBar::new();
        bar.set_fraction(lim.left());
        bar.set_show_text(false);
        bar.add_css_class("usage-bar");
        bar.add_css_class(lim.level_class());
        row.append(&bar);

        let sub = format_limit_subtitle(lim);
        if !sub.is_empty() {
            row.append(&small_label(&sub, "usage-meta"));
        }

        root.append(&row);
    }

    // Tokens / activity.
    let mut activity = format!(
        "Today {} prompts • {} sessions",
        u.today_prompts, u.today_sessions
    );
    if u.today_tokens > 0 {
        activity.push_str(&format!(" • {} tokens", format_tokens(u.today_tokens)));
    }
    root.append(&small_label(&activity, "usage-meta"));

    let mut life = format!(
        "Lifetime {} prompts • {} sessions",
        u.total_prompts, u.total_sessions
    );
    if u.model_tokens > 0 {
        life.push_str(&format!(" • {} tokens", format_tokens(u.model_tokens)));
    }
    root.append(&small_label(&life, "usage-meta"));

    if !u.ready && !u.auth_help.is_empty() {
        root.append(&small_label(&u.auth_help, "usage-warn"));
    }

    let age = format_updated_ago(&u.updated_at);
    let src = if age.is_empty() {
        "Omarchy usage cache".to_string()
    } else {
        format!("Omarchy • {age}")
    };
    root.append(&footer_box(&launch_line(info.agent_key), &src));

    root
}

/// Bottom of the card: what click does + where the numbers came from.
fn footer_box(launch: &str, src: &str) -> gtk4::Box {
    let f = gtk4::Box::new(Orientation::Vertical, 2);
    let sep = Separator::new(Orientation::Horizontal);
    sep.add_css_class("usage-sep");
    f.append(&sep);
    f.append(&small_label(launch, "usage-launch"));
    if !src.is_empty() {
        f.append(&small_label(src, "usage-src"));
    }
    f
}

#[cfg(test)]
mod tests {
    #[test]
    fn launcher_snapshot_preserves_unknown_counts_and_excludes_private_fields() {
        let snapshot = super::launcher_usage_snapshot("codex", &serde_json::json!({
            "ready": true, "token": "private", "todaySessions": 0,
            "limits": [{"label": "Weekly", "percent": 0.15}, {"label": "Unknown"}]
        })).unwrap();
        assert!(snapshot.get("token").is_none());
        assert!(snapshot["todayTokens"].is_null());
        assert_eq!(snapshot["todaySessions"], 0);
        assert_eq!(snapshot["limits"][0]["percentUsed"], 0.15);
        assert!(snapshot["limits"][1]["percentUsed"].is_null());
        assert!(super::launcher_usage_snapshot("codex", &serde_json::Value::Null).is_none());
    }

    use super::*;

    #[test]
    fn test_usage_id_mapping() {
        assert_eq!(usage_id_for_agent("claude"), Some("claude"));
        assert_eq!(usage_id_for_agent("codex"), Some("codex"));
        assert_eq!(usage_id_for_agent("grok"), Some("grok"));
        assert_eq!(usage_id_for_agent("antigravity"), Some("agy"));
        assert_eq!(usage_id_for_agent("agy"), Some("agy"));
        assert_eq!(usage_id_for_agent("opencode"), Some("opencode"));
        assert_eq!(usage_id_for_agent("hermes"), Some("hermes"));
        assert_eq!(usage_id_for_agent("cursor"), Some("cursor"));
        assert_eq!(usage_id_for_agent("dsh"), Some("deepseek"));
        assert_eq!(usage_id_for_agent("kimi"), Some("kimi"));
        assert_eq!(usage_id_for_agent("qwen"), Some("qwen"));
        assert_eq!(usage_id_for_agent("copilot"), Some("copilot"));
        assert_eq!(usage_id_for_agent("shell"), None);
        assert_eq!(usage_id_for_agent("bogus"), None);
    }

    #[test]
    fn usage_collectors_prefer_current_ids_fall_back_and_filter_inactive_extras() {
        let dir = std::env::temp_dir().join(format!(
            "sd-usage-test-{}",
            crate::tmux::unique_session_name()
        ));
        fs::create_dir_all(&dir).unwrap();
        // Fallback when agy.json is absent.
        fs::write(dir.join("antigravity.json"), r#"{"name":"Antigravity Legacy","ready":true,"totalPrompts":5}"#).unwrap();
        assert_eq!(load_usage_in(&dir, "agy").unwrap().name, "Antigravity Legacy");
        // Current id wins when present.
        fs::write(dir.join("agy.json"), r#"{"name":"Gemini","ready":true,"totalPrompts":9}"#).unwrap();
        assert_eq!(load_usage_in(&dir, "agy").unwrap().name, "Gemini");
        // OpenCode falls back to fireworks.json when opencode.json is missing.
        fs::write(dir.join("fireworks.json"), r#"{"name":"Fireworks","ready":true,"todayTotalTokens":100}"#).unwrap();
        assert_eq!(load_usage_in(&dir, "opencode").unwrap().name, "Fireworks");

        fs::write(dir.join("codex.json"), r#"{"name":"Codex","ready":true}"#).unwrap();
        // Active extra collector vs inactive placeholder collector.
        fs::write(dir.join("hermes.json"), r#"{"name":"Hermes","ready":false,"todayPrompts":0,"totalPrompts":0}"#).unwrap();
        fs::write(dir.join("kimi.json"), r#"{"name":"Kimi","ready":true,"todayPrompts":2,"secret":"drop"}"#).unwrap();

        let snap = launcher_usage_in(&dir);
        let ids: Vec<_> = snap.iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["codex", "agy", "fireworks", "kimi"]);
        assert!(snap[3].get("secret").is_none());

        // When opencode.json exists, both opencode and active fireworks appear without duplicates.
        fs::write(dir.join("opencode.json"), r#"{"name":"OpenCode","ready":true,"totalPrompts":12}"#).unwrap();
        let snap2 = launcher_usage_in(&dir);
        let ids2: Vec<_> = snap2.iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids2, vec!["codex", "agy", "opencode", "fireworks", "kimi"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(1250), "1.2K");
        assert_eq!(format_tokens(2_400_000), "2.4M");
        assert_eq!(format_tokens(3_100_000_000), "3.1B");
    }

    #[test]
    fn test_limit_levels() {
        let mk = |p| UsageLimit {
            label: "x".into(),
            percent_used: p,
            resets_at: String::new(),
            used: None,
            allowance: None,
        };
        assert_eq!(mk(0.0).left_percent_int(), 100);
        assert_eq!(mk(0.0).level_class(), "ok");
        assert_eq!(mk(0.85).level_class(), "warn");
        assert_eq!(mk(0.99).level_class(), "crit");
    }

    #[test]
    fn test_parse_iso() {
        // 2026-01-01T00:00:00Z == 1767225600
        assert_eq!(parse_iso_to_epoch("2026-01-01T00:00:00Z"), Some(1767225600));
        assert_eq!(
            parse_iso_to_epoch("2026-01-01T00:00:00+00:00"),
            Some(1767225600)
        );
        assert_eq!(
            parse_iso_to_epoch("2026-01-01T02:00:00+02:00"),
            Some(1767225600)
        );
        assert_eq!(
            parse_iso_to_epoch("2026-09-19T08:10:41.123456+00:00"),
            parse_iso_to_epoch("2026-09-19T08:10:41+00:00")
        );
        assert_eq!(parse_iso_to_epoch("garbage"), None);
        assert_eq!(parse_iso_to_epoch(""), None);
    }

    #[test]
    fn parse_iso_accepts_every_zone_shape_usage_apis_send() {
        let epoch = Some(1_789_805_441); // 2026-09-19T08:10:41Z
        for value in [
            "2026-09-19T08:10:41Z",
            "2026-09-19T08:10:41z",
            " 2026-09-19T08:10:41Z\n",
            "2026-09-19T08:10:41.5Z",
            "2026-09-19T08:10:41.123456789Z",
            "2026-09-19T08:10:41+00:00",
            "2026-09-19T11:10:41+03:00",
            "2026-09-19T03:40:41-04:30",
            "2026-09-19T11:10:41+0300",
            "2026-09-19T11:10:41.250+0300",
            "2026-09-19 08:10:41Z",
            "2026-09-19 11:10:41+03:00",
            // Without a zone the time is UTC.
            "2026-09-19T08:10:41",
            "2026-09-19T08:10:41.123",
            "2026-09-19 08:10:41",
        ] {
            assert_eq!(parse_iso_to_epoch(value), epoch, "{value:?}");
        }
        for value in ["2026-09-19", "2026-13-19T08:10:41Z", "2026-09-19T25:10:41Z", "08:10:41"] {
            assert_eq!(parse_iso_to_epoch(value), None, "{value:?}");
        }
    }

    #[test]
    fn all_provider_usage_reads_tiers_limits_and_skips_inactive() {
        let dir = std::env::temp_dir().join(format!(
            "sd-all-usage-test-{}",
            crate::tmux::unique_session_name()
        ));
        fs::create_dir_all(&dir).unwrap();

        fs::write(
            dir.join("codex.json"),
            r#"{"name":"Codex","tierLabel":"Pro 100","ready":true,"todayTotalTokens":500,"limits":[{"label":"Weekly (7-day)","percent":0.07,"resetsAt":"2026-11-29T17:09:42Z"}]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("claude.json"),
            r#"{"name":"Claude","tierLabel":"Max 5x","ready":true,"limits":[{"label":"Session (5-hour)","percent":0.1},{"label":"Weekly (7-day)","percent":0.96},{"label":"MissingPercent"},{"title":"Derived","used":77,"allowance":100}]}"#,
        )
        .unwrap();
        // Ready=false and zero activity -> skipped even if primary id.
        fs::write(
            dir.join("grok.json"),
            r#"{"name":"Grok","ready":false,"todayPrompts":0,"totalPrompts":0,"limits":[]}"#,
        )
        .unwrap();
        // Extra active provider included.
        fs::write(
            dir.join("local.json"),
            r#"{"name":"Local models","ready":true,"totalPrompts":3,"limits":[]}"#,
        )
        .unwrap();

        let items = all_provider_usage_in(&dir);
        let ids: Vec<_> = items.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["codex", "claude", "local"]);
        assert_eq!(items[0].tier, "Pro 100");
        assert_eq!(items[0].today_tokens, 500);
        assert_eq!(items[1].limits.len(), 3);
        assert_eq!(items[1].limits[2].label, "Derived");
        assert!((items[1].limits[2].percent_used - 0.77).abs() < 1e-6);
        assert_eq!(items[1].limits[2].used, Some(77));
        assert_eq!(items[1].limits[2].allowance, Some(100));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agy_live_merge_replaces_empty_limits_and_suppresses_stale_resets_soon() {
        let dir = std::env::temp_dir().join(format!(
            "sd-agy-merge-test-{}",
            crate::tmux::unique_session_name()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("agy.json"),
            r#"{"name":"Gemini","tierLabel":"Ultra","ready":true,"limits":[],"todayPrompts":34,"todaySessions":3,"todayTotalTokens":160000,"totalPrompts":496,"totalSessions":74,"updatedAt":"2026-11-24T10:00:00Z"}"#,
        )
        .unwrap();

        let disk = load_usage_direct(&dir, "agy");
        let live = crate::antigravity_usage::LiveSnapshot {
            tier: "Ultra".into(),
            limits: vec![
                crate::antigravity_usage::LiveLimit {
                    label: "Gemini".into(),
                    percent_used: 0.45,
                    resets_at: "2099-01-01T00:00:00Z".into(),
                },
                crate::antigravity_usage::LiveLimit {
                    label: "Claude & GPT (5-hour)".into(),
                    percent_used: 0.0,
                    resets_at: "2020-01-01T00:00:00Z".into(),
                },
            ],
            updated_at: "2026-11-24T11:20:00Z".into(),
            fetched_at_ms: 1,
        };

        let merged = apply_agy_live(disk, Some(&live)).unwrap();
        assert_eq!(merged.name, "Antigravity");
        assert_eq!(merged.tier, "Ultra");
        assert_eq!(merged.today_prompts, 34);
        assert_eq!(merged.today_tokens, 160_000);
        assert_eq!(merged.updated_at, "2026-11-24T11:20:00Z");
        assert_eq!(merged.limits.len(), 2);
        assert_eq!(merged.limits[0].left_percent_int(), 55);
        assert!(!format_limit_subtitle(&merged.limits[0]).is_empty());
        assert_eq!(format_limit_subtitle(&merged.limits[1]), "");

        // Works even when no agy.json exists on disk.
        let live_only = apply_agy_live(None, Some(&live)).unwrap();
        assert_eq!(live_only.id, "agy");
        assert_eq!(live_only.limits.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
