//! The `sd-qml-host` process for one surface: spawning it with its end of a
//! SOCK_SEQPACKET socketpair as fd 3, packets with an optional fd, and the
//! shared-memory frame buffer it renders into.
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const MAX_PACKET: usize = 1 << 20;

// Plugins run on Linux only (the overlay is GTK on Hyprland); these keep the
// shared library building elsewhere, where `spawn` refuses.
#[cfg(target_os = "linux")]
const SOCKET_KIND: libc::c_int = libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const SOCKET_KIND: libc::c_int = libc::SOCK_SEQPACKET;
#[cfg(target_os = "linux")]
const RECEIVE_CLOEXEC: libc::c_int = libc::MSG_CMSG_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const RECEIVE_CLOEXEC: libc::c_int = 0;

pub struct SpawnOptions<'a> {
    pub host: &'a Path,
    pub qml: &'a Path,
    pub plugin_dir: &'a Path,
    pub plugin_id: &'a str,
    pub surface_id: &'a str,
    /// Which copy of a card surface ("" for the first).
    pub instance: &'a str,
    pub width: i32,
    pub height: i32,
    /// Device scale of the output (1, 1.25, 2…).
    pub scale: f64,
    pub theme: &'a Value,
    /// Host stderr goes here (the plugin log).
    pub log: Option<PathBuf>,
    /// The plugin's saved values (`sd.values`).
    pub data: Option<PathBuf>,
}

pub struct HostProcess {
    child: Option<Child>,
    socket: OwnedFd,
}

fn cvt(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

impl HostProcess {
    pub fn spawn(options: &SpawnOptions) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "plugins run on Linux only"));
        }
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` has room for the two descriptors socketpair writes.
        cvt(unsafe {
            libc::socketpair(libc::AF_UNIX, SOCKET_KIND, 0, fds.as_mut_ptr())
        })?;
        // SAFETY: socketpair succeeded, so both are fresh descriptors we own.
        let (ours, theirs) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

        let mut command = Command::new(options.host);
        command
            .arg("--serve")
            .args(["--fd", "3"])
            .arg("--qml")
            .arg(options.qml)
            .arg("--plugin-dir")
            .arg(options.plugin_dir)
            .args(["--plugin-id", options.plugin_id, "--surface-id", options.surface_id, "--instance-id", options.instance])
            .args(["--width", &options.width.max(1).to_string()])
            .args(["--height", &options.height.max(1).to_string()])
            .args(["--theme", &options.theme.to_string()])
            .args(options.data.iter().flat_map(|d| [std::ffi::OsStr::new("--data"), d.as_os_str()]))
            .current_dir(options.plugin_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // The host sets QT_SCALE_FACTOR from this itself, and forces Qt off
        // screen, so programs a plugin starts keep the user's environment
        // (display included) unchanged.
        if options.scale > 0.0 {
            command.args(["--scale", &format!("{}", options.scale)]);
        }
        match options.log.as_deref().and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok()) {
            Some(file) => command.stderr(Stdio::from(file)),
            None => command.stderr(Stdio::null()),
        };
        let child_fd = theirs.as_raw_fd();
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                // dup2 clears close-on-exec on the copy.
                if libc::dup2(child_fd, 3) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // The host ends with the overlay, whatever way it ends.
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(theirs);
        Ok(Self { child: Some(child), socket: ours })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// A second handle on the connection for a reader thread: `next()`
    /// blocks until a packet arrives and fails with `UnexpectedEof` once the
    /// host is gone.
    pub fn reader(&self) -> io::Result<Reader> {
        Ok(Reader { socket: self.socket.try_clone()? })
    }

    pub fn send(&self, message: &Value) -> io::Result<()> {
        send_packet(self.socket.as_raw_fd(), message.to_string().as_bytes())
    }

    /// The next packet, or `Ok(None)` when none is waiting. `Err` with
    /// `UnexpectedEof` when the host closed its end.
    pub fn receive(&self) -> io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
        receive_packet(self.socket.as_raw_fd(), false)
    }

    /// Whether the process has ended (and reaps it).
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut().and_then(|child| child.try_wait().ok().flatten())
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.send(&serde_json::json!({"t": "quit"}));
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap off the GTK thread: a host stuck in uninterruptible sleep
            // must not hold the overlay.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

pub struct Reader {
    socket: OwnedFd,
}

impl Reader {
    pub fn next(&self) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
        loop {
            if let Some(packet) = receive_packet(self.socket.as_raw_fd(), true)? {
                return Ok(packet);
            }
        }
    }
}

pub fn send_packet(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_PACKET {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message larger than 1 MiB"));
    }
    loop {
        // SAFETY: the buffer is valid for its length.
        let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
        if n >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// One packet. `block` waits for it; otherwise `Ok(None)` when none is there.
pub fn receive_packet(fd: RawFd, block: bool) -> io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
    let flags = RECEIVE_CLOEXEC | if block { 0 } else { libc::MSG_DONTWAIT };
    let mut buffer = vec![0u8; MAX_PACKET];
    let mut control = [0u8; 64];
    loop {
        let mut iov = libc::iovec { iov_base: buffer.as_mut_ptr().cast(), iov_len: buffer.len() };
        // SAFETY: zeroed msghdr is a valid empty header; fields set below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len() as _;
        // SAFETY: msg points at live buffers for the duration of the call.
        let n = unsafe { libc::recvmsg(fd, &mut msg, flags) };
        if n < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(error),
            }
        }
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "host closed the connection"));
        }
        let mut passed = None;
        // SAFETY: walking the control messages the kernel filled in.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        let raw = std::ptr::read_unaligned(data.add(i));
                        let owned = OwnedFd::from_raw_fd(raw);
                        if passed.is_none() {
                            passed = Some(owned);
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        buffer.truncate(n as usize);
        return Ok(Some((buffer, passed)));
    }
}

/// A frame buffer the host shares with us (read only on this side).
pub struct SharedBuffer {
    ptr: *mut libc::c_void,
    len: usize,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub scale: f64,
    _file: File,
}

impl SharedBuffer {
    pub fn map(fd: OwnedFd, width: i32, height: i32, stride: i32, scale: f64) -> io::Result<Self> {
        if width <= 0 || height <= 0 || stride < width.saturating_mul(4) || width > 16384 || height > 16384 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame buffer size"));
        }
        let len = stride as usize * height as usize;
        let file = File::from(fd);
        if file.metadata()?.len() < len as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame buffer smaller than announced"));
        }
        // SAFETY: mapping a file we hold, read only, for its announced size.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, file.as_raw_fd(), 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr, len, width, height, stride, scale, _file: file })
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes and lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }
}

impl Drop for SharedBuffer {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `map` mapped.
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::protocol::{self, FromHost};
    use std::time::{Duration, Instant};

    fn wait_for(host: &HostProcess, deadline: Duration, mut want: impl FnMut(FromHost, Option<OwnedFd>) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            match host.receive() {
                Ok(Some((bytes, fd))) => {
                    if want(protocol::parse(&bytes), fd) {
                        return true;
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => {
                    eprintln!("host connection ended: {e}");
                    return false;
                }
            }
        }
        false
    }

    /// `sd.values` hands QML plain JS objects and arrays: a saved list
    /// passes Array.isArray (a Qt list wrapper did not), `sd.value` too.
    #[test]
    fn plugins_host_values_are_plain_js() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-values-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("values.json");
        std::fs::write(&data, r#"{"items": [{"at": 1, "tags": ["a", "b"]}], "count": 3, "on": true}"#).unwrap();
        // Each binding throws (a warning in the report) when a value is not what JS expects.
        let check = |qml: &str, with_data: bool| -> serde_json::Value {
            let file = dir.join("Probe.qml");
            std::fs::write(&file, qml).unwrap();
            let mut command = std::process::Command::new(&binary);
            command.arg("--check").arg(&file);
            if with_data {
                command.arg("--data").arg(&data);
            }
            serde_json::from_slice(&command.output().unwrap().stdout).unwrap()
        };
        let probe = r#"import QtQuick
Item {
    function must(ok, what) { if (!ok) throw new Error(what) }
    property bool a: { must(Array.isArray(sd.values.items), "items is not an array"); return true }
    property bool b: { must(Array.isArray(sd.values.items[0].tags), "nested list is not an array"); return true }
    property bool c: { must(sd.values.items.slice().concat([2]).length === 2, "array methods"); return true }
    property bool d: { must(Array.isArray(sd.value("items", [])) && sd.value("missing", 7) === 7, "sd.value"); return true }
    property bool e: { must(sd.values.count === 3 && sd.values.on === true, "scalars"); return true }
    property bool f: { must(sd.visible === true, "visible"); return true }
}
"#;
        let report = check(probe, true);
        assert_eq!(report["ok"], true, "{report}");
        assert_eq!(report["warnings"], serde_json::json!([]), "{report}");
        // No saved values: an empty object, not undefined.
        let empty = check("import QtQuick\nItem { property int n: Object.keys(sd.values).length }\n", false);
        assert_eq!(empty["warnings"], serde_json::json!([]), "{empty}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--do` clicks shown text, types, presses keys and checks what is
    /// shown; the report has each step, the calls the QML made (recorded,
    /// not done), its log and its values afterwards.
    #[test]
    fn plugins_host_snapshot_steps_use_the_surface_like_a_user() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-steps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Form.qml");
        std::fs::write(&file, r#"import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
Rectangle {
    color: "white"
    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 10
        TextField { id: field; placeholderText: "What to remember"; Layout.fillWidth: true
            Keys.onReturnPressed: save() }
        Button { text: "Add it"; onClicked: save() }
        Text { objectName: "count"; text: (sd.values.items || []).length + " saved" }
        Item { Layout.fillHeight: true }
    }
    MouseArea { anchors.fill: parent; z: -1; acceptedButtons: Qt.RightButton; onClicked: sd.run(["xdg-open", "https://example.com"]) }
    function save() {
        sd.setValue("items", (sd.values.items || []).concat([field.text]))
        sd.log("saved " + field.text)
        sd.call("popup.close", {surface: sd.surfaceId})
        field.text = ""
    }
}
"#).unwrap();
        let data = dir.join("values.json");
        std::fs::write(&data, "{}").unwrap();
        let run = |steps: &str| -> serde_json::Value {
            let out = std::process::Command::new(&binary).arg("--snapshot").arg(&file).arg("--out").arg(dir.join("out.png"))
                .arg("--data").arg(&data).args(["--surface-id", "form", "--width", "300", "--height", "200", "--wait", "50", "--do", steps])
                .output().unwrap();
            serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)))
        };
        let report = run("click \"What to remember\"; type milk; click \"Add it\"; expect \"1 saved\"\n\
                          click What; type eggs; key Return; expect \"2 saved\"; absent \"3 saved\"; click 5,190 right; shot done");
        assert_eq!(report["ok"], true, "{report}");
        let steps = report["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 11);
        assert!(steps.iter().all(|s| s["ok"] == true), "{report}");
        assert!(steps[0]["at"][0].as_f64().unwrap() > 100.0, "the field's center: {}", steps[0]);
        assert!(dir.join("out-done.png").is_file());
        assert_eq!(report["values"], serde_json::json!({"items": ["milk", "eggs"]}));
        assert_eq!(report["calls"], serde_json::json!([
            {"method": "popup.close", "params": {"surface": "form"}}, {"method": "popup.close", "params": {"surface": "form"}}]));
        assert_eq!(report["runs"], serde_json::json!([["xdg-open", "https://example.com"]]), "sd.run is recorded, not run");
        assert_eq!(report["logs"], serde_json::json!(["saved milk", "saved eggs"]));
        // A missing target fails the run at that step and says what is shown.
        let report = run("click \"Nope\"; type never");
        assert_eq!(report["ok"], false);
        assert_eq!(report["steps"].as_array().unwrap().len(), 1);
        let error = report["steps"][0]["error"].as_str().unwrap();
        assert!(error.contains("no visible text \"Nope\"") && error.contains("\"Add it\""), "{error}");
        let report = run("key Hyper+Q");
        assert!(report["steps"][0]["error"].as_str().unwrap().starts_with("unknown key"), "{report}");
        let report = run("click @count; frobnicate");
        assert!(report["steps"][1]["error"].as_str().unwrap().starts_with("unknown step"), "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Standard controls follow the theme: highlighted buttons (and checked
    /// switches, sliders) use the accent with readable text on it, and
    /// placeholder text reads on the field, in a dark and a light theme.
    #[test]
    fn plugins_host_palette_follows_the_theme() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-palette-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Probe.qml");
        std::fs::write(&file, r#"import QtQuick
import QtQuick.Controls
Item {
    function must(ok, what) { if (!ok) throw new Error(what) }
    function lum(c) {
        var v = [c.r, c.g, c.b].map(x => x <= 0.03928 ? x / 12.92 : Math.pow((x + 0.055) / 1.055, 2.4))
        return 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]
    }
    function contrast(a, b) { var x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05) }
    Button { id: button; highlighted: true }
    TextField { id: field; y: 50 }
    // Controls take the palette once they are in the window: check a moment
    // later, and paint only when every check passed.
    Rectangle { id: done; anchors.fill: parent; color: "transparent"; z: 10 }
    Timer {
        interval: 50; running: true
        onTriggered: {
            must(Qt.colorEqual(button.palette.dark, sd.theme.accent), "highlight is not the accent")
            must(contrast(button.palette.brightText, button.palette.dark) >= 2.5, "text on the accent does not read")
            must(contrast(field.placeholderTextColor, field.palette.base) >= 4.5, "placeholder does not read")
            done.color = "red"
        }
    }
}
"#).unwrap();
        // Matte Black (accent orange) and Lupine (light, accent blue).
        for theme in [
            serde_json::json!({"mode": "dark", "background": "#121212", "foreground": "#bebebe", "surface": "#1e1e1e", "darkBackground": "#0d0d0d", "muted": "#333333", "accent": "#e68e0d"}),
            serde_json::json!({"mode": "light", "background": "#fafafa", "foreground": "#212121", "surface": "#f5f5f5", "darkBackground": "#ececec", "muted": "#9e9e9e", "accent": "#3264eb"}),
        ] {
            let out = std::process::Command::new(&binary).arg("--snapshot").arg(&file).arg("--out").arg(dir.join("out.png"))
                .args(["--width", "120", "--height", "100", "--wait", "300", "--theme", &theme.to_string()]).output().unwrap();
            let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(report["warnings"], serde_json::json!([]), "{theme}: {report}");
            assert_eq!(report["paintedFraction"], 1.0, "the checks did not run: {report}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs the real `sd-qml-host` when it is built
    /// (`scripts/build-qml-host.sh debug`), else says why it is skipped.
    #[test]
    fn plugins_host_round_trip_renders_and_forwards_clicks() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let qml = dir.join("Dock.qml");
        std::fs::write(&qml, r#"import QtQuick
Rectangle {
    color: sd.theme.accent
    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: {
            sd.setValue("clicks", (sd.values.clicks || 0) + 1)
            sd.call("overlay.hide", {"from": "test", "clicks": sd.values.clicks})
        }
    }
}
"#).unwrap();
        let theme = serde_json::json!({"accent": "#ff0000"});
        let host = HostProcess::spawn(&SpawnOptions {
            host: &binary, qml: &qml, plugin_dir: &dir, plugin_id: "test", surface_id: "main", instance: "",
            width: 40, height: 30, scale: 1.0, theme: &theme, log: Some(dir.join("host.log")), data: Some(dir.join("data.json")),
        }).unwrap();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| m == FromHost::Ready),
            "no ready: {}", std::fs::read_to_string(dir.join("host.log")).unwrap_or_default());
        host.send(&protocol::resize(40, 30)).unwrap();
        let mut buffer = None;
        assert!(wait_for(&host, Duration::from_secs(10), |m, fd| {
            if let FromHost::Buffer { width, height, stride, scale } = m {
                buffer = Some(SharedBuffer::map(fd.expect("buffer fd"), width, height, stride, scale).unwrap());
            }
            matches!(m, FromHost::Frame { .. }) && buffer.as_ref().is_some_and(|b| b.width == 40)
        }), "no frame");
        let pixels = buffer.unwrap();
        // BGRA premultiplied: pure red, opaque.
        assert_eq!(&pixels.bytes()[..4], &[0, 0, 255, 255]);
        host.send(&protocol::ack(1)).unwrap();
        host.send(&protocol::pointer("enter", 10.0, 10.0, 0, 0)).unwrap();
        host.send(&protocol::pointer("move", 10.0, 10.0, 0, 0)).unwrap();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| m == FromHost::Cursor { shape: 13 }),
            "the hand cursor was not reported");
        host.send(&protocol::pointer("press", 10.0, 10.0, 1, 0)).unwrap();
        host.send(&protocol::pointer("release", 10.0, 10.0, 1, 0)).unwrap();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| matches!(m,
            FromHost::Call { ref method, ref params, .. } if method == "overlay.hide" && params["from"] == "test" && params["clicks"] == 1)),
            "the click did not reach QML");
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("data.json")).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({"clicks": 1}));
        drop(host);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_host_pty_runs_programs_with_the_users_environment() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-pty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let qml = dir.join("Term.qml");
        std::fs::write(&qml, r#"import QtQuick
Item {
    property string printed: ""
    property string echoed: ""
    Component.onCompleted: {
        // Programs see the user's display, not the host's offscreen Qt.
        const p = sd.openPty(["sh", "-c", "printf '%s|%s|%s' \"$TERM\" \"$QT_QPA_PLATFORM\" \"$SD_TEST_DISPLAY\"; exit 3"], {cols: 100, rows: 30})
        p.output.connect(function(text) { printed += text })
        p.exited.connect(function(code) { sd.call("done", {printed: printed, code: code}) })
        const cat = sd.openPty(["cat"])
        cat.output.connect(function(text) {
            echoed += text
            if (echoed.indexOf("ping") >= 0 && echoed.indexOf("\n") >= 0) { sd.call("echo", {text: echoed}); cat.terminate() }
        })
        cat.write("ping\n")
    }
}
"#).unwrap();
        let theme = serde_json::json!({});
        // The display variable stands in for WAYLAND_DISPLAY: it must reach the program.
        std::env::set_var("SD_TEST_DISPLAY", "wayland-test");
        let host = HostProcess::spawn(&SpawnOptions {
            host: &binary, qml: &qml, plugin_dir: &dir, plugin_id: "term", surface_id: "main", instance: "",
            width: 10, height: 10, scale: 2.0, theme: &theme, log: Some(dir.join("host.log")), data: None,
        }).unwrap();
        let mut done = None;
        let mut echo = None;
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| {
            if let FromHost::Call { method, params, .. } = m {
                match method.as_str() {
                    "done" => done = Some(params),
                    "echo" => echo = Some(params),
                    _ => {}
                }
            }
            done.is_some() && echo.is_some()
        }), "no pty results: {}", std::fs::read_to_string(dir.join("host.log")).unwrap_or_default());
        let done = done.unwrap();
        assert_eq!(done["code"], 3);
        // The program gets the user's QT_QPA_PLATFORM (if any), never the
        // host's own "offscreen".
        let platform = std::env::var("QT_QPA_PLATFORM").unwrap_or_default();
        assert_ne!(platform, "offscreen");
        assert_eq!(done["printed"], format!("xterm-256color|{platform}|wayland-test"));
        assert!(echo.unwrap()["text"].as_str().unwrap().contains("ping"));
        drop(host);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_host_keys_reach_the_focused_item() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sd-plugins-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let qml = dir.join("Game.qml");
        std::fs::write(&qml, r#"import QtQuick
Rectangle {
    focus: true
    Keys.onPressed: (event) => sd.call("key", {key: event.key, text: event.text, active: activeFocus, shift: (event.modifiers & Qt.ShiftModifier) !== 0})
}
"#).unwrap();
        let theme = serde_json::json!({});
        let host = HostProcess::spawn(&SpawnOptions {
            host: &binary, qml: &qml, plugin_dir: &dir, plugin_id: "keys", surface_id: "main", instance: "",
            width: 20, height: 20, scale: 1.0, theme: &theme, log: Some(dir.join("host.log")), data: None,
        }).unwrap();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| m == FromHost::Ready), "no ready");
        host.send(&protocol::resize(20, 20)).unwrap();
        host.send(&protocol::focus(true)).unwrap();
        host.send(&protocol::key(true, protocol::qt_key(0xff52), "", 0)).unwrap(); // Up
        host.send(&protocol::key(true, protocol::qt_key(u32::from(b'A')), "A", protocol::qt_modifiers(1))).unwrap();
        let mut keys = Vec::new();
        assert!(wait_for(&host, Duration::from_secs(10), |m, _| {
            if let FromHost::Call { method, params, .. } = m {
                if method == "key" {
                    keys.push(params);
                }
            }
            keys.len() == 2
        }), "keys did not reach QML: {}", std::fs::read_to_string(dir.join("host.log")).unwrap_or_default());
        assert_eq!(keys[0]["key"], 0x0100_0013);
        assert_eq!(keys[0]["active"], true, "the focused item has active focus, so games can use activeFocus");
        assert_eq!(keys[1]["text"], "A");
        assert_eq!(keys[1]["shift"], true);
        drop(host);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs `body` (QML statements using `t`, a TerminalView of 20×6 cells
    /// fed by hand) in the real host and returns what it reports with
    /// `report(value)`.
    fn terminal_run(name: &str, body: &str) -> Option<serde_json::Value> {
        let binary = crate::plugins::paths::qml_host_binary()?;
        let dir = std::env::temp_dir().join(format!("sd-plugins-vt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let qml = dir.join("T.qml");
        std::fs::write(&qml, format!(r#"import QtQuick
import SuperDesktop.Terminal
Item {{
    width: 400; height: 400
    TerminalView {{
        id: t
        autoStart: false
        width: 20 * cellWidth; height: 6 * cellHeight
        property real cellWidth: fm.advanceWidth("M")
        property real cellHeight: Math.ceil(fm.height)
        FontMetrics {{ id: fm; font.family: t.fontFamily; font.pixelSize: t.fontPixelSize }}
    }}
    function report(value) {{ sd.call("report", {{value: value}}) }}
    function rows() {{ const out = []; for (let r = 0; r < 6; r++) out.push(t.lineText(r).replace(/ +$/, "")); return out }}
    Timer {{ interval: 150; running: true; onTriggered: {{ {body} }} }}
}}
"#)).unwrap();
        let theme = serde_json::json!({});
        let host = HostProcess::spawn(&SpawnOptions {
            host: &binary, qml: &qml, plugin_dir: &dir, plugin_id: "vt", surface_id: "main", instance: "",
            width: 400, height: 400, scale: 1.0, theme: &theme, log: Some(dir.join("host.log")), data: None,
        }).unwrap();
        host.send(&protocol::resize(400, 400)).unwrap();
        let mut value = None;
        let got = wait_for(&host, Duration::from_secs(10), |m, _| {
            if let FromHost::Call { method, params, .. } = m {
                if method == "report" {
                    value = Some(params["value"].clone());
                }
            }
            value.is_some()
        });
        assert!(got, "{name}: no report: {}", std::fs::read_to_string(dir.join("host.log")).unwrap_or_default());
        drop(host);
        let _ = std::fs::remove_dir_all(&dir);
        value
    }

    #[test]
    fn plugins_terminal_view_emulates_the_screen() {
        if crate::plugins::paths::qml_host_binary().is_none() {
            eprintln!("skipped: sd-qml-host is not built (scripts/build-qml-host.sh debug)");
            return;
        }
        let rows = |v: serde_json::Value| -> Vec<String> {
            v.as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_owned()).collect()
        };
        // Text, CR/LF, cursor addressing, erase to end of line.
        let v = terminal_run("basic", r#"t.feed("hello\r\nworld\x1b[1;3Hxx\x1b[2;3H\x1b[K"); report(rows())"#).unwrap();
        assert_eq!(rows(v), ["hexxo", "wo", "", "", "", ""]);
        // Wrapping at the last column and scrolling at the bottom.
        let v = terminal_run("wrap", r#"t.feed("abcdefghijklmnopqrstuvwxyz\r\n2\r\n3\r\n4\r\n5\r\n6"); report(rows())"#).unwrap();
        assert_eq!(rows(v), ["uvwxyz", "2", "3", "4", "5", "6"]);
        // The alternate screen is cleared on entry and the main one comes back.
        let v = terminal_run("alt", r#"t.feed("main\x1b[?1049h\x1b[2Jfull screen\x1b[?1049l"); report(rows())"#).unwrap();
        assert_eq!(rows(v), ["main", "", "", "", "", ""]);
        // A scroll region moves only its own lines; insert and delete characters.
        let v = terminal_run("region", r#"t.feed("1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[2;4r\x1b[4;1H\n\x1b[1;1Habc\x1b[1;2H\x1b[P\x1b[1;1H\x1b[2@"); report(rows())"#).unwrap();
        assert_eq!(rows(v), ["  ac", "3", "4", "", "5", "6"]);
        // Colors and attributes, the cursor position, the window title.
        let v = terminal_run("sgr", r#"t.feed("\x1b]0;my title\x07\x1b[1;31mR\x1b[38;5;196mX\x1b[38;2;1;2;3;48;5;4mT\x1b[0mn");
            const c = (col) => t.cellAt(0, col).attr;
            report({red: c(0).fg, bold: c(0).bold, indexed: c(1).fg, rgb: c(2).fg, bg: c(2).bg, plain: c(3).fg,
                    cursor: [t.cursorPosition().x, t.cursorPosition().y], title: t.title})"#).unwrap();
        assert_eq!(v, serde_json::json!({"red": 1, "bold": true, "indexed": "#ff0000", "rgb": "#010203", "bg": 4, "plain": -1, "cursor": [4, 0], "title": "my title"}));
        // Selecting across rows gives the text, trailing blanks trimmed.
        let v = terminal_run("select", r#"t.feed("first line\r\nsecond   \r\nthird"); t.select(0, 6, 2, 2); const a = t.selectedText();
            t.select(2, 4, 0, 6); const b = t.selectedText(); t.clearSelection(); report([a, b, t.selectedText()])"#).unwrap();
        assert_eq!(v, serde_json::json!(["line\nsecond\nthi", "line\nsecond\nthird", ""]));
    }

    /// Frames per second for a constantly animating surface, acked at once.
    /// `cargo test --release --lib plugins_host_frame_rate -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check"]
    fn plugins_host_frame_rate() {
        let Some(binary) = crate::plugins::paths::qml_host_binary() else { return };
        let dir = std::env::temp_dir().join(format!("sd-plugins-fps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let qml = dir.join("Anim.qml");
        std::fs::write(&qml, r##"import QtQuick
Rectangle {
    color: "#202030"
    Repeater {
        model: 40
        Rectangle {
            width: 80; height: 80; radius: 20; color: Qt.hsla(index / 40, 0.6, 0.5, 1)
            x: (index % 10) * 180 + 40; y: Math.floor(index / 10) * 220 + 60
            RotationAnimation on rotation { from: 0; to: 360; duration: 2000; loops: Animation.Infinite }
        }
    }
}
"##).unwrap();
        for (w, h) in [(400, 300), (1920, 1080), (3840, 2160)] {
            let theme = serde_json::json!({});
            let host = HostProcess::spawn(&SpawnOptions {
                host: &binary, qml: &qml, plugin_dir: &dir, plugin_id: "fps", surface_id: "main", instance: "",
                width: w, height: h, scale: 1.0, theme: &theme, log: None, data: None,
            }).unwrap();
            assert!(wait_for(&host, Duration::from_secs(10), |m, _| m == FromHost::Ready));
            host.send(&protocol::resize(w, h)).unwrap();
            let start = Instant::now();
            let mut frames = 0u32;
            let mut buffer: Option<SharedBuffer> = None;
            while start.elapsed() < Duration::from_secs(3) {
                if let Ok(Some((bytes, fd))) = host.receive() {
                    match protocol::parse(&bytes) {
                        FromHost::Buffer { width, height, stride, scale } => {
                            buffer = Some(SharedBuffer::map(fd.unwrap(), width, height, stride, scale).unwrap());
                        }
                        FromHost::Frame { seq } => {
                            // The overlay's copy into a texture.
                            let copy = buffer.as_ref().map(|b| b.bytes().to_vec());
                            std::hint::black_box(copy);
                            frames += 1;
                            host.send(&protocol::ack(seq)).unwrap();
                        }
                        _ => {}
                    }
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            eprintln!("{w}x{h}: {:.1} frames/s", f64::from(frames) / start.elapsed().as_secs_f64());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
