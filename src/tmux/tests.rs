fn resolve_resume_command(agent_type: &str, custom: Option<&str>) -> String {
    resolve_resume_command_in(agent_type, custom, None, None)
}

fn resolve_resume_command_with_session(agent_type: &str, custom: Option<&str>, id: Option<&str>) -> String {
    resolve_resume_command_in(agent_type, custom, id, None)
}

fn ensure_session_with_agent_id(session: &str, agent: &str, custom: Option<&str>, id: Option<&str>, dir: Option<&str>) {
    ensure_session_with_inventory(session, agent, custom, id, dir, None)
}

fn ensure_session(session: &str, agent: &str, custom: Option<&str>, dir: Option<&str>) {
    ensure_session_with_agent_id(session, agent, custom, None, dir)
}
use super::*;
use crate::test_isolation::wait_until;

/// `tmux <args>` on this test process's own server (see `test_isolation`):
/// whether it ran and succeeded.
fn tmux(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn has_tmux() -> bool {
    tmux(&["-V"])
}

/// Kills the listed sessions on drop so a failing assertion cannot leak
/// harness sessions into the test's tmux server.
struct SessionCleanup(Vec<String>);

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        for name in &self.0 {
            tmux(&["kill-session", "-t", name]);
        }
    }
}

/// A fresh session on the test server, killed when the guard drops.
fn new_session(name: &str, args: &[&str]) -> SessionCleanup {
    tmux(&["kill-session", "-t", name]);
    tmux(&[&["new-session", "-d", "-s", name], args].concat());
    SessionCleanup(vec![name.to_string()])
}

#[test]
fn harnesses_wait_before_return_and_shells_do_not() {
    for agent in ["claude", "codex", "grok", "opencode", "gemini", "custom-launcher"] {
        assert!(settles_before_enter(agent), "{agent}");
    }
    for agent in ["shell", "bash", "terminal"] {
        assert!(!settles_before_enter(agent), "{agent}");
    }
}

#[test]
fn return_waits_for_the_typed_text_to_reach_the_screen() {
    use std::time::{Duration, Instant};
    let (min, max) = (Duration::from_millis(40), Duration::from_millis(400));
    // Already shown: only the minimum.
    let started = Instant::now();
    settle_before_enter_within(Some("> "), || Some("> hello".into()), min, max);
    assert!((min..max).contains(&started.elapsed()));
    // A harness that lags shows the text later; Return waits for it.
    let started = Instant::now();
    let mut polls = 0;
    settle_before_enter_within(Some("> "), || {
        polls += 1;
        Some(if polls < 5 { "> ".into() } else { "> hello".into() })
    }, min, max);
    assert_eq!(polls, 5);
    assert!(started.elapsed() < max);
    // A screen that never changes still gets Return at the maximum.
    let started = Instant::now();
    settle_before_enter_within(Some("> "), || Some("> ".into()), min, max);
    assert!(started.elapsed() >= max && started.elapsed() < max * 2);
    // No screen to compare, or a failed capture: the minimum only.
    let started = Instant::now();
    settle_before_enter_within(None, || unreachable!(), min, max);
    settle_before_enter_within(Some("> "), || None, min, max);
    assert!(started.elapsed() < max);
}

/// Driven by tests/harness_phone_matrix.py: launch one harness through the
/// desktop's own `create_session` (resolved command, default flags, shell
/// title and harness-metadata wrapper) inside the caller's private tmux
/// server. Does nothing unless the matrix provides its spec file.
#[test]
fn phone_matrix_launch() {
    let Ok(spec) = std::env::var("SUPER_DESKTOP_PHONE_MATRIX_LAUNCH") else {
        return;
    };
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&spec).unwrap()).unwrap();
    let agent = spec["agent"].as_str().unwrap();
    let workspace = spec["workspace"].as_str();
    let (session, command) = create_session(agent, None, workspace);
    let reply = serde_json::json!({"session": session, "command": command});
    std::fs::write(spec["out"].as_str().unwrap(), reply.to_string()).unwrap();
}

#[test]
fn card_title_opencode_skips_synthetic_parts_and_injected_turns() {
    let dir = std::env::temp_dir().join(format!("sd-opencode-title-{}", unique_session_name()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    let user = r#"{"role":"user"}"#;
    let text = |t: &str, extra: &str| format!(r#"{{"type":"text","text":"{t}"{extra}}}"#);
    let mut sql = String::from(
        "CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
    );
    for (message, at, parts) in [
        ("m1", 1, vec![text("Fix the toolbar", ""), text("fn main() {}", r#","synthetic":true"#)]),
        ("m2", 2, vec![text("<task-notification>done</task-notification>", "")]),
    ] {
        sql += &format!("INSERT INTO message VALUES('{message}','s',{at},'{user}');");
        for (i, part) in parts.iter().enumerate() {
            sql += &format!("INSERT INTO part VALUES('{message}p{i}','{message}','s',{at},'{part}');");
        }
    }
    assert!(Command::new("sqlite3").arg(&db).arg(&sql).status().unwrap().success());
    assert_eq!(opencode_last_prompt_in(&db, "s").as_deref(), Some("Fix the toolbar"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unchanged_database_answers_without_a_sqlite_process() {
    let dir = std::env::temp_dir().join(format!("sd-sqlite-cache-{}", unique_session_name()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    let run = |sql: &str| {
        assert!(Command::new("sqlite3").arg(&db).arg(sql).status().unwrap().success());
    };
    run("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT, title TEXT); INSERT INTO session VALUES('a','First');");
    let runs = || SQLITE_RUNS.with(|runs| runs.get());
    let sql = "SELECT title FROM session WHERE id = 'a';";
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("First"));
    // Some SQLite builds create a WAL on the first read; others do not.
    // Warm both cases before asserting that stable reads start no process.
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("First"));
    let before = runs();
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("First"));
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("First"));
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("First"));
    assert_eq!(runs() - before, 0, "an unchanged database must be answered from memory");
    let before = runs();
    // A write (through the WAL or a checkpoint) must be seen.
    run("UPDATE session SET title = 'Renamed' WHERE id = 'a';");
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("Renamed"));
    assert_eq!(sqlite_query(&db, sql).as_deref(), Some("Renamed"));
    assert!(runs() - before >= 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn status_uses_turn_events_and_not_response_text() {
    assert_eq!(explicit_turn_status("completed"), Some(("FINISHED", "✓ FINISHED")));
    assert_eq!(explicit_turn_status("working"), Some(("WORKING", "● WORKING")));
    assert_eq!(explicit_turn_status("error"), crate::harness_record::status("error"));
    assert_eq!(explicit_turn_status("unknown"), None);
    assert!(!screen_indicates_work("Use Ctrl+C to cancel this command.\nThe output contains ⠋ and thinking...\n› Ask anything", 24));
    assert!(!screen_indicates_work("› explain working... and esc to interrupt", 24));
    assert!(screen_indicates_work("• Working (2m 10s · esc to interrupt)\n› Ask anything", 24));
    assert!(screen_indicates_work("✻ Thinking… (esc to interrupt)", 24));
    assert!(screen_indicates_work("⠋ Generating…", 24));
    assert!(!screen_indicates_work(&format!("⠋ Generating…\n{}Ready", "\n".repeat(30)), 24));
}

/// Real phone-stream frames (tailAnsi, bottom rows, sanitized) captured by
/// tests/harness_phone_matrix.py --submit on 2026-09-24: Grok 1.0.41
/// `--minimal`, Reasonix 1.39.0 `code` and Hermes 0.19.0 `--cli`.
/// The frames are git-ignored local captures (dump raw reply frames with
/// `SD_MATRIX_FRAME_DUMP=DIR python3 tests/harness_phone_matrix.py --submit`,
/// then sanitize and keep the bottom rows). Without them the frame checks
/// are skipped and only the inline cases run.
fn status_frames_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/status-frames");
    if dir.join("grok-thinking.ansi").is_file() {
        Some(dir)
    } else {
        eprintln!("skipping captured status frames: {} is missing", dir.display());
        None
    }
}

fn status_frame(dir: &std::path::Path, name: &str) -> String {
    let path = dir.join(format!("{name}.ansi"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn screen_status_sees_grok_reasonix_and_hermes_replies_in_progress() {
    if let Some(dir) = status_frames_dir() {
        for (agent, working, idle) in [
            ("grok", &["grok-waiting", "grok-thinking", "grok-responding"][..], &["grok-idle-start", "grok-idle-done"][..]),
            ("reasonix", &["reasonix-thinking", "reasonix-working"][..], &["reasonix-idle-done"][..]),
            ("hermes", &["hermes-running"][..], &["hermes-idle-error"][..]),
        ] {
            for name in working {
                assert!(agent_screen_indicates_work(agent, &status_frame(&dir, name), 35), "{name} should be WORKING");
            }
            for name in idle {
                assert!(!agent_screen_indicates_work(agent, &status_frame(&dir, name), 35), "{name} should be IDLE");
            }
        }
        // Hermes' running placeholder is Hermes-only; other agents never see it.
        assert!(!agent_screen_indicates_work("grok", &status_frame(&dir, "hermes-running"), 35));
    }
    // The reply text itself ("OK", or prose about waiting) is never a signal.
    for text in [
        "Waiting for response… 0.8s\n❯",
        "OK\nWorked for 2.1s\n❯",
        "❯ show msg=interrupt · /queue usage\n",
        "⠋ and then 5s later",
        "⠋ Deploy finished in 12 minutes",
        "│ uses ⏱ 5s timers",
    ] {
        assert!(!agent_screen_indicates_work("hermes", text, 24), "{text:?}");
        assert!(!agent_screen_indicates_work("grok", text, 24), "{text:?}");
    }
    assert!(screen_indicates_work("⠙ Running command… 1m 5s", 24));
    assert!(!screen_indicates_work("⠙ Running command… 1m", 24));
}

/// dsh-TUI 0.11.0 footers, laid out from its source (`WorkingSpinner`,
/// `SpinnerAnimationRow`, `StatusLine`): the working row, the composer and
/// the status bar whose last row carries the hint. No captured frames yet:
/// DeepSeek Harness is not installed on the development machine.
#[test]
fn screen_status_sees_dsh_tui_turns_in_progress() {
    let footer = |row: &str, hint: &str| {
        format!("⏺ Earlier reply text\n\n{row}\n\n╭──────────────╮\n│ >            │\n╰──────────────╯\n deepseek-v4-pro · ~/proj · #1a2b3c4d\n {hint}\n")
    };
    for screen in [
        footer("● Thinking… (3s · ↓ 12 tokens)", "esc to interrupt"),
        footer("• 思考中… (1m 5s · ↑ 3.1k · ↓ 1.2k tokens)", "esc 中断"),
        // The hint yields to a hovered status field; the working row remains.
        footer("· Reading… (12s)", "42.1% · 84k/200k · free 116k"),
        footer("● Reasoning… (thinking)", ""),
        footer("● Considering… (thought for 4s)", ""),
        footer("● Working…", ""),
        // The working-activity row (free-form phrases) keeps the hint.
        footer("🌔 reading src/main.rs · 4s · ↓ 210 tokens", "esc to interrupt"),
        footer("🌔 读取 src/main.rs", "esc 中断   ▁▂▃"),
    ] {
        assert!(agent_screen_indicates_work("dsh", &screen, 24), "{screen}");
    }
    for screen in [
        footer("Finished 3 tools · thought 2s worked 5s", "? for shortcuts"),
        footer("搞定 ✓ 3 tools · 想2s 干5s", "? 查看快捷键"),
        footer("", "esc to return to input"),
        // Reply text shaped like the working row is not the working row.
        footer("• Reading… the config first", ""),
        footer("● Thinking… (see the note above)", ""),
        footer("- Working… (3s)", ""),
        footer("Thinking… (3s)", ""),
        footer("● Deploying… (3s)", ""),
        footer("│ esc 中断 stops the run", ""),
    ] {
        assert!(!agent_screen_indicates_work("dsh", &screen, 24), "{screen}");
    }
    // Scrolled far above the visible rows, the working row is history.
    let stale = format!("● Thinking… (3s)\n{}? for shortcuts", "\n".repeat(30));
    assert!(!agent_screen_indicates_work("dsh", &stale, 24));
    // dsh's own rows mean nothing for another harness.
    for screen in [footer("● Thinking… (3s)", ""), footer("", "esc 中断")] {
        assert!(!agent_screen_indicates_work("grok", &screen, 24), "{screen}");
    }
}

#[test]
fn background_children_do_not_make_shell_busy() {
    assert!(!foreground_job_from_stat("10 (bash) S 1 10 10 123 10 0 0"));
    assert!(foreground_job_from_stat("10 (bash) S 1 10 10 123 20 0 0"));
    assert!(!foreground_job_from_stat("10 (bash) S 1 10 10 0 -1 0 0"));
    assert!(!foreground_job_from_stat("bad stat"));
}

#[test]
fn shared_capture_keeps_status_within_the_original_history_window() {
    let screen = format!("old thinking...\n{}ready\n", "\n".repeat(40));
    assert_eq!(recent_status_lines(&screen, 24).collect::<Vec<_>>(), ["ready"]);
    let screen = (0..40).map(|n| format!("line {n}\n")).collect::<String>();
    assert_eq!(recent_status_lines(&screen, 24).collect::<Vec<_>>(),
        ["line 39", "line 38", "line 37", "line 36", "line 35", "line 34", "line 33", "line 32"]);
}

#[test]
fn shared_capture_preview_keeps_internal_blank_lines_and_unicode() {
    assert_eq!(preview_from_screen("old\nfirst\n\n🦀 last\n \n\n", 3), "first\n\n🦀 last");
    assert_eq!(preview_from_screen("\n \n", 8), "Ready. Waiting for input...");
    assert_eq!(preview_from_screen("one\ntwo", 1), "two");
}

#[test]
fn terminal_escape_stripping_preserves_only_visible_text() {
    let styled = concat!(
        "\x1b[1mBold\x1b[0m ",
        "\x1b[38;2;137;180;250mblue\x1b[39m\n",
        "\x1b]0;hidden title\x07next\tline\x1bPignored\x1b\\!"
    );
    assert_eq!(strip_terminal_escapes(styled), "Bold blue\nnext\tline!");
}

#[test]
fn test_nonexistent_session_exited() {
    let status = inspect_status("nonexistent_session_xyz_999", "agy");
    assert_eq!(status.status, "EXITED");
    assert_eq!(status.label, "○ EXITED");
}

#[test]
fn test_braille_spinner_range() {
    let spinners = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];
    for s in spinners {
        assert!(s >= '\u{2801}' && s <= '\u{28FF}', "Character {} should be in braille range", s);
    }
}

#[test]
fn test_shell_idle_and_working() {
    let sess = "test_sd_tmux_idle_test_session";
    let _session = new_session(sess, &["bash", "--norc", "--noprofile"]);

    let mut status_idle = inspect_status(sess, "bash");
    wait_until("the shell at its prompt to be IDLE", || {
        status_idle = inspect_status(sess, "bash");
        status_idle.status == "IDLE"
    });
    assert!(
        std::path::Path::new(&status_idle.cwd).is_dir(),
        "tmux must expose its live pane cwd, got {:?}",
        status_idle.cwd
    );
    let screen = capture_pane_text(sess).unwrap_or_default();
    assert_eq!(inspect_status_with_screen(sess, "bash", &screen).status, status_idle.status);

    // Send a sleep command
    tmux(&["send-keys", "-t", sess, "sleep 1.5", "Enter"]);
    std::thread::sleep(std::time::Duration::from_millis(300));

    let status_busy = inspect_status(sess, "bash");
    assert_eq!(status_busy.status, "WORKING", "Shell executing sleep should be WORKING");
    let screen = capture_pane_text(sess).unwrap_or_default();
    assert_eq!(inspect_status_with_screen(sess, "bash", &screen).status, status_busy.status);

    // Wait for sleep to complete
    wait_until("the shell to return to IDLE after the command", || {
        inspect_status(sess, "bash").status == "IDLE"
    });
}

#[test]
fn test_agent_idle_detection() {
    if which("agy").is_some() {
        let sess = "test_sd_tmux_agy_idle_test";
        let _session = new_session(sess, &["agy"]);

        // Wait for agent to finish initialization and arrive at the prompt
        let mut idle = false;
        for _ in 0..15 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let status = inspect_status(sess, "agy");
            if status.status == "IDLE" {
                assert_eq!(status.label, "● IDLE");
                idle = true;
                break;
            }
        }

        assert!(idle, "AI agent waiting at prompt must be detected as IDLE");
    }
}

#[test]
fn grok_default_launch_uses_native_scrollback() {
    let cfg = get_agent_config("grok");
    let command = with_default_args("/test/bin/grok", cfg.default_args);
    assert_eq!(command, "/test/bin/grok --minimal --dangerously-skip-permissions");
    assert!(!get_agent_config("codex").default_args.contains(&"--minimal"));
}

#[test]
fn mobile_scrollback_defaults_preserve_explicit_modes_and_permissions() {
    for (agent, command, expected) in [
        ("opencode", "/bin/opencode --auto", "/bin/opencode --auto --mini"),
        ("codex", "/bin/codex --dangerously-bypass-approvals-and-sandbox", "/bin/codex --dangerously-bypass-approvals-and-sandbox --no-alt-screen"),
        ("pi", "/bin/pi", "/bin/pi --tui-mode regular"),
        ("pi", "/bin/pi --tui-mode fullscreen", "/bin/pi --tui-mode fullscreen"),
        ("pi", "/bin/pi --tui-mode=fullscreen", "/bin/pi --tui-mode=fullscreen"),
        ("hermes", "/bin/hermes", "/bin/hermes --cli"),
        ("hermes", "/bin/hermes --tui", "/bin/hermes --tui"),
        ("grok", "/bin/grok --fullscreen --dangerously-skip-permissions", "/bin/grok --fullscreen --dangerously-skip-permissions"),
        ("opencode", "/bin/bash -l", "/bin/bash -l"),
    ] {
        let actual = resolve_command(agent, Some(command));
        assert_eq!(actual, expected, "{agent}");
        assert_eq!(resolve_command(agent, Some(&actual)), actual, "defaults must be idempotent");
    }
}

#[test]
fn saved_starting_parameters_replace_defaults_and_are_quoted() {
    let saved = vec!["--model".to_string(), "gpt 5".to_string()];
    // A restored card keeps its own value and gains only what it lacks.
    assert_eq!(resolve_command_with("aider", Some("/opt/aider --model x"), &saved), "/opt/aider --model x");
    assert_eq!(resolve_command_with("aider", Some("/opt/aider"), &saved), "/opt/aider --model 'gpt 5'");
    // A built-in default the user removed is not added back.
    assert_eq!(resolve_command_with("aider", Some("/opt/aider"), &[]), "/opt/aider");
    assert_eq!(resolve_command_with("aider", Some("/bin/bash"), &saved), "/bin/bash");
    assert_eq!(
        npx_fallback_command("reasonix", &["code", "a b"]),
        "npx -y reasonix code 'a b'"
    );
    // The built-in defaults read exactly as before quoting existed.
    assert_eq!(
        with_default_args("/usr/bin/pi", &crate::launch_args::builtin("pi")),
        "/usr/bin/pi --tui-mode regular"
    );
    assert_eq!(with_default_args::<&str>("/usr/bin/gemini", &[]), "/usr/bin/gemini");
}

#[test]
fn test_resolve_command_ai_agent_arguments() {
    // Antigravity (`agy`); without it the command falls back to the shell.
    if which("agy").is_some() || which("antigravity").is_some() {
        let agy_cmd = resolve_command("antigravity", None);
        assert!(agy_cmd.ends_with("--dangerously-skip-permissions"), "Antigravity must include --dangerously-skip-permissions, got: {}", agy_cmd);
    }

    // The flags come from the harness defaults, not from which binaries
    // happen to be on PATH (the release build container has none).
    for (agent, path, flag) in [
        ("claude", "/usr/bin/claude", "--dangerously-skip-permissions"),
        ("codex", "/usr/bin/codex", "--dangerously-bypass-approvals-and-sandbox"),
        ("opencode", "/usr/bin/opencode", "--auto"),
        ("grok", "/usr/bin/grok", "--dangerously-skip-permissions"),
    ] {
        let cmd = resolve_command(agent, Some(path));
        assert!(cmd.ends_with(flag), "{agent} must include {flag}, got: {cmd}");
    }

    // Shell (must not have AI permission flags)
    let shell_cmd = resolve_command("shell", None);
    assert!(!shell_cmd.contains("--dangerously-skip-permissions"));
    assert!(!shell_cmd.contains("--auto"));

    // Custom command for AI agent without flags gets flags appended
    let custom_agy = resolve_command("antigravity", Some("/custom/bin/agy"));
    assert_eq!(custom_agy, "/custom/bin/agy --dangerously-skip-permissions");

    // Custom command that already has flag does not duplicate it
    let custom_already = resolve_command("antigravity", Some("/custom/bin/agy --dangerously-skip-permissions"));
    assert_eq!(custom_already, "/custom/bin/agy --dangerously-skip-permissions");

    // Custom shell does not get AI flags
    let custom_bash = resolve_command("antigravity", Some("/bin/bash"));
    assert_eq!(custom_bash, "/bin/bash");
}

/// The one `tmux` call that replaced `session_exists` + a separate
/// `set-option` probe on the window-build path.
#[test]
fn test_session_state_reports_existence_and_pin_in_one_call() {
    if !has_tmux() {
        return;
    }
    let sess = "test_sd_session_state_probe";
    let _session = new_session(sess, &["-c", "/tmp", "/bin/bash"]);

    assert_eq!(session_state("test_sd_session_state_missing"), None);
    // Inherits the server config until pinned.
    tmux(&["set-option", "-t", sess, "detach-on-destroy", "off"]);
    assert_eq!(session_state(sess), Some(false));
    pin_client_exit(sess);
    assert_eq!(session_state(sess), Some(true));

    tmux(&["kill-session", "-t", sess]);
    assert_eq!(session_state(sess), None);
}

/// A new card's session is set up by one tmux process: pinned, with the
/// clipboard hook and bindings, never replacing a session that exists,
/// and a fresh card's command (not its resume form) runs exactly once.
/// Runs on a private tmux server in a child process.
#[test]
fn start_session_sets_up_a_card_in_one_tmux_process() {
    crate::test_isolation::rerun_in_private_root(
        "tmux::tests::start_session_inner",
        "SD_START_SESSION_ROOT",
        |root, child| {
            child.env("XDG_STATE_HOME", root.join("state"));
        },
    );
}

#[test]
fn start_session_inner() {
    let Some((root, _tmux)) = crate::test_isolation::private_root("SD_START_SESSION_ROOT") else {
        return;
    };
    let cwd = root.to_string_lossy().into_owned();
    let tmux = |args: &[&str]| {
        let out = Command::new("tmux").args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let start_command = |session: &str| {
        tmux(&["display-message", "-p", "-t", &format!("={session}:"), "#{pane_start_command}"])
    };

    let card = format!("sd_term_test_{}_a", std::process::id());
    assert_eq!(start_session(&card, "shell", "sleep 300", Some(&cwd)), Started::Yes);
    assert_eq!(session_state(&card), Some(true), "detach-on-destroy is pinned");
    let hooks = tmux(&["show-hooks", "-p", "-t", &format!("={card}:")]);
    assert!(hooks.contains("pane-set-clipboard"), "clipboard hook: {hooks:?}");
    let keys = tmux(&["list-keys"]);
    assert!(keys.contains("sd_term_*"), "copy bindings are wrapped for cards");

    // A name in use is refused, and the session keeps its command.
    assert_eq!(start_session(&card, "shell", "sleep 301", Some(&cwd)), Started::Duplicate);
    assert!(start_command(&card).contains("sleep 300"));

    // A brand-new card: its first attach starts the fresh command...
    let fresh = format!("sd_term_test_{}_b", std::process::id());
    let inventory = SessionInventory::fresh(&fresh, "shell", "sleep 302", &cwd);
    assert_eq!(session_state(&fresh), None);
    ensure_session_with_inventory(&fresh, "shell", Some("sleep 303"), None, Some(&cwd), Some(&inventory));
    assert_eq!(session_state(&fresh), Some(true));
    assert!(start_command(&fresh).contains("sleep 302"), "fresh command, not the resume form");
    // ...once: a later attach of the same card finds (or resumes) it.
    tmux(&["kill-session", "-t", &format!("={fresh}")]);
    ensure_session_with_inventory(&fresh, "shell", Some("sleep 303"), None, Some(&cwd), Some(&inventory));
    assert!(start_command(&fresh).contains("sleep 303"), "resumed with the saved command");
    // Another card's fresh session is not this one's.
    let other = SessionInventory::fresh("sd_term_someone_else", "shell", "sleep 304", &cwd);
    assert!(other.take_fresh(&fresh).is_none());

    // The public entry point: a new name, created before it returns.
    let (created, command) = create_session("shell", Some("sleep 305"), Some(&cwd));
    assert_eq!(command, "sleep 305");
    assert_eq!(session_state(&created), Some(true));
    assert_ne!(created, card);
}

#[test]
fn restoration_inventory_is_shared_and_not_a_global_cache() {
    let inventory = SessionInventory::default();
    inventory.0.set(Some(std::collections::HashMap::from([
        ("test_a".to_string(), true), ("test_b".to_string(), false),
    ]))).unwrap();
    assert_eq!(inventory.state("test_a"), Some(true));
    assert_eq!(inventory.state("test_b"), Some(false));
    assert_eq!(inventory.state("missing"), None);
    assert!(SessionInventory::default().0.get().is_none());
}

#[test]
fn test_which_lookup_is_in_process_and_needs_an_executable_bit() {
    let dir = std::env::temp_dir().join(format!("sd-which-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let file = dir.join("sd_fake_harness");
    std::fs::write(&file, "#!/bin/sh\n").expect("write fake harness");

    let path_var = dir.as_os_str();
    assert_eq!(
        which_in_path("sd_fake_harness", path_var),
        None,
        "a non-executable file must not resolve"
    );

    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");
    }
    assert_eq!(
        which_in_path("sd_fake_harness", path_var).as_deref(),
        file.to_str(),
        "an executable on PATH must resolve to its absolute path"
    );
    assert_eq!(which_in_path("sd_not_installed_xyz", path_var), None);
    assert_eq!(which("/definitely/not/here"), None);
    assert!(which(file.to_str().unwrap()).is_some(), "explicit paths are checked as-is");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_rescan_finds_new_user_bin_executable_without_restarting() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("sd-user-bin-{}", std::process::id()));
    let bin = home.join(".local/bin");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&bin).expect("create user bin");
    assert_eq!(which_in_common_user_bins("sd_new_harness", &home), None);
    let command = bin.join("sd_new_harness");
    std::fs::write(&command, "#!/bin/sh\n").expect("write executable");
    assert_eq!(which_in_common_user_bins("sd_new_harness", &home), None);
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))
        .expect("chmod +x");
    assert_eq!(which_in_common_user_bins("sd_new_harness", &home).as_deref(), command.to_str());
    std::fs::remove_dir_all(&home).expect("remove user bin");
    assert_eq!(which_in_common_user_bins("sd_new_harness", &home), None);
}

#[test]
fn test_new_catalog_uses_installed_cli_commands() {
    for (key, command, args) in [
        ("gemini", "gemini", &[][..]), ("hermes", "hermes", &["--cli"][..]),
        ("pi", "pi", &["--tui-mode", "regular"][..]), ("goose", "goose", &["session"][..]),
        ("qwen", "qwen", &[][..]), ("crush", "crush", &[][..]),
        ("kimi", "kimi", &[][..]), ("herder", "herder", &["worker"][..]),
        ("kiro", "kiro-cli", &[][..]), ("cursor", "cursor-agent", &[][..]),
    ] {
        assert!(HARNESS_KEYS.contains(&key));
        let cfg = get_agent_config(key);
        assert_eq!(cfg.commands, &[command]);
        assert_eq!(cfg.default_args, args);
        assert_eq!(cfg.npx_package, None);
    }
}

#[test]
fn test_reasonix_command_uses_code_and_no_permission_flag() {
    // Reasonix keeps its own `workspace-write` sandbox, so — unlike every
    // other AI harness here — no permission flag may be appended.
    let custom = resolve_command("reasonix", Some("/custom/bin/reasonix"));
    assert_eq!(custom, "/custom/bin/reasonix code");

    // A caller-supplied command that already names `code` is left alone.
    assert_eq!(
        resolve_command("reasonix", Some("npx -y reasonix code")),
        "npx -y reasonix code"
    );
}

#[test]
fn test_reasonix_resume_mirrors_opencode_fork() {
    assert_eq!(
        resolve_resume_command("reasonix", Some("/custom/bin/reasonix")),
        "/custom/bin/reasonix code --continue --copy"
    );
    // Existing resume flags are never duplicated; the missing half is added.
    assert_eq!(
        resolve_resume_command("reasonix", Some("/custom/bin/reasonix code --continue")),
        "/custom/bin/reasonix code --continue --copy"
    );
    // `--copy` alone gets completed with the missing `--continue`
    // (order follows the caller's command line, so assert on the flags).
    let only_copy =
        resolve_resume_command("reasonix", Some("/custom/bin/reasonix code --copy"));
    assert_eq!(only_copy.matches("--continue").count(), 1, "got: {only_copy}");
    assert_eq!(only_copy.matches("--copy").count(), 1, "got: {only_copy}");
}

#[test]
fn test_npx_fallback_avoids_install_prompt() {
    // `-y` keeps a card from blocking on npx's "Ok to proceed?" prompt.
    assert_eq!(
        npx_fallback_command("reasonix", &["code"]),
        "npx -y reasonix code"
    );
    assert_eq!(
        npx_fallback_command("reasonix", &["code", "--continue"]),
        "npx -y reasonix code --continue"
    );
}

#[test]
fn test_only_reasonix_declares_an_npx_fallback() {
    for agent in ["claude", "codex", "opencode", "grok", "aider", "dsh", "shell"] {
        assert!(
            get_agent_config(agent).npx_package.is_none(),
            "{agent} must not fall back to npx"
        );
    }
    assert_eq!(get_agent_config("reasonix").npx_package, Some("reasonix"));
}

#[test]
fn test_reasonix_resolution_prefers_path_then_npx() {
    // Whichever way Reasonix is installed, a card must never degrade to a
    // bare shell while `npx` is available.
    let resolved = resolve_command("reasonix", None);
    let has = |cmd: &str| {
        which(cmd).is_some()
    };
    if has("reasonix") {
        assert!(resolved.ends_with("code"), "got: {resolved}");
    } else if has("npx") {
        assert_eq!(resolved, "npx -y reasonix code");
    } else {
        assert_eq!(resolved, std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()));
    }
}

#[test]
fn test_dsh_launches_dsh_tui_then_dst_with_no_default_args() {
    let cfg = get_agent_config("dsh");
    assert_eq!((cfg.name, cfg.icon), ("DeepSeek Harness", "🐋"));
    assert_eq!(harness_candidates("dsh"), ["dsh-tui", "dst"]);
    // Permissions come from the dsh profile; there is no bypass flag.
    assert!(cfg.default_args.is_empty());
    // It needs the global dsh install and its profile, never `npx`.
    assert_eq!(cfg.npx_package, None);
    let at = |key: &str| HARNESS_KEYS.iter().position(|k| *k == key).unwrap();
    assert_eq!(at("dsh"), at("reasonix") + 1);

    // Preference order over whatever is installed, on a PATH of our own.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("sd-dsh-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["dst", "dsh-tui"] {
        std::fs::write(dir.join(name), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let lookup = |cmd: &str| which_in_path(cmd, dir.as_os_str());
    let found = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
    assert_eq!(first_installed(harness_candidates("dsh"), lookup), found("dsh-tui"));
    std::fs::remove_file(dir.join("dsh-tui")).unwrap();
    assert_eq!(first_installed(harness_candidates("dsh"), lookup), found("dst"));
    std::fs::remove_file(dir.join("dst")).unwrap();
    assert_eq!(first_installed(harness_candidates("dsh"), lookup), None);
    std::fs::remove_dir_all(&dir).unwrap();
    let installed = with_default_args(&found("dst").unwrap(), &crate::launch_args::builtin("dsh"));
    assert_eq!(installed, found("dst").unwrap(), "no arguments are appended");

    // A saved card or custom launcher command is kept as it is.
    assert_eq!(resolve_command("dsh", Some("/custom/bin/dsh-tui")), "/custom/bin/dsh-tui");
    assert_eq!(resolve_command("dsh", Some(" dst --resume abc ")), "dst --resume abc");
    assert_eq!(resolve_command("dsh", Some("/bin/bash")), "/bin/bash");
    // Starting parameters saved in Settings still reach it.
    let saved = vec!["--model".to_string(), "deepseek v4".to_string()];
    assert_eq!(resolve_command_with("dsh", Some("/opt/dsh-tui"), &saved), "/opt/dsh-tui --model 'deepseek v4'");
    // Whatever is installed here, a card never degrades to npx.
    assert!(!resolve_command("dsh", None).starts_with("npx"));
}

#[test]
fn test_dsh_relaunches_fresh_because_its_resume_target_is_global() {
    // A bare `dsh-tui --resume` reads one global ~/.dsh-tui/resume.txt,
    // so it would attach every restored card to the same session.
    assert_eq!(resolve_resume_command("dsh", Some("/usr/bin/dsh-tui")), "/usr/bin/dsh-tui");
    assert_eq!(resolve_resume_command("dsh", Some("/usr/bin/dst")), "/usr/bin/dst");
    // An explicit session a user put into the command is kept verbatim.
    assert_eq!(
        resolve_resume_command("dsh", Some("/usr/bin/dsh-tui --resume 1a2b3c4d")),
        "/usr/bin/dsh-tui --resume 1a2b3c4d"
    );
    assert_eq!(resolve_resume_command_with_session("dsh", Some("/usr/bin/dst"), Some("ses_x")), "/usr/bin/dst");
}

#[test]
fn test_truncate_prompt_title_caps_length() {
    let short = "fix bug";
    assert_eq!(truncate_prompt_title(short), "fix bug");
    let long = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim";
    let out = truncate_prompt_title(long);
    assert_eq!(out.chars().count(), PROMPT_TITLE_MAX_CHARS);
    assert!(out.ends_with('…'));
    assert!(out.starts_with("lorem ipsum dolor sit amet"));
}

#[test]
fn test_extract_composer_draft_typing() {
    let screen = "┃\n┃  fix the login bug please\n┃\n┃  Build · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n tab agents  ctrl+p commands\n";
    assert_eq!(
        extract_composer_draft(screen).as_deref(),
        Some("fix the login bug please")
    );
}

#[test]
fn test_extract_composer_draft_empty_and_busy() {
    // Empty composer: placeholder + model line only.
    let empty = "┃\n┃  Ask anything… \"Fix broken tests\"\n┃\n┃  Build · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n tab agents  ctrl+p commands\n";
    assert_eq!(extract_composer_draft(empty), None);

    // Busy agent: empty box, interrupt hint lives below the divider.
    let busy = "┃\n┃\n┃\n┃  Build auto · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n ⬝⬝■■  esc interrupt   131.5K (13%)  ctrl+p commands\n";
    assert_eq!(extract_composer_draft(busy), None);

    // Activity status line above the box must not leak into the title.
    let with_status = " ▣  Build · Muse Spark 1.3 Free · 28m 26s\n┃\n┃\n┃\n┃  Build auto · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n";
    assert_eq!(extract_composer_draft(with_status), None);

    // No divider at all (plain shell screen).
    assert_eq!(extract_composer_draft("~/project ❯ cargo test\n"), None);
}

#[test]
fn test_extract_composer_draft_status_plus_real_draft() {
    // Status line above the box is skipped; the real bordered draft wins.
    let screen = " ▣  Build · Muse Spark 1.3 Free · 28m 26s\n┃\n┃  can you restore the setup\n┃\n┃  Build auto · Muse Spark 1.3 Free OpenCode Zen · high\n╹▀▀▀▀▀▀▀▀▀▀\n";
    assert_eq!(
        extract_composer_draft(screen).as_deref(),
        Some("can you restore the setup")
    );
}

#[test]
fn test_extract_composer_draft_keeps_build_draft() {
    // A real draft starting with "Build" has no `·`, so it must match.
    let screen = "┃\n┃  Build me a dashboard\n┃\n┃  Build · Provider · high\n╹▀▀▀▀▀\n";
    assert_eq!(
        extract_composer_draft(screen).as_deref(),
        Some("Build me a dashboard")
    );
}

#[test]
fn test_sql_escape_doubles_quotes() {
    assert_eq!(sql_escape("a'b"), "a''b");
    assert_eq!(sql_escape("/home/gladimdim"), "/home/gladimdim");
}

#[test]
fn test_assign_opencode_sessions_chronological() {
    // Real-world shape: 4 same-directory cards created ~35s apart, each
    // opencode session born 20-55s after its pane. Nearest-matching would
    // shift cards onto neighbours; chronological 1:1 must hold.
    let panes = vec![
        ("sd_term_848323".to_string(), 1789580848),
        ("sd_term_883125".to_string(), 1789580883),
        ("sd_term_931822".to_string(), 1789580931),
        ("sd_term_278233".to_string(), 1789581278),
    ];
    let sessions = vec![
        ("ses_hover".to_string(), 1789580880),
        ("ses_title".to_string(), 1789580927),
        ("ses_120fps".to_string(), 1789580985),
        ("ses_dedup".to_string(), 1789581297),
    ];
    let got = assign_opencode_sessions(&panes, &sessions);
    let ids: Vec<Option<String>> =
        got.into_iter().map(|(_, s)| s.map(|(id, _)| id)).collect();
    assert_eq!(
        ids,
        vec![
            Some("ses_hover".to_string()),
            Some("ses_title".to_string()),
            Some("ses_120fps".to_string()),
            Some("ses_dedup".to_string()),
        ]
    );
}

#[test]
fn test_extract_session_flag() {
    assert_eq!(
        extract_session_flag("/usr/bin/opencode --auto --session ses_abc123"),
        Some("ses_abc123".to_string())
    );
    assert_eq!(
        extract_session_flag("/usr/bin/opencode --session=ses_xyz --auto"),
        Some("ses_xyz".to_string())
    );
    assert_eq!(extract_session_flag("/usr/bin/opencode --auto"), None);
    assert_eq!(extract_session_flag("/usr/bin/opencode --session"), None);
    assert_eq!(extract_session_flag("/usr/bin/opencode --session="), None);
    assert_eq!(extract_session_flag("/usr/bin/bash"), None);
}

#[test]
fn test_keep_opencode_panes_drops_other_harnesses() {
    // Regression: a reasonix card consumed an opencode session in the
    // chronological assignment, shifting an opencode card onto its
    // neighbour's session — the neighbour's prompt then showed in the
    // wrong card's title.
    let panes = vec![
        ("sd_term_a".to_string(), 1000),
        ("sd_term_r".to_string(), 1100),
        ("sd_term_b".to_string(), 1200),
        ("sd_term_unknown".to_string(), 1300),
    ];
    let mut info = std::collections::HashMap::new();
    info.insert("sd_term_a".to_string(), ("opencode".to_string(), None));
    info.insert(
        "sd_term_r".to_string(),
        ("reasonix".to_string(), None),
    );
    info.insert("sd_term_b".to_string(), ("opencode".to_string(), None));
    info.insert("sd_term_shell".to_string(), ("shell".to_string(), None));
    let kept = keep_opencode_panes(&panes, &info);
    let names: Vec<&str> = kept.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["sd_term_a", "sd_term_b", "sd_term_unknown"]);
}

#[test]
fn test_non_opencode_pane_cannot_steal_opencode_session() {
    // The reported bug, end to end at the pure level: panes A (opencode),
    // R (reasonix) and B (opencode) created in that order; opencode
    // sessions S1/S2/S3 born right after each *opencode* pane (TUI lag).
    // With R participating, R steals S2 and B shifts onto S3 (another
    // console's session); filtered to opencode panes, A→S1 and B→S2.
    let panes = vec![
        ("sd_term_a".to_string(), 1000),
        ("sd_term_r".to_string(), 1100),
        ("sd_term_b".to_string(), 1200),
    ];
    let sessions = vec![
        ("s1".to_string(), 1020),
        ("s2".to_string(), 1237),
        ("s3".to_string(), 1300),
    ];
    // Old behaviour (all panes participate): R steals S2, B lands on S3.
    let all = assign_opencode_sessions(&panes, &sessions);
    let id_of_all = |pane: &str| {
        all.iter()
            .find(|(p, _)| p == pane)
            .and_then(|(_, s)| s.as_ref().map(|(id, _)| id.as_str()))
    };
    assert_eq!(id_of_all("sd_term_r"), Some("s2"));
    assert_eq!(id_of_all("sd_term_b"), Some("s3"));
    // Fixed behaviour: only opencode panes participate.
    let mut info = std::collections::HashMap::new();
    info.insert("sd_term_a".to_string(), ("opencode".to_string(), None));
    info.insert("sd_term_r".to_string(), ("reasonix".to_string(), None));
    info.insert("sd_term_b".to_string(), ("opencode".to_string(), None));
    let filtered = keep_opencode_panes(&panes, &info);
    let got = assign_opencode_sessions(&filtered, &sessions);
    let id_of = |pane: &str| {
        got.iter()
            .find(|(p, _)| p == pane)
            .and_then(|(_, s)| s.as_ref().map(|(id, _)| id.clone()))
    };
    assert_eq!(id_of("sd_term_a").as_deref(), Some("s1"));
    assert_eq!(id_of("sd_term_b").as_deref(), Some("s2"));
}

#[test]
fn test_drop_determined_panes() {
    // Flag-owned pairs leave the ranking entirely: the satisfied pane no
    // longer shifts its neighbours, and its session cannot be stolen.
    let panes = vec![
        ("a".to_string(), 1000),
        ("b".to_string(), 1200),
    ];
    let sessions = vec![
        ("s1".to_string(), 1020),
        ("s2".to_string(), 1237),
    ];
    let mut flags = std::collections::HashMap::new();
    flags.insert("a".to_string(), "s1".to_string());
    let (free_panes, free_sessions) = drop_determined_panes(&panes, &sessions, &flags);
    assert_eq!(free_panes, vec![("b".to_string(), 1200)]);
    assert_eq!(free_sessions, vec![("s2".to_string(), 1237)]);
    let got = assign_opencode_sessions(&free_panes, &free_sessions);
    assert_eq!(
        got[0].1.as_ref().map(|(id, _)| id.as_str()),
        Some("s2")
    );
}

#[test]
fn test_should_adopt_heuristic_match() {
    // Stale guess S3 (+305s) vs nearer live match S2 (+37s): adopt.
    assert!(should_adopt_heuristic_match(Some(7305), 7037, 7000));
    // Persisted is nearer: keep.
    assert!(!should_adopt_heuristic_match(Some(7037), 7305, 7000));
    // Tie is not "strictly nearer": keep (no flapping).
    assert!(!should_adopt_heuristic_match(Some(7100), 6900, 7000));
    // Unknown birth (pre-reboot ground truth or garbage): always keep.
    // A newer heuristic hit there belongs to somebody else, and garbage
    // safely yields no DB text (own-pane text wins instead).
    assert!(!should_adopt_heuristic_match(None, 7037, 7000));
}

#[test]
fn test_assign_opencode_sessions_shortage_and_grace() {
    // More panes than sessions: last pane gets None.
    let panes = vec![
        ("a".to_string(), 1000),
        ("b".to_string(), 1100),
        ("c".to_string(), 1200),
    ];
    let sessions = vec![("s1".to_string(), 1020)];
    let got = assign_opencode_sessions(&panes, &sessions);
    assert_eq!(got[0].1.as_ref().map(|(id, _)| id.as_str()), Some("s1"));
    assert_eq!(got[1].1, None);
    assert_eq!(got[2].1, None);

    // Session born up to 30s before the pane still matches (clock skew);
    // older ones do not.
    let panes = vec![("a".to_string(), 1000)];
    let sessions = vec![("s1".to_string(), 969)];
    assert_eq!(assign_opencode_sessions(&panes, &sessions)[0].1, None);
    let sessions = vec![("s1".to_string(), 970)];
    assert!(assign_opencode_sessions(&panes, &sessions)[0].1.is_some());
}

/// Every restored Claude or Codex card resumes its own conversation, not
/// the latest one in its folder, while that conversation still exists.
#[test]
fn claude_and_codex_cards_resume_their_own_conversation() {
    let id = "0b8c4a7e-5d0f-4b61-9a43-2f7c1d9e6a10";
    let all = |_: &str, _: &str| true;
    let none = |_: &str, _: &str| false;
    // The launch command with this machine's default flags, which the
    // resume form only extends.
    let claude = resolve_command("claude", Some("/usr/bin/claude"));
    let codex = resolve_command("codex", Some("/usr/bin/codex --no-alt-screen"));
    assert_eq!(
        resume_command_with("claude", Some("/usr/bin/claude"), Some(id), &all),
        format!("{claude} --resume {id}")
    );
    assert_eq!(
        resume_command_with("codex", Some("/usr/bin/codex --no-alt-screen"), Some(id), &all),
        format!("{codex} resume {id}")
    );
    // Gone (deleted transcript, other machine): the old fallback, never a
    // harness that exits on an unknown id.
    assert_eq!(
        resume_command_with("claude", Some("/usr/bin/claude"), Some(id), &none),
        format!("{claude} --continue")
    );
    assert_eq!(
        resume_command_with("codex", Some("/usr/bin/codex --no-alt-screen"), Some(id), &none),
        format!("{codex} resume --last")
    );
    // No id stored yet (a card from an older build).
    assert_eq!(
        resume_command_with("claude", Some("/usr/bin/claude"), None, &all),
        format!("{claude} --continue")
    );
    // Ids reach a shell command line: anything but [A-Za-z0-9_-] is ignored.
    for bad in ["x; rm -rf ~", "$(id)", "a b", "", "'q'"] {
        assert_eq!(
            resume_command_with("claude", Some("/usr/bin/claude"), Some(bad), &all),
            format!("{claude} --continue"),
            "{bad:?}"
        );
    }
    // The user's own resume flag is kept as written.
    let own = resolve_command("claude", Some("/usr/bin/claude --resume other"));
    assert!(own.contains("--resume other"), "{own}");
    assert_eq!(
        resume_command_with("claude", Some("/usr/bin/claude --resume other"), Some(id), &all),
        own
    );
    // Only the harness's own id is asked about.
    let asked = std::cell::RefCell::new(Vec::new());
    resume_command_with("codex", Some("/usr/bin/codex"), Some(id), &|agent, id| {
        asked.borrow_mut().push((agent.to_string(), id.to_string()));
        true
    });
    assert_eq!(asked.into_inner(), vec![("codex".to_string(), id.to_string())]);
}

#[test]
fn conversation_files_are_found_where_each_cli_looks() {
    let root = std::env::temp_dir().join(format!("sd-resume-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let id = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";

    // Claude: the folder's project directory, letters and digits kept.
    let transcript = claude_transcript(&root, "/home/u/Github/super.desktop_2", id);
    assert_eq!(
        transcript,
        root.join("projects/-home-u-Github-super-desktop-2").join(format!("{id}.jsonl"))
    );

    // Codex: sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl, any day.
    let sessions = root.join("sessions");
    let day = sessions.join("2026/10/04");
    std::fs::create_dir_all(&day).unwrap();
    assert!(!codex_rollout_exists(&sessions, id));
    std::fs::write(day.join(format!("rollout-2026-10-04T09-00-00-{id}.jsonl")), "{}").unwrap();
    assert!(codex_rollout_exists(&sessions, id));
    assert!(!codex_rollout_exists(&sessions, "019a1b2c"), "a prefix is not the id");
    assert!(!codex_rollout_exists(&root.join("missing"), id));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn test_resolve_resume_command_per_agent() {
    // Deterministic: explicit binary paths, no `which` dependence.
    let claude = resolve_resume_command("claude", Some("/usr/bin/claude"));
    assert!(claude.ends_with("--continue"), "got: {claude}");
    assert!(claude.contains("--dangerously-skip-permissions"));

    let codex = resolve_resume_command("codex", Some("/usr/bin/codex"));
    assert!(codex.ends_with("resume --last"), "got: {codex}");
    assert!(codex.contains("--dangerously-bypass-approvals-and-sandbox"));

    let opencode = resolve_resume_command("opencode", Some("/usr/bin/opencode"));
    // No stored id -> forked continue (isolated copy, never shared live).
    assert!(opencode.contains("--continue"), "got: {opencode}");
    assert!(opencode.ends_with("--fork"), "got: {opencode}");
    assert!(opencode.contains("--auto"));

    // Stored per-card id -> exact session resume.
    let opencode_exact = resolve_resume_command_with_session(
        "opencode",
        Some("/usr/bin/opencode"),
        Some("ses_abc123"),
    );
    assert!(
        opencode_exact.ends_with("--session ses_abc123"),
        "got: {opencode_exact}"
    );

    let aider = resolve_resume_command("aider", Some("/usr/bin/aider"));
    assert!(aider.ends_with("--restore-chat-history"), "got: {aider}");
    assert!(aider.contains("--yes-always"));

    // No resume mechanism -> fresh launch unchanged.
    assert_eq!(resolve_resume_command("shell", Some("/bin/bash")), "/bin/bash");
    let grok = resolve_resume_command("grok", Some("/usr/bin/grok"));
    assert!(
        !grok.contains("resume") && !grok.contains("continue"),
        "got: {grok}"
    );
    let agy = resolve_resume_command("antigravity", Some("/usr/bin/agy"));
    assert!(
        !agy.contains("resume") && !agy.contains("continue"),
        "got: {agy}"
    );
}

#[test]
fn test_resolve_resume_command_idempotent_and_shell_safe() {
    // Already-resuming commands are returned untouched (no flag pile-up).
    assert_eq!(
        resolve_resume_command(
            "claude",
            Some("/usr/bin/claude --dangerously-skip-permissions --continue")
        ),
        "/usr/bin/claude --dangerously-skip-permissions --continue"
    );
    let codex_resumed =
        resolve_resume_command("codex", Some("/usr/bin/codex resume --last"));
    assert!(codex_resumed.contains("resume --last"));
    assert_eq!(codex_resumed.matches("resume").count(), 1);
    // Opencode fallback is idempotent: no flag pile-up on second reboot.
    let oc_once = resolve_resume_command("opencode", Some("/usr/bin/opencode"));
    let oc_twice = resolve_resume_command("opencode", Some(&oc_once));
    assert_eq!(oc_once, oc_twice, "got: {oc_twice}");
    assert_eq!(oc_twice.matches("--fork").count(), 1);
    assert_eq!(oc_twice.matches("--continue").count(), 1);
    // Explicit shells are never decorated, even for resumable agents.
    assert_eq!(
        resolve_resume_command("claude", Some("/bin/bash")),
        "/bin/bash"
    );
    assert_eq!(
        resolve_resume_command("opencode", Some("/bin/zsh")),
        "/bin/zsh"
    );
}

#[test]
fn test_ensure_session_recreates_with_resume_command() {
    // End-to-end reboot simulation: kill the tmux session, then verify
    // ensure_session() recreates it with the agent's resume command.
    // python3 tolerates trailing CLI flags (they land in sys.argv) and
    // sleeps, so the pane stays alive long enough to inspect.
    let has_py = which("python3").is_some();
    if !has_tmux() || !has_py {
        return;
    }
    for (sess, agent, marker) in [
        ("test_sd_resume_claude", "claude", "--continue"),
        ("test_sd_resume_codex", "codex", "resume --last"),
    ] {
        tmux(&["kill-session", "-t", sess]);
        let _cleanup = SessionCleanup(vec![sess.to_string()]);
        ensure_session(
            sess,
            agent,
            Some("python3 -c 'import time; time.sleep(30)'"),
            None,
        );
        let pane_cmdline = || read_cmdline(tmux_display(sess, "#{pane_pid}")?.parse().ok()?);
        wait_until(&format!("recreated {sess} to run the resume command"), || {
            pane_cmdline().is_some_and(|cmdline| cmdline.contains(marker) && cmdline.contains("sleep"))
        });
    }
}

#[test]
fn test_unique_session_names_never_collide() {
    // Rapid creation (same-millisecond) must still yield distinct names:
    // the old `millis % 1_000_000` scheme attached both cards to one tmux
    // session, mixing output/Ctrl+C across terminals.
    let mut names = std::collections::HashSet::new();
    for _ in 0..50 {
        let n = unique_session_name();
        assert!(n.starts_with("sd_term_"), "got: {n}");
        assert!(names.insert(n.clone()), "duplicate session name: {n}");
    }
}

/// Read tmux option output: `tmux show-options <args>`.
fn show_option(args: &[&str]) -> String {
    tmux_stdout(&[&["show-options"], args].concat()).unwrap_or_default().trim().to_string()
}

/// Session-local value of `detach-on-destroy`; empty when the session has
/// no override of its own (i.e. it inherits the user's global setting).
fn session_detach_on_destroy(sess: &str) -> String {
    show_option(&["-t", sess, "-v", "detach-on-destroy"])
}

#[test]
fn test_resolve_workspace_dir_prefers_a_real_folder() {
    // Pure: no tmux needed.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    assert_eq!(resolve_workspace_dir(None), home);
    assert_eq!(resolve_workspace_dir(Some("")), home);
    assert_eq!(resolve_workspace_dir(Some("   ")), home);
    // A folder that no longer exists (deleted checkout, unmounted drive)
    // must not produce a session that starts nowhere.
    assert_eq!(resolve_workspace_dir(Some("/definitely/gone/xyz")), home);
    assert_eq!(resolve_workspace_dir(Some("/tmp")), "/tmp");
}

#[test]
fn test_pane_grid_is_the_owned_pane_and_nothing_else() {
    // Pure: a session this bridge does not own is never resolved or probed.
    assert!(pane_grid("other_session").is_none());
    assert!(pane_grid("sd_term_bad name").is_none());
    assert!(pane_grid("").is_none());

    if !has_tmux() {
        return;
    }
    let (sess, _cmd) = create_session("shell", Some("/bin/bash"), None);
    let _cleanup = SessionCleanup(vec![sess.clone()]);
    let grid = pane_grid(&sess).expect("an owned pane has a grid");
    // It is the pane's own size — the width its captured text is rendered
    // at — not a client's, which would include the status line.
    let reported = tmux_display(&sess, "#{pane_width} #{pane_height}").expect("pane size");
    let mut fields = reported.split_whitespace();
    assert_eq!(grid.columns.to_string(), fields.next().unwrap());
    assert_eq!(grid.rows.to_string(), fields.next().unwrap());
    assert!(grid.validate().is_ok());
}

#[test]
fn test_harness_sessions_start_in_the_workspace_folder() {
    // The whole point of the top bar's workspace field: a harness card must
    // actually run in that folder, because every agent scopes its own
    // history/resume to the cwd — and Reasonix keys its workspace write
    // lease on it, which is what makes parallel cards collide at `$HOME`.
    if !has_tmux() {
        return;
    }

    let dir = std::env::temp_dir().join(format!("sd-workspace-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let dir_s = std::fs::canonicalize(&dir)
        .unwrap_or(dir.clone())
        .to_string_lossy()
        .into_owned();

    let pane_dir = |sess: &str| -> Option<String> {
        for _ in 0..40 {
            if let Some(p) = tmux_display(sess, "#{pane_current_path}") {
                // The pane reports the path it physically started in.
                let resolved = std::fs::canonicalize(p.trim())
                    .map(|c| c.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| p.trim().to_string());
                if resolved == dir_s {
                    return Some(resolved);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    };

    // 1. A new card runs in the folder from the top bar.
    let (sess, _cmd) = create_session("shell", Some("/bin/bash"), Some(&dir_s));
    let mut cleanup = SessionCleanup(vec![sess.clone()]);
    assert_eq!(
        pane_dir(&sess).as_deref(),
        Some(dir_s.as_str()),
        "a new harness must start in the workspace folder, not $HOME"
    );

    // 2. The same card after a tmux server restart (reboot) resumes in that
    //    same folder — resuming in another cwd would attach the card to a
    //    different history.
    tmux(&["kill-session", "-t", &sess]);
    ensure_session_with_agent_id(&sess, "shell", Some("/bin/bash"), None, Some(&dir_s));
    assert_eq!(
        pane_dir(&sess).as_deref(),
        Some(dir_s.as_str()),
        "a restored harness must resume in its own folder"
    );

    // 3. With nothing configured the old behaviour stands: $HOME.
    let (sess_home, _cmd) = create_session("shell", Some("/bin/bash"), None);
    cleanup.0.push(sess_home.clone());
    let home_s = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let home_s = std::fs::canonicalize(&home_s)
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or(home_s);
    let pane_home = {
        let mut found = None;
        for _ in 0..40 {
            if let Some(p) = tmux_display(&sess_home, "#{pane_current_path}") {
                found = Some(
                    std::fs::canonicalize(p.trim())
                        .map(|c| c.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| p.trim().to_string()),
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        found
    };
    assert_eq!(
        pane_home.as_deref(),
        Some(home_s.as_str()),
        "an unset folder must still start in $HOME"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_client_exits_with_its_own_session() {
    // Regression: with `set -g detach-on-destroy off` (omarchy's tmux
    // default) a client whose session is destroyed is switched to another
    // session instead of exiting. Each card here owns one tmux client, so a
    // card that lost its session rendered a DIFFERENT harness and the next
    // Ctrl-C typed into it killed that harness: closing one harness looked
    // like all of them closing. Sessions we create must pin the option
    // per session, without touching the user's global value.
    //
    // Start the server first: it (and its global options, i.e. the user's
    // config) only exist once a session has been created.
    let probe = "test_sd_detach_on_destroy_probe";
    let mut cleanup = SessionCleanup(vec![probe.to_string()]);
    tmux(&["new-session", "-d", "-s", probe, "-c", "/tmp", "/bin/bash"]);
    let global_before = show_option(&["-gv", "detach-on-destroy"]);
    assert!(
        !global_before.is_empty(),
        "tmux server must be reachable for this test"
    );

    // tmux < 3.2 kept detach-on-destroy as a server option, where the
    // per-session form is rejected; the fix is then a no-op (and so is
    // this test).
    tmux(&["set-option", "-t", probe, "detach-on-destroy", "off"]);
    if session_detach_on_destroy(probe).is_empty() {
        eprintln!("skipping: this tmux has no session-scoped detach-on-destroy");
        return;
    }

    let (sess, _cmd) = create_session("shell", Some("/bin/bash"), None);
    cleanup.0.push(sess.clone());
    assert_eq!(
        session_detach_on_destroy(&sess),
        "on",
        "new harness session must pin detach-on-destroy"
    );
    assert_eq!(
        show_option(&["-gv", "detach-on-destroy"]),
        global_before,
        "the user's global tmux config must stay untouched"
    );

    // Restored/legacy sessions (or ones created before this fix) inherit
    // the config value; startup must re-pin them too.
    tmux(&["set-option", "-t", &sess, "detach-on-destroy", "off"]);
    assert_eq!(session_detach_on_destroy(&sess), "off");
    ensure_session_with_agent_id(&sess, "shell", Some("/bin/bash"), None, None);
    assert_eq!(
        session_detach_on_destroy(&sess),
        "on",
        "restored harness session must be re-pinned"
    );
    assert_eq!(show_option(&["-gv", "detach-on-destroy"]), global_before);
}

#[test]
fn test_harness_candidates_prefer_own_binary() {
    // A harness is only detected through its own binary…
    assert_eq!(harness_candidates("claude"), ["claude"]);
    assert_eq!(harness_candidates("antigravity"), ["agy", "antigravity"]);
    assert_eq!(harness_candidates("dsh"), ["dsh-tui", "dst"]);
    // …while a terminal card accepts any POSIX shell on the box.
    let shell = harness_candidates("shell");
    assert!(shell.contains(&"bash") && shell.contains(&"zsh") && shell.contains(&"fish"));
}

#[test]
fn test_detected_harnesses_are_really_launchable() {
    // Whatever the settings panel offers must resolve to something that
    // exists here, and must agree with what a card would run.
    for info in detect_harnesses() {
        let bin = info.command.split_whitespace().next().unwrap_or_default();
        assert!(
            std::path::Path::new(bin).is_file() || bin == "npx",
            "{} resolved to a command that is not here: {}",
            info.key,
            info.command
        );
        assert_eq!(detect_harness_command(info.key).as_deref(), Some(info.command.as_str()));
        assert!(HARNESS_KEYS.contains(&info.key), "{} is not a supported harness", info.key);
    }
    // Detection is a filter of the declared order, never a reordering.
    let keys: Vec<&str> = detect_harnesses().iter().map(|h| h.key).collect();
    let expected: Vec<&str> = HARNESS_KEYS
        .iter()
        .copied()
        .filter(|k| keys.contains(k))
        .collect();
    assert_eq!(keys, expected);
    assert!(keys.len() <= HARNESS_KEYS.len());
}

#[test]
fn test_supported_harnesses_all_have_their_own_config() {
    // Every key shown in the top bar / settings panel needs its own agent
    // config; falling through to the `_` arm would label it "Terminal".
    for key in HARNESS_KEYS {
        let cfg = get_agent_config(key);
        assert!(!cfg.name.is_empty() && !cfg.icon.is_empty(), "{key} has no label");
        if *key != "shell" {
            assert_ne!(cfg.name, "Terminal", "{key} needs its own AgentConfig arm");
        }
    }
    // A terminal card can always be launched, even on a bash-less box.
    assert!(detect_harness_command("shell").is_some());
}

#[test]
fn test_resume_with_stored_opencode_session() {
    let cmd = resolve_resume_command_with_session(
        "opencode",
        Some("/usr/bin/opencode --auto"),
        Some("ses_f51a6ff90ffe11x52WFauyf57C"),
    );
    assert!(cmd.contains("--session ses_f51a6ff90ffe11x52WFauyf57C"));
    assert!(!cmd.contains("--continue"), "got: {cmd}");
    assert!(!cmd.contains("--fork"), "got: {cmd}");
    // Empty/blank stored ids fall back to forked continue.
    let fallback =
        resolve_resume_command_with_session("opencode", Some("/usr/bin/opencode"), Some("  "));
    assert!(fallback.contains("--continue") && fallback.contains("--fork"));
}

fn stamp(activity: u64) -> PaneStamp {
    PaneStamp {
        pane_id: "%3".into(),
        pid: "4242".into(),
        width: 80,
        height: 24,
        history_size: 12,
        cursor: (2, 23),
        alternate: false,
        activity,
    }
}

fn at(seconds: f64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_secs_f64(seconds)
}

#[test]
fn capture_reuse_needs_an_unchanged_pane_and_a_later_second() {
    let cached = stamp(1_000);
    // Last output at 1000.x, captured at 1001.2: nothing can have arrived
    // after the capture without moving window_activity past 1000.
    assert!(capture_reusable(&cached, at(1_001.2), &stamp(1_000), at(1_002.5)));
    // Captured in the second of the last output: more output later in
    // that second would leave window_activity at 1000.
    assert!(!capture_reusable(&cached, at(1_000.9), &stamp(1_000), at(1_002.5)));
    assert!(!capture_reusable(&cached, at(1_000.0), &stamp(1_000), at(1_001.0)));
    // Output after the capture.
    assert!(!capture_reusable(&cached, at(1_001.2), &stamp(1_001), at(1_002.5)));
    assert!(!capture_reusable(&cached, at(1_001.2), &stamp(1_003), at(1_003.5)));
    // Changes that print nothing: resize (reflow), clear-history,
    // respawn-pane, another pane, a reset that moves the cursor, and
    // switching screens.
    let changed = [
        PaneStamp { width: 60, ..stamp(1_000) },
        PaneStamp { height: 30, ..stamp(1_000) },
        PaneStamp { history_size: 0, ..stamp(1_000) },
        PaneStamp { pid: "4343".into(), ..stamp(1_000) },
        PaneStamp { pane_id: "%4".into(), ..stamp(1_000) },
        PaneStamp { cursor: (0, 0), ..stamp(1_000) },
        PaneStamp { alternate: true, ..stamp(1_000) },
    ];
    for current in &changed {
        assert!(!capture_reusable(&cached, at(1_001.2), current, at(1_002.5)), "{current:?}");
    }
    // Re-read at least every CAPTURE_REUSE_MAX, and after a clock step back.
    let max = CAPTURE_REUSE_MAX.as_secs_f64();
    assert!(capture_reusable(&cached, at(1_001.2), &stamp(1_000), at(1_001.2 + max - 0.1)));
    assert!(!capture_reusable(&cached, at(1_001.2), &stamp(1_000), at(1_001.2 + max)));
    assert!(!capture_reusable(&cached, at(1_001.2), &stamp(1_000), at(1_001.1)));
}

#[test]
fn unchanged_capture_is_reused_per_session_and_kind() {
    let cache = std::sync::Mutex::new(CaptureCache::new());
    let runs = std::cell::Cell::new(0);
    let capture = |text: &'static str| {
        let runs = &runs;
        move || {
            runs.set(runs.get() + 1);
            Some(text.to_string())
        }
    };
    // Output long enough ago that any capture now is in a later second.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let idle = stamp(now - 5);
    let first = reuse_or_capture(&cache, "sd_term_a", Some(&idle), CaptureKind::Card, capture("one"));
    let again = reuse_or_capture(&cache, "sd_term_a", Some(&idle), CaptureKind::Card, capture("two"));
    assert_eq!((first.as_deref(), again.as_deref(), runs.get()), (Some("one"), Some("one"), 1));
    // Each kind and session has its own capture.
    reuse_or_capture(&cache, "sd_term_a", Some(&idle), CaptureKind::Visible, capture("v"));
    reuse_or_capture(&cache, "sd_term_b", Some(&idle), CaptureKind::Card, capture("b"));
    assert_eq!(runs.get(), 3);
    // New output captures again and replaces the entry.
    let busy = stamp(now - 1);
    let fresh = reuse_or_capture(&cache, "sd_term_a", Some(&busy), CaptureKind::Card, capture("three"));
    assert_eq!((fresh.as_deref(), runs.get()), (Some("three"), 4));
    assert_eq!(reuse_or_capture(&cache, "sd_term_a", Some(&busy), CaptureKind::Card, capture("x")).as_deref(), Some("three"));
    assert_eq!(runs.get(), 4);
    // Output in the second a capture is taken is never trusted.
    let current = stamp(now + 60);
    reuse_or_capture(&cache, "sd_term_c", Some(&current), CaptureKind::Card, capture("c"));
    reuse_or_capture(&cache, "sd_term_c", Some(&current), CaptureKind::Card, capture("c"));
    assert_eq!(runs.get(), 6);
    // No stamp (split window, older tmux): always capture, never cache.
    reuse_or_capture(&cache, "sd_term_d", None, CaptureKind::Card, capture("d"));
    reuse_or_capture(&cache, "sd_term_d", None, CaptureKind::Card, capture("d"));
    assert_eq!(runs.get(), 8);
    // A failed capture is not remembered.
    let failed = reuse_or_capture(&cache, "sd_term_e", Some(&idle), CaptureKind::Card, || None);
    assert!(failed.is_none());
    reuse_or_capture(&cache, "sd_term_e", Some(&idle), CaptureKind::Card, capture("e"));
    assert_eq!(runs.get(), 9);
    // Panes gone from the inventory, or no longer stamped, are forgotten.
    let fields = |session: &str, active: &str| {
        let mut row = vec![session, "1", "4242", "codex", "0", "24", "/tmp", "", "", "", ""];
        row.extend(["%3", active, "80", "12", "2", "23", "0", "1000", "0"]);
        format!("\u{1e}{}\n", row.join("\u{1f}"))
    };
    let listing = [fields("sd_term_a", "1"), fields("sd_term_b", "0")].concat();
    retain_captures_in(&cache, &parse_pane_snapshot(&listing));
    let mut kept: Vec<_> = cache.lock().unwrap().keys().cloned().collect();
    kept.sort_by_key(|(session, kind)| (session.clone(), *kind == CaptureKind::Visible));
    assert_eq!(
        kept,
        [("sd_term_a".to_string(), CaptureKind::Card), ("sd_term_a".to_string(), CaptureKind::Visible)]
    );
}

#[test]
fn file_references_capture_joins_real_terminal_soft_wraps() {
    let server = crate::test_isolation::TmuxServer::new();
    server.run(&[
        "new-session", "-d", "-x", "35", "-y", "10", "-s", "files",
        "printf '%s\\n' 'Created image (game/assets/art/menu/prisoners_at_dawn_v1.png).'; sleep 30",
    ]);
    let mut text = String::new();
    wait_until("the wrapped path in the capture", || {
        text = capture_file_references_with(server.command(), "files").unwrap();
        text.contains("prisoners_at_dawn_v1.png")
    });
    let paths = crate::asset_references::candidates(&text, |p| p.ends_with(".png"));
    assert!(paths.contains(&"game/assets/art/menu/prisoners_at_dawn_v1.png".into()), "{paths:?}");
}

/// What reuse relies on, against a real tmux server: an idle pane's row
/// (and a capture of it) leaves the stamp alone, and output changes it.
#[test]
fn pane_stamp_changes_with_output_only() {
    let session = format!("test_sd_{}", unique_session_name());
    assert!(tmux(&["new-session", "-d", "-x", "80", "-y", "20", "-s", &session, "cat"]));
    let _cleanup = SessionCleanup(vec![session.clone()]);
    let row = || match pane_snapshot().expect("tmux answers").lookup(&session) {
        PaneLookup::Row(row) => row.clone(),
        _ => panic!("session row missing"),
    };
    let first = row();
    let stamp = first.stamp.clone().expect("a single-pane session is stamped");
    assert_eq!((stamp.width, stamp.height, stamp.pid.as_str()), (80, 20, first.pid.as_str()));
    assert!(stamp.pane_id.starts_with('%'));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    assert!(stamp.activity > 0 && stamp.activity <= now, "window_activity is in seconds: {stamp:?}");
    // Idle: capturing and listing again change nothing.
    let cache = std::sync::Mutex::new(CaptureCache::new());
    let capture = || capture_pane_raw(&session, false, CARD_CAPTURE_HISTORY);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let _ = capture();
    assert_eq!(row().stamp, Some(stamp.clone()));
    // Once a second has passed since the last output, a capture is reused.
    while SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() <= stamp.activity {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let idle = row().stamp;
    assert_eq!(idle, Some(stamp.clone()));
    let before = reuse_or_capture(&cache, &session, idle.as_ref(), CaptureKind::Card, capture).unwrap();
    let reused = reuse_or_capture(&cache, &session, row().stamp.as_ref(), CaptureKind::Card, || {
        panic!("an unchanged pane was captured again")
    });
    assert_eq!(reused.as_deref(), Some(before.as_str()));
    // Output (the pty's echo and cat's copy) changes the stamp at once.
    let marker = "stamp-marker";
    assert!(tmux(&["send-keys", "-t", &session, marker, "Enter"]));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut screen = before.clone();
    while !screen.contains(marker) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let current = row().stamp;
        screen = reuse_or_capture(&cache, &session, current.as_ref(), CaptureKind::Card, capture).unwrap();
    }
    assert!(screen.contains(marker), "output was not captured again: {screen:?}");
    assert_ne!(row().stamp, Some(stamp));
}
