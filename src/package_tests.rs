//! Package acceptance checks, run only inside a disposable filesystem.
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a package installed in a disposable filesystem"]
fn packaged_desktop_lifecycle() {
    assert!(
        std::path::Path::new("/super-desktop-package-test-root").is_file(),
        "Run only inside the isolated package root"
    );
    assert!(std::env::var_os("SD_GTK_TESTS_ON_DESKTOP").is_none());
    let proof = std::env::temp_dir().join("super-desktop-package-validated");
    let _ = std::fs::remove_file(&proof);
    crate::gtk_test::run_in_child_process("package_tests::packaged_desktop_inner");
    assert!(
        proof.is_file(),
        "Package acceptance must not skip its private display checks"
    );
    std::fs::remove_file(proof).unwrap();
}

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn packaged_desktop_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    assert!(std::path::Path::new("/super-desktop-package-test-root").is_file());
    let client = "/usr/bin/super-desktop";
    let home = std::env::temp_dir().join(format!("sd-package-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let log_path = home.join("daemon.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut daemon = Daemon(
        Command::new(client)
            .arg("daemon")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start installed daemon"),
    );
    let run = |args: &[&str]| {
        Command::new(client)
            .args(args)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .output()
            .expect("installed control client")
    };
    let status = || {
        let output = run(&["app", "status", "--format", "json"]);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap_or_default()
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if status()["ok"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline && daemon.0.try_wait().unwrap().is_none(),
            "installed daemon did not start: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    for (action, visible) in [("show", true), ("hide", false), ("show", true)] {
        let output = run(&[action]);
        assert!(output.status.success(), "{action}: {:?}", output);
        let deadline = Instant::now() + Duration::from_secs(10);
        while status()["data"]["visible"] != visible {
            assert!(
                Instant::now() < deadline && daemon.0.try_wait().unwrap().is_none(),
                "{action} was not applied: {}",
                std::fs::read_to_string(&log_path).unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert!(run(&["reload-theme"]).status.success());
    assert!(run(&["kill"]).status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "installed daemon did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::remove_dir_all(home).unwrap();
    std::fs::write(
        std::env::temp_dir().join("super-desktop-package-validated"),
        b"ok",
    )
    .unwrap();
}
