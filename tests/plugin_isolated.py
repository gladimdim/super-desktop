"""A real SUPER DESKTOP daemon, fully isolated, for plugin end-to-end tests.

Own HOME and XDG directories, runtime directory (socket), tmux server and a
private Broadway display; no phone bridge (SUPER_DESKTOP_NO_BRIDGE); a D-Bus
address that leads nowhere, so no notification reaches the user's desktop;
no Hyprland (hyprctl finds no instance). Nothing touches the user's session.
"""
import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SDK = ROOT / "skills/super-desktop-plugin/sdk/python/sd_plugin.py"


def wait_for(predicate, timeout=15.0, step=0.1):
    end = time.time() + timeout
    while time.time() < end:
        try:
            value = predicate()
        except (KeyError, TypeError, ValueError, IndexError, OSError):
            value = None
        if value:
            return value
        time.sleep(step)
    return None


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    try:
        return Path(f"/proc/{pid}/stat").read_text().split()[2] != "Z"
    except OSError:
        return False


class Isolated:
    def __init__(self, binary):
        self.bin = Path(binary)
        self.base = Path(tempfile.mkdtemp(prefix="sd-plugin-e2e-"))
        self.home, self.runtime = self.base / "home", self.base / "run"
        for d in (self.home, self.runtime, self.base / "tmux", self.base / "bin"):
            d.mkdir(mode=0o700)
        drop = ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE", "TMUX", "LD_PRELOAD", "DBUS_SESSION_BUS_ADDRESS")
        self.env = {k: v for k, v in os.environ.items() if k not in drop}
        self.env.update({
            "HOME": str(self.home),
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "XDG_STATE_HOME": str(self.home / ".local/state"),
            "XDG_DATA_HOME": str(self.home / ".local/share"),
            "XDG_CACHE_HOME": str(self.home / ".cache"),
            "XDG_RUNTIME_DIR": str(self.runtime),
            "TMUX_TMPDIR": str(self.base / "tmux"),
            "SUPER_DESKTOP_NO_BRIDGE": "1",
            "GDK_BACKEND": "broadway",
            "DBUS_SESSION_BUS_ADDRESS": f"unix:path={self.base}/no-bus",
            # Stubs (e.g. a fake `claude`) go first on PATH.
            "PATH": f"{self.base / 'bin'}:{os.environ.get('PATH', '')}",
        })
        self.broadway = self.daemon = None

    def start(self):
        display = 600 + os.getpid() % 300
        self.broadway = subprocess.Popen(["gtk4-broadwayd", "--address", "127.0.0.1", f":{display}"], env=self.env,
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        self.env["BROADWAY_DISPLAY"] = f":{display}"
        time.sleep(0.5)
        self.daemon = subprocess.Popen([str(self.bin), "daemon"], env=self.env, stdout=subprocess.DEVNULL,
                                       stderr=open(self.base / "daemon.err", "w"), start_new_session=True)
        return wait_for(lambda: (self.runtime / "super-desktop.sock").exists()) is not None

    def stop_daemon(self):
        try:
            subprocess.run([str(self.bin), "kill"], env=self.env, capture_output=True, timeout=10)
        except subprocess.TimeoutExpired:
            pass
        return wait_for(lambda: self.daemon.poll() is not None, timeout=10) is not None

    def close(self, keep=False):
        if self.daemon and self.daemon.poll() is None:
            self.stop_daemon()
        for process in (self.daemon, self.broadway):
            if process:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        subprocess.run(["tmux", "kill-server"], env=self.env, capture_output=True)
        if not keep:
            shutil.rmtree(self.base, ignore_errors=True)

    def daemon_log(self):
        path = self.base / "daemon.err"
        return path.read_text()[-4000:] if path.exists() else ""

    def cli(self, *args, timeout=30):
        return subprocess.run([str(self.bin), "plugin", *args], env=self.env, capture_output=True, text=True, timeout=timeout)

    def ipc(self, request):
        import socket
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(str(self.runtime / "super-desktop.sock"))
            s.sendall(f"plugin {json.dumps(request)}\n".encode())
            return json.loads(s.recv(1 << 20).decode())

    def status(self, plugin):
        return (self.ipc({"op": "status"}).get("plugins") or {}).get(plugin)

    def views(self, plugin):
        reply = self.ipc({"op": "views", "id": plugin})
        return reply.get("views") if reply.get("ok") else None

    def copy_plugin(self, source, name):
        target = self.base / name
        shutil.copytree(source, target, ignore=shutil.ignore_patterns("target", "__pycache__"))
        if not (target / "sd_plugin.py").exists():
            shutil.copy(SDK, target / "sd_plugin.py")
        return target

    def stub(self, name, script):
        path = self.base / "bin" / name
        path.write_text(script)
        path.chmod(0o755)
        return path
