use gtk4::prelude::*;
use gtk4::{Align, Image, Label, Orientation, ProgressBar, Separator};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Everything the hover card needs to render itself as an expansion of the
/// HUD button it hangs from.
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
        "antigravity" => Some("antigravity"),
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        // OpenCode has no dedicated Omarchy collector; its Fireworks backend
        // record explicitly mentions opencode, so surface that.
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

fn usage_file(id: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".local/state/omarchy/agents/usage")
        .join(format!("{id}.json"))
}

/// Public, allowlisted usage snapshots for the launcher header. Reads the
/// collector cache only; credentials and arbitrary collector fields stay local.
pub fn launcher_usage() -> Vec<Value> {
    ["codex", "claude", "antigravity", "grok", "fireworks"]
        .iter()
        .filter_map(|id| {
            let raw = fs::read_to_string(usage_file(id)).ok()?;
            let value: Value = serde_json::from_str(&raw).ok()?;
            launcher_usage_snapshot(id, &value)
        })
        .collect()
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
    let content = fs::read_to_string(usage_file(id)).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    if !v.is_object() {
        return None;
    }

    let mut limits = Vec::new();
    if let Some(arr) = v.get("limits").and_then(|l| l.as_array()) {
        for item in arr.iter().take(4) {
            let label = item
                .get("label")
                .or_else(|| item.get("title"))
                .and_then(|x| x.as_str())
                .unwrap_or("Limit")
                .to_string();
            let percent = item
                .get("percent")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            limits.push(UsageLimit {
                label,
                percent_used: percent,
                resets_at: item
                    .get("resetsAt")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                used: opt_u64(item, "used"),
                allowance: opt_u64(item, "allowance"),
            });
        }
    }

    Some(ProviderUsage {
        name: {
            let n = get_str(&v, "name");
            if n.is_empty() {
                id.to_string()
            } else {
                n
            }
        },
        tier: get_str(&v, "tierLabel"),
        ready: v.get("ready").and_then(|x| x.as_bool()).unwrap_or(false),
        limits,
        today_prompts: get_u64(&v, "todayPrompts"),
        today_sessions: get_u64(&v, "todaySessions"),
        today_tokens: get_u64(&v, "todayTotalTokens"),
        total_prompts: get_u64(&v, "totalPrompts"),
        total_sessions: get_u64(&v, "totalSessions"),
        model_tokens: sum_model_tokens(&v),
        status_text: get_str(&v, "usageStatusText"),
        auth_help: get_str(&v, "authHelpText"),
        updated_at: get_str(&v, "updatedAt"),
    })
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

/// Days since civil 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Minimal ISO-8601 parser (`2026-09-19T08:10:41[.frac][Z|±HH:MM]`) to unix
/// seconds. No extra deps for one hover label.
fn parse_iso_to_epoch(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let date = s.get(0..10)?;
    let time = s.get(11..19)?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let se: i64 = tp.next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut epoch = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se;

    // Tail after `YYYY-MM-DDTHH:MM:SS`: optional `.fraction`, then zone
    // (`Z`, `±HH:MM`, `±HHMM`) or nothing (assume UTC).
    let tail = s.get(19..).unwrap_or("");
    let mut chars = tail.chars().peekable();
    if chars.peek() == Some(&'.') {
        chars.next();
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            chars.next();
        }
    }
    let zone: String = chars.collect();
    if zone.starts_with('+') || zone.starts_with('-') {
        let sign = if zone.starts_with('+') { 1 } else { -1 };
        let digits: String = zone[1..].chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 4 {
            let zh: i64 = digits.get(0..2)?.parse().ok()?;
            let zm: i64 = digits.get(2..4)?.parse().ok()?;
            epoch -= sign * (zh * 3600 + zm * 60);
        }
    }
    // 'Z' or missing zone => already UTC.
    Some(epoch)
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

        let mut sub = String::new();
        if let (Some(used), Some(allow)) = (lim.used, lim.allowance) {
            if allow > 0 {
                sub = format!("{used} / {allow} used");
            }
        }
        let reset = format_reset_in(&lim.resets_at);
        if !reset.is_empty() {
            if !sub.is_empty() {
                sub.push_str(" • ");
            }
            sub.push_str(&reset);
        }
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
        assert_eq!(usage_id_for_agent("antigravity"), Some("antigravity"));
        assert_eq!(usage_id_for_agent("opencode"), Some("fireworks"));
        assert_eq!(usage_id_for_agent("shell"), None);
        assert_eq!(usage_id_for_agent("bogus"), None);
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
}
