pub(in crate::tmux) use std::path::Path;
pub(in crate::tmux) use std::process::Command;
pub(in crate::tmux) use std::time::{SystemTime, UNIX_EPOCH};

mod registry;
mod resume;
mod session;
mod status;
mod capture;
mod opencode;
#[cfg(test)]
mod tests;

pub use crate::terminal_text::strip_terminal_escapes;

#[allow(unused_imports)]
pub use registry::{
    detect_harness_command, detect_harnesses, get_agent_config, harness_binary,
    harness_command_with, is_owned_session, resolve_command, retired_successor,
    AgentConfig, HarnessInfo, HARNESS_KEYS, RETIRED_HARNESSES,
};
pub(crate) use registry::which;
pub(in crate::tmux) use registry::is_shell_command;
#[cfg(test)]
pub(in crate::tmux) use registry::{
    first_installed, harness_candidates, npx_fallback_command, resolve_command_with,
    which_in_common_user_bins, which_in_path, with_default_args,
};

pub use resume::{is_resumable_id, resolve_resume_command_in};
#[cfg(test)]
pub(in crate::tmux) use resume::{
    claude_transcript, codex_rollout_exists, resume_command_with,
};

pub use session::{
    create_session, ensure_session_with_inventory, kill_session, parse_pane_grid,
    resolve_workspace_dir, restart_fresh, send_keys, send_keys_unrecorded, session_exists,
    settle_before_enter, settles_before_enter, SessionInventory,
};
#[cfg(test)]
pub use session::{pane_grid, unique_session_name};
#[cfg(test)]
pub(in crate::tmux) use session::{
    pin_client_exit, session_state, settle_before_enter_within, start_session, Started,
};

pub use status::{
    exited_status, inspect_status, inspect_status_with_screen, preview_from_screen,
    status_for_pane, SessionStatus,
};
#[cfg(test)]
pub(in crate::tmux) use status::{
    agent_screen_indicates_work, aider_screen_waiting, explicit_turn_status,
    foreground_job_from_stat, screen_indicates_work,
};
pub(crate) use status::get_direct_children;

#[allow(unused_imports)]
pub use capture::{
    capture_file_references, capture_pane_history, capture_pane_text, capture_pane_text_for,
    capture_visible_screen, capture_visible_screen_for, extract_composer_draft,
    get_composer_draft, pane_snapshot, pane_snapshot_format, parse_pane_snapshot,
    retain_captures, tmux_bin, truncate_prompt_title, PaneLookup, PaneRow, PaneSnapshot,
    PaneStamp, CARD_CAPTURE_HISTORY, PROMPT_TITLE_MAX_CHARS,
};
#[cfg(test)]
pub use capture::capture_pane_ansi;
#[cfg(test)]
pub(in crate::tmux) use capture::{
    capture_file_references_with, capture_pane_raw, capture_reusable, retain_captures_in,
    reuse_or_capture, CaptureCache, CaptureKind, CAPTURE_REUSE_MAX,
};
pub(in crate::tmux) use capture::{command_stdout, non_empty, recent_status_lines, tmux_display};
pub(crate) use capture::tmux_stdout;

#[allow(unused_imports)]
pub use opencode::{
    assign_opencode_sessions, drop_determined_panes, extract_session_flag,
    get_opencode_title_by_id, get_opencode_user_prompts_by_id, get_opencode_user_text_by_id,
    keep_opencode_panes, opencode_prompt_rows, pane_opencode_session_flag,
    resolve_own_opencode_id, should_adopt_heuristic_match,
};
#[allow(unused_imports)]
pub(crate) use opencode::{opencode_db_title, sql_escape, sqlite_query, sqlite_query_as};
#[cfg(test)]
pub(in crate::tmux) use opencode::{opencode_last_prompt_in, read_cmdline, SQLITE_RUNS};
