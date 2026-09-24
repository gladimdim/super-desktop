#!/usr/bin/env python3
"""Real isolated gateway/plugin check; no account, model call or user config."""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
EXE = REPO / "target/debug/super-desktop"


def main():
    cli = shutil.which("openclaw")
    if not cli:
        print("openclaw SKIP: not installed")
        return
    with tempfile.TemporaryDirectory(prefix="sd-openclaw-") as directory:
        home = Path(directory)
        # Keep ambient providers/channels and the user's gateway out of the probe.
        env = {key: os.environ[key] for key in ("PATH", "LANG", "TMPDIR") if key in os.environ}
        env.update(HOME=str(home), OPENCLAW_STATE_DIR=str(home / "gateway"),
                   OPENCLAW_CONFIG_PATH=str(home / "gateway/openclaw.json"),
                   XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
                   XDG_STATE_HOME=str(home / "state"), XDG_CACHE_HOME=str(home / "cache"),
                   OPENCLAW_SKIP_CHANNELS="1", OPENCLAW_SKIP_CRON="1")
        root = home / ".local/state/super-desktop/harness"
        root.mkdir(parents=True)
        state = root / "probe.json"
        state.write_text(json.dumps({"version": 1, "agent": "openclaw", "status": "unknown"}))
        reporter_env = dict(env, SD_HARNESS_FILE=str(state), SD_HARNESS_EXE=str(EXE),
                            SD_HARNESS_PID=str(os.getpid()))
        subprocess.run([EXE, "harness-event", "init"], input="{}", text=True,
                       env=reporter_env, check=True, timeout=5)
        (root / "sd_term_probe.link.json").write_text(json.dumps({"path": str(state), "exe": str(EXE)}))
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = home / "gateway/openclaw.json"
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({"gateway": {"mode": "local", "port": port,
            "auth": {"mode": "token", "token": "isolated-super-desktop-probe"}},
            "agents": {"defaults": {"workspace": str(home / "workspace")}},
            "discovery": {"mdns": {"mode": "off"}}}))
        installed = subprocess.run([EXE, "integrate-openclaw"], env=env, cwd=home,
                                   capture_output=True, text=True, timeout=60)
        if installed.returncode:
            raise RuntimeError("plugin activation failed: " + installed.stdout[-3000:] + installed.stderr[-3000:])
        with (home / "gateway.log").open("w+") as log:
            process = subprocess.Popen([cli, "gateway", "run", "--port", str(port), "--bind", "loopback"],
                                       env=env, cwd=home, stdout=log, stderr=log, start_new_session=True)
            def call(method, params):
                result = subprocess.run([cli, "gateway", "call", method, "--url", f"ws://127.0.0.1:{port}",
                    "--token", "isolated-super-desktop-probe", "--json", "--timeout", "2000",
                    "--params", json.dumps(params)], env=env, cwd=home, capture_output=True, text=True, timeout=10)
                return result
            try:
                deadline = time.monotonic() + 45
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError("gateway exited")
                    if call("health", {}).returncode == 0:
                        break
                    time.sleep(.3)
                else:
                    raise RuntimeError("gateway readiness timeout")
                key = "agent:main:sd_term_probe"
                result = call("sessions.create", {"key": key, "label": "Integration probe", "emitCommandHooks": True})
                if result.returncode:
                    raise RuntimeError(result.stdout + result.stderr)
                result = call("sessions.reset", {"key": key})
                if result.returncode:
                    raise RuntimeError(result.stdout + result.stderr)
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    value = json.loads(state.read_text())
                    if value.get("native_session", "").startswith(key + "/") and value.get("status") == "idle":
                        assert value["title"] == "Integration probe", value
                        result = call("sessions.patch", {"key": key, "label": "Renamed while idle"})
                        if result.returncode:
                            raise RuntimeError(result.stdout + result.stderr)
                        renamed_deadline = time.monotonic() + 6
                        while time.monotonic() < renamed_deadline:
                            if json.loads(state.read_text()).get("title") == "Renamed while idle":
                                break
                            time.sleep(.1)
                        else:
                            raise RuntimeError("idle title heartbeat failed: " + state.read_text())
                        print("openclaw PASS: production install, real gateway SessionStart, native identity and title")
                        print("openclaw PASS: idle gateway rename reaches the native metadata heartbeat")
                        return
                    time.sleep(.1)
                raise RuntimeError("missing native metadata: " + state.read_text())
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read()[-5000:])
                raise
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=5)
                except ProcessLookupError:
                    pass
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


if __name__ == "__main__":
    main()
