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

/// Mask secrets and bot tokens while preserving ANSI SGR color sequences (`\x1b[...m`).
pub fn redact_ansi_preserving(text: &str, bot_token: &str, redact_secrets: bool) -> String {
    // Strip unsafe non-SGR control characters (keeping \n, \t, and \x1b[<params>m).
    let mut safe = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b':') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'm' && j - (i + 2) <= 48 {
                safe.push_str(&text[i..=j]);
                i = j + 1;
                continue;
            }
            i += 1;
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        if !ch.is_control() || ch == '\n' || ch == '\t' {
            safe.push(ch);
        }
        i += ch.len_utf8();
    }
    let token_masked = mask_token_in_text(&safe, bot_token);
    if !redact_secrets {
        return token_masked;
    }
    // Redact known secrets while ensuring ANSI SGR suffixes (`...m`) don't block inline prefix matching.
    redact_known_secrets(&token_masked.replace('m', "m\0"))
        .replace("m\0", "m")
}

fn redact_known_secrets(text: &str) -> String {
    let mut lines_out = Vec::new();
    let mut in_pem = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("-----BEGIN ") && trimmed.contains("PRIVATE KEY") {
            in_pem = true;
            lines_out.push("[REDACTED_PRIVATE_KEY]".to_string());
            continue;
        }
        if in_pem {
            if trimmed.starts_with("-----END ") && trimmed.contains("PRIVATE KEY") {
                in_pem = false;
            }
            continue;
        }
        if let Some(redacted_env) = redact_env_line(line) {
            lines_out.push(redacted_env);
        } else {
            lines_out.push(redact_inline_tokens(line));
        }
    }
    let mut joined = lines_out.join("\n");
    if text.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

fn redact_env_line(line: &str) -> Option<String> {
    let (sep_idx, sep_len) = if let Some(idx) = line.find('=') {
        (idx, 1)
    } else if let Some(idx) = line.find(": ") {
        (idx, 2)
    } else {
        return None;
    };
    let lhs = &line[..sep_idx];
    let rhs = line[sep_idx + sep_len..].trim();
    if rhs.is_empty() || rhs == "[REDACTED]" {
        return None;
    }
    let key_candidate = lhs.trim().trim_start_matches("export ").trim();
    if key_candidate.is_empty()
        || !key_candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let upper = key_candidate.to_ascii_uppercase();
    let sensitive = ["SECRET", "TOKEN", "PASSWORD", "API_KEY", "PRIVATE_KEY"]
        .iter()
        .any(|needle| upper.contains(needle));
    if !sensitive {
        return None;
    }
    let sep = &line[sep_idx..sep_idx + sep_len];
    Some(format!("{lhs}{sep}[REDACTED]"))
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

fn redact_inline_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Bearer <20+ token chars>
        if line[i..].starts_with("Bearer ") {
            let rest = &line[i + 7..];
            let len = rest.bytes().take_while(|&b| is_token_char(b)).count();
            if len >= 20 {
                out.push_str("Bearer [REDACTED]");
                i += 7 + len;
                continue;
            }
        }
        // Known provider prefixes
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
            out.push_str("[REDACTED]");
            i += total_len;
            continue;
        }
        // AWS Access Key ID: AKIA + 16 uppercase alnum
        if (i == 0 || !is_token_char(bytes[i - 1])) && line[i..].starts_with("AKIA") {
            let tail = &line[i + 4..];
            let count = tail
                .bytes()
                .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                .count();
            if count == 16 && !tail.as_bytes().get(16).is_some_and(|&b| is_token_char(b)) {
                out.push_str("[REDACTED]");
                i += 20;
                continue;
            }
        }
        // Telegram bot token shape: <8..12 digits>:<30+ base64url chars>
        if (i == 0 || !bytes[i - 1].is_ascii_digit()) && bytes[i].is_ascii_digit() {
            let digits = line[i..].bytes().take_while(u8::is_ascii_digit).count();
            if (8..=12).contains(&digits) && bytes.get(i + digits) == Some(&b':') {
                let secret_len = line[i + digits + 1..]
                    .bytes()
                    .take_while(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    .count();
                if secret_len >= 30 {
                    out.push_str("[REDACTED_BOT_TOKEN]");
                    i += digits + 1 + secret_len;
                    continue;
                }
            }
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
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
