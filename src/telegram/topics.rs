//! Forum Topics synchronization (1 card = 1 Telegram Forum Topic), live Dashboard,
//! live screen streaming, and shared keyboards.
use super::api::{ApiError, InlineButton, InlineKeyboardMarkup, TelegramApi};
use super::config::{Config, LiveStream, Permissions, RuntimeState, TopicBinding, MAX_TOPICS};
use super::desktop::{CardSummary, DesktopBackend, ProviderUsageLimit, ProviderUsageSummary};
use super::redact::{
    escape_html, sanitize_terminal_text, status_emoji, strip_legacy_status_prefix,
    tag_to_topic_color, topic_title, truncate_chars,
};
use std::collections::HashMap;

pub const DASHBOARD_MIN_INTERVAL_SECS: u64 = 6;
pub const LIVE_EDIT_INTERVAL_SECS: u64 = 4;
pub const LIVE_IDLE_STOP_SECS: u64 = 20;
pub const LIVE_MAX_DURATION_SECS: u64 = 600;
pub const TYPING_ACTION_INTERVAL_SECS: u64 = 4;

pub fn quick_slash_commands(agent_type: &str) -> &'static [&'static str] {
    match agent_type {
        "agy" | "antigravity" => &["/resume", "/compact", "/clear"],
        "grok" => &["/minimal", "/clear"],
        _ => &["/compact", "/clear", "/status"],
    }
}

pub fn card_action_keyboard(card: &CardSummary, perms: Permissions) -> Option<InlineKeyboardMarkup> {
    card_action_keyboard_with_app(card, perms, None)
}

pub fn card_action_keyboard_with_app(
    card: &CardSummary,
    perms: Permissions,
    mini_app_url: Option<&str>,
) -> Option<InlineKeyboardMarkup> {
    let mut row1 = Vec::new();
    if perms.read_output {
        row1.push(InlineButton::callback("📷 Screen", format!("scr:{}", card.id)));
        row1.push(InlineButton::callback("📜 History", format!("his:{}", card.id)));
        row1.push(InlineButton::callback("📺 Live", format!("liv:{}", card.id)));
        row1.push(InlineButton::callback("📎 Files", format!("fil:{}", card.id)));
    }
    let can_control = perms.controls && (!card.is_shell || perms.shell_input);
    let mut row2 = Vec::new();
    if can_control {
        row2.push(InlineButton::callback("⌨ Keys", format!("kyp:{}", card.id)));
        row2.push(InlineButton::callback("⏎ Enter", format!("key:e:{}", card.id)));
        row2.push(InlineButton::callback("⛔ Ctrl-C", format!("key:c:{}", card.id)));
    }
    if perms.close {
        row2.push(InlineButton::callback("🗑 Close", format!("cls:{}", card.id)));
    }
    let mut rows = Vec::new();
    if !row1.is_empty() {
        rows.push(row1);
    }
    if !row2.is_empty() {
        rows.push(row2);
    }
    if let Some(url) = mini_app_url {
        rows.push(vec![InlineButton::url("📱 Open App", url)]);
    }
    (!rows.is_empty()).then_some(InlineKeyboardMarkup {
        inline_keyboard: rows,
    })
}

pub fn waiting_keyboard(card: &CardSummary, perms: Permissions) -> Option<InlineKeyboardMarkup> {
    let mut rows = Vec::new();
    let can_control = perms.controls && (!card.is_shell || perms.shell_input);
    if can_control {
        rows.push(vec![
            InlineButton::callback("Yes", format!("key:y:{}", card.id)),
            InlineButton::callback("No", format!("key:n:{}", card.id)),
            InlineButton::callback("1", format!("key:1:{}", card.id)),
            InlineButton::callback("2", format!("key:2:{}", card.id)),
        ]);
        rows.push(vec![
            InlineButton::callback("⏎ Enter", format!("key:e:{}", card.id)),
            InlineButton::callback("Esc", format!("key:x:{}", card.id)),
            InlineButton::callback("⛔ Ctrl-C", format!("key:c:{}", card.id)),
        ]);
    }
    if perms.read_output {
        rows.push(vec![
            InlineButton::callback("📷 Screen", format!("scr:{}", card.id)),
            InlineButton::callback("📎 Files", format!("fil:{}", card.id)),
        ]);
    }
    (!rows.is_empty()).then_some(InlineKeyboardMarkup {
        inline_keyboard: rows,
    })
}

pub fn quick_keys_keyboard(card: &CardSummary, perms: Permissions) -> Option<InlineKeyboardMarkup> {
    let mut rows = Vec::new();
    let can_control = perms.controls && (!card.is_shell || perms.shell_input);
    if can_control {
        rows.push(vec![
            InlineButton::callback("Yes", format!("key:y:{}", card.id)),
            InlineButton::callback("No", format!("key:n:{}", card.id)),
            InlineButton::callback("1", format!("key:1:{}", card.id)),
            InlineButton::callback("2", format!("key:2:{}", card.id)),
            InlineButton::callback("3", format!("key:3:{}", card.id)),
        ]);
        rows.push(vec![
            InlineButton::callback("↑", format!("key:u:{}", card.id)),
            InlineButton::callback("↓", format!("key:d:{}", card.id)),
            InlineButton::callback("Tab", format!("key:t:{}", card.id)),
            InlineButton::callback("⇧Tab", format!("key:b:{}", card.id)),
        ]);
        rows.push(vec![
            InlineButton::callback("⏎ Enter", format!("key:e:{}", card.id)),
            InlineButton::callback("Esc", format!("key:x:{}", card.id)),
            InlineButton::callback("⛔ Ctrl-C", format!("key:c:{}", card.id)),
        ]);
    }
    if !card.is_shell && perms.prompts {
        let slash_row: Vec<InlineButton> = quick_slash_commands(&card.agent_type)
            .iter()
            .enumerate()
            .map(|(idx, cmd)| InlineButton::callback(*cmd, format!("qc:{idx}:{}", card.id)))
            .collect();
        if !slash_row.is_empty() {
            rows.push(slash_row);
        }
    }
    (!rows.is_empty()).then_some(InlineKeyboardMarkup {
        inline_keyboard: rows,
    })
}

pub fn format_card_header_html(card: &CardSummary) -> String {
    let dot = status_emoji(&card.status);
    let mut out = format!(
        "{dot} <b>{} {}</b> · <code>{}</code>\nStatus: <b>{}</b>",
        escape_html(&card.icon),
        escape_html(&card.agent_name),
        escape_html(&card.directory_display),
        escape_html(&card.label)
    );
    if let Some(model) = card.model.as_deref().filter(|m| !m.is_empty()) {
        out.push_str(&format!(" · {}", escape_html(model)));
        if let Some(effort) = card.effort.as_deref().filter(|e| !e.is_empty()) {
            out.push_str(&format!(" ({})", escape_html(effort)));
        }
    }
    if let Some(title) = card.title.as_deref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("\nPrompt: <i>{}</i>", escape_html(title)));
    }
    out
}

pub fn format_topic_header_html(card: &CardSummary) -> String {
    format!(
        "{}\n\n<i>Send messages here to talk directly to this card.</i>",
        format_card_header_html(card)
    )
}

pub fn topic_header_keyboard_with_app(
    card: &CardSummary,
    perms: Permissions,
    mini_app_url: Option<&str>,
) -> Option<InlineKeyboardMarkup> {
    if card.status == "WAITING" {
        let mut kb = waiting_keyboard(card, perms);
        if let Some(url) = mini_app_url {
            match &mut kb {
                Some(m) => m.inline_keyboard.push(vec![InlineButton::url("📱 Open App", url)]),
                None => {
                    kb = Some(InlineKeyboardMarkup {
                        inline_keyboard: vec![vec![InlineButton::url("📱 Open App", url)]],
                    });
                }
            }
        }
        kb
    } else {
        card_action_keyboard_with_app(card, perms, mini_app_url)
    }
}

fn status_card_fingerprint(html: &str, kb: Option<&InlineKeyboardMarkup>) -> String {
    format!("{html}\0{}", serde_json::to_string(&kb).unwrap_or_default())
}

fn format_closed_topic_header_html(topic_name: &str) -> String {
    format!(
        "🛑 <b>{}</b>\nStatus: <b>Closed on PC</b>",
        escape_html(topic_name)
    )
}

fn clamp_tail_chars(joined: String, max_chars: usize) -> String {
    let limit = max_chars.max(1);
    if joined.chars().count() <= limit {
        return joined;
    }
    let keep = limit.saturating_sub(1).max(1);
    let byte_start = joined
        .char_indices()
        .rev()
        .nth(keep.saturating_sub(1))
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    format!("…{}", &joined[byte_start..])
}

/// Sanitize raw terminal text and keep its trailing lines bounded cleanly.
pub fn tail_output_snippet(
    raw: &str,
    bot_token: &str,
    redact_secrets: bool,
    max_lines: usize,
    max_chars: usize,
) -> Option<String> {
    let clean = sanitize_terminal_text(raw, bot_token, redact_secrets);
    let mut lines: Vec<&str> = clean.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(max_lines.max(1));
    Some(clamp_tail_chars(lines[start..].join("\n"), max_chars))
}

fn collapse_blank_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev_blank = true;
    for raw in lines {
        let trimmed = raw.trim_end();
        if trimmed.trim().is_empty() {
            if !prev_blank {
                out.push(String::new());
                prev_blank = true;
            }
        } else {
            out.push(trimmed.to_string());
            prev_blank = false;
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// Detect active working/streaming TUI banners in the bottom of the screen so
/// heuristic IDLE fallback notifications wait until work truly settles.
pub fn screen_shows_active_work(raw: &str) -> bool {
    let clean = sanitize_terminal_text(raw, "", false);
    clean
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(8)
        .any(|line| {
            if line.chars().next().is_some_and(|ch| {
                matches!(
                    ch,
                    '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏'
                )
            }) {
                return true;
            }
            let lower = line.to_ascii_lowercase();
            [
                "esc to interrupt",
                "ctrl+c to cancel",
                "ctrl+c to interrupt",
                "ctrl-c to cancel",
                "working…",
                "working...",
                "thinking…",
                "thinking...",
                "responding…",
                "responding...",
                "generating…",
                "generating...",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
        })
}

/// Sanitize and format a structured final assistant reply from the top, keeping
/// a concise readable summary bounded by lines and characters.
pub fn format_assistant_done_summary(
    raw: &str,
    bot_token: &str,
    redact_secrets: bool,
    max_lines: usize,
    max_chars: usize,
) -> Option<String> {
    let clean = sanitize_terminal_text(raw, bot_token, redact_secrets);
    let mut normalized = collapse_blank_lines(clean.lines());
    if normalized.is_empty() {
        return None;
    }
    let line_limit = max_lines.max(1);
    let cut_by_lines = normalized.len() > line_limit;
    if cut_by_lines {
        normalized.truncate(line_limit);
        while normalized.last().is_some_and(|l| l.is_empty()) {
            normalized.pop();
        }
    }
    let mut joined = normalized.join("\n");
    let char_limit = max_chars.max(1);
    if cut_by_lines && joined.chars().count() < char_limit {
        joined.push('…');
    }
    let trimmed = if joined.chars().count() > char_limit {
        truncate_chars(&joined, char_limit)
    } else {
        joined
    };
    (!trimmed.trim().is_empty()).then_some(trimmed)
}

fn is_tui_border_line(trimmed: &str) -> bool {
    trimmed.is_empty()
        || trimmed.chars().all(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '─' | '━'
                        | '═'
                        | '┄'
                        | '┈'
                        | '┌'
                        | '┐'
                        | '└'
                        | '┘'
                        | '╭'
                        | '╮'
                        | '╰'
                        | '╯'
                        | '│'
                        | '┃'
                        | '├'
                        | '┤'
                        | '┬'
                        | '┴'
                        | '┼'
                        | '-'
                        | '='
                        | '_'
                        | '*'
                )
        })
}

fn is_tui_prompt_prefix(trimmed: &str) -> bool {
    trimmed.starts_with('›') || trimmed.starts_with('❯')
}

fn is_tui_footer_chrome(trimmed: &str) -> bool {
    if is_tui_border_line(trimmed) || is_tui_prompt_prefix(trimmed) || trimmed == ">" {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if [
        "for shortcuts",
        "for agents",
        "bypass permissions",
        "shift+tab to cycle",
        "update available",
        "turn off recaps",
        "context left",
        "auto-accept",
        "ctrl+c to",
        "? for help",
        "esc to interrupt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }
    if lower.contains("% left") && lower.contains("resets ") {
        return true;
    }
    (trimmed.contains(" · ~/") || trimmed.contains(" · /home/"))
        && !trimmed.starts_with('•')
        && !trimmed.starts_with('-')
}

/// Extract the current turn's output from raw terminal capture, stripping bottom
/// TUI prompt boxes, keybinding footers, and model status bars.
pub fn extract_done_summary(
    raw: &str,
    bot_token: &str,
    redact_secrets: bool,
    max_lines: usize,
    max_chars: usize,
) -> Option<String> {
    let clean = sanitize_terminal_text(raw, bot_token, redact_secrets);
    let mut lines: Vec<&str> = clean.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|l| is_tui_footer_chrome(l.trim())) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    if let Some(prompt_idx) = lines.iter().rposition(|l| is_tui_prompt_prefix(l.trim())) {
        let after = &lines[prompt_idx + 1..];
        if after.iter().any(|l| !l.trim().is_empty()) {
            lines = after.to_vec();
        }
    }
    let normalized = collapse_blank_lines(lines);
    if normalized.is_empty() {
        return None;
    }
    let start = normalized.len().saturating_sub(max_lines.max(1));
    let out = clamp_tail_chars(normalized[start..].join("\n"), max_chars);
    (!out.trim().is_empty()).then_some(out)
}

pub fn render_progress_bar(percent_used: f64, width: usize) -> String {
    let width = width.max(1);
    let clamped = percent_used.clamp(0.0, 1.0);
    let mut filled = (clamped * width as f64).round() as usize;
    if clamped >= 0.995 {
        filled = width;
    } else if filled >= width && width > 1 {
        filled = width - 1;
    }
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn usage_level_emoji(percent_used: f64) -> &'static str {
    let left = 1.0 - percent_used.clamp(0.0, 1.0);
    if left < 0.05 {
        "🔴"
    } else if left < 0.20 {
        "🟡"
    } else {
        "🟢"
    }
}

fn ordered_limits(limits: &[ProviderUsageLimit]) -> Vec<&ProviderUsageLimit> {
    let mut out: Vec<&ProviderUsageLimit> = limits.iter().filter(|l| l.is_weekly()).collect();
    out.extend(limits.iter().filter(|l| !l.is_weekly()));
    out
}

fn format_limit_progress_line(lim: &ProviderUsageLimit) -> String {
    let bar = render_progress_bar(lim.percent_used, 10);
    let used_pct = lim.used_percent_int();
    let left_pct = lim.left_percent_int();
    let counts = match (lim.used, lim.allowance) {
        (Some(used), Some(allow)) if allow > 0 => format!(" ({used}/{allow})"),
        _ => String::new(),
    };
    let reset_txt = crate::usage::format_reset_in(&lim.resets_at);
    let reset_part = if reset_txt.is_empty() {
        String::new()
    } else {
        format!(" · {}", escape_html(&reset_txt))
    };
    format!("<code>{bar}</code> <b>{used_pct}%</b> used{counts} · {left_pct}% left{reset_part}")
}

fn plural_suffix(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn format_activity_line(p: &ProviderUsageSummary) -> Option<String> {
    if p.today_tokens > 0 || p.today_prompts > 0 || p.today_sessions > 0 {
        let mut parts = Vec::new();
        if p.today_tokens > 0 {
            parts.push(format!("{} tokens", crate::usage::format_tokens(p.today_tokens)));
        }
        if p.today_prompts > 0 {
            parts.push(format!("{} prompt{}", p.today_prompts, plural_suffix(p.today_prompts)));
        }
        if p.today_sessions > 0 {
            parts.push(format!("{} session{}", p.today_sessions, plural_suffix(p.today_sessions)));
        }
        Some(format!("Today: {}", parts.join(" · ")))
    } else if p.total_prompts > 0 || p.total_sessions > 0 {
        let mut parts = Vec::new();
        if p.total_prompts > 0 {
            parts.push(format!("{} prompt{}", p.total_prompts, plural_suffix(p.total_prompts)));
        }
        if p.total_sessions > 0 {
            parts.push(format!("{} session{}", p.total_sessions, plural_suffix(p.total_sessions)));
        }
        Some(format!("Lifetime: {}", parts.join(" · ")))
    } else {
        None
    }
}

pub fn format_usage_html(usage: &[ProviderUsageSummary]) -> String {
    if usage.is_empty() {
        return "📊 <b>Provider Usage</b>\n\n<i>No provider usage data cached yet.</i>".to_string();
    }
    let mut out = String::from("📊 <b>Provider Usage &amp; Weekly Allowance</b>");
    for p in usage.iter().take(12) {
        out.push_str(&format!("\n\n<b>{}</b>", escape_html(&p.name)));
        if !p.tier.is_empty() {
            out.push_str(&format!(" <i>({})</i>", escape_html(&p.tier)));
        }
        if !p.limits.is_empty() {
            for lim in ordered_limits(&p.limits) {
                let dot = usage_level_emoji(lim.percent_used);
                out.push_str(&format!(
                    "\n{dot} <b>{}</b>\n{}",
                    escape_html(&lim.label),
                    format_limit_progress_line(lim)
                ));
            }
        } else {
            let status = if p.status_text.is_empty() {
                "No weekly allowance reported"
            } else {
                p.status_text.as_str()
            };
            out.push_str(&format!("\n⚪ <i>{}</i>", escape_html(status)));
        }
        if let Some(act) = format_activity_line(p) {
            out.push_str(&format!("\n<i>{}</i>", escape_html(&act)));
        }
    }
    if usage.len() > 12 {
        out.push_str(&format!(
            "\n\n<i>…and {} more provider(s).</i>",
            usage.len() - 12
        ));
    }
    out
}

pub fn usage_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineButton::callback("🔄 Refresh", "usg:0")]],
    }
}

pub fn format_dashboard_html(cards: &[CardSummary], usage: &[ProviderUsageSummary]) -> String {
    let mut out = format!("⚡ <b>SUPER DESKTOP</b> · <b>{}</b> active card(s)", cards.len());
    if cards.is_empty() {
        out.push_str("\n\n<i>No running terminal cards. Use /new to launch one.</i>");
    } else {
        out.push('\n');
        for card in cards.iter().take(20) {
            let dot = status_emoji(&card.status);
            let model_part = card
                .model
                .as_deref()
                .filter(|m| !m.is_empty())
                .map(|m| format!(" · {}", escape_html(&truncate_chars(m, 24))))
                .unwrap_or_default();
            let prompt_part = card
                .title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map(|t| format!("\n   <i>{}</i>", escape_html(&truncate_chars(t, 44))))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n{dot} {} <b>{}</b> · <code>{}</code> · <b>{}</b>{}{}",
                escape_html(&card.icon),
                escape_html(&card.agent_name),
                escape_html(&card.directory_display),
                escape_html(&card.label),
                model_part,
                prompt_part
            ));
        }
        if cards.len() > 20 {
            out.push_str(&format!("\n<i>…and {} more card(s).</i>", cards.len() - 20));
        }
    }

    let dash_usage: Vec<&ProviderUsageSummary> = usage
        .iter()
        .filter(|p| !p.limits.is_empty() || p.today_tokens > 0)
        .take(6)
        .collect();
    if !dash_usage.is_empty() {
        out.push_str("\n\n<b>Usage:</b>");
        for provider in dash_usage {
            let ordered = ordered_limits(&provider.limits);
            if let Some(lim) = ordered.first() {
                let dot = usage_level_emoji(lim.percent_used);
                let bar = render_progress_bar(lim.percent_used, 8);
                let used_pct = lim.used_percent_int();
                let left_pct = lim.left_percent_int();
                let reset_txt = crate::usage::format_reset_in(&lim.resets_at);
                let reset_part = if reset_txt.is_empty() {
                    String::new()
                } else {
                    format!(", {}", escape_html(&reset_txt))
                };
                out.push_str(&format!(
                    "\n• <b>{}</b>: {dot} <code>{bar}</code> <b>{used_pct}%</b> used ({left_pct}% left{reset_part}) · {}",
                    escape_html(&provider.name),
                    escape_html(&lim.label)
                ));
            } else {
                out.push_str(&format!(
                    "\n• <b>{}</b>: {} tokens today",
                    escape_html(&provider.name),
                    escape_html(&crate::usage::format_tokens(provider.today_tokens))
                ));
            }
        }
    }
    out
}

pub fn dashboard_keyboard(
    perms: Permissions,
    has_forum: bool,
    mini_app_url: Option<&str>,
) -> InlineKeyboardMarkup {
    let mut row = Vec::new();
    if perms.launch {
        row.push(InlineButton::callback("➕ New", "nwo:0"));
    }
    if has_forum {
        row.push(InlineButton::callback("🔄 Sync", "syn:0"));
    }
    row.push(InlineButton::callback("🔄 Refresh", "dsh:0"));
    let mut rows = vec![row];
    if let Some(url) = mini_app_url {
        rows.push(vec![InlineButton::url("📱 Open App", url)]);
    }
    InlineKeyboardMarkup {
        inline_keyboard: rows,
    }
}

pub fn sync_dashboard(
    api: &dyn TelegramApi,
    cfg: &Config,
    runtime: &mut RuntimeState,
    cards: &[CardSummary],
    usage: &[ProviderUsageSummary],
    now_unix: u64,
    force: bool,
) -> bool {
    let Some(forum) = cfg.forum.as_ref() else {
        return false;
    };
    let html = format_dashboard_html(cards, usage);
    let app_url = cfg
        .mini_app
        .dashboard_deep_link(cfg.bot_username.as_deref());
    let kb = dashboard_keyboard(cfg.permissions, true, app_url.as_deref());
    let fp = status_card_fingerprint(&html, Some(&kb));
    let mut changed = false;

    match runtime.dashboard_message_id {
        Some(msg_id) => {
            if !runtime.dashboard_pinned {
                match api.pin_chat_message(forum.chat_id, msg_id, true) {
                    Ok(()) | Err(ApiError::BadRequest(_)) => {
                        runtime.dashboard_pinned = true;
                        changed = true;
                    }
                    Err(ApiError::MessageNotFound) => {
                        runtime.dashboard_message_id = None;
                        runtime.dashboard_pinned = false;
                        runtime.last_dashboard_card = Some(String::new());
                        changed = true;
                    }
                    Err(_) => {}
                }
            }
            if let Some(msg_id) = runtime.dashboard_message_id {
                let due = force
                    || now_unix.saturating_sub(runtime.last_dashboard_edit_at)
                        >= DASHBOARD_MIN_INTERVAL_SECS;
                if due && runtime.last_dashboard_card.as_deref() != Some(fp.as_str()) {
                    match api.edit_message_text(forum.chat_id, msg_id, &html, Some(&kb)) {
                        Ok(()) => {
                            runtime.last_dashboard_card = Some(fp);
                            runtime.last_dashboard_edit_at = now_unix;
                            changed = true;
                        }
                        Err(ApiError::MessageNotFound) => {
                            runtime.dashboard_message_id = None;
                            runtime.dashboard_pinned = false;
                            runtime.last_dashboard_card = Some(String::new());
                            changed = true;
                        }
                        Err(ApiError::RateLimited { retry_after }) => {
                            runtime.last_dashboard_edit_at =
                                now_unix.saturating_add(retry_after.max(5));
                            changed = true;
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        None => {
            let should_create = runtime.last_dashboard_card.is_none() || force;
            if should_create {
                if let Ok(msg_id) = api.send_message(forum.chat_id, None, &html, None, Some(&kb)) {
                    let _ = api.pin_chat_message(forum.chat_id, msg_id, true);
                    runtime.dashboard_message_id = Some(msg_id);
                    runtime.dashboard_pinned = true;
                    runtime.last_dashboard_card = Some(fp);
                    runtime.last_dashboard_edit_at = now_unix;
                    changed = true;
                }
            }
        }
    }

    changed
}

pub fn format_live_message_html(card: &CardSummary, snippet: &str) -> String {
    let dot = status_emoji(&card.status);
    format!(
        "📺 <b>Live</b> · {dot} {} <b>{}</b> · <b>{}</b>\n<pre><code>{}</code></pre>",
        escape_html(&card.icon),
        escape_html(&card.agent_name),
        escape_html(&card.label),
        escape_html(snippet)
    )
}

pub fn live_stream_keyboard(card: &CardSummary, perms: Permissions) -> InlineKeyboardMarkup {
    let mut row = vec![InlineButton::callback("⏸ Stop live", format!("lst:{}", card.id))];
    let can_control = perms.controls && (!card.is_shell || perms.shell_input);
    if can_control {
        row.push(InlineButton::callback("⛔ Ctrl-C", format!("key:c:{}", card.id)));
    }
    InlineKeyboardMarkup {
        inline_keyboard: vec![row],
    }
}

pub fn tick_live_streams(
    api: &dyn TelegramApi,
    backend: &dyn DesktopBackend,
    cfg: &Config,
    runtime: &mut RuntimeState,
    cards: &[CardSummary],
    now_unix: u64,
) -> bool {
    if runtime.live_streams.is_empty() {
        return false;
    }
    let mut changed = false;

    if !cfg.permissions.read_output {
        for stream in runtime.live_streams.drain(..) {
            let kb = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineButton::callback(
                    "📷 Screen",
                    format!("scr:{}", stream.card_id),
                )]],
            };
            let _ = api.edit_message_text(
                stream.chat_id,
                stream.message_id,
                "📺 <b>Live stopped</b> (<i>Read terminal output</i> disabled in Settings).",
                Some(&kb),
            );
        }
        return true;
    }

    let mut idx = 0usize;
    while idx < runtime.live_streams.len() {
        let stream: LiveStream = runtime.live_streams[idx].clone();
        let card_opt = cards.iter().find(|c| c.id == stream.card_id);
        let Some(card) = card_opt else {
            let _ = api.edit_message_text(
                stream.chat_id,
                stream.message_id,
                "📺 <b>Live ended</b> · Card closed on PC.",
                None,
            );
            runtime.live_streams.remove(idx);
            changed = true;
            continue;
        };

        let active = card.status == "WORKING" || card.status == "WAITING";
        if active {
            if runtime.live_streams[idx].idle_since.is_some() {
                runtime.live_streams[idx].idle_since = None;
                changed = true;
            }
        } else if runtime.live_streams[idx].idle_since.is_none() {
            runtime.live_streams[idx].idle_since = Some(now_unix);
            changed = true;
        }

        let idle_elapsed = runtime.live_streams[idx]
            .idle_since
            .is_some_and(|since| now_unix.saturating_sub(since) >= LIVE_IDLE_STOP_SECS);
        let expired = now_unix.saturating_sub(stream.started_at) >= LIVE_MAX_DURATION_SECS;

        if idle_elapsed || expired {
            let reason = if expired {
                "10-minute live window ended"
            } else {
                "card idle"
            };
            let html = format!(
                "📺 <b>Live ended</b> ({reason}) · {} {} · <b>{}</b>",
                escape_html(&card.icon),
                escape_html(&card.agent_name),
                escape_html(&card.label)
            );
            let kb = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![
                    InlineButton::callback("📺 Resume live", format!("liv:{}", card.id)),
                    InlineButton::callback("📷 Screen", format!("scr:{}", card.id)),
                ]],
            };
            let _ = api.edit_message_text(stream.chat_id, stream.message_id, &html, Some(&kb));
            runtime.live_streams.remove(idx);
            changed = true;
            continue;
        }

        if now_unix.saturating_sub(stream.last_edited_at) < LIVE_EDIT_INTERVAL_SECS {
            idx += 1;
            continue;
        }

        let Ok(raw) = backend.capture_screen(&card.id, 0) else {
            idx += 1;
            continue;
        };
        let snippet = tail_output_snippet(&raw, &cfg.bot_token, cfg.permissions.redact_secrets, 30, 3200)
            .unwrap_or_else(|| "(empty screen)".to_string());
        let html = format_live_message_html(card, &snippet);
        if html == stream.last_text {
            idx += 1;
            continue;
        }
        let kb = live_stream_keyboard(card, cfg.permissions);
        match api.edit_message_text(stream.chat_id, stream.message_id, &html, Some(&kb)) {
            Ok(()) => {
                runtime.live_streams[idx].last_text = html;
                runtime.live_streams[idx].last_edited_at = now_unix;
                changed = true;
                idx += 1;
            }
            Err(ApiError::TopicNotFound) | Err(ApiError::MessageNotFound) => {
                runtime.live_streams.remove(idx);
                changed = true;
            }
            Err(ApiError::RateLimited { retry_after }) => {
                runtime.live_streams[idx].last_edited_at =
                    now_unix.saturating_add(retry_after.max(5));
                changed = true;
                idx += 1;
            }
            Err(_) => {
                idx += 1;
            }
        }
    }

    changed
}

pub fn tick_typing_actions(
    api: &dyn TelegramApi,
    cfg: &Config,
    runtime: &RuntimeState,
    cards: &[CardSummary],
    now_unix: u64,
    last_typing: &mut HashMap<String, u64>,
) {
    last_typing.retain(|id, _| cards.iter().any(|c| c.id == *id));
    let Some(forum) = cfg.forum.as_ref() else {
        return;
    };
    for card in cards {
        if card.status != "WORKING" {
            continue;
        }
        let Some(binding) = runtime
            .topics
            .iter()
            .find(|t| t.card_id == card.id && !t.closed && !t.deleted_in_telegram)
        else {
            continue;
        };
        let last = last_typing.get(&card.id).copied().unwrap_or(0);
        if now_unix.saturating_sub(last) >= TYPING_ACTION_INTERVAL_SECS {
            let _ = api.send_chat_action(forum.chat_id, Some(binding.thread_id), "typing");
            last_typing.insert(card.id.clone(), now_unix);
        }
    }
}

pub fn sync_topics(
    api: &dyn TelegramApi,
    cfg: &Config,
    runtime: &mut RuntimeState,
    cards: &[CardSummary],
    now_unix: u64,
    force_recreate_deleted: bool,
) -> bool {
    let Some(forum) = cfg.forum.as_ref() else {
        return false;
    };
    let mut changed = false;

    for card in cards {
        let desired_name = topic_title(
            &card.icon,
            &card.agent_name,
            &card.directory_display,
            card.title.as_deref(),
        );
        let color = tag_to_topic_color(card.tag);

        let existing_idx = runtime.topics.iter().position(|t| t.card_id == card.id);
        match existing_idx {
            Some(idx) => {
                let deleted = runtime.topics[idx].deleted_in_telegram;
                if deleted && !force_recreate_deleted {
                    continue;
                }
                if deleted && force_recreate_deleted {
                    let app_url = cfg
                        .mini_app
                        .card_deep_link(cfg.bot_username.as_deref(), &card.id);
                    if let Some(binding) = create_binding(
                        api,
                        forum.chat_id,
                        card,
                        &desired_name,
                        color,
                        now_unix,
                        cfg.permissions,
                        app_url.as_deref(),
                    ) {
                        if let Some(msg_id) = binding.status_message_id {
                            runtime.record_message_card(forum.chat_id, msg_id, &card.id);
                        }
                        runtime.topics[idx] = binding;
                        changed = true;
                    }
                    continue;
                }

                let thread_id = runtime.topics[idx].thread_id;
                if runtime.topics[idx].closed {
                    match api.reopen_forum_topic(forum.chat_id, thread_id) {
                        Ok(()) => {
                            runtime.topics[idx].closed = false;
                            runtime.topics[idx].delete_failed = false;
                            changed = true;
                        }
                        Err(ApiError::TopicNotFound) => {
                            runtime.topics[idx].deleted_in_telegram = true;
                            changed = true;
                            continue;
                        }
                        Err(_) => continue,
                    }
                }

                // A. Best-effort one-time silent pin (upgrades pre-existing topics).
                if let Some(msg_id) = runtime.topics[idx].status_message_id {
                    if !runtime.topics[idx].status_pinned {
                        match api.pin_chat_message(forum.chat_id, msg_id, true) {
                            Ok(()) => {
                                runtime.topics[idx].status_pinned = true;
                                changed = true;
                            }
                            Err(ApiError::TopicNotFound) => {
                                runtime.topics[idx].deleted_in_telegram = true;
                                changed = true;
                                continue;
                            }
                            Err(ApiError::MessageNotFound) => {
                                runtime.topics[idx].status_message_id = None;
                                runtime.topics[idx].last_status_card = None;
                                runtime.topics[idx].status_pinned = true;
                                changed = true;
                            }
                            Err(ApiError::BadRequest(_)) => {
                                runtime.topics[idx].status_pinned = true;
                                changed = true;
                            }
                            Err(_) => {}
                        }
                    }
                }

                // B. Live silent status header card edit (no topic rename service line).
                let desired_header = format_topic_header_html(card);
                let app_url = cfg
                    .mini_app
                    .card_deep_link(cfg.bot_username.as_deref(), &card.id);
                let desired_kb =
                    topic_header_keyboard_with_app(card, cfg.permissions, app_url.as_deref());
                let desired_fp = status_card_fingerprint(&desired_header, desired_kb.as_ref());
                if let Some(msg_id) = runtime.topics[idx].status_message_id {
                    if runtime.topics[idx].last_status_card.as_deref() != Some(desired_fp.as_str()) {
                        match api.edit_message_text(
                            forum.chat_id,
                            msg_id,
                            &desired_header,
                            desired_kb.as_ref(),
                        ) {
                            Ok(()) => {
                                runtime.topics[idx].last_status_card = Some(desired_fp);
                                changed = true;
                            }
                            Err(ApiError::TopicNotFound) => {
                                runtime.topics[idx].deleted_in_telegram = true;
                                changed = true;
                                continue;
                            }
                            Err(ApiError::MessageNotFound) => {
                                runtime.topics[idx].status_message_id = None;
                                runtime.topics[idx].last_status_card = None;
                                changed = true;
                            }
                            Err(_) => {}
                        }
                    }
                }

                // C. Track status silently (the pinned header shows live dots) and never rename
                // topics on status flickers, which inject noisy Telegram service lines into chat.
                if runtime.topics[idx].last_status != card.status {
                    runtime.topics[idx].last_status = card.status.clone();
                    changed = true;
                }
                if runtime.topics[idx].pending_status.is_some() {
                    runtime.topics[idx].pending_status = None;
                    changed = true;
                }
                let initial_title_fill = !strip_legacy_status_prefix(&runtime.topics[idx].topic_name)
                    .contains(" — ")
                    && desired_name.contains(" — ")
                    && now_unix >= runtime.topics[idx].next_rename_allowed_at;
                let should_rename = (force_recreate_deleted || initial_title_fill)
                    && runtime.topics[idx].topic_name != desired_name;
                if should_rename {
                    match api.edit_forum_topic(forum.chat_id, thread_id, &desired_name) {
                        Ok(()) => {
                            runtime.topics[idx].topic_name = desired_name;
                            runtime.topics[idx].last_renamed_at = now_unix;
                            runtime.topics[idx].next_rename_allowed_at =
                                now_unix.saturating_add(60);
                            changed = true;
                        }
                        Err(ApiError::TopicNotFound) => {
                            runtime.topics[idx].deleted_in_telegram = true;
                            changed = true;
                        }
                        Err(ApiError::RateLimited { retry_after }) => {
                            runtime.topics[idx].next_rename_allowed_at =
                                now_unix.saturating_add(retry_after.max(5));
                            changed = true;
                        }
                        Err(_) => {}
                    }
                }
            }
            None => {
                if !cfg.auto_sync_topics && !force_recreate_deleted {
                    continue;
                }
                let app_url = cfg
                    .mini_app
                    .card_deep_link(cfg.bot_username.as_deref(), &card.id);
                if let Some(binding) = create_binding(
                    api,
                    forum.chat_id,
                    card,
                    &desired_name,
                    color,
                    now_unix,
                    cfg.permissions,
                    app_url.as_deref(),
                ) {
                    if let Some(msg_id) = binding.status_message_id {
                        runtime.record_message_card(forum.chat_id, msg_id, &card.id);
                    }
                    runtime.topics.push(binding);
                    changed = true;
                }
            }
        }
    }

    let mut idx = 0usize;
    while idx < runtime.topics.len() {
        let card_alive = cards.iter().any(|c| c.id == runtime.topics[idx].card_id);
        if card_alive || runtime.topics[idx].deleted_in_telegram {
            idx += 1;
            continue;
        }
        let card_id = runtime.topics[idx].card_id.clone();
        let thread_id = runtime.topics[idx].thread_id;

        if cfg.archive_closed_topics && !runtime.topics[idx].delete_failed {
            match api.delete_forum_topic(forum.chat_id, thread_id) {
                Ok(()) | Err(ApiError::TopicNotFound) => {
                    runtime.topics.remove(idx);
                    runtime.live_streams.retain(|s| s.card_id != card_id);
                    runtime.message_card_map.retain(|m| m.card_id != card_id);
                    changed = true;
                    continue;
                }
                Err(ApiError::RateLimited { .. }) | Err(ApiError::Network(_)) => {
                    idx += 1;
                    continue;
                }
                Err(_) => {
                    runtime.topics[idx].delete_failed = true;
                    changed = true;
                }
            }
        }

        if runtime.topics[idx].closed {
            idx += 1;
            continue;
        }
        if let Some(msg_id) = runtime.topics[idx].status_message_id {
            let closed_html = format_closed_topic_header_html(&runtime.topics[idx].topic_name);
            let closed_fp = status_card_fingerprint(&closed_html, None);
            if runtime.topics[idx].last_status_card.as_deref() != Some(closed_fp.as_str()) {
                match api.edit_message_text(forum.chat_id, msg_id, &closed_html, None) {
                    Ok(()) => {
                        runtime.topics[idx].last_status = "CLOSED".into();
                        runtime.topics[idx].last_status_card = Some(closed_fp);
                        runtime.topics[idx].pending_status = None;
                        changed = true;
                    }
                    Err(ApiError::TopicNotFound) => {
                        runtime.topics[idx].deleted_in_telegram = true;
                        changed = true;
                        idx += 1;
                        continue;
                    }
                    Err(ApiError::MessageNotFound) => {
                        runtime.topics[idx].status_message_id = None;
                        runtime.topics[idx].last_status_card = None;
                        changed = true;
                    }
                    Err(_) => {}
                }
            }
        }
        if cfg.permissions.notifications {
            match api.send_message(
                forum.chat_id,
                Some(thread_id),
                "🛑 Card closed on PC.",
                None,
                None,
            ) {
                Ok(msg_id) => {
                    runtime.record_message_card(forum.chat_id, msg_id, &card_id);
                }
                Err(ApiError::TopicNotFound) => {
                    runtime.topics[idx].deleted_in_telegram = true;
                    changed = true;
                    idx += 1;
                    continue;
                }
                Err(_) => {}
            }
        }
        if cfg.archive_closed_topics && !runtime.topics[idx].deleted_in_telegram {
            match api.close_forum_topic(forum.chat_id, thread_id) {
                Ok(()) => {
                    runtime.topics[idx].closed = true;
                    changed = true;
                }
                Err(ApiError::TopicNotFound) => {
                    runtime.topics[idx].deleted_in_telegram = true;
                    changed = true;
                }
                Err(_) => {}
            }
        } else if !runtime.topics[idx].deleted_in_telegram {
            runtime.topics[idx].closed = true;
            changed = true;
        }
        idx += 1;
    }

    if runtime.topics.len() > MAX_TOPICS {
        let mut idx = 0;
        while runtime.topics.len() > MAX_TOPICS && idx < runtime.topics.len() {
            let live = cards.iter().any(|c| c.id == runtime.topics[idx].card_id);
            if !live && (runtime.topics[idx].closed || runtime.topics[idx].deleted_in_telegram) {
                runtime.topics.remove(idx);
                changed = true;
            } else {
                idx += 1;
            }
        }
    }

    changed
}

fn create_binding(
    api: &dyn TelegramApi,
    chat_id: i64,
    card: &CardSummary,
    desired_name: &str,
    color: u32,
    now_unix: u64,
    perms: Permissions,
    mini_app_url: Option<&str>,
) -> Option<TopicBinding> {
    let topic = api.create_forum_topic(chat_id, desired_name, color).ok()?;
    let header = format_topic_header_html(card);
    let kb = topic_header_keyboard_with_app(card, perms, mini_app_url);
    let status_message_id = api
        .send_message(
            chat_id,
            Some(topic.message_thread_id),
            &header,
            None,
            kb.as_ref(),
        )
        .ok();
    if let Some(msg_id) = status_message_id {
        let _ = api.pin_chat_message(chat_id, msg_id, true);
    }
    let last_status_card =
        status_message_id.map(|_| status_card_fingerprint(&header, kb.as_ref()));
    Some(TopicBinding {
        card_id: card.id.clone(),
        thread_id: topic.message_thread_id,
        topic_name: desired_name.to_string(),
        icon_color: color,
        closed: false,
        delete_failed: false,
        deleted_in_telegram: false,
        last_renamed_at: now_unix,
        status_message_id,
        last_status: card.status.clone(),
        last_status_card,
        status_pinned: status_message_id.is_some(),
        next_rename_allowed_at: 0,
        pending_status: None,
        pending_status_since: 0,
    })
}
