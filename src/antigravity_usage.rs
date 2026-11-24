//! Live quota reader for Google Antigravity (`agy` CLI).
//!
//! Queries Google's Cloud Code endpoints (`v1internal:loadCodeAssist`,
//! `v1internal:retrieveUserQuotaSummary`, and `v1internal:retrieveUserQuota`)
//! read-only using the local OAuth access token managed by `agy`, and caches
//! the merged snapshot in `~/.local/state/super-desktop/agy-usage.json`.
use chrono::{DateTime, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_CLOUD_CODE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
const CACHE_TTL_MS: u64 = 60_000;
const ERROR_BACKOFF_MS: u64 = 30_000;
static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveLimit {
    pub label: String,
    pub percent_used: f64,
    pub resets_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSnapshot {
    pub tier: String,
    pub limits: Vec<LiveLimit>,
    pub updated_at: String,
    pub fetched_at_ms: u64,
}

#[derive(Default)]
struct CacheState {
    snapshot: Option<LiveSnapshot>,
    loaded_from_disk: bool,
    last_attempt_ms: u64,
    last_ok: bool,
    in_flight: bool,
}

static CACHE: OnceLock<Mutex<CacheState>> = OnceLock::new();

fn cache_mutex() -> &'static Mutex<CacheState> {
    CACHE.get_or_init(|| Mutex::new(CacheState::default()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_epoch() -> i64 {
    (now_ms() / 1000) as i64
}

fn parse_epoch_value(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_f64().filter(|f| f.is_finite() && *f > 0.0) {
        let secs = if n > 1e12 { n / 1000.0 } else { n };
        return Some(secs as i64);
    }
    let s = v.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(num) = s.parse::<f64>() {
        if num.is_finite() && num > 0.0 {
            let secs = if num > 1e12 { num / 1000.0 } else { num };
            return Some(secs as i64);
        }
    }
    parse_iso_epoch(s)
}

fn parse_iso_epoch(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(s) {
        return Some(at.timestamp());
    }
    for sep in ["T", " "] {
        let fmt = format!("%Y-%m-%d{sep}%H:%M:%S%.f");
        if let Ok(at) = DateTime::parse_from_str(s, &format!("{fmt}%z")) {
            return Some(at.timestamp());
        }
        if let Ok(at) = NaiveDateTime::parse_from_str(s, &fmt) {
            return Some(at.and_utc().timestamp());
        }
    }
    None
}

fn parse_iso_normalized(v: &Value) -> String {
    let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    if let Some(secs) = parse_iso_epoch(s) {
        if let Some(dt) = DateTime::from_timestamp(secs, 0) {
            return dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        }
    }
    String::new()
}

/// Extract a fresh OAuth access token from an Antigravity keyring/file JSON blob.
pub fn parse_oauth_token(raw: &str, now_epoch_secs: i64) -> Option<String> {
    let data: Value = serde_json::from_str(raw.trim()).ok()?;
    let obj = data.as_object()?;
    let token_obj = obj.get("token").and_then(Value::as_object);
    let access = token_obj
        .and_then(|t| t.get("access_token").or_else(|| t.get("accessToken")))
        .or_else(|| obj.get("access_token").or_else(|| obj.get("access")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    let expiry = token_obj
        .and_then(|t| t.get("expiry").or_else(|| t.get("expiresAt")))
        .or_else(|| obj.get("expiry").or_else(|| obj.get("expires")))
        .and_then(parse_epoch_value);

    if let Some(exp) = expiry {
        if exp <= now_epoch_secs {
            return None;
        }
    }
    Some(access.to_string())
}

pub fn extract_tier(load_code_assist: &Value) -> String {
    for key in ["paidTier", "currentTier"] {
        let Some(tier) = load_code_assist.get(key).and_then(Value::as_object) else {
            continue;
        };
        let name = tier.get("name").and_then(Value::as_str).unwrap_or("").trim();
        let id = tier.get("id").and_then(Value::as_str).unwrap_or("").trim();
        if name.is_empty() && id.is_empty() {
            continue;
        }
        let name_lower = name.to_ascii_lowercase();
        let id_lower = id.to_ascii_lowercase();
        for (needle, label) in [("ultra", "Ultra"), ("pro", "Pro"), ("free", "Free")] {
            if id_lower.contains(needle) || name_lower.contains(needle) {
                return label.to_string();
            }
        }
        if name_lower.starts_with("google ai ") {
            return name[10..].trim().to_string();
        }
        if !name.is_empty() {
            return name.to_string();
        }
        return id.trim_end_matches("-tier").to_string();
    }
    String::new()
}

pub fn extract_project(load_code_assist: &Value) -> Option<String> {
    let field = load_code_assist.get("cloudaicompanionProject")?;
    let project = field
        .as_str()
        .or_else(|| field.get("id").and_then(Value::as_str))?
        .trim();
    (!project.is_empty()).then(|| project.to_string())
}

fn bucket_percent_used(bucket: &Value) -> Option<f64> {
    if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let remaining = bucket
        .get("remainingFraction")
        .or_else(|| bucket.get("remaining_fraction"));
    let amount = bucket
        .get("remainingAmount")
        .or_else(|| bucket.get("remaining_amount"));
    if remaining.is_none() || remaining == Some(&Value::Null) {
        if let Some(amt) = amount {
            if amt == 0 || amt.as_str() == Some("0") {
                return Some(1.0);
            }
            return None;
        }
        if !bucket_reset_time(bucket).is_empty() {
            return Some(1.0);
        }
        return None;
    }
    let rem = remaining?.as_f64().filter(|f| f.is_finite())?;
    let used = (1.0 - rem).clamp(0.0, 1.0);
    Some((used * 10_000.0).round() / 10_000.0)
}

fn bucket_reset_time(bucket: &Value) -> String {
    let Some(raw) = bucket.get("resetTime").or_else(|| bucket.get("reset_time")) else {
        return String::new();
    };
    parse_iso_normalized(raw)
}

fn window_order_and_label(bucket: &Value) -> (u8, String) {
    let window = bucket
        .get("window")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let bucket_id = bucket
        .get("bucketId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let name = bucket
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let name_lower = name.to_ascii_lowercase();
    if window == "5h" || bucket_id.contains("5h") || name_lower.contains("five") || name_lower.contains("5h") {
        (0, "5-hour".to_string())
    } else if window == "weekly" || bucket_id.contains("weekly") || name_lower.contains("week") {
        (1, "Weekly".to_string())
    } else {
        let trimmed = name
            .trim_end_matches(" Limit Remaining")
            .trim_end_matches(" Remaining")
            .trim();
        (2, trimmed.to_string())
    }
}

/// Parse `v1internal:retrieveUserQuotaSummary`, keeping both touched and untouched
/// (100% remaining) quota groups. Returns `(limits, has_gemini_group)`.
pub fn parse_quota_summary(summary: &Value) -> (Vec<LiveLimit>, bool) {
    let Some(obj) = summary.as_object() else {
        return (Vec::new(), false);
    };
    if obj.contains_key("error") {
        return (Vec::new(), false);
    }
    let mut groups: Vec<(bool, String, &[Value])> = Vec::new();
    if let Some(arr) = obj.get("groups").and_then(Value::as_array) {
        for g in arr {
            let Some(g_obj) = g.as_object() else {
                continue;
            };
            let display = g_obj.get("displayName").and_then(Value::as_str).unwrap_or("").trim();
            let is_gemini = display.to_ascii_lowercase().contains("gemini");
            let short = if is_gemini {
                "Gemini".to_string()
            } else if display.to_ascii_lowercase().contains("claude") {
                "Claude & GPT".to_string()
            } else if !display.is_empty() {
                display.to_string()
            } else {
                "Models".to_string()
            };
            if let Some(buckets) = g_obj.get("buckets").and_then(Value::as_array) {
                groups.push((is_gemini, short, buckets));
            }
        }
    } else if let Some(buckets) = obj.get("buckets").and_then(Value::as_array) {
        if !buckets.is_empty() {
            groups.push((true, "Gemini".to_string(), buckets));
        }
    }

    let has_gemini = groups.iter().any(|(is_gem, _, _)| *is_gem);
    groups.sort_by_key(|(is_gem, _, _)| !*is_gem);

    let mut out = Vec::new();
    for (_is_gem, group_label, buckets) in groups {
        let mut items: Vec<(u8, LiveLimit)> = Vec::new();
        for bucket in buckets {
            let Some(used) = bucket_percent_used(bucket) else {
                continue;
            };
            let (order, win_label) = window_order_and_label(bucket);
            let label = if win_label.is_empty() {
                group_label.clone()
            } else if group_label.is_empty() {
                win_label
            } else {
                format!("{group_label} ({win_label})")
            };
            items.push((
                order,
                LiveLimit {
                    label,
                    percent_used: used,
                    resets_at: bucket_reset_time(bucket),
                },
            ));
        }
        items.sort_by_key(|(order, _)| *order);
        out.extend(items.into_iter().map(|(_, lim)| lim));
    }
    (out, has_gemini)
}

fn min_reset(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        (false, false) => match (parse_iso_epoch(a), parse_iso_epoch(b)) {
            (Some(ta), Some(tb)) if tb < ta => b.to_string(),
            (Some(_), Some(_)) => a.to_string(),
            _ => a.min(b).to_string(),
        },
    }
}

/// Aggregate per-model rolling buckets from `v1internal:retrieveUserQuota`
/// (or `fetchAvailableModels`), ignoring internal `tab_*` completion previews.
pub fn parse_model_quota_buckets(quota: &Value, include_3p: bool) -> Vec<LiveLimit> {
    let mut entries: Vec<(&str, f64, String)> = Vec::new();
    if let Some(buckets) = quota.get("buckets").and_then(Value::as_array) {
        for b in buckets {
            let Some(model_id) = b.get("modelId").and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            if model_id.is_empty() || model_id.starts_with("tab_") {
                continue;
            }
            if let Some(used) = bucket_percent_used(b) {
                entries.push((model_id, used, bucket_reset_time(b)));
            }
        }
    } else if let Some(models) = quota.get("models").and_then(Value::as_object) {
        for (model_id, info) in models {
            if model_id.starts_with("tab_") {
                continue;
            }
            let Some(qi) = info.get("quotaInfo") else {
                continue;
            };
            if let Some(used) = bucket_percent_used(qi) {
                entries.push((model_id.as_str(), used, bucket_reset_time(qi)));
            }
        }
    }

    let fold_group = |pred: &dyn Fn(&str) -> bool, label: &str| -> Option<LiveLimit> {
        let mut matched: Option<(f64, String)> = None;
        for (id, used, reset) in &entries {
            if !pred(id) {
                continue;
            }
            matched = Some(match matched {
                None => (*used, reset.clone()),
                Some((prev_used, prev_reset)) => {
                    let best_used = prev_used.max(*used);
                    let best_reset = min_reset(&prev_reset, reset);
                    (best_used, best_reset)
                }
            });
        }
        matched.map(|(percent_used, resets_at)| LiveLimit {
            label: label.to_string(),
            percent_used,
            resets_at,
        })
    };

    let mut out = Vec::new();
    if let Some(gem) = fold_group(&|id| id.starts_with("gemini-"), "Gemini") {
        out.push(gem);
    }
    if include_3p {
        if let Some(tp) = fold_group(
            &|id| id.starts_with("claude-") || id.starts_with("gpt-"),
            "Claude & GPT",
        ) {
            out.push(tp);
        }
    }
    out
}

pub fn merge_quota_responses(
    load_assist: &Value,
    summary: Option<&Value>,
    model_quota: Option<&Value>,
    updated_at: String,
    fetched_at_ms: u64,
) -> Option<LiveSnapshot> {
    let tier = extract_tier(load_assist);
    let (mut summary_limits, has_gemini) = summary
        .map(parse_quota_summary)
        .unwrap_or_else(|| (Vec::new(), false));

    let mut limits = Vec::new();
    if !has_gemini {
        if let Some(mq) = model_quota {
            limits.extend(parse_model_quota_buckets(mq, summary_limits.is_empty()));
        }
    }
    limits.append(&mut summary_limits);

    if tier.is_empty() && limits.is_empty() {
        return None;
    }
    Some(LiveSnapshot {
        tier,
        limits,
        updated_at,
        fetched_at_ms,
    })
}

pub fn cache_path() -> PathBuf {
    crate::state::home_dir().join(".local/state/super-desktop/agy-usage.json")
}

pub fn read_cache_file(path: &Path) -> Option<LiveSnapshot> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write_cache_file(path: &Path, snap: &LiveSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        crate::control::ensure_private_dir(parent)?;
    }
    let json = serde_json::to_vec(snap)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nonce = crate::control::random_hex(6)
        .unwrap_or_else(|_| format!("{}", std::process::id()));
    let tmp = path.with_extension(format!("json.{}.{seq}.{nonce}.tmp", std::process::id()));
    let res = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(&json)?;
            f.flush()?;
            fs::rename(&tmp, path)
        });
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn secret_tool_token(now: i64) -> Option<String> {
    let mut child = Command::new("secret-tool")
        .args(["lookup", "service", "gemini", "username", "antigravity"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if started.elapsed() >= Duration::from_secs(3) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let out = child.wait_with_output().ok()?;
    let raw = String::from_utf8(out.stdout).ok()?;
    parse_oauth_token(&raw, now)
}

fn read_local_token() -> Option<String> {
    let now = now_epoch();
    if let Some(tok) = secret_tool_token(now) {
        return Some(tok);
    }
    let agy_dir = std::env::var_os("AGY_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gemini/antigravity-cli")))?;
    let raw = fs::read_to_string(agy_dir.join("antigravity-oauth-token")).ok()?;
    parse_oauth_token(&raw, now)
}

fn validate_cloud_code_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url = reqwest::Url::parse(trimmed).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.port(), None | Some(443))
    {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "cloudcode-pa.googleapis.com" && !host.ends_with(".googleapis.com") {
        return None;
    }
    Some(url.as_str().trim_end_matches('/').to_string())
}

fn cloud_code_base_url() -> String {
    if let Some(raw) = std::env::var_os("CLOUD_CODE_URL").and_then(|v| v.into_string().ok()) {
        if let Some(valid) = validate_cloud_code_url(&raw) {
            return valid;
        }
    }
    DEFAULT_CLOUD_CODE_URL.to_string()
}

fn post_cloud_code(
    http: &reqwest::blocking::Client,
    base: &str,
    endpoint: &str,
    token: &str,
    body: &Value,
) -> Option<Value> {
    let resp = http
        .post(format!("{base}/v1internal:{endpoint}"))
        .bearer_auth(token)
        .header("User-Agent", "antigravity")
        .json(body)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().ok()
}

/// Perform a live, read-only Antigravity quota fetch against Cloud Code.
/// Returns `None` if OAuth credentials are missing/expired or network calls fail.
pub fn fetch_live() -> Option<LiveSnapshot> {
    if cfg!(test) {
        return None;
    }
    let token = read_local_token()?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    let base = cloud_code_base_url();
    let load_assist = post_cloud_code(
        &http,
        &base,
        "loadCodeAssist",
        &token,
        &json!({"metadata": {"ideType": "ANTIGRAVITY"}}),
    )?;
    let project_body = match extract_project(&load_assist) {
        Some(project) => json!({ "project": project }),
        None => json!({}),
    };
    let summary = post_cloud_code(
        &http,
        &base,
        "retrieveUserQuotaSummary",
        &token,
        &project_body,
    );
    let need_models = summary
        .as_ref()
        .map(|s| !parse_quota_summary(s).1)
        .unwrap_or(true);
    let model_quota = if need_models {
        post_cloud_code(&http, &base, "retrieveUserQuota", &token, &project_body)
    } else {
        None
    };
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    merge_quota_responses(
        &load_assist,
        summary.as_ref(),
        model_quota.as_ref(),
        stamp,
        now_ms(),
    )
}

/// Pure memory/disk lookup safe on the GTK thread. Returns `None` in tests.
pub fn cached_snapshot() -> Option<LiveSnapshot> {
    if cfg!(test) {
        return None;
    }
    let mut state = cache_mutex().lock().ok()?;
    if !state.loaded_from_disk {
        state.loaded_from_disk = true;
        if let Some(disk) = read_cache_file(&cache_path()) {
            state.last_attempt_ms = disk.fetched_at_ms;
            state.last_ok = true;
            state.snapshot = Some(disk);
        }
    }
    state.snapshot.clone()
}

fn is_stale_state(state: &CacheState, now: u64) -> bool {
    if state.last_attempt_ms == 0 {
        return true;
    }
    let ttl = if state.last_ok { CACHE_TTL_MS } else { ERROR_BACKOFF_MS };
    now.saturating_sub(state.last_attempt_ms) >= ttl
}

/// Synchronously refreshes the live cache when stale and not already in flight.
/// Returns `true` if a new snapshot was fetched and stored.
pub fn refresh_if_stale() -> bool {
    if cfg!(test) {
        return false;
    }
    let now = now_ms();
    {
        let Ok(mut state) = cache_mutex().lock() else {
            return false;
        };
        if !state.loaded_from_disk {
            state.loaded_from_disk = true;
            if let Some(disk) = read_cache_file(&cache_path()) {
                state.last_attempt_ms = disk.fetched_at_ms;
                state.last_ok = true;
                state.snapshot = Some(disk);
            }
        }
        if state.in_flight || !is_stale_state(&state, now) {
            return false;
        }
        state.in_flight = true;
    }

    let fetched = fetch_live();
    let finish_ms = now_ms();
    let updated = if let Some(ref snap) = fetched {
        let _ = write_cache_file(&cache_path(), snap);
        true
    } else {
        false
    };
    if let Ok(mut state) = cache_mutex().lock() {
        state.in_flight = false;
        state.last_attempt_ms = finish_ms;
        if let Some(snap) = fetched {
            state.last_ok = true;
            state.snapshot = Some(snap);
        } else {
            state.last_ok = false;
        }
    }
    updated
}

/// Trigger a non-blocking background refresh if stale and not already in flight.
pub fn refresh_async() {
    if cfg!(test) {
        return;
    }
    let now = now_ms();
    let should_spawn = {
        let Ok(mut state) = cache_mutex().lock() else {
            return;
        };
        if !state.loaded_from_disk {
            state.loaded_from_disk = true;
            if let Some(disk) = read_cache_file(&cache_path()) {
                state.last_attempt_ms = disk.fetched_at_ms;
                state.last_ok = true;
                state.snapshot = Some(disk);
            }
        }
        if state.in_flight || !is_stale_state(&state, now) {
            return;
        }
        state.in_flight = true;
        true
    };
    if should_spawn {
        std::thread::spawn(|| {
            let fetched = fetch_live();
            let finish_ms = now_ms();
            if let Some(ref snap) = fetched {
                let _ = write_cache_file(&cache_path(), snap);
            }
            if let Ok(mut state) = cache_mutex().lock() {
                state.in_flight = false;
                state.last_attempt_ms = finish_ms;
                if let Some(snap) = fetched {
                    state.last_ok = true;
                    state.snapshot = Some(snap);
                } else {
                    state.last_ok = false;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_token_handles_nested_and_flat_and_rejects_expired() {
        let valid_nested = r#"{"token":{"access_token":"tok-1","expiry":"2026-11-24T12:00:00Z"}}"#;
        assert_eq!(parse_oauth_token(valid_nested, 1_795_518_000).as_deref(), Some("tok-1"));
        assert_eq!(parse_oauth_token(valid_nested, 1_795_521_600), None);

        let valid_flat = r#"{"access_token":"tok-2","expiry":1795525000}"#;
        assert_eq!(parse_oauth_token(valid_flat, 1_795_520_000).as_deref(), Some("tok-2"));
        assert_eq!(parse_oauth_token(r#"{"access_token":""}"#, 0), None);
    }

    #[test]
    fn ultra_plan_merges_gemini_rolling_pool_and_claude_gpt_windows() {
        let load_assist = json!({
            "paidTier": {"id": "g1-ultra-lite-tier", "name": "Google AI Ultra"},
            "currentTier": {"id": "free-tier", "name": "Antigravity"},
            "cloudaicompanionProject": "aicode-consumers"
        });
        // Real retrieveUserQuotaSummary response on Ultra: weekly listed before 5h, no Gemini group.
        let summary = json!({
            "groups": [{
                "displayName": "Claude and GPT models",
                "buckets": [
                    {
                        "bucketId": "3p-weekly",
                        "displayName": "Weekly Limit Remaining",
                        "window": "weekly",
                        "resetTime": "2026-11-30T14:29:48Z",
                        "remainingFraction": 1
                    },
                    {
                        "bucketId": "3p-5h",
                        "displayName": "Five Hour Limit Remaining",
                        "window": "5h",
                        "resetTime": "2026-11-24T12:59:32Z",
                        "remainingFraction": 1
                    }
                ]
            }]
        });
        let model_quota = json!({
            "buckets": [
                {"modelId": "claude-opus-5-5-high", "remainingFraction": 1, "resetTime": "2026-11-24T16:17:55Z"},
                {"modelId": "gemini-2.5-pro", "remainingFraction": 0.55, "resetTime": "2026-11-24T14:16:10Z"},
                {"modelId": "gemini-3-flash", "remainingFraction": 0.55, "resetTime": "2026-11-24T14:16:10Z"},
                {"modelId": "tab_flash_lite_preview", "remainingFraction": 0.0}
            ]
        });

        let snap = merge_quota_responses(
            &load_assist,
            Some(&summary),
            Some(&model_quota),
            "2026-11-24T11:20:00Z".into(),
            1_795_519_200_000,
        )
        .unwrap();

        assert_eq!(snap.tier, "Ultra");
        assert_eq!(snap.limits.len(), 3);
        assert_eq!(snap.limits[0].label, "Gemini");
        assert!((snap.limits[0].percent_used - 0.45).abs() < 1e-6);
        assert_eq!(snap.limits[0].resets_at, "2026-11-24T14:16:10Z");
        // Untouched 100%-left 3P buckets preserved in 5h -> Weekly order.
        assert_eq!(snap.limits[1].label, "Claude & GPT (5-hour)");
        assert_eq!(snap.limits[1].percent_used, 0.0);
        assert_eq!(snap.limits[2].label, "Claude & GPT (Weekly)");
        assert_eq!(snap.limits[2].percent_used, 0.0);
    }

    #[test]
    fn pro_plan_summary_with_gemini_group_keeps_all_windows_ordered() {
        let load_assist = json!({
            "paidTier": {"id": "g1-pro-tier", "name": "Google AI Pro"}
        });
        let summary = json!({
            "groups": [
                {
                    "displayName": "Claude and GPT models",
                    "buckets": [
                        {"bucketId": "3p-weekly", "window": "weekly", "remainingAmount": 0, "resetTime": "2026-12-01T00:00:00Z"}
                    ]
                },
                {
                    "displayName": "Gemini",
                    "buckets": [
                        {"bucketId": "gem-weekly", "window": "weekly", "remainingFraction": 0.8},
                        {"bucketId": "gem-5h", "window": "5h", "remainingFraction": 0.7},
                        {"bucketId": "gem-disabled", "window": "5h", "disabled": true, "remainingFraction": 0.0}
                    ]
                }
            ]
        });
        let snap = merge_quota_responses(&load_assist, Some(&summary), None, "2026-11-24T11:20:00Z".into(), 1).unwrap();
        assert_eq!(snap.tier, "Pro");
        let labels: Vec<_> = snap.limits.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, vec!["Gemini (5-hour)", "Gemini (Weekly)", "Claude & GPT (Weekly)"]);
        assert!((snap.limits[0].percent_used - 0.3).abs() < 1e-6);
        assert!((snap.limits[1].percent_used - 0.2).abs() < 1e-6);
        assert_eq!(snap.limits[2].percent_used, 1.0);
    }

    #[test]
    fn cache_round_trip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("sd-agy-usage-{}", std::process::id()));
        let path = dir.join("agy-usage.json");
        let snap = LiveSnapshot {
            tier: "Ultra".into(),
            limits: vec![LiveLimit {
                label: "Gemini".into(),
                percent_used: 0.45,
                resets_at: "2026-11-24T14:16:10Z".into(),
            }],
            updated_at: "2026-11-24T11:20:00Z".into(),
            fetched_at_ms: 1_795_519_200_000,
        };
        write_cache_file(&path, &snap).unwrap();
        assert_eq!(read_cache_file(&path), Some(snap));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_code_url_allowlist_blocks_untrusted_endpoints() {
        assert_eq!(
            validate_cloud_code_url("https://cloudcode-pa.googleapis.com/"),
            Some("https://cloudcode-pa.googleapis.com".into())
        );
        for bad in [
            "http://cloudcode-pa.googleapis.com",
            "https://evil.example",
            "https://user:pass@cloudcode-pa.googleapis.com",
            "https://cloudcode-pa.googleapis.com?next=evil",
            "https://cloudcode-pa.googleapis.com:8443",
        ] {
            assert_eq!(validate_cloud_code_url(bad), None, "{bad}");
        }
    }
}
