//! Formatting, HTML escaping, message chunking, and best-effort secret redaction.
use super::config::mask_token_in_text;

pub const MAX_TOPIC_NAME_CHARS: usize = 128;
pub const DEFAULT_CHUNK_CHARS: usize = 3500;

/// Valid `icon_color` values accepted by Telegram's `createForumTopic`.
pub const TELEGRAM_TOPIC_COLORS: [u32; 6] = [
    7_322_096,  // 0x6FB9F0 blue
    16_766_590, // 0xFFD67E yellow
    13_338_331, // 0xCB86DB violet
    9_367_192,  // 0x8EEE98 green
    16_749_490, // 0xFF93B2 rose
    16_478_047, // 0xFB6F5F red
];

pub fn tag_to_topic_color(tag: u8) -> u32 {
    if tag == 0 {
        return TELEGRAM_TOPIC_COLORS[0];
    }
    TELEGRAM_TOPIC_COLORS[(tag as usize - 1) % TELEGRAM_TOPIC_COLORS.len()]
}

pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn strip_controls_keep_newlines(input: &str) -> String {
    input
        .chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
        .collect()
}

pub fn sanitize_terminal_text(raw: &str, bot_token: &str, redact_secrets: bool) -> String {
    let stripped = super_desktop::terminal_text::strip_terminal_escapes(raw);
    let clean = strip_controls_keep_newlines(&stripped);
    let token_masked = mask_token_in_text(&clean, bot_token);
    if !redact_secrets {
        return token_masked;
    }
    redact_known_secrets(&token_masked)
}

/// Sanitize and redact a URL or text string before displaying or linking it.
/// Returns `(display_text, changed_by_redaction)`.
pub fn redact_link_text(raw: &str, bot_token: &str, redact_secrets: bool) -> (String, bool) {
    let mut masked = mask_token_in_text(raw, bot_token);
    if redact_secrets {
        masked = redact_known_secrets(&masked);
        if let Ok(mut parsed) = reqwest::Url::parse(&masked) {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("[REDACTED]"));
                masked = parsed.to_string();
            }
        }
        // Mask sensitive query parameters (?token=..., &api_key=..., &secret=..., &password=...).
        if let Some((base, query)) = masked.split_once('?') {
            let mut out_q = Vec::new();
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    let upper = k.to_ascii_uppercase();
                    let sensitive = !v.is_empty()
                        && v != "[REDACTED]"
                        && ["TOKEN", "SECRET", "PASSWORD", "API_KEY", "APIKEY", "KEY", "AUTH"]
                            .iter()
                            .any(|n| upper.contains(n));
                    if sensitive {
                        out_q.push(format!("{k}=[REDACTED]"));
                    } else {
                        out_q.push(pair.to_string());
                    }
                } else {
                    out_q.push(pair.to_string());
                }
            }
            masked = format!("{base}?{}", out_q.join("&"));
        }
    }
    let changed = masked != raw;
    (masked, changed)
}

/// Parse `\x1b[<params>m` SGR span starting at `bytes[i]`, returning end byte index (exclusive).
fn sgr_span_end(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes.get(i) != Some(&0x1B) || bytes.get(i + 1) != Some(&b'[') {
        return None;
    }
    let mut j = i + 2;
    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b':') {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'm' && j - (i + 2) <= 48 {
        Some(j + 1)
    } else {
        None
    }
}

/// Mask secrets and bot tokens while preserving ANSI SGR color sequences (`\x1b[...m`)
/// without breaking on `'m'` or ANSI-colored env/PEM lines.
pub fn redact_ansi_preserving(text: &str, bot_token: &str, redact_secrets: bool) -> String {
    let mut safe = String::with_capacity(text.len());
    let mut plain = String::with_capacity(text.len());
    // For each byte in `plain`, stores its byte index in `safe`.
    let mut plain_to_safe: Vec<usize> = Vec::with_capacity(text.len() + 1);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = sgr_span_end(bytes, i) {
            safe.push_str(&text[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == 0x1B && bytes.get(i + 1) == Some(&b'[') {
            i += 1;
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        if !ch.is_control() || ch == '\n' || ch == '\t' {
            let ch_start = safe.len();
            safe.push(ch);
            for (k, b) in safe.as_bytes()[ch_start..].iter().enumerate() {
                let _ = b;
                plain.push_str("");
                plain_to_safe.push(ch_start + k);
            }
            plain.push(ch);
        }
        i += ch.len_utf8();
    }
    plain_to_safe.push(safe.len());

    // Find redaction spans in `plain`, first masking bot token occurrences.
    let redacted_plain = if !bot_token.trim().is_empty() {
        let token_masked = mask_token_in_text(&plain, bot_token);
        if redact_secrets {
            redact_known_secrets(&token_masked)
        } else {
            token_masked
        }
    } else if redact_secrets {
        redact_known_secrets(&plain)
    } else {
        plain.clone()
    };

    if redacted_plain == plain {
        return safe;
    }

    // Re-apply replacements over `safe` when bot_token or known secrets match.
    // First apply exact bot_token replacement across byte spans in `plain` mapped to `safe`.
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let trimmed_tok = bot_token.trim();
    if !trimmed_tok.is_empty() {
        let mut pos = 0;
        while let Some(found) = plain[pos..].find(trimmed_tok) {
            let p_start = pos + found;
            let p_end = p_start + trimmed_tok.len();
            let s_start = plain_to_safe[p_start];
            let s_end = plain_to_safe[p_end - 1] + 1;
            spans.push((s_start, s_end, "[REDACTED_BOT_TOKEN]".to_string()));
            pos = p_end;
        }
    }
    if redact_secrets {
        for (p_start, p_end, rep) in collect_secret_spans(&plain) {
            if p_start < p_end && p_end <= plain.len() {
                let s_start = plain_to_safe[p_start];
                let s_end = plain_to_safe[p_end - 1] + 1;
                spans.push((s_start, s_end, rep));
            }
        }
    }
    spans.sort_by_key(|(s, e, _)| (*s, *e));
    // Merge/skip overlapping spans so earliest wins cleanly.
    let mut out = String::with_capacity(safe.len());
    let mut cursor = 0usize;
    for (s_start, s_end, rep) in spans {
        if s_start < cursor || s_end <= s_start {
            continue;
        }
        out.push_str(&safe[cursor..s_start]);
        // Preserve any SGR escape codes inside the replaced range? No—emitting clean replacement
        // keeps surrounding SGR state clean without breaking sequences; preserve trailing SGR if split.
        out.push_str(&rep);
        cursor = s_end;
    }
    out.push_str(&safe[cursor..]);
    out
}

/// Collects non-overlapping secret spans `(start_byte, end_byte, replacement)` in plain text.
fn collect_secret_spans(text: &str) -> Vec<(usize, usize, String)> {
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let mut offset = 0usize;
    let mut in_pem = false;
    let mut pem_start = 0usize;
    let mut pem_end = 0usize;
    for chunk in text.split_inclusive('\n') {
        let line_end_no_nl = chunk.trim_end_matches(&['\r', '\n'][..]).len();
        let line = &chunk[..line_end_no_nl];
        let trimmed = line.trim();
        if !in_pem && trimmed.starts_with("-----BEGIN ") && trimmed.contains("PRIVATE KEY") {
            in_pem = true;
            pem_start = offset;
            pem_end = offset + line_end_no_nl;
            offset += chunk.len();
            continue;
        }
        if in_pem {
            pem_end = offset + line_end_no_nl;
            if trimmed.starts_with("-----END ") && trimmed.contains("PRIVATE KEY") {
                in_pem = false;
                spans.push((pem_start, pem_end, "[REDACTED_PRIVATE_KEY]".to_string()));
            }
            offset += chunk.len();
            continue;
        }
        collect_line_secret_spans(line, offset, &mut spans);
        offset += chunk.len();
    }
    if in_pem {
        spans.push((pem_start, pem_end.max(pem_start + 1), "[REDACTED_PRIVATE_KEY]".to_string()));
    }
    spans
}

fn collect_line_secret_spans(line: &str, base: usize, spans: &mut Vec<(usize, usize, String)>) {
    // 1. Check full-line / env / quoted config key-value pairs (multiple assignments per line supported).
    let mut scan = 0usize;
    while scan < line.len() {
        let rest = &line[scan..];
        let eq_pos = rest.find('=');
        let colon_pos = rest.find(": ");
        let Some((rel_idx, sep_len)) = (match (eq_pos, colon_pos) {
            (Some(a), Some(b)) if a <= b => Some((a, 1usize)),
            (Some(_), Some(b)) => Some((b, 2usize)),
            (Some(a), None) => Some((a, 1)),
            (None, Some(b)) => Some((b, 2)),
            (None, None) => None,
        }) else {
            break;
        };
        let sep_idx = scan + rel_idx;
        // Identify the key token immediately preceding `sep_idx`.
        let pre = line[scan..sep_idx].trim_end();
        let raw_token_start = pre
            .rfind(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == ';')
            .map_or(0, |p| p + pre[p..].chars().next().map_or(1, char::len_utf8));
        let key_token = pre[raw_token_start..]
            .trim_start_matches("export ")
            .trim()
            .trim_matches(|c| c == '"' || c == '\'');
        let is_valid_key = !key_token.is_empty()
            && key_token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        let upper = key_token.to_ascii_uppercase();
        let sensitive = is_valid_key
            && ["SECRET", "TOKEN", "PASSWORD", "API_KEY", "APIKEY", "PRIVATE_KEY", "PRIVATEKEY", "ACCESS_KEY"]
                .iter()
                .any(|needle| upper.contains(needle));

        let val_start_raw = sep_idx + sep_len;
        let after_ws = val_start_raw
            + line[val_start_raw..]
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(0);
        if sensitive && after_ws < line.len() {
            let rhs_rem = &line[after_ws..];
            // Find end of quoted value, single assignment token, or whole RHS when no multi-assignment follows.
            let val_len = if let Some(q) = rhs_rem.bytes().next().filter(|&b| b == b'"' || b == b'\'') {
                let mut j = 1usize;
                let rb = rhs_rem.as_bytes();
                while j < rb.len() {
                    if rb[j] == b'\\' && j + 1 < rb.len() {
                        j += 2;
                        continue;
                    }
                    if rb[j] == q {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                j
            } else if line[scan..sep_idx].contains(' ') && sep_len == 1 {
                rhs_rem.find(char::is_whitespace).unwrap_or(rhs_rem.len())
            } else {
                rhs_rem.trim_end().len()
            };
            let rhs_slice = rhs_rem[..val_len].trim();
            if !rhs_slice.is_empty() && rhs_slice != "[REDACTED]" {
                spans.push((base + after_ws, base + after_ws + val_len, "[REDACTED]".to_string()));
                scan = after_ws + val_len;
                continue;
            }
        }
        scan = val_start_raw;
    }

    // 2. Inline tokens (Bearer / sk- / ghp_ / AKIA / bot tokens).
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(prefix_len) = (bytes.len() - i >= 7 && bytes[i..i + 7].eq_ignore_ascii_case(b"Bearer "))
            .then_some(7usize)
        {
            let rest = &line[i + prefix_len..];
            let len = rest.bytes().take_while(|&b| is_bearer_token_char(b)).count();
            if len >= 20 {
                spans.push((base + i, base + i + prefix_len + len, "Bearer [REDACTED]".to_string()));
                i += prefix_len + len;
                continue;
            }
        }
        let prefix_match = [
            ("sk-ant-", 20usize),
            ("sk-", 20),
            ("ghp_", 16),
            ("gho_", 16),
            ("github_pat_", 20),
        ]
        .iter()
        .find(|(prefix, min_len)| {
            let boundary_ok = i == 0 || !is_token_char(bytes[i - 1]);
            boundary_ok
                && line[i..].starts_with(prefix)
                && line[i + prefix.len()..]
                    .bytes()
                    .take_while(|&b| is_token_char(b))
                    .count()
                    >= *min_len
        });
        if let Some((prefix, _)) = prefix_match {
            let total_len = prefix.len()
                + line[i + prefix.len()..]
                    .bytes()
                    .take_while(|&b| is_token_char(b))
                    .count();
            spans.push((base + i, base + i + total_len, "[REDACTED]".to_string()));
            i += total_len;
            continue;
        }
        if (i == 0 || !is_token_char(bytes[i - 1])) && line[i..].starts_with("AKIA") {
            let tail = &line[i + 4..];
            let count = tail
                .bytes()
                .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                .count();
            if count == 16 && !tail.as_bytes().get(16).is_some_and(|&b| is_token_char(b)) {
                spans.push((base + i, base + i + 20, "[REDACTED]".to_string()));
                i += 20;
                continue;
            }
        }
        if (i == 0 || !bytes[i - 1].is_ascii_digit()) && bytes[i].is_ascii_digit() {
            let digits = line[i..].bytes().take_while(u8::is_ascii_digit).count();
            if (8..=12).contains(&digits) && bytes.get(i + digits) == Some(&b':') {
                let secret_len = line[i + digits + 1..]
                    .bytes()
                    .take_while(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    .count();
                if secret_len >= 30 {
                    spans.push((base + i, base + i + digits + 1 + secret_len, "[REDACTED_BOT_TOKEN]".to_string()));
                    i += digits + 1 + secret_len;
                    continue;
                }
            }
        }
        let ch = line[i..].chars().next().unwrap();
        i += ch.len_utf8();
    }
}

pub fn redact_known_secrets(text: &str) -> String {
    let mut spans = collect_secret_spans(text);
    if spans.is_empty() {
        return text.to_string();
    }
    spans.sort_by_key(|(s, e, _)| (*s, *e));
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (s, e, rep) in spans {
        if s < cursor || e <= s {
            continue;
        }
        out.push_str(&text[cursor..s]);
        out.push_str(&rep);
        cursor = e;
    }
    out.push_str(&text[cursor..]);
    out
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

fn is_bearer_token_char(b: u8) -> bool {
    is_token_char(b) || b == b'+' || b == b'/' || b == b'='
}

pub fn split_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let limit = max_chars.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;

    for line in text.split_inclusive('\n') {
        let line_chars = line.chars().count();
        if line_chars > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            let mut piece = String::new();
            let mut piece_chars = 0usize;
            for ch in line.chars() {
                if piece_chars >= limit {
                    chunks.push(std::mem::take(&mut piece));
                    piece_chars = 0;
                }
                piece.push(ch);
                piece_chars += 1;
            }
            if !piece.is_empty() {
                current = piece;
                current_chars = piece_chars;
            }
            continue;
        }
        if current_chars + line_chars > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push_str(line);
        current_chars += line_chars;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub fn status_emoji(status: &str) -> &'static str {
    match status {
        "WORKING" => "🟢",
        "WAITING" => "🟡",
        "ERROR" => "🔴",
        "FINISHED" => "✅",
        "EXITED" => "⚫",
        _ => "⚪",
    }
}

pub fn strip_legacy_status_prefix(name: &str) -> &str {
    for prefix in ["🟢 ", "🟡 ", "✅ ", "🔴 ", "⚪ ", "⚫ "] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}

pub fn topic_title(
    icon: &str,
    agent_name: &str,
    dir_display: &str,
    card_title: Option<&str>,
) -> String {
    let clean_one_line = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let icon = clean_one_line(icon);
    let agent = clean_one_line(agent_name);
    let folder_leaf = dir_display
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(dir_display);
    let folder = clean_one_line(folder_leaf);
    let mut base = if icon.is_empty() {
        format!("{agent} · {folder}")
    } else {
        format!("{icon} {agent} · {folder}")
    };
    if let Some(title) = card_title.map(clean_one_line).filter(|s| !s.is_empty()) {
        base.push_str(" — ");
        base.push_str(&title);
    }
    truncate_chars(&base, MAX_TOPIC_NAME_CHARS)
}

pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_ansi_preserving_handles_m_in_tokens_and_multibyte_boundaries() {
        let styled = "\x1b[31mBearer sk_live_m_secret12345678\x1b[0m привіт";
        let out = redact_ansi_preserving(styled, "", true);
        assert!(out.contains("[REDACTED]"), "{out:?}");
        assert!(!out.contains("sk_live_m_secret12345678"), "{out:?}");
        assert!(out.starts_with("\x1b[31m"));
        assert!(out.contains("привіт"));

        let url = "https://user:pass123@example.org/path?token=ghp_1234567890abcdef1234567890ab";
        let (link, redacted) = redact_link_text(url, "", true);
        assert!(redacted);
        assert!(!link.contains("pass123"));
        assert!(!link.contains("ghp_1234567890abcdef1234567890ab"));
    }
}
