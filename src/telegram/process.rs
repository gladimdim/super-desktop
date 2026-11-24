//! Daemon-owned supervision and start/stop lifecycle for `super-desktop telegram-bot`.
use super::config::{
    load_runtime_at, log_path, lock_path, runtime_path, save_runtime_at, should_run_from_disk,
};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CHECK_INTERVAL: Duration = Duration::from_secs(5);
pub const TELEGRAM_PROCESS: &str = "^([^ ]*/)?super-desktop telegram-bot( |$)";

static LIFECYCLE: Mutex<()> = Mutex::new(());

pub fn worker_running() -> bool {
    let Ok(path) = lock_path() else {
        return false;
    };
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return false;
        }
    } else {
        return false;
    }
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    else {
        return false;
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        true
    }
}

fn read_locked_pid() -> Option<i32> {
    let path = lock_path().ok()?;
    let mut text = String::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse::<i32>().ok().filter(|&pid| pid > 1)
}

fn pid_is_telegram_bot(pid: i32) -> bool {
    let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let args: Vec<&[u8]> = raw.split(|&b| b == 0).filter(|s| !s.is_empty()).collect();
    args.first().is_some_and(|a| a.ends_with(b"super-desktop"))
        && args.get(1).is_some_and(|a| *a == b"telegram-bot")
}

fn self_exe() -> Result<PathBuf, String> {
    let proc_exe = PathBuf::from("/proc/self/exe");
    let path_var = std::env::var("PATH").unwrap_or_default();
    let candidates = std::env::current_exe()
        .ok()
        .into_iter()
        .chain([proc_exe])
        .chain(
            path_var
                .split(':')
                .filter(|dir| !dir.is_empty())
                .map(|dir| PathBuf::from(dir).join("super-desktop")),
        );
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("Cannot locate super-desktop executable to start Telegram worker.".into())
}

pub fn start_worker() -> Result<(), String> {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    start_worker_inner()
}

fn start_worker_inner() -> Result<(), String> {
    if !should_run_from_disk() {
        return Ok(());
    }
    if worker_running() {
        return Ok(());
    }
    let exe = self_exe()?;
    let log_file_path = log_path().map_err(|e| e.to_string())?;
    if let Some(parent) = log_file_path.parent() {
        super::config::ensure_private_dir(parent, "Telegram state").map_err(|e| e.to_string())?;
    }
    let _ = fs::rename(
        &log_file_path,
        log_file_path.with_file_name("telegram.previous.log"),
    );
    let log = if fs::symlink_metadata(&log_file_path).is_ok_and(|m| m.file_type().is_symlink()) {
        None
    } else {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&log_file_path)
            .ok()
    };
    let log_err = log.as_ref().and_then(|f| f.try_clone().ok());

    let mut command = Command::new(&exe);
    command
        .arg("telegram-bot")
        .arg0("super-desktop")
        .process_group(0)
        .stdin(Stdio::null());
    match (log, log_err) {
        (Some(out), Some(err)) => {
            command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        _ => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to spawn telegram-bot: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if worker_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub fn stop_worker() -> Result<(), String> {
    let _guard = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    stop_worker_inner()
}

fn stop_worker_inner() -> Result<(), String> {
    if !worker_running() {
        mark_stopped();
        return Ok(());
    }
    let uid = unsafe { libc::getuid() }.to_string();
    if let Some(pid) = read_locked_pid().filter(|&p| pid_is_telegram_bot(p)) {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
    let _ = Command::new("pkill")
        .args(["-u", &uid, "-f", TELEGRAM_PROCESS])
        .status();

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !worker_running() {
            mark_stopped();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if let Some(pid) = read_locked_pid().filter(|&p| pid_is_telegram_bot(p)) {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    let _ = Command::new("pkill")
        .args(["-u", &uid, "-9", "-f", TELEGRAM_PROCESS])
        .status();

    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if !worker_running() {
            mark_stopped();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("Telegram worker did not stop cleanly.".into())
}

fn mark_stopped() {
    if let Ok(rt_path) = runtime_path() {
        if let Ok(mut rt) = load_runtime_at(&rt_path) {
            if rt.worker_status.state != "stopped" && rt.worker_status.state != "error" {
                rt.worker_status.state = "stopped".to_string();
                rt.worker_status.pid = None;
                let _ = save_runtime_at(&rt_path, &rt);
            }
        }
    }
}

pub fn supervise() {
    supervise_while(|| true);
}

pub fn supervise_while(active: impl Fn() -> bool) {
    loop {
        if !active() {
            std::thread::sleep(CHECK_INTERVAL);
            continue;
        }
        {
            let _guard = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
            let should = should_run_from_disk();
            let running = worker_running();
            if should && !running {
                if let Err(err) = start_worker_inner() {
                    eprintln!("SUPER DESKTOP: telegram worker start failed: {err}");
                }
            } else if !should && running {
                let _ = stop_worker_inner();
            }
        }
        std::thread::sleep(CHECK_INTERVAL);
    }
}
