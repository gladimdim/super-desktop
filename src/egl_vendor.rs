//! Keep the overlay's GL renderer on the GPU that drives the screen.
//!
//! libglvnd tries NVIDIA's EGL before Mesa's (`10_nvidia.json` sorts before
//! `50_mesa.json`). On a hybrid laptop whose screens are all on the Intel or
//! AMD GPU, GTK's renderer then ran on the NVIDIA GPU and every frame was
//! copied across. That GPU runtime-suspends a few seconds after its last
//! frame, so a show after it slept blocked the GTK thread while it woke: about
//! 0.8 s before the first frame, against 12 ms on the GPU that shows it.
//!
//! When no connected output is on an NVIDIA GPU and Mesa's EGL is installed,
//! the daemon makes Mesa glvnd's only EGL vendor. glvnd reads its vendor list
//! once and keeps it, so the variable is set, the list is loaded and the
//! variable is removed again, all before GTK starts. Nothing the daemon
//! spawns (tmux, card shells and agents, games started from a card) inherits
//! it: they still get the NVIDIA GPU.
//!
//! A user who sets either glvnd variable keeps their own choice.
use std::path::{Path, PathBuf};

const VENDOR_FILES: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";
const VENDOR_DIRS: &str = "__EGL_VENDOR_LIBRARY_DIRS";
const NVIDIA_PCI_VENDOR: &str = "0x10de";
/// glvnd's configuration directory first, then the data directory.
const MESA_VENDOR_FILES: [&str; 2] = [
    "/etc/glvnd/egl_vendor.d/50_mesa.json",
    "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
];

/// One DRM card: whether it is an NVIDIA GPU, and how many of its outputs
/// have a display connected.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub nvidia: bool,
    pub connected_outputs: usize,
}

/// The cards under a sysfs DRM class directory (`/sys/class/drm`): `cardN`
/// entries, each with `device/vendor`, and `cardN-<connector>/status`.
pub fn read_gpus(drm: &Path) -> Vec<Gpu> {
    let Ok(entries) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    let names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    let mut cards: Vec<&String> = names
        .iter()
        .filter(|name| name.strip_prefix("card").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        .collect();
    cards.sort();
    cards
        .into_iter()
        .map(|card| {
            let vendor = std::fs::read_to_string(drm.join(card).join("device/vendor")).unwrap_or_default();
            let prefix = format!("{card}-");
            let connected_outputs = names
                .iter()
                .filter(|name| name.starts_with(&prefix))
                .filter(|name| {
                    std::fs::read_to_string(drm.join(name).join("status"))
                        .is_ok_and(|status| status.trim() == "connected")
                })
                .count();
            Gpu { nvidia: vendor.trim().eq_ignore_ascii_case(NVIDIA_PCI_VENDOR), connected_outputs }
        })
        .collect()
}

/// True when the screens are all on a non-NVIDIA GPU while an NVIDIA GPU is
/// present: the case where glvnd's default puts the renderer on the wrong GPU.
/// No connected output at all (lid closed, sysfs unreadable) keeps the default.
pub fn prefers_mesa(gpus: &[Gpu]) -> bool {
    gpus.iter().any(|gpu| gpu.nvidia)
        && gpus.iter().any(|gpu| !gpu.nvidia && gpu.connected_outputs > 0)
        && !gpus.iter().any(|gpu| gpu.nvidia && gpu.connected_outputs > 0)
}

/// Run once in the daemon, before GTK and before any thread exists:
/// environment changes are not thread-safe.
///
/// Returns whether Mesa was made the EGL vendor.
pub fn prefer_display_gpu() -> bool {
    prefer_with(Path::new("/sys/class/drm"), &MESA_VENDOR_FILES.map(PathBuf::from))
}

/// [`prefer_display_gpu`] against a given sysfs DRM directory and Mesa
/// vendor file candidates.
fn prefer_with(drm: &Path, mesa_files: &[PathBuf]) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        return false; // Headless: no renderer to place.
    }
    if std::env::var_os(VENDOR_FILES).is_some() || std::env::var_os(VENDOR_DIRS).is_some() {
        return false;
    }
    if !prefers_mesa(&read_gpus(drm)) {
        return false;
    }
    let Some(mesa) = mesa_files.iter().find(|path| path.is_file()) else {
        return false;
    };
    std::env::set_var(VENDOR_FILES, &mesa);
    let loaded = load_vendor_list();
    std::env::remove_var(VENDOR_FILES);
    loaded
}

/// Make glvnd read and cache its vendor list now. Querying the client
/// extensions of `EGL_NO_DISPLAY` loads every vendor; GTK later opens the same
/// `libEGL.so.1` and gets that list.
fn load_vendor_list() -> bool {
    const EGL_EXTENSIONS: i32 = 0x3055;
    type QueryString = unsafe extern "C" fn(*mut libc::c_void, i32) -> *const libc::c_char;
    // SAFETY: plain dlopen/dlsym of libglvnd's libEGL, and a call that is
    // valid with EGL_NO_DISPLAY. The handle stays open: GTK uses the library.
    unsafe {
        let handle = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            return false;
        }
        let symbol = libc::dlsym(handle, c"eglQueryString".as_ptr());
        if symbol.is_null() {
            return false;
        }
        let query: QueryString = std::mem::transmute(symbol);
        !query(std::ptr::null_mut(), EGL_EXTENSIONS).is_null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &str = "SD_EGL_VENDOR_PROBE_ROOT";

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The variable is set only while glvnd loads its list: neither this
    /// process nor anything it spawns afterwards sees it. Environment changes
    /// are not thread-safe, so the check runs in a child test process.
    #[test]
    fn egl_vendor_variable_never_reaches_children() {
        if let Some(root) = std::env::var_os(PROBE) {
            let root = PathBuf::from(root);
            let mesa = root.join("50_mesa.json");
            prefer_with(&root.join("drm"), &[mesa]);
            assert!(std::env::var_os(VENDOR_FILES).is_none());
            let child = std::process::Command::new("sh")
                .args(["-c", &format!("printf '%s' \"${VENDOR_FILES}\"")])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&child.stdout), "");
            return;
        }
        let root = std::env::temp_dir().join(format!("sd-egl-vendor-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "drm/card1/device/vendor", "0x10de\n");
        write(&root, "drm/card1-eDP-1/status", "disconnected\n");
        write(&root, "drm/card2/device/vendor", "0x8086\n");
        write(&root, "drm/card2-eDP-2/status", "connected\n");
        write(&root, "50_mesa.json", "{}\n");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "egl_vendor::tests::egl_vendor_variable_never_reaches_children", "--test-threads=1"])
            .env(PROBE, &root)
            .env("WAYLAND_DISPLAY", "sd-egl-vendor-test")
            .env_remove(VENDOR_FILES)
            .env_remove(VENDOR_DIRS)
            .status()
            .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(status.success());
    }

    fn gpu(nvidia: bool, connected_outputs: usize) -> Gpu {
        Gpu { nvidia, connected_outputs }
    }

    #[test]
    fn egl_vendor_mesa_only_when_screens_are_off_the_nvidia_gpu() {
        // Hybrid laptop, internal screen on Intel/AMD: the overlay belongs there.
        assert!(prefers_mesa(&[gpu(true, 0), gpu(false, 1)]));
        // Monitor on the NVIDIA GPU, or an NVIDIA-only machine: keep glvnd's order.
        assert!(!prefers_mesa(&[gpu(true, 1), gpu(false, 1)]));
        assert!(!prefers_mesa(&[gpu(true, 1)]));
        // No NVIDIA GPU: nothing to steer away from.
        assert!(!prefers_mesa(&[gpu(false, 1)]));
        // Nothing connected or nothing readable: keep the default.
        assert!(!prefers_mesa(&[gpu(true, 0), gpu(false, 0)]));
        assert!(!prefers_mesa(&[]));
    }

    #[test]
    fn egl_vendor_reads_cards_and_connectors_from_sysfs() {
        let root = std::env::temp_dir().join(format!("sd-egl-vendor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| write(&root, path, text);
        write("card1/device/vendor", "0x10de\n");
        write("card1-eDP-1/status", "disconnected\n");
        write("card1-HDMI-A-1/status", "disconnected\n");
        write("card2/device/vendor", "0x8086\n");
        write("card2-eDP-2/status", "connected\n");
        write("card2-DP-4/status", "disconnected\n");
        write("renderD128/dev", "226:128\n");
        write("version", "drm 1.1.0\n");
        let gpus = read_gpus(&root);
        assert_eq!(gpus, vec![gpu(true, 0), gpu(false, 1)]);
        assert!(prefers_mesa(&gpus));
        // An external monitor on the NVIDIA GPU flips the decision.
        write("card1-HDMI-A-1/status", "connected\n");
        assert!(!prefers_mesa(&read_gpus(&root)));
        // `card1` must not count `card10`'s connectors.
        write("card10/device/vendor", "0x1002\n");
        write("card10-DP-1/status", "connected\n");
        assert_eq!(read_gpus(&root)[0], gpu(true, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
