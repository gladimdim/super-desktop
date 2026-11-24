use super::*;

/// One pane of `tmux list-panes -a`: the first pane of each session's active
/// window, with the session options the card refresh reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneRow {
    pub pid: String,
    pub cmd: String,
    pub dead: bool,
    pub height: usize,
    pub cwd: String,
    /// `@super_desktop_metadata` (path of the native adapter file).
    pub metadata_option: String,
    /// `@super_desktop_last_prompt` (see `prompt_history`).
    pub last_prompt: String,
    /// `@super_desktop_shell_tracking == 1` (see `shell_title`).
    pub shell_tracking: bool,
    /// `@super_desktop_shell_command`.
    pub shell_command: String,
    /// What a capture of this session would read; `None` when this pane is
    /// not the one a capture addresses, or tmux left a field empty.
    pub stamp: Option<PaneStamp>,
    /// The pane is in a mode, which for a card is copy mode: its view is
    /// scrolled back through the history instead of following new output.
    pub in_mode: bool,
}

/// The parts of an inventory row a pane's text depends on. Output updates
/// `window_activity` (tmux does this whatever `monitor-activity` says);
/// resizes, `clear-history`, `respawn-pane` and `select-pane` do not, so size,
/// history, cursor, process and pane identity are compared as well.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneStamp {
    pub pane_id: String,
    pub pid: String,
    pub width: u64,
    pub height: u64,
    pub history_size: u64,
    pub cursor: (u64, u64),
    pub alternate: bool,
    /// `window_activity`: the second of the window's last output.
    pub activity: u64,
}

/// Every session's pane in one `tmux list-panes -a` call.
#[derive(Default, Debug)]
pub struct PaneSnapshot {
    rows: std::collections::HashMap<String, PaneRow>,
    /// Sessions whose row could not be parsed unambiguously (a value held a
    /// separator): callers use the per-session queries for those.
    damaged: std::collections::HashSet<String>,
}

pub enum PaneLookup<'a> {
    Row(&'a PaneRow),
    /// tmux answered and does not know this session.
    Missing,
    /// This session's row was ambiguous.
    Unknown,
}

impl PaneSnapshot {
    /// Every session tmux listed, including ones whose row was ambiguous.
    pub fn sessions(&self) -> impl Iterator<Item = &String> {
        self.rows.keys().chain(self.damaged.iter())
    }

    pub fn lookup(&self, session: &str) -> PaneLookup<'_> {
        if self.damaged.contains(session) {
            PaneLookup::Unknown
        } else {
            self.rows.get(session).map_or(PaneLookup::Missing, PaneLookup::Row)
        }
    }
}

// Records start with RS and fields are separated by US: neither occurs in
// session names, paths or prompts in practice, and a value that does contain
// one only makes that row fall back (field count), never misattributes data.
// Records are split on RS, not newlines, so a value may contain newlines.
pub(in crate::tmux) const PANE_RECORD: char = '\u{1e}';
pub(in crate::tmux) const PANE_FIELD: char = '\u{1f}';
pub(in crate::tmux) const PANE_FORMAT_FIELDS: [&str; 20] = [
    "#{session_name}",
    "#{window_active}",
    "#{pane_pid}",
    "#{pane_current_command}",
    "#{pane_dead}",
    "#{pane_height}",
    "#{pane_current_path}",
    "#{@super_desktop_metadata}",
    "#{@super_desktop_last_prompt}",
    "#{@super_desktop_shell_tracking}",
    "#{@super_desktop_shell_command}",
    // `PaneStamp`, for reusing an unchanged pane's capture.
    "#{pane_id}",
    "#{pane_active}",
    "#{pane_width}",
    "#{history_size}",
    "#{cursor_x}",
    "#{cursor_y}",
    "#{alternate_on}",
    "#{window_activity}",
    // `PaneRow::in_mode`: scrolled back through the history (copy mode).
    "#{pane_in_mode}",
];

pub fn pane_snapshot_format() -> String {
    format!(
        "{PANE_RECORD}{}",
        PANE_FORMAT_FIELDS.join(&PANE_FIELD.to_string())
    )
}

/// One `tmux list-panes -a` for every card. `None` when tmux did not answer
/// (bounded in time and size so a stuck server cannot pile up workers).
pub fn pane_snapshot() -> Option<PaneSnapshot> {
    let mut command = Command::new(tmux_bin());
    command.args(["list-panes", "-a", "-F", &pane_snapshot_format()]);
    let bytes = crate::workspace_model::bounded_output(command)?;
    Some(parse_pane_snapshot(&String::from_utf8_lossy(&bytes)))
}

pub fn parse_pane_snapshot(text: &str) -> PaneSnapshot {
    let mut snapshot = PaneSnapshot::default();
    for record in text.split(PANE_RECORD).skip(1) {
        let record = record.strip_suffix('\n').unwrap_or(record);
        let fields: Vec<&str> = record.split(PANE_FIELD).collect();
        let Some(session) = fields.first().map(|s| s.to_string()) else {
            continue;
        };
        if fields.len() != PANE_FORMAT_FIELDS.len() {
            snapshot.damaged.insert(session);
            continue;
        }
        // Like `list-panes -t <session>`: the first pane of the active window.
        if fields[1] != "1" || snapshot.rows.contains_key(&session) {
            continue;
        }
        snapshot.rows.insert(
            session,
            PaneRow {
                pid: fields[2].trim().to_string(),
                cmd: fields[3].trim().to_string(),
                dead: fields[4].trim() == "1",
                height: fields[5].trim().parse().unwrap_or(24),
                cwd: fields[6].trim().to_string(),
                metadata_option: fields[7].trim().to_string(),
                last_prompt: fields[8].trim().to_string(),
                shell_tracking: fields[9].trim() == "1",
                shell_command: fields[10].trim().to_string(),
                stamp: parse_pane_stamp(&fields),
                in_mode: fields[19].trim() == "1",
            },
        );
    }
    snapshot
}

pub(in crate::tmux) fn parse_pane_stamp(fields: &[&str]) -> Option<PaneStamp> {
    // `capture-pane -t <session>` reads the active pane; the row is the first
    // pane of the active window, which is that pane unless the window is split.
    if fields[12].trim() != "1" {
        return None;
    }
    let number = |index: usize| fields[index].trim().parse::<u64>().ok();
    let pane_id = fields[11].trim();
    Some(PaneStamp {
        pane_id: (!pane_id.is_empty()).then(|| pane_id.to_string())?,
        pid: fields[2].trim().to_string(),
        width: number(13)?,
        height: number(5)?,
        history_size: number(14)?,
        cursor: (number(15)?, number(16)?),
        alternate: fields[17].trim() == "1",
        activity: number(18)?,
    })
}

/// Only visible rows can describe the current TUI state. Ignore scrollback.
pub(in crate::tmux) fn recent_status_lines(screen: &str, height: usize) -> impl Iterator<Item = &str> {
    screen
        .lines()
        .rev()
        .take(height)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(8)
}

pub fn tmux_bin() -> String {
    // In-process lookup: this runs once per card at window build.
    which("tmux").unwrap_or_else(|| "tmux".to_string())
}

/// Max characters of the last prompt shown in the terminal card title.
/// Long enough to fill the whole header; the label ellipsizes the visual
/// overflow, the full text stays in the tooltip.
pub const PROMPT_TITLE_MAX_CHARS: usize = 120;

/// Capture the visible text of a tmux pane (TUI apps like opencode run
/// fullscreen, so this is the current screen, not scrollback history).
///
/// Card previews (at most 28 lines), the bridge's launcher preview (11) and
/// footer model/effort (6), and status (visible rows) only need the screen,
/// so this reads the visible screen plus 30 lines of history. The phone's
/// terminal stream keeps its own 300-line capture (`tmux_control`).
pub fn capture_pane_text(session_name: &str) -> Option<String> {
    capture_pane(session_name, false, CARD_CAPTURE_HISTORY)
}

/// History lines above the visible screen kept by `capture_pane_text`.
pub const CARD_CAPTURE_HISTORY: u32 = 30;

/// Screen plus 300 lines of history, for callers that scan output history
/// (asset discovery), not for per-second status work.
pub fn capture_pane_history(session_name: &str) -> Option<String> {
    capture_pane(session_name, false, 300)
}

/// Join terminal soft wraps for desktop and phone file discovery, without
/// changing captures used for display, status, links or terminal streaming.
pub fn capture_file_references(session_name: &str) -> Option<String> {
    capture_file_references_with(Command::new(tmux_bin()), session_name)
}

pub(in crate::tmux) fn capture_file_references_with(mut command: Command, session_name: &str) -> Option<String> {
    if session_name.is_empty() || session_name.starts_with('-') {
        return None;
    }
    let target = format!("={session_name}:");
    command_stdout(command.args(["capture-pane", "-p", "-J", "-t", &target, "-S", "-300"]))
}

/// Visible rows only: the status fallback for screen-based agents.
pub fn capture_visible_screen(session_name: &str) -> Option<String> {
    if session_name.is_empty() || session_name.starts_with('-') {
        return None;
    }
    let target = format!("={session_name}:");
    tmux_stdout(&["capture-pane", "-p", "-t", &target])
}

/// Capture tmux's real terminal styling as ANSI SGR sequences. The bridge
/// sends this beside its plain-text fallback so capable clients can reproduce
/// the agent's colours without changing the desktop card path.
#[cfg(test)]
pub fn capture_pane_ansi(session_name: &str) -> Option<String> {
    // Must match the phone stream's `tmux_control::Control::capture` (300).
    capture_pane(session_name, true, 300)
}

pub(in crate::tmux) fn capture_pane(session_name: &str, ansi: bool, history: u32) -> Option<String> {
    capture_pane_raw(session_name, ansi, history).filter(|text| !text.trim().is_empty())
}

/// `capture-pane` output, blank or not; `None` when tmux failed.
pub(in crate::tmux) fn capture_pane_raw(session_name: &str, ansi: bool, history: u32) -> Option<String> {
    if session_name.is_empty() || session_name.starts_with('-') {
        return None;
    }
    let start = format!("-{history}");
    let target = format!("={session_name}:");
    let mut args = vec!["capture-pane"];
    if ansi {
        args.push("-e");
    }
    args.extend(["-p", "-t", &target, "-S", &start]);
    tmux_stdout(&args)
}

/// `capture_pane_text` of a listed session, reusing the previous capture while
/// its inventory row says the pane has not changed (see `capture_reusable`).
pub fn capture_pane_text_for(session_name: &str, row: &PaneRow) -> Option<String> {
    reused_capture(session_name, row, CaptureKind::Card).filter(|text| !text.trim().is_empty())
}

/// `capture_visible_screen` of a listed session, reused like
/// `capture_pane_text_for`.
pub fn capture_visible_screen_for(session_name: &str, row: &PaneRow) -> Option<String> {
    reused_capture(session_name, row, CaptureKind::Visible)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::tmux) enum CaptureKind {
    /// Visible screen plus `CARD_CAPTURE_HISTORY` lines (`capture_pane_text`).
    Card,
    /// Visible rows only (`capture_visible_screen`).
    Visible,
}

pub(in crate::tmux) struct CachedCapture {
    stamp: PaneStamp,
    /// Wall clock just before `capture-pane` ran.
    taken: SystemTime,
    text: String,
}

/// Unchanged captures are still re-read this often, for changes tmux's
/// inventory does not show (a `send-keys -R` reset that keeps cursor and
/// history, a clock step).
pub(in crate::tmux) const CAPTURE_REUSE_MAX: std::time::Duration = std::time::Duration::from_secs(10);
/// Entries are evicted with their pane (`retain_captures`); this only bounds
/// a process that never passes an inventory.
pub(in crate::tmux) const CAPTURE_CACHE_MAX: usize = 256;

pub(in crate::tmux) type CaptureCache = std::collections::HashMap<(String, CaptureKind), CachedCapture>;
pub(in crate::tmux) static CAPTURES: std::sync::OnceLock<std::sync::Mutex<CaptureCache>> = std::sync::OnceLock::new();

/// Whether a capture taken at `taken` of the pane described by `cached` still
/// shows the pane now described by `current`.
///
/// Any output after the capture moves `window_activity` to that output's
/// second, which is at least the capture's second. `window_activity` only
/// has one-second resolution, so a capture taken in the same second as the
/// pane's last output may have missed output later in that second: it is
/// reused only when it was taken in a later second than `current.activity`.
pub(in crate::tmux) fn capture_reusable(cached: &PaneStamp, taken: SystemTime, current: &PaneStamp, now: SystemTime) -> bool {
    let Ok(taken_second) = taken.duration_since(UNIX_EPOCH).map(|since| since.as_secs()) else {
        return false;
    };
    cached == current
        && taken_second > current.activity
        // A clock stepped backwards makes the age an error: capture again.
        && now.duration_since(taken).is_ok_and(|age| age < CAPTURE_REUSE_MAX)
}

pub(in crate::tmux) fn reused_capture(session_name: &str, row: &PaneRow, kind: CaptureKind) -> Option<String> {
    let cache = CAPTURES.get_or_init(Default::default);
    reuse_or_capture(cache, session_name, row.stamp.as_ref(), kind, || match kind {
        CaptureKind::Card => capture_pane_raw(session_name, false, CARD_CAPTURE_HISTORY),
        CaptureKind::Visible => capture_visible_screen(session_name),
    })
}

/// The cached text of `session_name`'s `kind` capture if `stamp` shows the
/// pane unchanged, else `capture()` (remembered when it succeeds). Without a
/// stamp there is nothing to compare, so it always captures.
pub(in crate::tmux) fn reuse_or_capture(
    cache: &std::sync::Mutex<CaptureCache>,
    session_name: &str,
    stamp: Option<&PaneStamp>,
    kind: CaptureKind,
    capture: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let Some(stamp) = stamp else {
        return capture();
    };
    let key = (session_name.to_string(), kind);
    if let Ok(cache) = cache.lock() {
        if let Some(cached) = cache
            .get(&key)
            .filter(|cached| capture_reusable(&cached.stamp, cached.taken, stamp, SystemTime::now()))
        {
            return Some(cached.text.clone());
        }
    }
    // Before the capture: output while it runs must not look older than it.
    let taken = SystemTime::now();
    let text = capture()?;
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= CAPTURE_CACHE_MAX && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, CachedCapture { stamp: stamp.clone(), taken, text: text.clone() });
    }
    Some(text)
}

/// Forget captures of sessions `snapshot` (a full inventory) no longer lists
/// with a stamp.
pub fn retain_captures(snapshot: &PaneSnapshot) {
    if let Some(cache) = CAPTURES.get() {
        retain_captures_in(cache, snapshot);
    }
}

pub(in crate::tmux) fn retain_captures_in(cache: &std::sync::Mutex<CaptureCache>, snapshot: &PaneSnapshot) {
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|(session, _), _| {
            matches!(snapshot.lookup(session), PaneLookup::Row(row) if row.stamp.is_some())
        });
    }
}

/// Capture the in-progress composer draft (text the user typed into the
/// prompt box but has not submitted yet) from an opencode-style TUI screen.
pub fn get_composer_draft(session_name: &str) -> Option<String> {
    let text = capture_pane_text(session_name)?;
    extract_composer_draft(&text)
}

/// Truncate a prompt to a title-friendly length.
/// Caps at `PROMPT_TITLE_MAX_CHARS` (120 chars, 117 + "…"); shorter prompts are returned whole.
pub fn truncate_prompt_title(prompt: &str) -> String {
    let cleaned: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = cleaned.chars().count();
    if count <= PROMPT_TITLE_MAX_CHARS {
        return cleaned;
    }
    let truncated: String = cleaned.chars().take(PROMPT_TITLE_MAX_CHARS - 1).collect();
    format!("{truncated}…")
}

/// Pure helper: extract the in-progress composer draft from an opencode-style
/// TUI screen (testable).
///
/// The composer is the bordered box directly above the `╹` divider at the
/// bottom of the screen. Its text is literally what the user entered into
/// the prompt box. Skips the empty state, the `Ask anything…` placeholder
/// and the `Build … · …` model line. Returns the draft truncated to title
/// length, or None when the composer is empty.
pub fn extract_composer_draft(captured: &str) -> Option<String> {
    let lines: Vec<&str> = captured.lines().collect();
    let div_idx = lines.iter().rposition(|l| l.contains('╹'))?;
    let start = div_idx.saturating_sub(7);
    let mut draft_parts: Vec<String> = Vec::new();
    for line in &lines[start..div_idx] {
        let trimmed = line.trim();
        // Only bordered lines belong to the composer box interior. This
        // excludes the `▣ Build · …` activity status line sitting right
        // above the box, which would otherwise leak agent/model info
        // ("Build · Muse Spark …") into the title.
        if !trimmed.starts_with(['┃', '│']) {
            continue;
        }
        let t = trimmed.trim_start_matches(['┃', '│', '|', ' ']).trim();
        if t.is_empty() {
            continue;
        }
        // Placeholder shown when nothing is typed yet.
        if t.starts_with("Ask anything") {
            continue;
        }
        // Model selector line inside the composer box ("Build · …", "Build auto · …").
        // Requires the `·` so a real draft starting with "Build …" still matches.
        if t.starts_with("Build") && t.contains('·') {
            continue;
        }
        // Tool-call markers should never appear in the composer, but guard anyway.
        if t.starts_with('$') || t.starts_with('#') {
            continue;
        }
        draft_parts.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    if draft_parts.is_empty() {
        return None;
    }
    let draft = draft_parts.join(" ");
    if draft.chars().count() >= 2 {
        Some(truncate_prompt_title(&draft))
    } else {
        None
    }
}

/// Standard output of `command`; `None` when it could not run or failed.
pub(in crate::tmux) fn command_stdout(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `command_stdout` of `tmux <args>`.
pub(crate) fn tmux_stdout(args: &[&str]) -> Option<String> {
    command_stdout(Command::new(tmux_bin()).args(args))
}

/// `stdout` trimmed; `None` when that leaves nothing.
pub(in crate::tmux) fn non_empty(stdout: String) -> Option<String> {
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(in crate::tmux) fn tmux_display(session_name: &str, format: &str) -> Option<String> {
    tmux_stdout(&["display-message", "-p", "-t", session_name, format]).and_then(non_empty)
}

