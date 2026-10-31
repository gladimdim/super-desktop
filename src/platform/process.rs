//! Process identity used to distinguish a harness from a reused PID.

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    parent: u32,
    start: String,
}

pub fn start_time(pid: u32) -> Option<String> {
    identity(pid).map(|identity| identity.start)
}

pub fn parent_pid(pid: u32) -> Option<u32> {
    identity(pid).map(|identity| identity.parent)
}

pub fn exists(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(target_os = "macos")]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        (unsafe { libc::kill(pid, 0) == 0 })
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(target_os = "macos")]
pub fn name(pid: u32) -> Option<String> {
    let info = bsd_info(pid)?;
    let bytes: Vec<_> = info
        .pbi_comm
        .iter()
        .map(|&b| b as u8)
        .take_while(|&b| b != 0)
        .collect();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(target_os = "macos")]
pub fn children(pid: u32) -> Vec<u32> {
    let Ok(pid) = i32::try_from(pid) else {
        return Vec::new();
    };
    if pid <= 0 {
        return Vec::new();
    }
    let needed = unsafe { libc::proc_listchildpids(pid, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Vec::new();
    }
    // The process tree may grow during enumeration. Leave room and bound work.
    // This wrapper returns a PID count (proc_listpids itself returns bytes).
    let mut children = vec![0i32; (needed as usize + 64).min(65536)];
    let count = unsafe {
        libc::proc_listchildpids(
            pid,
            children.as_mut_ptr().cast(),
            (children.len() * 4) as i32,
        )
    };
    if count <= 0 {
        return Vec::new();
    }
    children.truncate(count as usize);
    children
        .into_iter()
        .filter(|&child| child > 0)
        .map(|child| child as u32)
        .collect()
}

#[cfg(target_os = "macos")]
pub fn has_foreground_job(pid: u32) -> bool {
    bsd_info(pid).is_some_and(|info| {
        info.e_tpgid > 0 && info.e_tpgid != u32::MAX && info.pbi_pgid != info.e_tpgid
    })
}

/// The process group of `pid` and the foreground process group of its
/// terminal (0 without one), from `/proc`; `None` where `/proc` is absent.
pub fn terminal_groups(pid: u32) -> Option<(u32, u32)> {
    terminal_groups_from_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

fn terminal_groups_from_stat(stat: &str) -> Option<(u32, u32)> {
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    Some((fields.get(2)?.parse().ok()?, fields.get(5)?.parse().ok()?))
}

/// The non-empty arguments of `pid`'s command line, from `/proc`; `None`
/// where `/proc` is absent or the process is gone.
pub fn cmdline(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// Only a live, named child is eligible for a signal from its owner.
pub fn is_live_child_named(pid: i32, parent: i32, prefix: &str) -> bool {
    if pid <= 0 || parent <= 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| live_child_from_stat(&stat, parent, prefix))
    }
    #[cfg(target_os = "macos")]
    {
        let Some(info) = bsd_info(pid as u32) else {
            return false;
        };
        let bytes: Vec<u8> = info
            .pbi_comm
            .iter()
            .map(|&byte| byte as u8)
            .take_while(|&byte| byte != 0)
            .collect();
        info.pbi_ppid == parent as u32
            && info.pbi_status != libc::SZOMB
            && String::from_utf8_lossy(&bytes).starts_with(prefix)
    }
}

#[cfg(any(target_os = "linux", test))]
fn live_child_from_stat(stat: &str, parent: i32, prefix: &str) -> bool {
    let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
        return false;
    };
    if close < open {
        return false;
    }
    let comm = &stat[open + 1..close];
    let mut rest = stat[close + 1..].split_whitespace();
    let state = rest.next().unwrap_or("");
    let ppid = rest.next().and_then(|value| value.parse::<i32>().ok());
    comm.starts_with(prefix) && !matches!(state, "Z" | "X" | "x") && ppid == Some(parent)
}

#[cfg(target_os = "linux")]
fn identity(pid: u32) -> Option<Identity> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(any(target_os = "linux", test))]
fn parse_stat(stat: &str) -> Option<Identity> {
    let fields = stat.rsplit_once(')')?.1.split_whitespace();
    Some(Identity {
        parent: fields.clone().nth(1)?.parse().ok()?,
        // Preserve the Linux start-time token exactly, including its units.
        start: fields.clone().nth(19)?.to_string(),
    })
}

#[cfg(target_os = "macos")]
fn identity(pid: u32) -> Option<Identity> {
    let info = bsd_info(pid)?;
    Some(Identity {
        parent: info.pbi_ppid,
        // This is an opaque, local metadata identity, never a wire timestamp.
        start: format!("{}:{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    })
}

#[cfg(target_os = "macos")]
fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32 {
        return None;
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_identity_preserves_start_token_with_parentheses_in_name() {
        let fields: Vec<_> = (0..20)
            .map(|i| match i {
                0 => "S".to_owned(),
                1 => "42".to_owned(),
                19 => "0012345".to_owned(),
                _ => "0".to_owned(),
            })
            .collect();
        assert_eq!(
            parse_stat(&format!(
                "123 (a command ) with spaces) {}",
                fields.join(" ")
            )),
            Some(Identity {
                parent: 42,
                start: "0012345".into()
            })
        );
        for invalid in ["", "123 (short) S 42", "123 no closing paren"] {
            assert_eq!(parse_stat(invalid), None);
        }
    }

    #[test]
    fn current_process_identity_is_stable_and_has_the_actual_parent() {
        let pid = std::process::id();
        let start = start_time(pid).expect("own process identity");
        assert!(!start.is_empty());
        assert_eq!(start_time(pid).as_deref(), Some(start.as_str()));
        assert_eq!(parent_pid(pid), Some(unsafe { libc::getppid() } as u32));
    }

    #[test]
    fn terminal_groups_come_after_the_command_name() {
        assert_eq!(
            terminal_groups_from_stat("77 (a ) b) S 1 40 40 34816 41 4194304"),
            Some((40, 41))
        );
        assert_eq!(terminal_groups_from_stat("77 (bash) S 1 40 40 0 -1 0"), None);
        assert_eq!(terminal_groups_from_stat("77 (bash) S 1 40"), None);
        assert_eq!(terminal_groups_from_stat("no name"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_command_line_is_read_without_empty_arguments() {
        let args = cmdline(std::process::id()).expect("own cmdline");
        assert_eq!(args, std::env::args().filter(|arg| !arg.is_empty()).collect::<Vec<_>>());
        assert_eq!(cmdline(u32::MAX), None);
    }

    #[test]
    fn invalid_processes_have_no_identity() {
        for pid in [0, u32::MAX] {
            assert_eq!(start_time(pid), None);
            assert_eq!(parent_pid(pid), None);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_process_inspection_finds_a_live_child() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = Child(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        assert!(exists(pid));
        assert!(children(std::process::id()).contains(&pid));
        assert_eq!(name(pid).as_deref(), Some("sleep"));
        assert!(is_live_child_named(
            pid as i32,
            std::process::id() as i32,
            "sleep"
        ));
        assert!(!exists(0));
        assert!(!exists(u32::MAX));
    }

    #[test]
    fn hidden_pause_resume_only_signals_live_tmux_children() {
        let valid = |stat| live_child_from_stat(stat, 4242, "tmux");
        assert!(valid("77 (tmux: client) S 4242 77 77 0"));
        assert!(
            valid("77 (tmux: client) T 4242 77 77 0"),
            "stopped client needs resuming"
        );
        assert!(!valid("77 (tmux: client) Z 4242 77 77 0"), "zombie");
        assert!(!valid("77 (tmux: client) X 4242 77 77 0"), "dead");
        assert!(!valid("77 (tmux: client) S 1 77 77 0"), "not our child");
        assert!(!valid("77 (bash) S 4242 77 77 0"), "pid reused");
        assert!(!valid("77 (a) b) S 4242"));
        assert!(!valid(""));
    }

    #[test]
    fn native_child_check_rejects_wrong_parent_and_name() {
        let pid = std::process::id() as i32;
        let parent = unsafe { libc::getppid() };
        assert!(is_live_child_named(pid, parent, ""));
        assert!(!is_live_child_named(pid, pid, ""));
        assert!(!is_live_child_named(pid, parent, "not-this-process"));
        assert!(!is_live_child_named(-1, parent, ""));
    }
}
