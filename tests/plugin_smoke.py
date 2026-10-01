#!/usr/bin/env python3
"""End-to-end plugin check against a real, fully isolated SUPER DESKTOP daemon.

The daemon gets its own HOME, XDG directories, runtime directory (socket),
tmux server and a private Broadway display; the phone bridge is off
(SUPER_DESKTOP_NO_BRIDGE). Nothing reaches the user's desktop, Hyprland,
tmux server, bridge or settings.

Covers: link, activate (onStartup), a command over IPC, toolbar badge, a panel
opened and patched, a patch error carrying its hint, a permission refusal,
crash → automatic restart, a grandchild that ignores SIGTERM killed with the
process group on deactivate, the footprint being empty after deactivate, and
remove --purge.

Usage: tests/plugin_smoke.py [path/to/super-desktop]   (default: target/debug)
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/debug/super-desktop"
FIXTURE = ROOT / "tests/fixtures/plugins/smoke"
SDK = ROOT / "skills/super-desktop-plugin/sdk/python/sd_plugin.py"
failures = []


def check(ok, what, detail=""):
    print(("ok   " if ok else "FAIL ") + what + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(what)


def wait_for(predicate, timeout=15.0, step=0.1):
    end = time.time() + timeout
    while time.time() < end:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    # A zombie is gone for our purposes.
    try:
        return Path(f"/proc/{pid}/stat").read_text().split()[2] != "Z"
    except OSError:
        return False


def main():
    if not shutil.which("gtk4-broadwayd"):
        print("SKIP: gtk4-broadwayd is not installed (never falling back to the desktop)")
        return 0
    if not BIN.exists():
        print(f"SKIP: {BIN} is not built")
        return 0
    base = Path(tempfile.mkdtemp(prefix="sd-plugin-smoke-"))
    home, runtime, tmux = base / "home", base / "run", base / "tmux"
    for d in (home, runtime, tmux):
        d.mkdir(mode=0o700)
    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE", "TMUX", "LD_PRELOAD")}
    env.update({
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_STATE_HOME": str(home / ".local/state"),
        "XDG_DATA_HOME": str(home / ".local/share"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_RUNTIME_DIR": str(runtime),
        "TMUX_TMPDIR": str(tmux),
        "SUPER_DESKTOP_NO_BRIDGE": "1",
        "GDK_BACKEND": "broadway",
    })
    # A bindings.lua of the user's own, which plugins must give back unchanged.
    bindings = home / ".config/hypr/bindings.lua"
    bindings.parent.mkdir(parents=True)
    original_bindings = '-- my own binds\no.bind("SUPER + RETURN", "Terminal", "alacritty")\n'
    bindings.write_text(original_bindings)
    display = 600 + os.getpid() % 300
    broadway = subprocess.Popen(["gtk4-broadwayd", "--address", "127.0.0.1", f":{display}"], env=env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    env["BROADWAY_DISPLAY"] = f":{display}"
    time.sleep(0.5)
    daemon = subprocess.Popen([str(BIN), "daemon"], env=env, stdout=subprocess.DEVNULL,
                              stderr=open(base / "daemon.err", "w"), start_new_session=True)

    def cli(*args, check_ok=True):
        result = subprocess.run([str(BIN), "plugin", *args], env=env, capture_output=True, text=True, timeout=30)
        if check_ok and result.returncode != 0:
            print(result.stdout, result.stderr)
        return result

    def status():
        result = cli("list", "--json", check_ok=False)
        try:
            return {row["id"]: row for row in json.loads(result.stdout)}
        except ValueError:
            return {}

    def events():
        path = home / ".local/state/super-desktop/plugins/smoke/events.log"
        return path.read_text().splitlines() if path.exists() else []

    try:
        check(wait_for(lambda: (runtime / "super-desktop.sock").exists()) is not None, "isolated daemon is listening")
        plugin_dir = base / "smoke"
        shutil.copytree(FIXTURE, plugin_dir)
        shutil.copy(SDK, plugin_dir / "sd_plugin.py")

        check(cli("validate", str(plugin_dir)).returncode == 0, "fixture validates")
        check(cli("link", str(plugin_dir), "--yes").returncode == 0, "link")
        check(cli("activate", "smoke").returncode == 0, "activate")
        check(wait_for(lambda: "activated hello" in events()) is not None, "activated with the default setting", events())
        row = wait_for(lambda: status().get("smoke") if status().get("smoke", {}).get("state", {}).get("state") == "running" else None)
        check(row is not None, "list shows it running")
        pid = row["state"]["pid"] if row else None
        check(row is not None and row["state"]["toolbarItems"] == 1, "its toolbar item is in the bar")
        bound = wait_for(lambda: "super-desktop plugin run smoke smoke.open" in bindings.read_text())
        check(bound is not None and bindings.read_text().startswith(original_bindings), "its global shortcut is bound after the user's own lines", bindings.read_text())

        check(cli("run", "smoke", "smoke.open").returncode == 0, "run a command over IPC")
        opened = wait_for(lambda: [e for e in events() if e.startswith("opened ")])
        check(bool(opened) and "api=1" in opened[0] and "source=cli" in opened[0], "command ran: describe, badge, open, patch", opened)
        patch_error = [e for e in events() if e.startswith("patch-error ")]
        check(bool(patch_error) and "no node `missing`" in patch_error[0] and "references/ui.md" in patch_error[0], "a bad patch is refused with a docs pointer", patch_error)
        check("denied -32001" in events(), "terminal.send without its permission is refused", events())
        check(wait_for(lambda: ((status().get("smoke") or {}).get("state") or {}).get("views") == 1) is not None, "the panel is open")
        check(cli("run", "smoke", "smoke.nope", check_ok=False).returncode != 0, "an undeclared command is refused")

        # Crash: restarted after about a second, with a new process.
        cli("run", "smoke", "smoke.crash")
        restarted = wait_for(lambda: events().count("activated hello") >= 2, timeout=20)
        check(restarted is not None, "a crashed plugin is restarted", events())
        row = wait_for(lambda: status().get("smoke") if (status().get("smoke") or {}).get("state", {}) and status()["smoke"]["state"]["state"] == "running" else None)
        new_pid = row["state"]["pid"] if row else None
        check(new_pid is not None and new_pid != pid, "the restart is a new process")
        check(row is not None and row["state"]["views"] == 0, "the dead process's panel was closed")

        # A grandchild that ignores SIGTERM must still go with the plugin.
        cli("run", "smoke", "smoke.spawn")
        spawned = wait_for(lambda: [e for e in events() if e.startswith("spawned ")])
        grandchild = int(spawned[0].split()[1]) if spawned else None
        check(grandchild is not None and alive(grandchild), "the plugin started a child")

        check(cli("deactivate", "smoke").returncode == 0, "deactivate")
        check(wait_for(lambda: "deactivated" in events()) is not None, "the plugin was told to deactivate")
        check(wait_for(lambda: not alive(new_pid), timeout=6) is not None, "its process is gone")
        check(grandchild is not None and wait_for(lambda: not alive(grandchild), timeout=6) is not None, "its SIGTERM-ignoring child is gone too")
        state = status().get("smoke", {})
        check(state.get("active") is False and not state.get("state"), "nothing of it is registered", state)
        footprint = json.loads(subprocess.run(
            ["python3", "-c", "import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(b'plugin {\"op\":\"footprint\",\"id\":\"smoke\"}\\n');print(s.recv(65536).decode())", str(runtime / "super-desktop.sock")],
            capture_output=True, text=True).stdout)
        check(footprint.get("footprint") == [], "footprint is empty after deactivate", footprint)
        check(wait_for(lambda: bindings.read_text() == original_bindings) is not None, "bindings.lua is byte for byte what it was", bindings.read_text())

        check(cli("activate", "smoke").returncode == 0 and wait_for(lambda: events().count("activated hello") >= 3) is not None, "it can be turned on again")
        check(wait_for(lambda: "plugin run smoke" in bindings.read_text()) is not None, "on again: bound again")
        # The safety switch works without the daemon too.
        subprocess.run([str(BIN), "kill"], env=env, capture_output=True, timeout=10)
        check(wait_for(lambda: not (runtime / "super-desktop.sock").exists() or daemon.poll() is not None, timeout=10) is not None, "daemon stopped")
        check(cli("disable-all").returncode == 0, "disable-all without a daemon")
        check(bindings.read_text() == original_bindings, "disable-all took the shortcut out of bindings.lua", bindings.read_text())
        check(cli("remove", "smoke", "--purge").returncode == 0, "remove --purge")
        check(not (home / ".local/state/super-desktop/plugins/smoke").exists(), "its data is deleted")
    finally:
        try:
            subprocess.run([str(BIN), "kill"], env=env, capture_output=True, timeout=10)
        except subprocess.TimeoutExpired:
            pass
        for process in (daemon, broadway):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        subprocess.run(["tmux", "kill-server"], env=env, capture_output=True)
        if failures:
            print("daemon stderr:\n" + (base / "daemon.err").read_text()[-4000:])
        shutil.rmtree(base, ignore_errors=True)
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
