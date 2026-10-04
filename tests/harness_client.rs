use std::fs;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use super_desktop::harness_record::{self, Metadata};

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn card_title_client_records_only_its_live_launch_and_submitted_prompt() {
    let home = Home(std::env::temp_dir().join(format!(
        "sd-hook-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    fs::DirBuilder::new().mode(0o700).create(&home.0).unwrap();
    let root = home.0.join(".local/state/super-desktop/harness");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("sd_term_hook_test.json");
    let initial = Metadata {
        version: 1,
        agent: "claude".into(),
        ..Default::default()
    };
    harness_record::atomic_write(&path, &initial).unwrap();
    let invoke = |kind: &str, input: serde_json::Value| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_super-desktop-client"))
            .args(["harness-event", kind])
            .env("HOME", &home.0)
            .env("SD_HARNESS_FILE", &path)
            .env("SD_HARNESS_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty(), "hooks must not print prompt data");
        output
    };

    assert!(invoke("init", serde_json::json!({})).stderr.is_empty());
    let initialized = harness_record::read(&path).unwrap();
    assert_eq!(initialized.pid, std::process::id());
    assert_eq!(
        Some(initialized.process_start),
        harness_record::start_time(std::process::id())
    );

    for prompt in [
        "Fix the login bug",
        "<task-notification>Background result</task-notification>",
    ] {
        let output = invoke(
            "claude",
            serde_json::json!({
                "hook_event_name": "UserPromptSubmit", "session_id": "own", "prompt": prompt
            }),
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            harness_record::read(&path).unwrap().prompt,
            "Fix the login bug"
        );
    }

    let mut stale = harness_record::read(&path).unwrap();
    stale.process_start = "another-launch".into();
    harness_record::atomic_write(&path, &stale).unwrap();
    let before = fs::read(&path).unwrap();
    let output = invoke(
        "claude",
        serde_json::json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "own", "prompt": "Wrong launch"
        }),
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("wrong launch"));
    assert_eq!(fs::read(&path).unwrap(), before);
}
