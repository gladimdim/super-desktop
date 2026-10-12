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
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from plugin_isolated import ROOT, Isolated, alive, wait_for  # noqa: E402

BIN = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/debug/super-desktop"
FIXTURE = ROOT / "tests/fixtures/plugins/smoke"
failures = []


def check(ok, what, detail=""):
    print(("ok   " if ok else "FAIL ") + what + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(what)


def main():
    if not shutil.which("gtk4-broadwayd"):
        print("SKIP: gtk4-broadwayd is not installed (never falling back to the desktop)")
        return 0
    if not BIN.exists():
        print(f"SKIP: {BIN} is not built")
        return 0
    box = Isolated(BIN)
    env, home, runtime, base = box.env, box.home, box.runtime, box.base
    # A bindings.lua of the user's own, which plugins must give back unchanged.
    bindings = home / ".config/hypr/bindings.lua"
    bindings.parent.mkdir(parents=True)
    original_bindings = '-- my own binds\no.bind("SUPER + RETURN", "Terminal", "alacritty")\n'
    bindings.write_text(original_bindings)

    def cli(*args, check_ok=True):
        result = box.cli(*args)
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
        check(box.start(), "isolated daemon is listening")
        plugin_dir = box.copy_plugin(FIXTURE, "smoke")

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
        footprint = box.ipc({"op": "footprint", "id": "smoke"})
        check(footprint.get("footprint") == [], "footprint is empty after deactivate", footprint)
        check(wait_for(lambda: bindings.read_text() == original_bindings) is not None, "bindings.lua is byte for byte what it was", bindings.read_text())

        check(cli("activate", "smoke").returncode == 0 and wait_for(lambda: events().count("activated hello") >= 3) is not None, "it can be turned on again")
        check(wait_for(lambda: "plugin run smoke" in bindings.read_text()) is not None, "on again: bound again")
        # The safety switch works without the daemon too.
        check(box.stop_daemon(), "daemon stopped")
        check(cli("disable-all").returncode == 0, "disable-all without a daemon")
        check(bindings.read_text() == original_bindings, "disable-all took the shortcut out of bindings.lua", bindings.read_text())
        check(cli("remove", "smoke", "--purge").returncode == 0, "remove --purge")
        check(not (home / ".local/state/super-desktop/plugins/smoke").exists(), "its data is deleted")
    finally:
        if failures:
            print("daemon stderr:\n" + box.daemon_log())
        box.close()
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
