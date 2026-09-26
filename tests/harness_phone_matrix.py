#!/usr/bin/env python3
"""Phone-path compatibility matrix for every installed harness.

    python3 tests/harness_phone_matrix.py [--only NAME ...] [--submit] [--claude-bang]
                                          [--no-fixtures] [--fixtures-dir DIR]
                                          [BINARY TEST_BINARY]

Runtime: about 2 minutes for the ten installed harnesses (about 10 s each,
longer when a trust fallback relaunches). `--submit` adds up to about a minute
per harness (about 7 minutes in total). Builds the binaries first unless
BINARY and TEST_BINARY are given.

For each harness this launches it exactly as a desktop card would: the
desktop's own `tmux::create_session` (resolved command, default flags,
shell-title and harness-metadata wrapper), called through the env-gated unit
test `tmux::tests::phone_matrix_launch`. It runs in a PRIVATE tmux server
(`TMUX_TMPDIR`, `$TMUX` dropped, the user's tmux.conf loaded like the real
server) with a mirror HOME. The mirror symlinks everything to the user's HOME
except SUPER DESKTOP's own state (state.json, harness-metadata root), which is
private. A second, isolated `harness-bridge` serves it with its own state dir,
TLS identity, token, runtime dir and ephemeral port. The script then acts as
the Android app: pinned-TLS `WS /api/v1/harnesses/<id>/stream?ansiOnly=1` and
the ordered input socket `/api/v1/harnesses/<id>/input`, using Android's
payloads (`{"sequence","text","enter","checkIdle"}` and TerminalKey bytes).

Checks per harness:
- The first frame arrives with a sane grid.
- A typed marker (`enter:false`) echoes, and Backspace clears it.
- Arrows, Tab, Shift-Tab, PgUp, PgDn and Esc are acknowledged, the harness
  survives, and the input mode is restored.
- tmux `alternate_on` and `history_size` (whether the phone gets scrollback).
- The harness-list and stream titles never contain typed or injected text.
- The final EXITED frame is followed by a 1000 close.
With `--submit` (a real model request, one per harness), it sends one tiny,
tool-free prompt with `enter:true`. It then records status transitions, the
reply in the stream, scrollback growth, title == prompt,
`POST /api/v1/completions`, and any error the phone would show. Captured
tailAnsi frames are sanitized, then saved as Android parser fixtures
(`harness-frames/*.ansi` plus `manifest.tsv`).

With `--attachments`, it also checks phone prompt attachments without
submitting anything: the ignored unit test
`prompt_attachments::tests::native_attachment_probe` stages a real PNG and a
text file and fills the harness's composer exactly as the bridge does (native
`[Image …]` attachment, or the paths in the prompt), reports what the
harness shows, and the draft is cleared. Enter is never sent.

SAFETY: without `--submit`, typed text is never submitted. `--claude-bang`
opts in to Claude's `!` bash mode, which in some Claude input modes also
triggers a model reply. Trust, login and onboarding dialogs are never
accepted; the harness is reported BLOCKED. For trust, it is retried in a
scratch folder under a repository the user already trusted. Keys that a
harness saves to its config (Grok's Shift-Tab) are not sent. Harnesses use the
user's real configuration and logins, so a launch may write their normal
session and state files (reported per harness). OpenClaw talks to a private,
isolated gateway, never the user's. The user's daemon, bridge (8759), default
tmux server, ~/.config/super-desktop and paired devices are never touched.
Every process started here is stopped by its own PID or private socket.
"""
import argparse
import base64
import base64
import hashlib
import json
import os
import pathlib
import re
import secrets
import shutil
import signal
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
ANDROID_FIXTURES = (REPO.parent / "OmarchyAILauncher/android/app/src/test/resources/harness-frames")
# Git-ignored build output of the Android repository (a trusted folder for some CLIs).
ANDROID_BUILD = REPO.parent / "OmarchyAILauncher/android/app/build"
# Scratch space outside /tmp (some CLIs refuse to install helpers under /tmp).
MATRIX_DIR = REPO / "target" / f"phone-matrix-{os.getpid()}"
STARTED = time.monotonic()
HOME = pathlib.Path.home()

# (key, commands, npx package) — mirrors tmux::get_agent_config for the
# harnesses this matrix knows how to drive.
HARNESSES = [
    ("claude", ["claude"], None),
    ("codex", ["codex"], None),
    ("opencode", ["opencode"], None),
    ("grok", ["grok"], None),
    ("pi", ["pi"], None),
    ("hermes", ["hermes"], None),
    ("gemini", ["gemini"], None),
    ("cursor", ["cursor-agent"], None),
    ("crush", ["crush"], None),
    ("openclaw", ["openclaw"], None),
    ("reasonix", ["reasonix"], "reasonix"),
    ("aider", ["aider"], None),
    ("goose", ["goose"], None),
    ("antigravity", ["agy", "antigravity"], None),
]

# Android's TerminalKey bytes (TerminalSettings.kt); all sent with enter:false.
KEYS = [
    ("Up", "\x1b[A"), ("Down", "\x1b[B"), ("Left", "\x1b[D"), ("Right", "\x1b[C"),
    ("PgUp", "\x1b[5~"), ("PgDn", "\x1b[6~"),
    # Toggles/cycles are sent in pairs so a harness ends in its original mode.
    ("Tab", "\t"), ("Tab", "\t"), ("Shift-Tab", "\x1b[Z"), ("Shift-Tab", "\x1b[Z"),
    ("Esc", "\x1b"),
]
BACKSPACE = "\x7f"
CTRL_U = "\x15"

# Screens that need a human decision we must not make for the user.
BLOCKERS = [
    (re.compile(r"do you trust|trust (the files|this folder|this directory|this workspace)|workspace trust", re.I), "workspace trust dialog"),
    (re.compile(r"signing in with the browser|click this link to log in|select login method|please (log|sign) ?in|login required|sign in with|log in with|authenticate (with|to)", re.I), "login required"),
    (re.compile(r"let's choose a provider|choose a provider and model|choose the text style|let's get started|welcome to .* setup|onboarding|first[- ]run setup", re.I), "onboarding dialog"),
    (re.compile(r"(enter|paste) (your )?(an )?api key|missing api key|no api key", re.I), "API key required"),
]

# Checked before launching: launching these would start an interactive login
# (cursor-agent opens a browser sign-in on startup when logged out).
PRECHECKS = {
    "cursor": (["cursor-agent", "status"], r"not logged in",
               "login required (`cursor-agent status`: not logged in); not launched"),
}

# Keys whose effect a harness writes to the user's own configuration.
PERSISTED_KEYS = {
    "grok": {"Shift-Tab": "Grok saves the cycled permission mode to ~/.grok/config.toml"},
}

# `--submit`: one tiny, tool-free prompt per harness (user-approved scope).
PROMPT = "Reply with just the word OK and nothing else."
COMPLETION_AGENTS = {"claude", "codex", "pi", "opencode"}
ERROR_LINE = re.compile(r"error|limit|quota|unauthori[sz]ed|forbidden|\b40[13]\b|\b429\b|credit|rate[- ]limit|"
                        r"no model|not logged|api key|failed|overloaded", re.I)
REPLY_LINE = re.compile(r"^[\s●⏺•>│┃*⎿✦◆⚕❯›─-]*OK\.?[\s│┃]*$", re.M)

# Non-modal warnings: the composer still works, so the probe continues.
WARNINGS = re.compile(r"no models available|use /login|not (logged|signed) in|no (model|provider) (is )?configured", re.I)

ESCAPES = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")

SUBMIT = False
CLAUDE_BANG = False
ATTACHMENTS = False
# Harnesses that turn a pasted image path into their own attachment
# (prompt_attachments::delivery); OpenCode only outside its `--mini` interface.
NATIVE_ATTACHMENTS = {"claude", "codex", "opencode", "grok"}


def log(message):
    print(f"[{time.monotonic() - STARTED:6.1f}s] {message}", flush=True)


def strip(ansi):
    return ESCAPES.sub("", ansi or "")


def wait_for(condition, timeout, interval=0.1):
    deadline = time.monotonic() + timeout
    while True:
        value = condition()
        if value:
            return value
        if time.monotonic() >= deadline:
            return None
        time.sleep(interval)


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def private_dir(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path, 0o700)
    return path


# Variables that would attach a child to the CALLER's world: its tmux server,
# its own harness card metadata, a parent Claude Code session, or the
# daemon's layer-shell preload (which the daemon strips from its children).
LEAKY = ("TMUX", "TMUX_PANE", "LD_PRELOAD", "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
         "CLAUDECODE", "CLAUDE_PID", "CLAUDE_EFFORT", "AI_AGENT")
LEAKY_PREFIXES = ("SD_HARNESS_", "CLAUDE_CODE_", "SUPER_DESKTOP_")


def base_env(tmux_dir, home):
    env = {k: v for k, v in os.environ.items()
           if k not in LEAKY and not k.startswith(LEAKY_PREFIXES)}
    env["TMUX_TMPDIR"] = str(tmux_dir)
    env["HOME"] = str(home)
    return env


def mirror_home(home):
    """A HOME whose entries are symlinks to the user's, except SUPER DESKTOP's.

    Harnesses keep their real configuration, logins and caches, while the
    desktop's own per-HOME state (~/.config/super-desktop/state.json and the
    harness-metadata root ~/.local/state/super-desktop/harness, which the
    launcher, hooks and bridge must all agree on) is private to this run.
    """
    private_dir(home)
    shadowed = {".config": "super-desktop", ".local": "state", ".local/state": "super-desktop"}

    def fill(relative):
        private_dir(home / relative)
        skip = shadowed[relative]
        for entry in (HOME / relative).iterdir():
            name = entry.name
            target = pathlib.Path(relative) / name
            if name == skip:
                if str(target) in shadowed:
                    fill(str(target))
                continue
            (home / target).symlink_to(entry)
    for entry in HOME.iterdir():
        if entry.name in (".config", ".local"):
            fill(entry.name)
        else:
            (home / entry.name).symlink_to(entry)
    private_dir(home / ".config/super-desktop")
    private_dir(home / ".local/state/super-desktop")
    return home


# --------------------------------------------------------------------------
# Minimal pinned-TLS WebSocket client (what OkHttp does for Android).
# --------------------------------------------------------------------------

class WebSocket:
    def __init__(self, context, port, path, token, fingerprint):
        raw = socket.create_connection(("127.0.0.1", port), timeout=10)
        self.sock = context.wrap_socket(raw, server_hostname="super-desktop.local")
        peer = hashlib.sha256(self.sock.getpeercert(binary_form=True)).hexdigest()
        assert peer == fingerprint, "certificate pin mismatch"
        key = base64.b64encode(secrets.token_bytes(16)).decode()
        self.sock.sendall((f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
                           f"Authorization: Bearer {token}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                           f"Sec-WebSocket-Version: 13\r\n\r\n").encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(1)
            if not chunk:
                raise ConnectionError("upgrade refused: " + head.decode(errors="replace"))
            head += chunk
        if b" 101 " not in head.split(b"\r\n", 1)[0]:
            raise ConnectionError("upgrade refused: " + head.decode(errors="replace")[:200])
        self.sock.settimeout(None)
        self.messages = []          # (monotonic time, text)
        self.cond = threading.Condition()
        self.closed = None
        self.lock = threading.Lock()
        threading.Thread(target=self._reader, daemon=True).start()

    def _exact(self, n):
        data = b""
        while len(data) < n:
            chunk = self.sock.recv(n - len(data))
            if not chunk:
                raise ConnectionError("eof")
            data += chunk
        return data

    def _reader(self):
        buffer = b""
        try:
            while True:
                b0, b1 = self._exact(2)
                opcode, length = b0 & 0x0F, b1 & 0x7F
                if length == 126:
                    length = struct.unpack(">H", self._exact(2))[0]
                elif length == 127:
                    length = struct.unpack(">Q", self._exact(8))[0]
                payload = self._exact(length)
                if opcode == 0x8:
                    code = struct.unpack(">H", payload[:2])[0] if len(payload) >= 2 else None
                    self.closed = (code, payload[2:].decode(errors="replace"))
                    break
                if opcode == 0x9:
                    self._send(0xA, payload)
                    continue
                if opcode in (0x1, 0x0):
                    buffer += payload
                    if b0 & 0x80:
                        with self.cond:
                            self.messages.append((time.monotonic(), buffer.decode()))
                            self.cond.notify_all()
                        buffer = b""
        except (OSError, ConnectionError, ValueError):
            if self.closed is None:
                self.closed = (None, "eof")
        with self.cond:
            self.cond.notify_all()

    def _send(self, opcode, payload):
        mask = secrets.token_bytes(4)
        header = bytes([0x80 | opcode])
        n = len(payload)
        if n < 126:
            header += bytes([0x80 | n])
        elif n < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", n)
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", n)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        with self.lock:
            self.sock.sendall(header + mask + masked)

    def send_text(self, text):
        self._send(0x1, text.encode())

    def wait(self, predicate, start, timeout):
        """First message at index >= start satisfying predicate (index, value)."""
        deadline = time.monotonic() + timeout
        with self.cond:
            while True:
                for index in range(start, len(self.messages)):
                    value = predicate(self.messages[index][1])
                    if value:
                        return index, value
                start = len(self.messages)
                remaining = deadline - time.monotonic()
                if remaining <= 0 or self.closed:
                    return None
                self.cond.wait(remaining)

    def close(self):
        try:
            self._send(0x8, struct.pack(">H", 1000))
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass


class Input:
    """TerminalInput.kt: one ordered socket, numbered inputs, acknowledged."""

    def __init__(self, ws):
        self.ws = ws
        self.sequence = 0

    def send(self, text, enter=False, timeout=10):
        self.sequence += 1
        number = self.sequence
        start = len(self.ws.messages)
        self.ws.send_text(json.dumps({"sequence": number, "text": text, "enter": enter,
                                      "checkIdle": False}))

        def ack(message):
            value = json.loads(message)
            return value if value.get("sequence") == number else None
        found = self.ws.wait(ack, start, timeout)
        return found[1] if found else None


# --------------------------------------------------------------------------
# Environment: private tmux server, isolated bridge, desktop launch entry.
# --------------------------------------------------------------------------

class World:
    def __init__(self, root, binary, test_binary):
        self.root = root
        self.binary = binary
        self.tmux_dir = private_dir(root / "tmux")
        # One HOME for the tmux server (so every harness), the launch entry
        # and the bridge, exactly as they share one on a desktop.
        private_dir(MATRIX_DIR)
        self.home = mirror_home(MATRIX_DIR / "home")
        self.env = base_env(self.tmux_dir, self.home)
        self.processes = []
        self.gateways = []
        self.terminals = []
        # The tmux server gets the user's normal environment and tmux.conf
        # (history-limit, extended keys…) exactly like the desktop's server.
        self.tmux("new-session", "-d", "-s", "sd_matrix_keepalive", "-x", "120", "-y", "35",
                  "sleep", "86400")
        self.tmux_pid = int(self.tmux("display-message", "-p", "#{pid}"))
        socket_path = self.tmux("display-message", "-p", "#{socket_path}")
        assert socket_path.startswith(str(self.tmux_dir)), socket_path
        log(f"private tmux server pid {self.tmux_pid} at {socket_path}")
        self._launcher(test_binary)
        self._bridge()

    def tmux(self, *args, check=True):
        result = subprocess.run(["tmux", *args], env=self.env, capture_output=True, encoding="utf-8", errors="replace",
                                timeout=15)
        if check and result.returncode:
            raise RuntimeError(f"tmux {args}: {result.stderr.strip()}")
        return result.stdout.strip()

    # -- the desktop's own launch path --------------------------------------
    def _launcher(self, test_binary):
        # Hooks call `<dir of current_exe>/super-desktop-client harness-event`.
        # The unit-test executable lives in target/debug/deps, so give it a
        # directory where that client exists (hard link: same inode, and
        # /proc/self/exe keeps this path).
        self.launch_dir = MATRIX_DIR
        exe = self.launch_dir / "super-desktop-launch-test"
        try:
            os.link(test_binary, exe)
        except OSError:
            shutil.copy2(test_binary, exe)
        (self.launch_dir / "super-desktop-client").symlink_to(
            pathlib.Path(self.binary).with_name("super-desktop-client"))
        self.launch_exe = exe
        self.android_scratch = ANDROID_BUILD / f"phone-matrix-{os.getpid()}"

    def launch(self, agent, workspace):
        spec = self.root / f"launch-{agent}.json"
        out = self.root / f"launched-{agent}.json"
        spec.write_text(json.dumps({"agent": agent, "workspace": str(workspace), "out": str(out)}))
        env = dict(self.env, SUPER_DESKTOP_PHONE_MATRIX_LAUNCH=str(spec), RUST_TEST_THREADS="1")
        result = subprocess.run([str(self.launch_exe), "--exact", "tmux::tests::phone_matrix_launch",
                                 "--nocapture", "--quiet"], env=env, capture_output=True, encoding="utf-8", errors="replace",
                                timeout=60)
        if result.returncode or not out.exists():
            raise RuntimeError("launch entry failed: " + result.stdout[-2000:] + result.stderr[-2000:])
        launched = json.loads(out.read_text())
        self.terminals.append({
            "id": launched["session"], "session_name": launched["session"], "agent_type": agent,
            "command": launched["command"], "x": 0, "y": 0, "created_at": time.time(),
            "tag": 1, "workspace_dir": str(workspace),
        })
        self._write_state()
        return launched

    # -- isolated bridge ---------------------------------------------------
    def _write_state(self):
        path = self.home / ".config/super-desktop/state.json"
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"notes": [], "terminals": self.terminals,
                                         "visible_harnesses": []}))
        temporary.replace(path)

    def _bridge(self):
        self._write_state()
        self.bridge_state = private_dir(self.root / "bridge-state")
        self.bridge_runtime = private_dir(self.root / "bridge-run")
        # Own runtime dir: no owner IPC can reach the user's daemon.
        env = dict(self.env, XDG_RUNTIME_DIR=str(self.bridge_runtime),
                   SUPER_DESKTOP_BRIDGE_STATE_DIR=str(self.bridge_state))
        self.port = free_port()
        assert self.port != 8759
        self.bridge_log = open(self.root / "bridge.log", "w")
        self.bridge = subprocess.Popen([self.binary, "harness-bridge", str(self.port)], env=env,
                                       stdout=self.bridge_log, stderr=self.bridge_log)
        self.processes.append(self.bridge)
        control = self.bridge_state / "control.sock"
        if not wait_for(lambda: control.exists() or self.bridge.poll() is not None, 10) \
                or self.bridge.poll() is not None:
            raise RuntimeError("isolated bridge did not start")
        identity = json.loads((self.bridge_state / "tls-identity.json").read_text())
        cert = bytes(identity["certificate"])
        self.fingerprint = hashlib.sha256(cert).hexdigest()
        self.context = ssl.create_default_context(cadata=ssl.DER_cert_to_PEM_cert(cert))
        self.context.check_hostname = False  # the exact leaf certificate is the pin
        invitation = self.admin("/api/v1/pair/invitation", {})
        assert invitation["fingerprint"].lower() == self.fingerprint, "QR pin != served certificate"
        status, pending = self.https("/api/v1/pair", {"deviceName": "Phone matrix",
                                                      "secret": invitation["secret"]})
        assert status == 202, pending
        self.admin("/api/v1/pair/approve", {"requestId": pending["requestId"]})
        self.token = self.https("/api/v1/pair/poll", {"requestId": pending["requestId"]})[1]["token"]
        log(f"isolated bridge pid {self.bridge.pid} on 127.0.0.1:{self.port}, paired test phone")

    def admin(self, path, body=None):
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(5)
            client.connect(str(self.bridge_state / "control.sock"))
            payload = json.dumps(body or {}).encode()
            client.sendall((f"{'POST' if body is not None else 'GET'} {path} HTTP/1.1\r\n"
                            f"Host: local\r\nContent-Length: {len(payload)}\r\n\r\n").encode() + payload)
            response = b""
            while chunk := client.recv(65536):
                response += chunk
        assert response.startswith(b"HTTP/1.1 200"), response[:200]
        return json.loads(response.split(b"\r\n\r\n", 1)[1])

    def https(self, path, body=None, token=None):
        import http.client
        connection = http.client.HTTPSConnection("127.0.0.1", self.port, context=self.context, timeout=15)
        try:
            headers = {"Authorization": "Bearer " + token} if token else {}
            connection.request("POST" if body is not None else "GET", path,
                               json.dumps(body) if body is not None else None, headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read() or b"null")
        finally:
            connection.close()

    def ws(self, path):
        return WebSocket(self.context, self.port, path, self.token, self.fingerprint)

    def harness_entry(self, session):
        status, document = self.https("/api/v1/harnesses", token=self.token)
        assert status == 200, document
        return next((h for h in document["harnesses"] if h["id"] == session), None)

    # -- OpenClaw: a private gateway, never the user's -------------------------
    def openclaw_gateway(self):
        home = private_dir(self.root / "openclaw")
        port = free_port()
        token = "isolated-phone-matrix-" + secrets.token_hex(8)
        config = home / "openclaw.json"
        config.write_text(json.dumps({
            "gateway": {"mode": "local", "port": port, "auth": {"mode": "token", "token": token}},
            "agents": {"defaults": {"workspace": str(home / "workspace")}},
            "discovery": {"mdns": {"mode": "off"}}}))
        env = {k: os.environ[k] for k in ("PATH", "LANG", "TMPDIR") if k in os.environ}
        env.update(HOME=str(home), OPENCLAW_STATE_DIR=str(home), OPENCLAW_CONFIG_PATH=str(config),
                   XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
                   XDG_STATE_HOME=str(home / "state"), XDG_CACHE_HOME=str(home / "cache"),
                   OPENCLAW_SKIP_CHANNELS="1", OPENCLAW_SKIP_CRON="1")
        logfile = open(home / "gateway.log", "w")
        process = subprocess.Popen(["openclaw", "gateway", "run", "--port", str(port), "--bind", "loopback"],
                                   env=env, cwd=home, stdout=logfile, stderr=logfile,
                                   start_new_session=True)
        self.gateways.append(process)

        def healthy():
            if process.poll() is not None:
                raise RuntimeError("isolated OpenClaw gateway exited")
            return subprocess.run(["openclaw", "gateway", "call", "health", "--url", f"ws://127.0.0.1:{port}",
                                   "--token", token, "--json", "--timeout", "2000", "--params", "{}"],
                                  env=env, cwd=home, capture_output=True, timeout=15).returncode == 0
        if not wait_for(healthy, 60, 0.5):
            raise RuntimeError("isolated OpenClaw gateway not healthy")
        # Only the TUI launched next sees these (set on OUR private server).
        return {"OPENCLAW_STATE_DIR": str(home), "OPENCLAW_CONFIG_PATH": str(config),
                "OPENCLAW_GATEWAY_TOKEN": token, "OPENCLAW_GATEWAY_URL": f"ws://127.0.0.1:{port}"}

    # -- pane inspection ----------------------------------------------------
    def pane(self, session, fmt):
        result = subprocess.run(["tmux", "display-message", "-p", "-t", f"={session}:", fmt],
                                env=self.env, capture_output=True, encoding="utf-8", errors="replace", timeout=10)
        return result.stdout.strip() if result.returncode == 0 else None

    def alive(self, session):
        value = self.pane(session, "#{pane_dead}")
        return value == "0"

    def capture(self, session):
        result = subprocess.run(["tmux", "capture-pane", "-p", "-t", f"={session}:"], env=self.env,
                                capture_output=True, encoding="utf-8", errors="replace", timeout=10)
        return result.stdout if result.returncode == 0 else ""

    def close(self):
        for process in self.gateways:
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)
            except ProcessLookupError:
                pass
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        # Our private server only: its socket lives in our TMUX_TMPDIR.
        self.tmux("kill-server", check=False)
        try:
            os.kill(self.tmux_pid, 0)
            time.sleep(0.5)
            os.kill(self.tmux_pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        shutil.rmtree(self.launch_dir, ignore_errors=True)
        shutil.rmtree(self.android_scratch, ignore_errors=True)


# --------------------------------------------------------------------------
# Persistent-state observation (reported, never reverted).
# --------------------------------------------------------------------------

# Each harness's own configuration/state roots. Other live sessions of the
# user may touch these concurrently, so a change is a hint, not proof.
WATCHED = {
    "claude": [".claude.json", ".claude"], "codex": [".codex"],
    "opencode": [".config/opencode", ".local/state/opencode", ".local/share/opencode"],
    "grok": [".grok"], "pi": [".pi"], "hermes": [".hermes"], "gemini": [".gemini"],
    "cursor": [".cursor", ".config/cursor"], "crush": [".config/crush", ".local/share/crush"],
    "openclaw": [".openclaw"], "reasonix": [".reasonix"],
}


def snapshot_state(key):
    seen = {}
    for name in WATCHED.get(key, []):
        top = HOME / name
        if top.is_file():
            stat = top.stat()
            seen[name] = (stat.st_mtime_ns, stat.st_size)
            continue
        if not top.is_dir():
            continue
        for directory, dirs, files in os.walk(top):
            depth = pathlib.Path(directory).relative_to(top).parts
            if len(depth) >= 2 or any(p in ("node_modules", ".git", "cache", "Cache") for p in depth):
                dirs[:] = []
            for file in files[:500]:
                path = pathlib.Path(directory) / file
                try:
                    stat = path.stat()
                except OSError:
                    continue
                seen[str(path.relative_to(HOME))] = (stat.st_mtime_ns, stat.st_size)
    return seen


def mirror_writes(home):
    """Entries a harness created in (or replaced a symlink of) the mirror HOME."""
    ours = {".config", ".config/super-desktop", ".local", ".local/state", ".local/state/super-desktop"}
    found = set()
    for relative in ("", ".config", ".local", ".local/state"):
        for entry in (home / relative).iterdir():
            name = str(pathlib.Path(relative) / entry.name)
            if not entry.is_symlink() and name not in ours:
                found.add("~/" + name)
    return found


def state_changes(before, after):
    changed = sorted(k for k in after if before.get(k) != after[k])
    return ["~/" + k for k in changed]


# --------------------------------------------------------------------------
# Fixture sanitization.
# --------------------------------------------------------------------------

def sanitizer(root):
    user = os.environ.get("USER") or HOME.name
    host = socket.gethostname()
    git_name = subprocess.run(["git", "config", "user.name"], capture_output=True, text=True).stdout.strip()
    words = [w for w in git_name.split() if len(w) >= 3]
    rules = [
        (re.escape(str(MATRIX_DIR / "home")), "~"),
        (r"phone-matrix-\d+", "phone-matrix"),
        # Reasonix shows the account balance (styled: SGR between label and value).
        (r"BAL((?:\x1b\[[0-9;:]*m|\s)+)\$[0-9.,]+", r"BAL\1$0.00"),
        (re.escape(str(root)), "/tmp/sd-phone-matrix"),
        (re.escape(str(HOME)), "~"),
        (r"\b(challenge|uuid|token|code|key|secret|state)=[^&\s]+", r"\1=<redacted>"),
        (r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "user@example.com"),
        (r"\b(sk-|sk_|pk_|rk_|xai-|gsk_|ghp_|gho_|github_pat_|AIza)[-_A-Za-z0-9]{16,}", "<redacted-key>"),
        (r"\b[0-9a-fA-F]{32,}\b", "<redacted-hex>"),
        (r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
         "00000000-0000-0000-0000-000000000000"),
    ]
    if host:
        rules.append((re.escape(host), "host"))
    if user:
        rules.append((re.escape(user), "user"))
    for word in words:
        rules.append((re.escape(word), "User"))
    compiled = [(re.compile(p, re.I), r) for p, r in rules]

    def clean(text):
        for pattern, replacement in compiled:
            text = pattern.sub(replacement, text)
        return text
    return clean


def leaks(text, root):
    """Identifying strings still present after sanitization."""
    needles = [str(HOME), os.environ.get("USER") or HOME.name, socket.gethostname(), str(root)]
    git_name = subprocess.run(["git", "config", "user.name"], capture_output=True, text=True).stdout.strip()
    git_mail = subprocess.run(["git", "config", "user.email"], capture_output=True, text=True).stdout.strip()
    needles += [w for w in git_name.split() if len(w) >= 3] + ([git_mail] if git_mail else [])
    # Styling can split a word, so look at the rendered text as well.
    plain = strip(text).lower()
    return [n for n in needles if n and (n.lower() in text.lower() or n.lower() in plain)]


# --------------------------------------------------------------------------
# One harness.
# --------------------------------------------------------------------------

def installed(key, commands, npx_package):
    for command in commands:
        path = shutil.which(command)
        if path:
            return path
    if npx_package and shutil.which("npx"):
        cache = HOME / ".npm/_npx"
        if any((d / "node_modules" / npx_package).is_dir() for d in cache.glob("*")):
            return "npx -y " + npx_package
    return None


def blocker(text):
    for pattern, reason in BLOCKERS:
        match = pattern.search(text)
        if match:
            return f"{reason} ({match.group(0)!r})"
    return None


def frame_of(message):
    try:
        value = json.loads(message)
    except ValueError:
        return None
    return value if isinstance(value, dict) and "sequence" not in value else None


def visible(frame):
    """The pane's visible rows: the last `rows` lines of the capture."""
    lines = strip(frame.get("tailAnsi")).split("\n")
    if lines and lines[-1] == "":
        lines = lines[:-1]
    return "\n".join(lines[-(frame.get("rows") or 35):])


def probe(world, key, row, clean, fixtures, workspace):
    workspace = private_dir(workspace)
    before = snapshot_state(key)
    mirror_before = mirror_writes(world.home)
    extra_env = {}
    if key == "openclaw":
        extra_env = world.openclaw_gateway()
        for name, value in extra_env.items():
            world.tmux("set-environment", "-g", name, value)
    try:
        launched = world.launch(key, workspace)
    finally:
        for name in extra_env:
            world.tmux("set-environment", "-gu", name, check=False)
    session = launched["session"]
    row["command"] = clean(launched["command"])
    row["launched"] = "yes"
    start_command = world.pane(session, "#{pane_start_command}") or ""
    row["wrapped"] = "super-desktop-harness" in start_command
    log(f"{key}: {session} ← {row['command']} (metadata wrapper: {row['wrapped']})")
    stream = inputs = None
    frames_to_save = {}
    try:
        # Startup: wait until the screen is stable for 2 s (min 3 s, max 45 s).
        started = time.monotonic()
        last, stable_since = None, time.monotonic()
        while time.monotonic() - started < 45:
            if not world.alive(session):
                break
            screen = world.capture(session)
            if screen != last:
                last, stable_since = screen, time.monotonic()
            elif screen.strip() and time.monotonic() - stable_since >= 2 and time.monotonic() - started >= 3:
                break
            time.sleep(0.25)
        row["startup_s"] = round(time.monotonic() - started, 1)
        if not world.alive(session):
            row["result"] = "FAIL"
            row["notes"].append("exited during startup: " + clean((last or "").strip()[-300:]))
            return
        blocked = blocker(last or "")
        warning = WARNINGS.search(last or "")
        if warning:
            row["notes"].append(f"startup warning (non-modal): {warning.group(0)!r}")

        # (a) Android's terminal stream.
        stream = world.ws(f"/api/v1/harnesses/{session}/stream?ansiOnly=1")
        first = stream.wait(frame_of, 0, 10)
        assert first, "no first frame"
        frame = first[1]
        grid = world.pane(session, "#{pane_width} #{pane_height}").split()
        row["first_frame"] = (frame.get("id") == session and "tail" not in frame
                              and frame.get("tailFormat") == "ansi-sgr"
                              and bool(frame.get("tailAnsi"))
                              and [str(frame.get("columns")), str(frame.get("rows"))] == grid)
        row["grid"] = f"{frame.get('columns')}x{frame.get('rows')}"
        if not row["first_frame"]:
            row["notes"].append(f"first frame odd: id={frame.get('id')} keys={sorted(frame)} grid={grid}")
        frames_to_save["startup"] = frame
        inputs = Input(world.ws(f"/api/v1/harnesses/{session}/input"))
        if blocked:
            row["result"] = "BLOCKED"
            row["block"] = blocked
            row["notes"].append(blocked)
            log(f"{key}: BLOCKED — {blocked}")
            return

        marker = "sdpm" + secrets.token_hex(3)

        # (b) typed, NOT submitted.
        mark = len(stream.messages)
        sent_at = time.monotonic()
        ack = inputs.send(marker, enter=False)
        typed = stream.wait(lambda m: (f := frame_of(m)) and marker in visible(f) and f, mark, 8)
        row["echo"] = bool(ack and ack.get("ok") and typed)
        if typed:
            frames_to_save["typed"] = typed[1]
            row["echo_ms"] = int((stream.messages[typed[0]][0] - sent_at) * 1000)
        else:
            row["notes"].append("typed marker never appeared in a frame")

        # (f) list entry while a draft is in the composer.
        entry = world.harness_entry(session)
        check_entry(row, entry, key, [marker])

        # (c) clear with Backspace (the phone keyboard's delete), Ctrl-U as fallback.
        def cleared(timeout):
            m = len(stream.messages)
            return stream.wait(lambda msg: (f := frame_of(msg)) and marker not in visible(f) and f, m, timeout) \
                or (marker not in world.capture(session))
        inputs.send(BACKSPACE * len(marker))
        if cleared(4):
            row["clear"] = "Backspace"
        else:
            inputs.send(CTRL_U)
            row["clear"] = "Ctrl-U" if cleared(3) else "NO"
            if row["clear"] == "NO":
                inputs.send(BACKSPACE * 64)
                row["notes"].append("typed marker not cleared")

        # (d) navigation keys: acknowledged, harness survives.
        time.sleep(1)
        mode_before = mode_signature(world, session)
        failures = []
        for name, data in KEYS:
            if name in PERSISTED_KEYS.get(key, {}):
                note = f"{name} not sent: {PERSISTED_KEYS[key][name]}"
                if note not in row["notes"]:
                    row["notes"].append(note)
                continue
            ack = inputs.send(data)
            if not (ack and ack.get("ok")):
                failures.append(f"{name}: {ack}")
            time.sleep(0.35)
            if not world.alive(session):
                failures.append(f"{name}: harness exited")
                break
        time.sleep(0.5)
        # History recall (Up) or an autocomplete may have filled the composer:
        # wipe it before anything else.
        inputs.send(BACKSPACE * 64)
        time.sleep(0.4)
        restore_mode(world, row, key, session, inputs, mode_before)
        row["keys"] = "ok" if not failures and world.alive(session) else "FAIL"
        if failures:
            row["notes"].append("keys: " + "; ".join(failures))
        if marker in visible(json.loads(last_frame_message(stream))):
            row["notes"].append("marker visible after keys")

        if ATTACHMENTS:
            attachment_probe(world, row, key, session, inputs, launched["command"])

        # (g) Claude `!` bash mode. NOT guaranteed local: in some input modes
        # (e.g. "manual mode") Claude Code 2.1.281 sends the command output to
        # the model and replies. Opt-in only.
        if key == "claude" and CLAUDE_BANG:
            local_command(world, row, session, stream, inputs, frames_to_save, key)

        # (e) screen mode and scrollback.
        mode = world.pane(session, "#{alternate_on} #{history_size}").split()
        row["alt_screen"] = mode[0] == "1"
        row["history"] = int(mode[1])
        entry = world.harness_entry(session)
        check_entry(row, entry, key, [marker] + row.get("injected", []))
        final = stream.wait(frame_of, max(0, len(stream.messages) - 1), 3)
        if final:
            frames_to_save["final"] = final[1]
        # Every stream frame's native title must be free of typed/local text too.
        for _, message in stream.messages:
            f = frame_of(message)
            title = (f or {}).get("sessionTitle") or ""
            if any(text in title for text in [marker] + row.get("injected", [])):
                row["title_ok"] = False
                row["notes"].append(f"stream sessionTitle contains injected text: {title!r}")
                break
        row["result"] = "PASS" if (row["first_frame"] and row["echo"] and row["clear"] != "NO"
                                   and row["keys"] == "ok" and row["title_ok"]
                                   and row.get("attach", "ok").startswith("ok")) else "FAIL"
        if SUBMIT and row["result"] == "PASS":
            submit(world, row, key, session, stream, inputs, frames_to_save, [marker] + row.get("injected", []))
    finally:
        teardown(world, row, session, inputs, stream, key)
        row["state_changes"] = [clean(p) for p in state_changes(before, snapshot_state(key))]
        row["state_changes"] += [clean(p) + " (new in mirror HOME, not the user's)"
                                 for p in sorted(mirror_writes(world.home) - mirror_before)]
        created = sorted(str(p.relative_to(workspace)) for p in workspace.rglob("*"))[:12]
        if created:
            row["workspace_files"] = created
        row["_frames"] = frames_to_save


def attachment_probe(world, row, key, session, inputs, launch):
    """Fill the composer with an image, a file and a line of text the way the
    bridge's attachment prompt does, check what the harness shows, clear it."""
    out = world.root / f"attachments-{key}.json"
    env = dict(world.env, SD_ATTACHMENT_PROBE_SESSION=session, SD_ATTACHMENT_PROBE_AGENT=key,
               SD_ATTACHMENT_PROBE_OUT=str(out), SD_ATTACHMENT_PROBE_LAUNCH=launch, RUST_TEST_THREADS="1")
    result = subprocess.run([str(world.launch_exe), "--exact", "prompt_attachments::tests::native_attachment_probe",
                             "--ignored", "--nocapture", "--quiet"], env=env, capture_output=True,
                            encoding="utf-8", errors="replace", timeout=90)
    report = json.loads(out.read_text()) if out.exists() else {}
    screen = report.get("screen", "")
    native = report.get("delivery") == "Native"
    failures = []
    if report.get("prepared") != "ok":
        failures.append(f"prepare: {report.get('prepared') or result.stdout[-400:] + result.stderr[-400:]}")
    if not report.get("textShown"):
        failures.append("prompt text not in the composer")
    if native and report.get("imageMarkers", 0) < 1:
        failures.append("no [Image …] attachment")
    if native and "probe.png" in screen:
        failures.append("image path left as text")
    if not native and "probe.png" not in screen.replace("\n", "").replace(" ", ""):
        failures.append("image path not in the prompt")
    mini = key == "opencode" and "--mini" in launch.split()
    if (key in NATIVE_ATTACHMENTS and not mini) != native:
        failures.append(f"delivery {report.get('delivery')} for `{report.get('command')}`")
    row["attach"] = ("ok " if not failures else "FAIL ") + f"{report.get('delivery')} ({report.get('command')})"
    if failures:
        row["notes"].append("attachments: " + "; ".join(failures))
        lines = [line.rstrip() for line in screen.rstrip().splitlines() if line.strip()]
        log(f"{key}: attachment screen (last lines):\n  " + "\n  ".join(lines[-12:]))
    # Clear the draft: attachment chips, the paths and the text.
    for _ in range(3):
        inputs.send(BACKSPACE * 200)
        time.sleep(0.3)
    inputs.send(CTRL_U)
    time.sleep(0.5)
    if "attachment check - not submitted" in world.capture(session):
        row["notes"].append("attachment draft not fully cleared (left for teardown)")


def shell_attachment_end_to_end(world, rows):
    """The whole phone path, with no model: POST an attachment prompt for a
    shell card exactly as Android does, let bash run `wc -c` on the staged
    file, then check the private copy and that the request cannot replay."""
    row = {"harness": "shell (end to end)", "launched": "no", "result": "FAIL", "notes": []}
    rows.append(row)
    workspace = private_dir(world.root / "ws-shell-attachment")
    session = world.launch("shell", workspace)["session"]
    row["launched"] = "yes"
    try:
        # Whatever the user's prompt looks like: drawn, and then quiet.
        if not wait_for(lambda: world.capture(session).strip(), 15):
            row["notes"].append("no shell prompt")
            return
        time.sleep(1.5)
        content = b"hello from the phone\n"
        body = {"requestId": secrets.token_hex(16), "text": "wc -c",
                "attachments": [{"kind": "file", "name": "hello note.txt",
                                 "dataBase64": base64.b64encode(content).decode()}]}
        status, answer = world.https(f"/api/v1/harnesses/{session}/attachment-prompt", body, token=world.token)
        if (status, answer) != (200, {"status": "submitted"}):
            row["notes"].append(f"submit answered {status} {answer}")
            return
        uploads = world.home / ".local/state/super-desktop/uploads"
        # `-J`: the long path wraps in the pane; join it back into one line.
        joined = lambda: world.tmux("capture-pane", "-p", "-J", "-t", f"={session}:", check=False)
        ran = wait_for(lambda: re.search(rf"^{len(content)} \S*/hello_note\.txt\s*$", joined(), re.M), 10)
        stored = list(uploads.glob("*/hello_note.txt"))
        row["attach"] = "ok Shell (bash)" if ran and stored else "FAIL Shell (bash)"
        if not ran:
            row["notes"].append("`wc -c` on the staged file never printed its size")
            log("shell screen:\n  " + "\n  ".join(joined().rstrip().splitlines()[-6:]))
        if not stored or stored[0].read_bytes() != content:
            row["notes"].append("the file was not stored byte for byte")
        elif (stored[0].stat().st_mode & 0o777, stored[0].parent.stat().st_mode & 0o777,
              uploads.stat().st_mode & 0o777) != (0o600, 0o700, 0o700):
            row["notes"].append("stored file or folders are not private")
        status, answer = world.https(f"/api/v1/harnesses/{session}/attachment-prompt", body, token=world.token)
        if status != 409 or "already_attempted" not in str(answer):
            row["notes"].append(f"a replayed request answered {status} {answer}")
        body["requestId"] = secrets.token_hex(16)
        body["text"] = ""
        status, answer = world.https(f"/api/v1/harnesses/{session}/attachment-prompt", body, token=world.token)
        if (status, answer) != (409, {"error": "type_a_command_for_the_attachment"}):
            row["notes"].append(f"a shell attachment without a command answered {status} {answer}")
        row["result"] = "PASS" if row["attach"].startswith("ok") and not row["notes"] else "FAIL"
    finally:
        log(f"shell attachment end to end: {row['result']} {row['notes']}")
        world.tmux("kill-session", "-t", f"={session}", check=False)


def completion(world, session):
    status, body = world.https("/api/v1/completions", {"sessions": [session]}, token=world.token)
    return body["terminals"][0] if status == 200 and body.get("terminals") else {"httpStatus": status}


def submit(world, row, key, session, stream, inputs, frames_to_save, injected):
    """One real prompt through Android's input path (enter:true), then watch
    status, reply, scrollback, title and completion exactly as the phone does."""
    row["prompt"] = "no"
    before = completion(world, session)
    history_before = int(world.pane(session, "#{history_size}") or 0)
    start = len(stream.messages)
    ack = inputs.send(PROMPT, enter=True, timeout=15)
    if not (ack and ack.get("ok")):
        row["prompt"] = f"refused: {ack}"
        return
    row["prompt"] = "yes"
    stream_statuses, list_statuses = [], []
    escaped = False
    working_seen = False
    settled_since = None
    started = time.monotonic()
    next_list = 0
    latest = None
    # SD_MATRIX_FRAME_DUMP=DIR: keep every raw reply frame (unsanitized, local
    # only) to derive status-indicator fixtures; nothing is written otherwise.
    dump = os.environ.get("SD_MATRIX_FRAME_DUMP")
    dumped = 0
    while time.monotonic() - started < 150:
        for _, message in stream.messages[start:]:
            frame = frame_of(message)
            if frame and dump:
                dumped += 1
                path = pathlib.Path(dump) / f"{key}-{dumped:04d}-{frame.get('status')}.ansi"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(frame.get("tailAnsi") or "")
            if frame:
                latest = frame
                if not stream_statuses or stream_statuses[-1] != frame.get("status"):
                    stream_statuses.append(frame.get("status"))
        start = len(stream.messages)
        if time.monotonic() >= next_list:
            entry = world.harness_entry(session) or {}
            if not list_statuses or list_statuses[-1] != entry.get("status"):
                list_statuses.append(entry.get("status"))
            next_list = time.monotonic() + 1
        current = stream_statuses[-1] if stream_statuses else None
        working_seen |= "WORKING" in stream_statuses or "WORKING" in list_statuses
        if current == "WAITING" and not escaped:
            # Never approve anything: dismiss/deny with Esc and record it.
            inputs.send("\x1b")
            escaped = True
            row["notes"].append("permission/question prompt appeared; dismissed with Esc")
        text = strip((latest or {}).get("tailAnsi"))
        after = text[text.rfind(PROMPT) + len(PROMPT):] if PROMPT in text else ""
        replied = bool(REPLY_LINE.search(after))
        quiet = current in ("IDLE", "FINISHED", "ERROR", "UNKNOWN") and (working_seen or replied
                                                                          or time.monotonic() - started > 45)
        if quiet:
            settled_since = settled_since or time.monotonic()
            if time.monotonic() - settled_since >= (3 if working_seen or replied else 8):
                break
        else:
            settled_since = None
        if current == "EXITED":
            break
        time.sleep(0.2)
    row["submit_s"] = round(time.monotonic() - started, 1)
    row["transitions"] = "→".join(s or "?" for s in stream_statuses) or "none"
    row["list_transitions"] = "→".join(s or "?" for s in list_statuses)
    text = strip((latest or {}).get("tailAnsi"))
    after = text[text.rfind(PROMPT) + len(PROMPT):] if PROMPT in text else text
    row["reply"] = bool(REPLY_LINE.search(after))
    errors = [line.strip() for line in after.splitlines() if ERROR_LINE.search(line) and line.strip()]
    row["error"] = errors[0][:160] if errors and not row["reply"] else ""
    if latest:
        frames_to_save["reply"] = latest
    history_after = int(world.pane(session, "#{history_size}") or 0)
    row["scrollback"] = f"{history_before}->{history_after}"
    entry = world.harness_entry(session) or {}
    row["title_after"] = entry.get("sessionTitle")
    row["prompt_after"] = entry.get("lastPrompt")
    row["title_eq"] = entry.get("lastPrompt") == PROMPT
    for field in ("sessionTitle", "lastPrompt"):
        value = entry.get(field) or ""
        if any(text_ in value for text_ in injected):
            row["title_eq"] = False
            row["notes"].append(f"after submit {field} contains injected text: {value!r}")
    # Completion alerts (Android polls POST /api/v1/completions).
    fired = None
    deadline = time.monotonic() + (20 if key in COMPLETION_AGENTS else 3)
    while time.monotonic() < deadline:
        now = completion(world, session)
        if now.get("state") == "completed" and now.get("completionId") \
                and now.get("completionId") != before.get("completionId"):
            fired = now
            break
        time.sleep(1)
    if fired:
        row["completion"] = "fired"
    elif now.get("supported"):
        row["completion"] = f"supported, not fired (state {now.get('state')})"
    else:
        row["completion"] = "unsupported" + ("" if key not in COMPLETION_AGENTS else " (expected supported!)")
    if key in COMPLETION_AGENTS and row["reply"] and not fired:
        row["notes"].append(f"completion not reported: {now}")
        if os.environ.get("SD_MATRIX_DEBUG"):
            option = world.tmux("show-options", "-qv", "-t", f"={session}", "@super_desktop_metadata", check=False)
            panes = world.tmux("list-panes", "-a", "-F", "#{session_name} #{pane_pid}", check=False)
            meta = {}
            try:
                meta = json.loads(pathlib.Path(option).read_text())
            except (OSError, ValueError):
                pass
            row["notes"].append("DEBUG " + json.dumps({"option": option, "panes": panes, "meta": {
                k: meta.get(k) for k in ("status", "completion_id", "completion_supported", "pid", "agent",
                                         "launcher", "native_session", "claude_turn", "claude_turn_active")}}))


def mode_signature(world, session):
    """The screen's bottom lines without digits/spinners: shows the input mode."""
    lines = [line for line in world.capture(session).splitlines() if line.strip()]
    return [re.sub(r"[0-9\u2800-\u28ff◐◓◑◒⏲·]+", "", line).strip() for line in lines[-6:]]


def restore_mode(world, row, key, session, inputs, before):
    """Shift-Tab cycles have 2-6 states; press on until the original mode is back."""
    if "Shift-Tab" in PERSISTED_KEYS.get(key, {}):
        return
    time.sleep(1.5)  # transient hints (e.g. "Esc again to …") fade
    if mode_signature(world, session) == before:
        return
    previous = mode_signature(world, session)
    for presses in range(1, 8):
        inputs.send("\x1b[Z")
        time.sleep(0.8)
        current = mode_signature(world, session)
        if current == before:
            row["notes"].append(f"mode restored with {presses} extra Shift-Tab")
            return
        if current == previous:
            # Shift-Tab changes nothing here (e.g. Pi without a thinking
            # model); the difference is a message line, not a mode.
            row["notes"].append("screen changed by keys (message only; Shift-Tab has no mode effect)")
            return
        previous = current
    row["notes"].append("input mode after keys differs from before (not restored)")


def last_frame_message(stream):
    for _, message in reversed(stream.messages):
        if frame_of(message):
            return message
    return "{}"


def check_entry(row, entry, key, forbidden):
    if entry is None:
        row["title_ok"] = False
        row["notes"].append("session missing from GET /api/v1/harnesses")
        return
    row["status"] = entry.get("status")
    row["title"] = entry.get("sessionTitle")
    row["lastPrompt"] = entry.get("lastPrompt")
    ok = entry.get("agentType") == key and entry.get("status") in (
        "IDLE", "WORKING", "WAITING", "ERROR", "UNKNOWN", "FINISHED")
    if not ok:
        row["notes"].append(f"list entry agentType/status: {entry.get('agentType')}/{entry.get('status')}")
    for field in ("sessionTitle", "lastPrompt"):
        value = entry.get(field) or ""
        if any(f and f in value for f in forbidden):
            ok = False
            row["notes"].append(f"{field} contains injected/unsubmitted text: {value!r}")
        if any(ord(c) < 32 for c in value):
            ok = False
            row["notes"].append(f"{field} contains control characters")
    row["title_ok"] = row.get("title_ok", True) and ok


def local_command(world, row, session, stream, inputs, frames_to_save, key):
    """`!echo` in Claude Code runs a local shell command without a model turn."""
    local = "sdlocal" + secrets.token_hex(3)
    inputs.send("!")
    time.sleep(0.6)
    screen = world.capture(session)
    if not re.search(r"bash mode|^\s*!\s*$|! for bash", screen, re.M | re.I):
        inputs.send(BACKSPACE * 4)
        row["local"] = "skipped (bash mode not confirmed)"
        return
    command = f"echo {local}; seq 1 60"
    inputs.send(command)
    shown = wait_for(lambda: re.search(r"^\s*!\s*" + re.escape(command), world.capture(session), re.M), 4)
    if not shown:
        inputs.send(BACKSPACE * (len(command) + 4))
        row["local"] = "skipped (bash prompt not confirmed)"
        return
    mark = len(stream.messages)
    inputs.send("", enter=True)
    out = stream.wait(lambda m: (f := frame_of(m)) and re.search(r"^\s*\S*\s*60\s*$", strip(f.get("tailAnsi")), re.M)
                      and strip(f.get("tailAnsi")).count(local) >= 2 and f, mark, 15)
    row["injected"] = [local]
    if out:
        frames_to_save["local"] = out[1]
        history = int(world.pane(session, "#{history_size}"))
        in_history = local in "\n".join(strip(out[1]["tailAnsi"]).split("\n")[:-(out[1].get("rows") or 35)])
        row["local"] = f"ok (history {history}, marker in scrollback: {in_history})"
    else:
        row["local"] = "FAIL (output not seen)"
    time.sleep(1.5)


def teardown(world, row, session, inputs, stream, key):
    """Clear any draft, exit the harness politely, then remove only its session."""
    exited = False
    try:
        if inputs and world.alive(session):
            inputs.send(BACKSPACE * 64)
            for data in ("\x1b", "\x03", "\x03", "\x04"):
                try:
                    inputs.send(data)
                except OSError:
                    if not world.alive(session):  # the bridge closes the socket on exit
                        exited = True
                        break
                    raise
                time.sleep(0.5)
                if not world.alive(session):
                    exited = True
                    break
            if not exited:
                exited = bool(wait_for(lambda: not world.alive(session), 3))
    except Exception as error:  # teardown must still remove the session
        row["notes"].append(f"teardown: {error}")
    row["clean_exit"] = exited
    if exited and stream:
        # PROTOCOL.md: a final EXITED frame with null tails, then close 1000.
        gone = stream.wait(lambda m: (f := frame_of(m)) and f.get("status") == "EXITED" and f, 0, 5)
        wait_for(lambda: stream.closed, 3)
        row["exit_frame"] = bool(gone and gone[1].get("tailAnsi") is None
                                 and stream.closed and stream.closed[0] == 1000)
        if not row["exit_frame"]:
            row["notes"].append(f"no EXITED frame/close 1000 after exit: {stream.closed}")
    for socket_ in (inputs.ws if inputs else None, stream):
        if socket_:
            socket_.close()
    world.tmux("kill-session", "-t", f"={session}", check=False)


def save_fixtures(world, row, key, frames, clean, directory):
    saved = []
    for stale in list(directory.glob(f"{key}.ansi")) + list(directory.glob(f"{key}-*.ansi")):
        stale.unlink()
    if row["result"] == "BLOCKED" and re.search(r"login|api key", row.get("block", ""), re.I):
        row["fixtures"] = []
        return  # a sign-in screen may carry one-time login data
    for kind in ("final", "typed", "local", "reply"):
        frame = frames.get(kind) or (frames.get("startup") if kind == "final" else None)
        if not frame or not frame.get("tailAnsi"):
            continue
        text = clean(frame["tailAnsi"])
        problems = leaks(text, world.root)
        if problems:
            row["notes"].append(f"fixture {kind} NOT saved: still identifying ({len(problems)} hits)")
            continue
        name = key if kind == "final" else f"{key}-{kind}"
        (directory / f"{name}.ansi").write_text(text)
        saved.append((name, frame.get("columns") or 0, frame.get("rows") or 0))
    row["fixtures"] = [s[0] for s in saved]
    row["_manifest"] = saved


# --------------------------------------------------------------------------

def locate_binaries(args):
    if args.binary:
        return str(pathlib.Path(args.binary).resolve()), str(pathlib.Path(args.test_binary).resolve())
    log("building target/debug/super-desktop and its unit-test executable")
    subprocess.run(["cargo", "build", "--bins"], cwd=REPO, check=True)
    built = subprocess.run(["cargo", "test", "--bin", "super-desktop", "--no-run", "--message-format=json"],
                           cwd=REPO, check=True, capture_output=True, text=True)
    test_binary = None
    for line in built.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable") \
                and message["profile"]["test"] and message["target"]["name"] == "super-desktop":
            test_binary = message["executable"]
    assert test_binary, "unit-test executable not found"
    return str(REPO / "target/debug/super-desktop"), test_binary


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary", nargs="?")
    parser.add_argument("test_binary", nargs="?")
    parser.add_argument("--only", action="append", default=[],
                        help="harness key or command (repeatable), e.g. claude, cursor-agent")
    parser.add_argument("--no-fixtures", action="store_true", help="do not write Android fixtures")
    parser.add_argument("--claude-bang", action="store_true",
                        help="Claude only: run `!echo …; seq 1 60` to prove scrollback (may cause a model turn)")
    parser.add_argument("--submit", action="store_true",
                        help="also submit ONE tiny tool-free prompt per harness (real model request)")
    parser.add_argument("--attachments", action="store_true",
                        help="also check phone prompt attachments in each composer (never submitted)")
    parser.add_argument("--fixtures-dir", default=str(ANDROID_FIXTURES))
    args = parser.parse_args()
    if bool(args.binary) != bool(args.test_binary):
        parser.error("give both BINARY and TEST_BINARY, or neither")
    wanted = {name.lower() for name in args.only}
    selected = [h for h in HARNESSES if not wanted or h[0] in wanted or wanted & set(h[1])]
    if wanted and not selected:
        parser.error("unknown harness: " + ", ".join(sorted(wanted)))
    global SUBMIT, CLAUDE_BANG, ATTACHMENTS
    SUBMIT = args.submit
    CLAUDE_BANG = args.claude_bang
    ATTACHMENTS = args.attachments
    binary, test_binary = locate_binaries(args)
    fixtures = None
    if not args.no_fixtures:
        fixtures = pathlib.Path(args.fixtures_dir)
        fixtures.mkdir(parents=True, exist_ok=True)
    # Unix socket paths must stay short, so everything lives under /tmp.
    root = pathlib.Path(tempfile.mkdtemp(prefix="sdphone-", dir="/tmp"))
    os.chmod(root, 0o700)
    clean = sanitizer(root)
    rows = []
    world = None
    try:
        world = World(root, binary, test_binary)
        if ATTACHMENTS:
            shell_attachment_end_to_end(world, rows)
        for key, commands, npx in selected:
            row = {"harness": key, "launched": "no", "result": "SKIP", "notes": []}
            rows.append(row)
            if not installed(key, commands, npx):
                row["notes"].append("not installed")
                log(f"{key}: SKIP (not installed)")
                continue
            precheck = PRECHECKS.get(key)
            if precheck:
                result = subprocess.run(precheck[0], capture_output=True, text=True, timeout=30,
                                        env=world.env, cwd=root)
                if re.search(precheck[1], result.stdout + result.stderr, re.I):
                    row.update(result="BLOCKED")
                    row["notes"].append(precheck[2])
                    log(f"{key}: BLOCKED before launch — {precheck[2]}")
                    continue
            # Never accept a trust dialog. Scratch folders inside the two
            # repositories inherit trust the user already gave those folders.
            candidates = [root / f"ws-{key}", world.launch_dir / f"ws-{key}"]
            if ANDROID_BUILD.is_dir():
                candidates.append(world.android_scratch / f"ws-{key}")
            tried = []
            for attempt, workspace in enumerate(candidates):
                try:
                    probe(world, key, row, clean, fixtures, workspace)
                except Exception as error:
                    import traceback
                    traceback.print_exc()
                    row["result"] = "FAIL" if row["result"] in ("SKIP", "PASS") else row["result"]
                    row["notes"].append(f"{type(error).__name__}: {clean(str(error))[:300]}")
                trust = row["result"] == "BLOCKED" and "trust" in row.get("block", "")
                if not trust or attempt == len(candidates) - 1:
                    break
                tried.append(clean(str(workspace)))
                log(f"{key}: workspace untrusted; retrying in a scratch folder under a repository")
                row.pop("block", None)
                row.update(result="SKIP", notes=["trust dialog NOT accepted in: " + ", ".join(tried)
                                                 + f"; ran in {clean(str(candidates[attempt + 1]))}"])
            if fixtures is not None:
                save_fixtures(world, row, key, row.pop("_frames", {}), clean, fixtures)
            log(f"{key}: {row['result']} " + json.dumps({k: v for k, v in row.items()
                                                          if k not in ("harness", "_manifest", "_frames")}))
    finally:
        if world:
            world.close()
        shutil.rmtree(root, ignore_errors=True)
    if fixtures is not None:
        manifest = fixtures / "manifest.tsv"
        existing = {}
        if manifest.exists():
            for line in manifest.read_text().splitlines()[1:]:
                parts = line.split("\t")
                if len(parts) == 3:
                    existing[parts[0]] = line
        for row in rows:
            if "fixtures" in row:  # this run replaced this harness's fixtures
                existing = {name: line for name, line in existing.items()
                            if name != row["harness"] and not name.startswith(row["harness"] + "-")}
            for name, columns, rows_ in row.get("_manifest", []):
                existing[name] = f"{name}\t{columns}\t{rows_}"
        manifest.write_text("fixture\tcolumns\trows\n" + "".join(v + "\n" for _, v in sorted(existing.items())))
    print()
    header = ["harness", "result", "echo", "clear", "keys", "screen", "history", "status", "title_ok",
              "local", "fixtures", "notes"]
    if ATTACHMENTS:
        header[-1:-1] = ["attachments"]
    if SUBMIT:
        header[-1:-1] = ["prompt", "transitions", "reply", "scrollback", "title==prompt", "completion", "error"]
    print(" | ".join(header))
    for row in rows:
        screen = "" if "alt_screen" not in row else ("alternate" if row["alt_screen"] else "main")
        print(" | ".join(str(x) for x in [
            row["harness"], row["result"], row.get("echo", ""), row.get("clear", ""), row.get("keys", ""),
            screen, row.get("history", ""), row.get("status", ""), row.get("title_ok", ""),
            row.get("local", ""), ",".join(row.get("fixtures", []))]
            + ([row.get("attach", "")] if ATTACHMENTS else [])
            + ([row.get("prompt", ""), row.get("transitions", ""), row.get("reply", ""),
                row.get("scrollback", ""), row.get("title_eq", ""), row.get("completion", ""),
                row.get("error", "")] if SUBMIT else [])
            + ["; ".join(row["notes"])]))
    for row in rows:
        if row.get("state_changes") or row.get("workspace_files"):
            print(f"{row['harness']}: persistent files touched: {row.get('state_changes', [])[:12]} "
                  f"workspace: {row.get('workspace_files', [])}")
    log(f"matrix finished in {time.monotonic() - STARTED:.0f}s")
    failed = [r for r in rows if r["result"] == "FAIL"]
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    main()
