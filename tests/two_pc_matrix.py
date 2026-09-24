#!/usr/bin/env python3
"""Two-PC regression matrix on one machine.

    python3 tests/two_pc_matrix.py [--no-gui] [BINARY [TEST_BINARY]]

Without arguments the script builds `target/debug/super-desktop` and the unit
test executable itself (`cargo build`, `cargo test --no-run`), then runs every
scenario. `--no-gui` skips the GTK phase (no display needed).

Every "PC" is a fully isolated instance: its own bridge state (machine id,
TLS certificate, paired devices), its own HOME/XDG directories, its own owner
IPC socket, its own private tmux server (`TMUX_TMPDIR`, with `$TMUX` dropped so
nothing can reach the caller's server) and its own ephemeral bridge port. Each
viewer has its own peer registry and pairs through the real invitation /
approval flow. The host *daemon* is `HostOwner`, a stateful model of the owner
IPC contract (epoch, revisions, conflicts carrying the host's geometry, the
`desktop-watch` feed, real tmux sessions for created cards); TLS, credentials,
the bridge's routes, deduplication, the event hub, the attach transport, the
viewer's client, event worker and command feedback are the real binaries.

The user's own daemon, bridge (port 8759), default tmux server, credentials
and `~/.config/super-desktop` are never touched: every process started here is
recorded and stopped by its own PID, and every directory is a private temp dir.
"""
import argparse
import base64
import json
import os
import pathlib
import re
import select
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
STARTED = time.monotonic()
CHECKS = []


def log(message):
    print(f"[{time.monotonic() - STARTED:6.1f}s] {message}", flush=True)


def check(condition, message, detail=None):
    if not condition:
        raise AssertionError(f"{message}: {detail!r}" if detail is not None else message)


def passed(row, message):
    CHECKS.append((row, message))
    log(f"  ok  {row}: {message}")


def wait_for(condition, timeout, message, interval=0.05):
    deadline = time.monotonic() + timeout
    while True:
        value = condition()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise AssertionError(f"timed out after {timeout}s: {message}")
        time.sleep(interval)


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def private_dir(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path, 0o700)
    return path


def isolated_env(home, runtime):
    """A child environment that can only see this instance's own state."""
    env = {key: value for key, value in os.environ.items()
           if key not in ("TMUX", "TMUX_PANE", "SUPER_DESKTOP_BRIDGE_STATE_DIR",
                          "SUPER_DESKTOP_PEERS_STATE_DIR", "SUPER_DESKTOP_GTK_TEST_CHILD")}
    env.update({
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_STATE_HOME": str(home / ".local/state"),
        "XDG_DATA_HOME": str(home / ".local/share"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_RUNTIME_DIR": str(runtime),
    })
    return env


PROCESSES = []
LOOPBACK = iter(range(2, 250))


def next_loopback():
    return f"127.0.0.{next(LOOPBACK)}"


def track(process):
    PROCESSES.append(process)
    return process


def stop(process, timeout=10):
    """Stop one process we started, by its own PID, never by pattern."""
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


# --------------------------------------------------------------------------
# Host "daemon": the owner IPC contract, with state.
# --------------------------------------------------------------------------

def card_layout(x, y, width=640, height=400):
    return {"x": x, "y": y, "width": width, "height": height,
            "restoredWidth": width, "restoredHeight": height,
            "iconified": False, "iconX": 32, "iconY": 64, "tag": 1}


class HostOwner:
    """A host daemon's owner IPC: snapshots, typed commands, change feed.

    Mirrors `desktop_protocol::check_command` and the daemon's handler: the
    epoch lasts for this object's lifetime, revisions only grow inside it, a
    stale card revision is refused with the owner's current geometry, a stale
    workspace revision with `conflict`, and a create builds a real tmux session
    in the host's private server so its card can be attached.
    """

    def __init__(self, host, epoch, cards, workspace="/remote/project",
                 folders=("/remote/project", "/remote/other")):
        self.host = host
        self.epoch = epoch
        self.revision = 1
        self.workspace = workspace
        self.folders = list(folders)
        # Revisions restart with the daemon, as the real owner's counters do.
        self.cards = [dict(card, revision=1) for card in cards]
        self.lock = threading.RLock()
        self.log = []          # every desktop-command envelope received
        self.applied = []      # request ids applied
        self.reply_delay = 0.0
        self.on_apply = None   # called (outside the lock) after a mutation
        self.watchers = []
        self.change = 0
        self.stop_event = threading.Event()
        self.path = host.runtime / "super-desktop.sock"
        self.path.unlink(missing_ok=True)
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(self.path))
        self.listener.listen(64)
        self.listener.settimeout(0.1)
        self.threads = [threading.Thread(target=self.serve, daemon=True),
                        threading.Thread(target=self.idle_ticker, daemon=True)]
        for thread in self.threads:
            thread.start()

    # -- state ---------------------------------------------------------------
    def snapshot(self):
        with self.lock:
            return {
                "epoch": self.epoch, "revision": self.revision,
                "canvas": {"x": 0, "y": 0, "width": 1920, "height": 1080,
                           "scale": 1.0, "topInset": 56},
                "workspace": self.workspace, "folders": list(self.folders),
                "homeDirectory": "/remote",
                "visibleHarnesses": ["shell"],
                "harnessTypes": [{"id": "shell", "name": "Shell", "available": True}],
                "cards": [json.loads(json.dumps(card)) for card in self.cards],
            }

    def card(self, card_id):
        with self.lock:
            return next((dict(card) for card in self.cards if card["cardId"] == card_id), None)

    def bump(self, card=None):
        with self.lock:
            self.revision += 1
            if card is not None:
                card["revision"] += 1
            self.change += 1
            change = self.change
            watchers = list(self.watchers)
        for watcher in watchers:
            try:
                watcher.sendall(f"changed {change}\n".encode())
            except OSError:
                with self.lock:
                    if watcher in self.watchers:
                        self.watchers.remove(watcher)

    def host_move(self, card_id, x, y=None):
        """A move made on the host itself (its own mouse)."""
        with self.lock:
            card = next(card for card in self.cards if card["cardId"] == card_id)
            card["layout"]["x"] = x
            if y is not None:
                card["layout"]["y"] = y
        self.bump(card)
        return self.card(card_id)

    # -- the command handler ---------------------------------------------------
    def outcome(self, ok, card=None, error=None, card_id=None):
        return {"ok": ok, "epoch": self.epoch, "revision": self.revision,
                "cardId": card["cardId"] if card else card_id,
                "cardRevision": card["revision"] if card else None,
                "layout": json.loads(json.dumps(card["layout"])) if card else None,
                "expanded": card["expanded"] if card else None,
                "error": error}

    def apply(self, request):
        command = request["command"]
        kind = command["type"]
        with self.lock:
            if request.get("expectedEpoch") != self.epoch:
                return self.outcome(False, error="epoch_changed")
            if kind == "createTerminal":
                if command["agentType"] != "shell":
                    return self.outcome(False, error="unsupported_harness")
                if command["workspace"] not in self.folders:
                    return self.outcome(False, error="invalid_workspace")
                number = len(self.log)
                card = {
                    "cardId": f"{self.host.name}-new-{number}",
                    "sessionName": f"sd_term_{self.host.name}_new_{number}",
                    "agentType": "shell", "title": "Created", "status": "RUNNING",
                    "sessionAlive": True, "workspace": command["workspace"],
                    "revision": 1, "stackingOrder": len(self.cards), "expanded": False,
                    "terminalSize": {"columns": 100, "rows": 30},
                    "layout": card_layout(200 + 20 * number, 300),
                }
                self.host.new_session(card["sessionName"])
                self.cards.append(card)
                self.revision += 1
                result = self.outcome(True, card)
            elif kind == "setWorkspace":
                if command["expectedRevision"] != self.revision:
                    return self.outcome(False, error="conflict")
                if command["workspace"] not in self.folders:
                    return self.outcome(False, error="invalid_workspace")
                self.workspace = command["workspace"]
                self.revision += 1
                result = self.outcome(True)
            else:
                card = next((card for card in self.cards
                             if card["cardId"] == command.get("cardId")), None)
                if card is None:
                    return self.outcome(False, error="unknown_card")
                if card["expanded"] and kind == "setLayout":
                    return self.outcome(False, error="terminal_expanded")
                if command["expectedRevision"] != card["revision"]:
                    return self.outcome(False, card, error="conflict")
                if kind == "setLayout":
                    card["layout"] = command["layout"]
                    card["stackingOrder"] = max(c["stackingOrder"] for c in self.cards) + 1
                    card["revision"] += 1
                    self.revision += 1
                    result = self.outcome(True, card)
                elif kind == "setExpanded":
                    card["expanded"] = command["expanded"]
                    card["revision"] += 1
                    self.revision += 1
                    result = self.outcome(True, card)
                elif kind == "closeTerminal":
                    self.cards.remove(card)
                    self.host.kill_session(card["sessionName"])
                    self.revision += 1
                    result = self.outcome(True, card_id=card["cardId"])
                else:
                    return self.outcome(False, error="unsupported_command")
            self.applied.append(request["requestId"])
            self.change += 1
            change = self.change
            watchers = list(self.watchers)
        for watcher in watchers:
            try:
                watcher.sendall(f"changed {change}\n".encode())
            except OSError:
                pass
        if self.on_apply:
            self.on_apply(request)
        return result

    # -- IPC -------------------------------------------------------------------
    def serve(self):
        while not self.stop_event.is_set():
            try:
                client, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self.answer, args=(client,), daemon=True).start()

    def answer(self, client):
        keep = False
        try:
            client.settimeout(5)
            data = b""
            while not data.endswith(b"\n"):
                part = client.recv(65536)
                if not part:
                    break
                data += part
            line = data.decode().strip()
            if line == "desktop-watch":
                with self.lock:
                    self.watchers.append(client)
                    change = self.change
                client.sendall(f'{{"ok":true,"watch":{change}}}\n'.encode())
                keep = True
                return
            if line == "desktop-workspace":
                reply = {"ok": True, "workspace": self.snapshot()}
            elif line.startswith("desktop-command "):
                request = json.loads(line[len("desktop-command "):])
                with self.lock:
                    self.log.append(request)
                reply = self.apply(request)
                if self.reply_delay:
                    time.sleep(self.reply_delay)
            else:
                reply = {"ok": False, "error": "unsupported"}
            client.sendall(json.dumps(reply).encode())
        except OSError:
            pass
        finally:
            if not keep:
                client.close()

    def idle_ticker(self):
        """The daemon's `idle N` line after five quiet seconds."""
        while not self.stop_event.wait(5):
            with self.lock:
                watchers = list(self.watchers)
                change = self.change
            for watcher in watchers:
                try:
                    watcher.sendall(f"idle {change}\n".encode())
                except OSError:
                    with self.lock:
                        if watcher in self.watchers:
                            self.watchers.remove(watcher)

    def close(self):
        """The daemon exits: its socket and every change feed go away."""
        self.stop_event.set()
        with self.lock:
            for watcher in self.watchers:
                try:
                    watcher.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                watcher.close()
            self.watchers.clear()
        self.listener.close()
        for thread in self.threads:
            thread.join(timeout=6)
        self.path.unlink(missing_ok=True)


# --------------------------------------------------------------------------
# One isolated PC: bridge + owner + private tmux server.
# --------------------------------------------------------------------------

class Host:
    def __init__(self, binary, root, name, cards):
        self.binary = binary
        self.name = name
        base = private_dir(root / name)
        self.state = private_dir(base / "s")
        self.runtime = private_dir(base / "r")
        self.home = private_dir(base / "h")
        self.tmux_dir = private_dir(base / "t")
        self.env = isolated_env(self.home, self.runtime)
        self.env["SUPER_DESKTOP_BRIDGE_STATE_DIR"] = str(self.state)
        self.env["TMUX_TMPDIR"] = str(self.tmux_dir)
        # No desktop session: a pairing request's notify-send must not reach
        # the user's real notification daemon.
        for key in ("DBUS_SESSION_BUS_ADDRESS", "DISPLAY", "WAYLAND_DISPLAY"):
            self.env.pop(key, None)
        self.port = free_port()
        self.bridge = None
        self.owner = None
        self.devices = {}
        try:
            self.tmux("-f", "/dev/null", "new-session", "-d", "-s", f"sd_term_{name}_boot",
                      "-x", "100", "-y", "30", "bash", "--noprofile", "--norc")
            self.tmux("set-option", "-g", "status", "off")
            for card in cards:
                self.new_session(card["sessionName"])
            self.owner = HostOwner(self, f"{name}-epoch-1", cards)
            self.start_bridge()
            config = self.state / "config.json"
            wait_for(lambda: config.exists() and config.stat().st_size > 0, 10,
                     f"{name} bridge identity")
            self.machine_id = json.loads(config.read_text())["bridge_id"]
        except BaseException:
            # A half-started PC must not leave its tmux server or bridge behind.
            self.close()
            raise

    def tmux(self, *args, check_result=True):
        result = subprocess.run(["tmux", *args], env=self.env, capture_output=True,
                                text=True, timeout=15)
        check(result.returncode == 0 or not check_result, f"{self.name} tmux {args}",
              result.stderr)
        return result.stdout.strip()

    def new_session(self, session):
        self.tmux("new-session", "-d", "-s", session, "-x", "100", "-y", "30",
                  "bash", "--noprofile", "--norc")
        self.tmux("set-option", "-t", session, "detach-on-destroy", "on")

    def kill_session(self, session):
        self.tmux("kill-session", "-t", session, check_result=False)

    def clients(self):
        out = self.tmux("list-clients", "-F", "#{client_name}", check_result=False)
        return [line for line in out.splitlines() if line]

    def pane(self, session):
        return self.tmux("capture-pane", "-p", "-t", session, check_result=False)

    def start_bridge(self):
        (self.state / "control.sock").unlink(missing_ok=True)
        self.bridge = track(subprocess.Popen(
            [self.binary, "harness-bridge", str(self.port)], env=self.env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        wait_for(lambda: (self.state / "control.sock").exists() or self.bridge.poll() is not None,
                 10, f"{self.name} bridge control socket")
        check(self.bridge.poll() is None, f"{self.name} bridge exited at start")
        wait_for(self.accepting, 10, f"{self.name} bridge port")

    def accepting(self):
        try:
            with socket.create_connection(("127.0.0.1", self.port), timeout=0.5):
                return True
        except OSError:
            return False

    def stop_bridge(self):
        stop(self.bridge)
        self.bridge = None

    def restart_daemon(self, epoch):
        """The desktop daemon exits and starts again: same sessions, new epoch."""
        cards = self.owner.snapshot()["cards"]
        self.owner.close()
        self.owner = HostOwner(self, epoch, cards)
        return self.owner

    def admin(self, path, body=None):
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(5)
            client.connect(str(self.state / "control.sock"))
            payload = json.dumps(body or {}).encode()
            method = "POST" if body is not None else "GET"
            client.sendall(f"{method} /api/v1/pair/{path} HTTP/1.1\r\nHost: local\r\n"
                           f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
            response = b""
            while chunk := client.recv(65536):
                response += chunk
        check(response.startswith(b"HTTP/1.1 200"), f"{self.name} admin {path}", response[:120])
        return json.loads(response.split(b"\r\n\r\n", 1)[1])

    def identity_pem(self, directory):
        identity = json.loads((self.state / "tls-identity.json").read_text())
        cert = directory / f"{self.name}-cert.pem"
        key = directory / f"{self.name}-key.pem"
        cert.write_text(ssl.DER_cert_to_PEM_cert(bytes(identity["certificate"])))
        encoded = base64.encodebytes(bytes(identity["key"])).decode()
        key.write_text(f"-----BEGIN PRIVATE KEY-----\n{encoded}-----END PRIVATE KEY-----\n")
        os.chmod(key, 0o600)
        return cert, key

    def close(self):
        self.stop_bridge()
        if self.owner is not None:
            self.owner.close()
        # Only this PC's private server: TMUX_TMPDIR is ours and $TMUX is unset.
        self.tmux("kill-server", check_result=False)


# --------------------------------------------------------------------------
# A viewer PC: its own registry, driven through the real CLI.
# --------------------------------------------------------------------------

class Viewer:
    def __init__(self, binary, root, name):
        self.binary = binary
        self.name = name
        base = private_dir(root / name)
        self.home = private_dir(base / "h")
        self.runtime = private_dir(base / "r")
        self.peers = base / "p"
        self.env = isolated_env(self.home, self.runtime)
        self.env["SUPER_DESKTOP_BRIDGE_STATE_DIR"] = str(private_dir(base / "s"))
        self.env["SUPER_DESKTOP_PEERS_STATE_DIR"] = str(self.peers)

    def cli(self, *args, input=None, expected=0, timeout=30):
        result = subprocess.run([self.binary, *args], input=input, env=self.env,
                                capture_output=True, text=True, timeout=timeout)
        if expected is not None:
            check(result.returncode == expected, f"{self.name} {args[0]} exit",
                  (result.returncode, result.stderr[-400:]))
        return result

    def popen(self, *args, stdin=subprocess.DEVNULL):
        return track(subprocess.Popen([self.binary, *args], env=self.env, stdin=stdin,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      text=True))

    def pair(self, host, via=None):
        """Pair through a hop of its own source address, like a separate PC.

        The saved peer then talks to the host directly, unless `via` is the
        hop this viewer should keep using.
        """
        hop = via or Proxy(host.port)
        try:
            return self.pair_through(host, hop.port, keep=via is not None)
        finally:
            if via is None:
                hop.close()

    def pair_through(self, host, port, keep):
        known = {device["id"] for device in host.admin("devices")["devices"]}
        invitation = host.admin("invitation", {})
        adding = self.popen("peer-add", "--host", "127.0.0.1", "--port",
                            str(port), stdin=subprocess.PIPE)
        adding.stdin.write(json.dumps(invitation) + "\n")
        adding.stdin.close()
        adding.stdin = None
        try:
            pending = wait_for(lambda: host.admin("state")["requests"] or adding.poll() is not None,
                               15, f"{self.name} pairing request on {host.name}")
        except AssertionError:
            stop(adding)
            raise
        if adding.poll() is not None:
            raise AssertionError(f"{self.name} peer-add ended early: {adding.communicate()}")
        check(len(pending) == 1, "one pending request", pending)
        host.admin("approve", {"requestId": pending[0]["requestId"]})
        stdout, stderr = adding.communicate(timeout=20)
        check(adding.returncode == 0, f"{self.name} paired with {host.name}", stderr)
        machine = json.loads(stdout)["machineId"]
        check(machine == host.machine_id, "paired identity is the host's", machine)
        added = [device["id"] for device in host.admin("devices")["devices"]
                 if device["id"] not in known]
        check(len(added) == 1, "one new device on the host", added)
        host.devices[self.name] = added[0]
        if not keep:
            self.repoint(machine, host.port)
        return machine

    def token(self, machine):
        registry = json.loads((self.peers / "peers.json").read_text())
        return next(peer["token"] for peer in registry["peers"] if peer["machineId"] == machine)

    def repoint(self, machine, port):
        """Point a saved peer at another address (the pin stays)."""
        path = self.peers / "peers.json"
        registry = json.loads(path.read_text())
        for peer in registry["peers"]:
            if peer["machineId"] == machine:
                peer["endpoint"]["port"] = port
        path.write_text(json.dumps(registry))

    def workspace(self, host):
        return json.loads(self.cli("peer-workspace", host.machine_id).stdout)

    def command(self, host, command, epoch=None, request_id=None, expected=None):
        body = {"command": command}
        if epoch is not None:
            body["expectedEpoch"] = epoch
        if request_id is not None:
            body["requestId"] = request_id
        result = self.cli("peer-command", host.machine_id, input=json.dumps(body),
                          expected=expected)
        return json.loads(result.stdout)

    def command_popen(self, host, command, epoch, request_id=None):
        body = {"command": command, "expectedEpoch": epoch}
        if request_id:
            body["requestId"] = request_id
        process = self.popen("peer-command", host.machine_id, stdin=subprocess.PIPE)
        process.stdin.write(json.dumps(body))
        process.stdin.close()
        process.stdin = None
        return process


class Events:
    """A running `peer-events`: JSON events on stdout, state lines on stderr."""

    def __init__(self, viewer, host):
        self.process = viewer.popen("peer-events", host.machine_id)
        self.events = []
        self.states = []
        self.lock = threading.Lock()
        self.threads = [threading.Thread(target=self.read_out, daemon=True),
                        threading.Thread(target=self.read_err, daemon=True)]
        for thread in self.threads:
            thread.start()

    def read_out(self):
        for line in self.process.stdout:
            with self.lock:
                self.events.append((time.monotonic(), json.loads(line)))

    def read_err(self):
        for line in self.process.stderr:
            with self.lock:
                self.states.append((time.monotonic(), line.strip()))

    def mark(self):
        with self.lock:
            return len(self.events), len(self.states)

    def event_after(self, mark, predicate, timeout, message):
        def found():
            with self.lock:
                return next((event for _, event in self.events[mark[0]:] if predicate(event)), None)
        return wait_for(found, timeout, message)

    def state_after(self, mark, text, timeout, message):
        def found():
            with self.lock:
                return next((at for at, line in self.states[mark[1]:] if line == text), None)
        return wait_for(found, timeout, message)

    def close(self):
        stop(self.process)


# --------------------------------------------------------------------------
# Network between a viewer and a host.
# --------------------------------------------------------------------------

class Proxy:
    """A TCP hop we control: pass, refuse, reset, or lose/freeze a reply.

    TLS is end-to-end through it (the pin is the host's), exactly like a real
    network path. `arm("cut")` / `arm("freeze")` acts on the connection whose
    command the host owner applies next: the request arrives and is applied,
    and its reply is lost (cut: connection closed; freeze: nothing more ever
    arrives, like a cable pulled mid-command).
    """

    def __init__(self, upstream_port):
        self.upstream = upstream_port
        # A distinct loopback source per hop: a bridge admits one pairing
        # request per source address at a time, as it would per real PC.
        self.source = next_loopback()
        self.port = free_port()
        self.mode = "pass"
        self.attempts = []
        self.pairs = []
        self.lock = threading.Lock()
        self.armed = None
        self.stopped = threading.Event()
        self.listener = None
        self.listen()
        threading.Thread(target=self.accept_loop, daemon=True).start()

    def listen(self):
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", self.port))
        listener.listen(64)
        listener.settimeout(0.1)
        self.listener = listener

    def set_mode(self, mode):
        """pass | reset (accept, then close at once) | refuse (port closed)."""
        with self.lock:
            previous, self.mode = self.mode, mode
            if mode == "refuse" and self.listener is not None:
                self.listener.close()
                self.listener = None
            if mode != "refuse" and previous == "refuse":
                self.listen()

    def accept_loop(self):
        while not self.stopped.is_set():
            listener = self.listener
            if listener is None:
                time.sleep(0.05)
                continue
            try:
                client, _ = listener.accept()
            except (socket.timeout, OSError):
                continue
            with self.lock:
                self.attempts.append(time.monotonic())
                mode = self.mode
            if mode == "reset":
                client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, b"\x01\x00\x00\x00\x00\x00\x00\x00")
                client.close()
                continue
            try:
                upstream = socket.create_connection(("127.0.0.1", self.upstream), timeout=2,
                                                    source_address=(self.source, 0))
            except OSError:
                client.close()
                continue
            pair = {"client": client, "upstream": upstream, "state": "pass"}
            with self.lock:
                self.pairs.append(pair)
            threading.Thread(target=self.pump, args=(pair,), daemon=True).start()

    def pump(self, pair):
        client, upstream = pair["client"], pair["upstream"]
        try:
            while not self.stopped.is_set() and pair["state"] != "closed":
                readable, _, _ = select.select([client, upstream], [], [], 0.05)
                if pair["state"] == "frozen":
                    time.sleep(0.05)
                    continue
                for source in readable:
                    data = source.recv(65536)
                    if not data:
                        pair["state"] = "closed"
                        break
                    if source is upstream and pair["state"] == "cut":
                        continue  # the reply is lost on the way back
                    (upstream if source is client else client).sendall(data)
        except OSError:
            pass
        finally:
            for side in (client, upstream):
                try:
                    side.close()
                except OSError:
                    pass
            with self.lock:
                if pair in self.pairs:
                    self.pairs.remove(pair)

    def arm(self, action):
        self.armed = action

    def trigger(self, _request=None):
        """Called by the host owner right after it applied a command."""
        action, self.armed = self.armed, None
        if action is None:
            return
        with self.lock:
            pairs = list(self.pairs)
        for pair in pairs:
            if action == "cut":
                pair["state"] = "cut"
                try:
                    pair["client"].shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
            else:
                pair["state"] = "frozen"

    def freeze_all(self):
        with self.lock:
            for pair in self.pairs:
                pair["state"] = "frozen"

    def drop_all(self):
        with self.lock:
            pairs = list(self.pairs)
        for pair in pairs:
            pair["state"] = "closed"
            for side in (pair["client"], pair["upstream"]):
                try:
                    side.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def close(self):
        self.stopped.set()
        self.drop_all()
        if self.listener is not None:
            self.listener.close()


class LegacyHost:
    """A host that predates `workspace-events-v1`, in front of a real bridge.

    It terminates TLS with the host's own certificate (so the viewer's pin is
    satisfied), answers 404 for the event route, strips the capability from
    `/api/v1/desktop/capabilities` and forwards everything else unchanged.
    """

    def __init__(self, host, directory):
        cert, key = host.identity_pem(directory)
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(cert, key)
        self.upstream = host.port
        self.port = free_port()
        self.paths = []
        self.stopped = threading.Event()
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", self.port))
        self.listener.listen(64)
        self.listener.settimeout(0.1)
        threading.Thread(target=self.accept_loop, daemon=True).start()

    def accept_loop(self):
        while not self.stopped.is_set():
            try:
                client, _ = self.listener.accept()
            except (socket.timeout, OSError):
                continue
            threading.Thread(target=self.handle, args=(client,), daemon=True).start()

    @staticmethod
    def read_message(stream):
        data = b""
        while b"\r\n\r\n" not in data:
            part = stream.recv(65536)
            if not part:
                return None
            data += part
        head, body = data.split(b"\r\n\r\n", 1)
        length = re.search(rb"(?im)^content-length:\s*(\d+)", head)
        wanted = int(length.group(1)) if length else 0
        while len(body) < wanted:
            part = stream.recv(65536)
            if not part:
                break
            body += part
        return head, body

    def handle(self, raw):
        try:
            raw.settimeout(10)
            client = self.context.wrap_socket(raw, server_side=True)
            message = self.read_message(client)
            if message is None:
                return
            head, body = message
            path = head.split(b" ", 2)[1].decode()
            self.paths.append(path)
            if path.startswith("/api/v1/desktop/events"):
                payload = b'{"error":"not_found"}'
                client.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n"
                               b"Content-Length: %d\r\nConnection: close\r\n\r\n" % len(payload)
                               + payload)
                return
            upstream_context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            upstream_context.check_hostname = False
            upstream_context.verify_mode = ssl.CERT_NONE
            if re.search(rb"(?im)^upgrade:\s*websocket", head):
                # A console stream: the old host had these; relay it as is.
                return self.relay(client, upstream_context, head + b"\r\n\r\n" + body)
            lines = [line for line in head.split(b"\r\n")
                     if not line.lower().startswith(b"connection:")]
            request = b"\r\n".join(lines + [b"Connection: close"]) + b"\r\n\r\n" + body
            with socket.create_connection(("127.0.0.1", self.upstream), timeout=10) as tcp:
                with upstream_context.wrap_socket(tcp) as upstream:
                    upstream.sendall(request)
                    reply = self.read_message(upstream)
            if reply is None:
                return
            reply_head, reply_body = reply
            if path == "/api/v1/desktop/capabilities" and reply_head.startswith(b"HTTP/1.1 200"):
                document = json.loads(reply_body)
                document["capabilities"] = [c for c in document["capabilities"]
                                            if c != "workspace-events-v1"]
                reply_body = json.dumps(document).encode()
            reply_lines = [line for line in reply_head.split(b"\r\n")
                           if not line.lower().startswith((b"content-length:", b"connection:"))]
            client.sendall(b"\r\n".join(reply_lines + [b"Content-Length: %d" % len(reply_body),
                                                       b"Connection: close"])
                           + b"\r\n\r\n" + reply_body)
        except (OSError, ssl.SSLError, ValueError):
            pass
        finally:
            raw.close()

    def relay(self, client, context, request):
        tcp = socket.create_connection(("127.0.0.1", self.upstream), timeout=10)
        upstream = context.wrap_socket(tcp)
        upstream.sendall(request)
        done = threading.Event()

        def copy(source, target):
            try:
                source.settimeout(0.25)
                while not done.is_set() and not self.stopped.is_set():
                    try:
                        data = source.recv(65536)
                    except (socket.timeout, ssl.SSLWantReadError):
                        continue
                    if not data:
                        break
                    target.sendall(data)
            except (OSError, ssl.SSLError):
                pass
            finally:
                done.set()

        back = threading.Thread(target=copy, args=(upstream, client), daemon=True)
        back.start()
        copy(client, upstream)
        back.join(timeout=2)
        upstream.close()

    def close(self):
        self.stopped.set()
        self.listener.close()


def https_status(port, path, token, method="GET", body=None, headers=None):
    """A raw request with a given credential (the pin is not what is tested)."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    payload = json.dumps(body).encode() if body is not None else b""
    extra = "".join(f"{key}: {value}\r\n" for key, value in (headers or {}).items())
    with socket.create_connection(("127.0.0.1", port), timeout=5) as tcp:
        with context.wrap_socket(tcp) as stream:
            closing = "" if "Connection" in (headers or {}) else "Connection: close\r\n"
            head = (f"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n"
                    f"Authorization: Bearer {token}\r\n{extra}"
                    f"Content-Type: application/json\r\nContent-Length: {len(payload)}\r\n"
                    f"{closing}\r\n")
            stream.sendall(head.encode() + payload)
            response = b""
            stream.settimeout(5)
            try:
                while chunk := stream.recv(65536):
                    response += chunk
                    if b"\r\n\r\n" in response:
                        break
            except (OSError, ssl.SSLError):
                pass
    check(response.startswith(b"HTTP/1.1 "), f"an HTTP answer for {path}", response[:200])
    return int(response.split(b" ", 2)[1])


def layout_command(card, **changes):
    layout = dict(card["layout"], **changes)
    return {"type": "setLayout", "cardId": card["cardId"],
            "expectedRevision": card["revision"], "layout": layout}


def seed_cards(name, count):
    return [{
        "cardId": f"{name}-card-{index}", "sessionName": f"sd_term_{name}_{index}",
        "agentType": "shell", "title": f"{name.upper()} shell {index}", "status": "RUNNING",
        "sessionAlive": True, "workspace": "/remote/project", "revision": 1,
        "stackingOrder": index, "expanded": False,
        "terminalSize": {"columns": 100, "rows": 30},
        "layout": card_layout(100 + 300 * index, 150 + 100 * index),
    } for index in range(1, count + 1)]


# --------------------------------------------------------------------------
# Scenarios
# --------------------------------------------------------------------------

def scenario_pairing(env):
    log("pair: A and B for viewer 1 (A through a network hop), A for viewer 2")
    a, b, v1, v2 = env["a"], env["b"], env["v1"], env["v2"]
    env["proxy"] = Proxy(a.port)
    v1.pair(a, via=env["proxy"])
    v1.pair(b)
    v2.pair(a)
    listed = json.loads(v1.cli("peer-list").stdout)
    check(sorted(peer["machineId"] for peer in listed) == sorted([a.machine_id, b.machine_id]),
          "viewer 1 lists both PCs", listed)
    check(a.machine_id != b.machine_id, "two PCs have two identities")
    token = v1.token(a.machine_id)
    check(token not in json.dumps(listed), "peer-list never prints a credential")
    # Credentials are per host: A's credential means nothing to B, and a
    # viewer's registry is not a credential to that viewer itself.
    check(https_status(b.port, "/api/v1/desktop/capabilities", token) == 401,
          "a credential for A is refused by B")
    passed("Pair A→B and A→C", "both PCs selectable with verified, distinct identities; "
           "credentials do not cross hosts")


def scenario_switching(env):
    a, b, v1 = env["a"], env["b"], env["v1"]
    log("switching: each host serves only its own workspace, consoles and commands")
    wa, wb = v1.workspace(a), v1.workspace(b)
    check(wa["machineId"] == a.machine_id and wb["machineId"] == b.machine_id, "identities")
    ids_a = {card["cardId"] for card in wa["cards"]}
    ids_b = {card["cardId"] for card in wb["cards"]}
    check(ids_a and ids_b and not ids_a & ids_b, "no card of one host in the other", (ids_a, ids_b))
    # Host-side content, then A → B → A attaches with only that host's bytes.
    a.tmux("send-keys", "-t", "sd_term_a_1", "echo SD_ON_A", "Enter")
    b.tmux("send-keys", "-t", "sd_term_b_1", "echo SD_ON_B", "Enter")
    wait_for(lambda: "SD_ON_A" in a.pane("sd_term_a_1") and "SD_ON_B" in b.pane("sd_term_b_1"),
             5, "host panes printed their markers")
    for host, card, mine, other in ((a, "a-card-1", b"SD_ON_A", b"SD_ON_B"),
                                    (b, "b-card-1", b"SD_ON_B", b"SD_ON_A"),
                                    (a, "a-card-1", b"SD_ON_A", b"SD_ON_B")):
        result = subprocess.run([v1.binary, "peer-attach", host.machine_id, card, "--seconds", "2"],
                                env=v1.env, capture_output=True, timeout=30)
        check(result.returncode == 0 and mine in result.stdout and other not in result.stdout,
              f"attach {card} shows only its host", result.stderr[-300:])
    wait_for(lambda: not a.clients() and not b.clients(), 5,
             "every attach released its tmux client on both hosts")
    # A card id of B is unknown to A, for attach and for commands.
    refused = v1.cli("peer-attach", a.machine_id, "b-card-1", expected=1).stderr
    check("unknown_card" in refused, "B's card is not attachable on A", refused)
    card_b = b.owner.card("b-card-1")
    answer = v1.command(a, layout_command(card_b, x=5), epoch=wa["epoch"])
    check(answer["reply"]["result"] == {"type": "rejected", "error": "unknown_card"}
          and answer["notice"] == "Already closed on that PC", "B's card is unknown on A", answer)
    check(b.owner.card("b-card-1")["layout"]["x"] == card_b["layout"]["x"], "B untouched")
    # Typing reaches the host that was attached, never the other one.
    marker = f"SD_TYPED_{os.getpid()}"
    typed = subprocess.run([v1.binary, "peer-attach", b.machine_id, "b-card-2", "--seconds", "4"],
                           input=f"echo {marker}\n".encode(), env=v1.env, capture_output=True,
                           timeout=30)
    check(typed.returncode == 0 and marker.encode() in typed.stdout, "typing reached B", typed.stderr)
    check(marker not in a.pane("sd_term_a_1") + a.pane("sd_term_a_2"), "typing never reached A")
    passed("Rapid switching / no cross-host leakage",
           "A→B→A attaches carry only that host's bytes and release their tmux clients; "
           "cards, commands and keys stay on the host they name")


def scenario_concurrent(env):
    a, v1, v2 = env["a"], env["v1"], env["v2"]
    log("concurrent edits: host vs viewer, viewer vs viewer")
    events = Events(v2, a)
    try:
        first = events.event_after((0, 0), lambda e: e["type"] == "snapshot", 15, "initial snapshot")
        drawn = next(card for card in first["workspace"]["cards"] if card["cardId"] == "a-card-1")
        # 1. The host moves the card; a viewer drop based on the old revision
        #    is refused with the host's geometry.
        mark = events.mark()
        moved = a.owner.host_move("a-card-1", 1000, 500)
        pushed = events.event_after(mark, lambda e: e["type"] == "snapshot" and any(
            c["cardId"] == "a-card-1" and c["layout"]["x"] == 1000 for c in e["workspace"]["cards"]),
            5, "host move pushed to the subscribed viewer")
        answer = v1.command(a, layout_command(drawn, x=40, y=60), epoch=first["workspace"]["epoch"])
        result = answer["reply"]["result"]
        check(result["type"] == "conflict" and result["layout"]["x"] == 1000
              and result["cardRevision"] == moved["revision"], "stale drop refused with host geometry",
              answer)
        check(answer["notice"] == "Changed on that PC · showing its layout"
              and answer["geometry"] == "snapToHost" and answer["refresh"] is True, "conflict feedback",
              answer)
        check(a.owner.card("a-card-1")["layout"]["x"] == 1000, "host edit not overwritten")
        passed("Drag/resize from either machine",
               f"host move pushed as event #{pushed['sequence']}; stale viewer drop → conflict with "
               "host geometry, “Changed on that PC · showing its layout”")

        # 2. Two viewers race on the same revision: exactly one wins, the
        #    other is told the winner's geometry. Repeated to shake the timing.
        epoch = a.owner.epoch
        for round_number in range(3):
            card = a.owner.card("a-card-2")
            one = v1.command_popen(a, layout_command(card, x=300 + round_number), epoch)
            two = v2.command_popen(a, layout_command(card, x=700 + round_number), epoch)
            answers = [json.loads(process.communicate(timeout=30)[0]) for process in (one, two)]
            kinds = sorted(answer["reply"]["result"]["type"] for answer in answers)
            check(kinds == ["applied", "conflict"], f"race {round_number}: one applied, one conflict",
                  answers)
            winner = next(a_ for a_ in answers if a_["reply"]["result"]["type"] == "applied")
            loser = next(a_ for a_ in answers if a_["reply"]["result"]["type"] == "conflict")
            check(loser["reply"]["result"]["layout"] == winner["reply"]["result"]["layout"]
                  == a.owner.card("a-card-2")["layout"], "loser sees the winner's geometry", answers)
        passed("Two viewers racing", "3/3 same-revision races: one applied, the other a conflict "
               "carrying the winner's geometry")

        # 3. Close vs move on the same revision: whichever lands first, the
        #    other is told so, and nothing is half-applied.
        created = v1.command(a, {"type": "createTerminal", "agentType": "shell",
                                 "workspace": "/remote/project"}, epoch=epoch, request_id="create-1")
        new_id = created["reply"]["result"]["cardId"]
        again = v1.command(a, {"type": "createTerminal", "agentType": "shell",
                               "workspace": "/remote/project"}, epoch=epoch, request_id="create-1")
        check(again["reply"] == created["reply"], "a repeated create id replays the answer", again)
        check(sum(1 for card in a.owner.snapshot()["cards"] if card["cardId"].startswith("a-new")) == 1,
              "a deduplicated create made exactly one card")
        card = a.owner.card(new_id)
        closer = v1.command_popen(a, {"type": "closeTerminal", "cardId": new_id,
                                      "expectedRevision": card["revision"]}, epoch)
        mover = v2.command_popen(a, layout_command(card, x=900), epoch)
        close_answer = json.loads(closer.communicate(timeout=30)[0])
        move_answer = json.loads(mover.communicate(timeout=30)[0])
        close_kind = close_answer["reply"]["result"]["type"]
        move_result = move_answer["reply"]["result"]
        if close_kind == "applied":
            check(move_result == {"type": "rejected", "error": "unknown_card"}
                  and move_answer["notice"] == "Already closed on that PC", "move after close", move_answer)
            check(a.owner.card(new_id) is None
                  and card["sessionName"] not in a.tmux("list-sessions", "-F", "#{session_name}").split(),
                  "closed card and its session gone")
        else:
            check(move_result["type"] == "applied" and close_kind == "conflict"
                  and close_answer["notice"] == "Changed on that PC · not closed", "close after move",
                  close_answer)
        # 4. Folder picks from two viewers against one workspace revision.
        revision = a.owner.snapshot()["revision"]
        one = v1.command_popen(a, {"type": "setWorkspace", "workspace": "/remote/other",
                                   "expectedRevision": revision}, epoch)
        two = v2.command_popen(a, {"type": "setWorkspace", "workspace": "/remote/project",
                                   "expectedRevision": revision}, epoch)
        answers = [json.loads(process.communicate(timeout=30)[0]) for process in (one, two)]
        results = sorted(answer["reply"]["result"].get("error") or answer["reply"]["result"]["type"]
                         for answer in answers)
        check(results == ["applied", "conflict"], "one folder pick wins", answers)
        loser = next(answer for answer in answers if answer["reply"]["result"]["type"] != "applied")
        check(loser["notice"] == "Folder changed on that PC · showing its folder", "folder notice", loser)
        passed("Concurrent move/close, create dedup, folder race",
               f"close vs move settled as {close_kind}/{move_result['type']}; a repeated create id "
               "made one card; one of two folder picks applied, the other told why")
    finally:
        events.close()


def scenario_bridge_restart(env):
    a, v1, v2 = env["a"], env["v1"], env["v2"]
    log("bridge restart on A: streams end, nothing is replayed, everything reconnects")
    events = Events(v2, a)
    attach = v2.popen("peer-attach", a.machine_id, "a-card-1")
    try:
        events.event_after((0, 0), lambda e: e["type"] == "snapshot", 15, "initial snapshot")
        wait_for(lambda: len(a.clients()) == 1, 10, "attach holds one tmux client")
        before = len(a.owner.log)
        mark = events.mark()
        down_at = time.monotonic()
        a.stop_bridge()
        noticed = events.state_after(mark, "reconnecting", 5, "events notice the bridge went away")
        attach.wait(timeout=10)
        attach_reason = attach.stderr.read()
        check(attach.returncode != 0 and "connection_failed" in attach_reason,
              "attach ends with a transport failure", attach_reason)
        wait_for(lambda: not a.clients(), 5, "host reaped the attach client")
        # While the bridge is gone a command cannot reach the host at all.
        card = a.owner.card("a-card-1")
        answer = v2.command(a, layout_command(card, x=222), epoch=a.owner.epoch, expected=1)
        check(answer["error"] == "connection_failed_or_pin_mismatch"
              and answer["notice"] == "Cannot reach that PC · change not applied"
              and answer["refresh"] is False, "cannot reach while down", answer)
        a.start_bridge()
        up_at = time.monotonic()
        snapshot = events.event_after(mark, lambda e: e["type"] == "snapshot" and e["sequence"] == 1,
                                      40, "events resubscribe after the bridge returns")
        resubscribed = time.monotonic()
        check(snapshot["workspace"]["epoch"] == a.owner.epoch, "same daemon epoch after bridge restart")
        check(len(a.owner.log) == before, "no command replayed by the restart", a.owner.log[before:])
        # Same identity, pin and credential: nothing to re-pair.
        check(v2.workspace(a)["machineId"] == a.machine_id, "identity survives the restart")
        result = subprocess.run([v2.binary, "peer-attach", a.machine_id, "a-card-1", "--seconds", "2"],
                                env=v2.env, capture_output=True, timeout=30)
        check(result.returncode == 0 and b"attached 100x30" in result.stderr, "attach again",
              result.stderr)
        # A request id sent before the restart is not remembered by the new
        # bridge; the owner's revision check still refuses its replay.
        stale = v2.command(a, layout_command(card, x=333), epoch=a.owner.epoch, request_id="pre-restart")
        check(stale["reply"]["result"]["type"] == "applied", "fresh command applied", stale)
        a.stop_bridge()
        a.start_bridge()
        replay = v2.command(a, layout_command(card, x=333), epoch=a.owner.epoch,
                            request_id="pre-restart")
        check(replay["reply"]["result"]["type"] == "conflict", "replayed layout refused by revision",
              replay)
        passed("B asleep, bridge restart",
               f"events noticed the bridge going away in {noticed - down_at:.2f}s and resubscribed "
               f"{resubscribed - up_at:.2f}s after it returned; attach ended and re-attached; "
               "“Cannot reach” while down; no command replayed; identity and credential kept")
    finally:
        events.close()
        stop(attach)


def scenario_daemon_restart(env):
    a, v2 = env["a"], env["v2"]
    log("daemon restart on A: unavailable, then a new epoch; old-epoch commands refused")
    events = Events(v2, a)
    try:
        first = events.event_after((0, 0), lambda e: e["type"] == "snapshot", 15, "initial snapshot")
        old_epoch = first["workspace"]["epoch"]
        card = a.owner.card("a-card-1")
        mark = events.mark()
        cards = a.owner.snapshot()["cards"]
        a.owner.close()
        events.event_after(mark, lambda e: e["type"] == "unavailable", 10,
                           "event stream reports the daemon unavailable")
        down = v2.command(a, layout_command(card, x=444), epoch=old_epoch, expected=1)
        check(down["error"] == "desktop_unavailable"
              and down["notice"] == "That PC's desktop is not running · change not applied",
              "daemon down", down)
        unavailable = v2.cli("peer-workspace", a.machine_id, expected=1).stderr
        check("remote_desktop_unavailable" in unavailable, "snapshot says unavailable, not empty",
              unavailable)
        a.owner = HostOwner(a, "a-epoch-2", cards)
        restarted = events.event_after(mark, lambda e: e["type"] == "snapshot"
                                       and e["workspace"]["epoch"] == "a-epoch-2", 15,
                                       "event stream carries the new epoch")
        check(events.process.poll() is None, "the same subscription survived the daemon restart")
        check(a.owner.log == [], "the new daemon received no replayed command", a.owner.log)
        stale = v2.command(a, layout_command(card, x=555), epoch=old_epoch)
        check(stale["reply"]["result"] == {"type": "rejected", "error": "epoch_changed"}
              and stale["notice"] == "That PC restarted · change not applied", "old epoch refused", stale)
        check(a.owner.card("a-card-1")["layout"]["x"] != 555, "nothing applied from the old epoch")
        # Sessions survived the daemon: a console attaches again.
        result = subprocess.run([v2.binary, "peer-attach", a.machine_id, "a-card-1", "--seconds", "2"],
                                env=v2.env, capture_output=True, timeout=30)
        check(result.returncode == 0 and b"attached" in result.stderr, "attach after daemon restart",
              result.stderr)
        passed("Daemon restart", f"stream said unavailable, then pushed epoch a-epoch-2 as event "
               f"#{restarted['sequence']} on the same subscription; commands from the old epoch "
               "“That PC restarted”; nothing replayed; sessions survived")
    finally:
        events.close()


def scenario_network_drop(env):
    a, v1, proxy = env["a"], env["v1"], env["proxy"]
    log("network drop between viewer 1 and A")
    epoch = a.owner.epoch
    # 1. Port closed: the request never leaves, so it is "not applied".
    proxy.set_mode("refuse")
    before = len(a.owner.log)
    card = a.owner.card("a-card-2")
    answer = v1.command(a, layout_command(card, x=11), epoch=epoch, expected=1)
    check(answer["notice"] == "Cannot reach that PC · change not applied" and answer["refresh"] is False
          and answer["geometry"] == "revert", "refused port", answer)
    proxy.set_mode("reset")
    answer = v1.command(a, layout_command(card, x=12), epoch=epoch, expected=1)
    check(answer["notice"] == "Cannot reach that PC · change not applied", "reset during TLS", answer)
    check(len(a.owner.log) == before, "an unreachable host received nothing")
    proxy.set_mode("pass")
    passed("Unplug B's network mid-command (not sent)",
           "refused or reset before TLS completes → “Cannot reach that PC · change not applied”, "
           "no refresh, host untouched")

    # 2. The request arrives and is applied, the reply is lost: unknown.
    a.owner.on_apply = proxy.trigger
    # The owner answers a moment after applying, so the hop can lose the
    # reply first — the cable pulled between "applied" and "answered".
    a.owner.reply_delay = 1.0
    try:
        card = a.owner.card("a-card-2")
        proxy.arm("cut")
        answer = v1.command(a, layout_command(card, x=21), epoch=epoch, request_id="lost-reply",
                            expected=1)
        check(answer["error"] == "command_outcome_unknown"
              and answer["notice"] == "Result unknown · check before retrying"
              and answer["refresh"] is True and answer["geometry"] == "revert", "cut reply", answer)
        check(a.owner.applied.count("lost-reply") == 1
              and [r["requestId"] for r in a.owner.log].count("lost-reply") == 1,
              "applied once, never resent by the viewer", a.owner.log[-2:])
        # A deliberate retry of the same id while the host is still answering
        # the first is refused as unknown, never applied a second time...
        early = v1.command(a, layout_command(card, x=21), epoch=epoch, request_id="lost-reply")
        check(early["reply"] is None and early["error"] == "unknown_outcome"
              and early["notice"] == "Result unknown · check before retrying", "retry in flight", early)
        # ...and once it has answered, the retry replays the host's record.
        time.sleep(a.owner.reply_delay + 0.3)
        again = v1.command(a, layout_command(card, x=21), epoch=epoch, request_id="lost-reply")
        check(again["reply"] and again["reply"]["result"]["type"] == "applied"
              and a.owner.applied.count("lost-reply") == 1, "dedup answers the retry", again)
        # 3. A cable pulled mid-command: the reply never comes, the client times out.
        card = a.owner.card("a-card-2")
        proxy.arm("freeze")
        started = time.monotonic()
        answer = v1.command(a, layout_command(card, x=31), epoch=epoch, request_id="frozen-reply",
                            expected=1)
        waited = time.monotonic() - started
        check(answer["error"] == "command_outcome_unknown"
              and answer["notice"] == "Result unknown · check before retrying", "frozen reply", answer)
        check(a.owner.applied.count("frozen-reply") == 1
              and [r["requestId"] for r in a.owner.log].count("frozen-reply") == 1,
              "applied once, not resent after the timeout")
    finally:
        a.owner.on_apply = None
        a.owner.reply_delay = 0.0
        proxy.drop_all()
    passed("Unplug B's network mid-command (sent, no answer)",
           f"reply cut → “Result unknown · check before retrying”, applied exactly once; frozen link "
           f"timed out after {waited:.1f}s with the same notice; a manual retry of the id replays "
           "the host's answer")

    # 4. The event subscription through the same hop: drop, backoff, recovery.
    events = Events(v1, a)
    try:
        events.event_after((0, 0), lambda e: e["type"] == "snapshot", 15, "initial snapshot")
        mark = events.mark()
        with proxy.lock:
            proxy.attempts.clear()
        proxy.set_mode("reset")
        proxy.drop_all()
        dropped = time.monotonic()
        events.state_after(mark, "reconnecting", 5, "subscription notices the drop")
        time.sleep(7.5)
        with proxy.lock:
            attempts = list(proxy.attempts)
        gaps = [later - earlier for earlier, later in zip(attempts, attempts[1:])]
        # Each attempt is a capability check (one connection); jittered
        # doubling from one second, never a tight loop.
        check(2 <= len(attempts) <= 5, "bounded reconnect attempts while down", attempts)
        check(all(gap >= 0.75 for gap in gaps) and gaps == sorted(gaps), "backoff grows", gaps)
        a.owner.host_move("a-card-1", 640, 320)
        proxy.set_mode("pass")
        restored = time.monotonic()
        back = events.event_after(mark, lambda e: e["type"] == "snapshot" and any(
            c["cardId"] == "a-card-1" and c["layout"]["x"] == 640 for c in e["workspace"]["cards"]),
            20, "subscription recovers and shows the change made while down")
        recovered = time.monotonic() - restored
        check(back["sequence"] == 1, "a fresh subscription starts at sequence 1", back)
        # 5. A silent drop (no RST, no FIN): only the heartbeat rule notices.
        mark = events.mark()
        proxy.freeze_all()
        frozen = time.monotonic()
        events.state_after(mark, "reconnecting", 25, "silence is detected by missing heartbeats")
        silent = time.monotonic() - frozen
        events.event_after(mark, lambda e: e["type"] == "snapshot" and e["sequence"] == 1, 15,
                           "resubscribed after a silent drop")
        # Fifteen seconds after the last message heard, which was a heartbeat
        # at most five seconds before the link froze.
        check(9.5 <= silent <= 17, "three heartbeat intervals of silence", silent)
    finally:
        events.close()
        proxy.drop_all()
        proxy.set_mode("pass")
    passed("Network drop: events backoff and recovery",
           f"{len(attempts)} attempts in 7.5s with gaps {[round(g, 1) for g in gaps]}; recovered "
           f"{recovered:.1f}s after the link returned with the change made meanwhile; a silent "
           f"drop was detected after {silent:.1f}s (three missed heartbeats)")


def scenario_revocation(env):
    a, v1, v2 = env["a"], env["v1"], env["v2"]
    log("revocation of viewer 2 on A while viewer 1 keeps working")
    events = Events(v2, a)
    attach = v2.popen("peer-attach", a.machine_id, "a-card-1")
    try:
        events.event_after((0, 0), lambda e: e["type"] == "snapshot", 15, "initial snapshot")
        wait_for(lambda: len(a.clients()) == 1, 10, "attach holds one tmux client")
        token = v2.token(a.machine_id)
        mark = events.mark()
        revoked = time.monotonic()
        a.admin("revoke", {"deviceId": a.devices["v2"]})
        attach.wait(timeout=10)
        attach_ended = time.monotonic() - revoked
        reason = attach.stderr.read()
        check(attach.returncode != 0, "attach ended", reason)
        stream_ended = events.state_after(mark, "reconnecting", 5, "event stream torn down") - revoked
        events.process.wait(timeout=10)
        events_exited = time.monotonic() - revoked
        check(events.process.returncode != 0
              and any("peer_revoked_or_expired" in line for _, line in events.states),
              "subscription stops instead of retrying", events.states)
        check(attach_ended <= 1.5 and stream_ended <= 1.5,
              "streams end within about a second", (attach_ended, stream_ended))
        wait_for(lambda: not a.clients(), 5, "host reaped the revoked viewer's tmux client")
        for method, path, body in (("GET", "/api/v1/desktop/capabilities", None),
                                   ("GET", "/api/v1/desktop/workspace", None),
                                   ("POST", "/api/v1/desktop/commands", {"requestId": "x"}),
                                   ("GET", "/api/v1/desktop/terminals/a-card-1/attach", None),
                                   ("GET", "/api/v1/desktop/events", None)):
            headers = {}
            if path.endswith(("attach", "events")):
                headers = {"Upgrade": "websocket", "Connection": "Upgrade",
                           "Sec-WebSocket-Version": "13",
                           "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ=="}
            status = https_status(a.port, path, token, method, body, headers)
            check(status == 401, f"revoked credential refused on {path}", status)
        answer = v2.command(a, layout_command(a.owner.card("a-card-1"), x=5), epoch=a.owner.epoch,
                            expected=1)
        check(answer["error"] == "peer_revoked_or_expired"
              and answer["notice"] == "Pairing required · change not applied", "command refused", answer)
        # The other viewer and the host's sessions are untouched.
        check(v1.workspace(a)["machineId"] == a.machine_id, "viewer 1 still works")
        check("sd_term_a_1" in a.tmux("list-sessions", "-F", "#{session_name}").split(),
              "host session survives revocation")
    finally:
        events.close()
        stop(attach)
    passed("Revoke A on B", f"attach ended {attach_ended:.2f}s and events {stream_ended:.2f}s after "
           f"revocation (subscription exited after {events_exited:.1f}s, no retry loop); every desktop "
           "route answers 401; the other viewer and B's sessions are untouched")


def scenario_legacy(env):
    c, v1, root = env["c"], env["v1"], env["root"]
    log("old host without workspace-events-v1: the viewer keeps the poll")
    v1.pair(c)
    legacy = LegacyHost(c, root)
    env["legacy"] = legacy
    v1.repoint(c.machine_id, legacy.port)
    ended = v1.cli("peer-events", c.machine_id, expected=1, timeout=20).stderr
    check("update_remote_super_desktop" in ended, "an old host's events end at once", ended)
    check(not any(path.startswith("/api/v1/desktop/events") for path in legacy.paths),
          "the viewer never asks an old host for events")
    workspace = v1.workspace(c)
    check(workspace["machineId"] == c.machine_id, "snapshots still work through the old host")
    passed("Old host fallback (CLI)", "capability missing → the subscription ends with "
           "update_remote_super_desktop without touching /events; snapshots still served")


# --------------------------------------------------------------------------
# GTK phase: the real MachineView against the same isolated hosts.
# --------------------------------------------------------------------------

def gui_phase(env, test_binary):
    a, b, c, root = env["a"], env["b"], env["c"], env["root"]
    log("GUI: the real selector and remote canvas against A, B and an old host")
    viewer = Viewer(env["binary"], root, "v3")
    viewer.pair(a)
    viewer.pair(b)
    viewer.pair(c)
    viewer.repoint(c.machine_id, env["legacy"].port)
    control = private_dir(root / "gui-control")
    spec = {
        "control": str(control),
        "hosts": {
            name: {"machineId": host.machine_id, "port": port, "tmuxTmpdir": str(host.tmux_dir),
                   "cards": [card["cardId"] for card in host.owner.snapshot()["cards"]]}
            for name, host, port in (("a", a, a.port), ("b", b, b.port), ("c", c, env["legacy"].port))
        },
    }
    spec_path = root / "gui-spec.json"
    spec_path.write_text(json.dumps(spec))
    gui_env = dict(viewer.env)
    # The display lives in the caller's runtime dir; nothing else does.
    gui_env["XDG_RUNTIME_DIR"] = os.environ.get("XDG_RUNTIME_DIR", str(viewer.runtime))
    gui_env["SUPER_DESKTOP_TWO_PC_TEST_INPUT"] = str(spec_path)
    gui_env["SUPER_DESKTOP_GTK_TEST_CHILD"] = "1"
    gui_env["RUST_TEST_THREADS"] = "1"
    child = track(subprocess.Popen(
        [test_binary, "--exact", "machine_selector::tests::two_pc_inner", "--nocapture"],
        env=gui_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True))
    output = []
    threading.Thread(target=lambda: output.extend(child.stdout), daemon=True).start()
    handled = 0
    deadline = time.monotonic() + 240
    hosts = {"a": a, "b": b, "c": c}
    while child.poll() is None:
        check(time.monotonic() < deadline, "GUI phase finished in time", "".join(output[-40:]))
        request = control / f"request-{handled}.json"
        if not request.exists():
            time.sleep(0.02)
            continue
        action = json.loads(request.read_text())
        host = hosts[action["host"]]
        result = {}
        if action["do"] == "host_move":
            card = host.owner.host_move(action["card"], action["x"], action.get("y"))
            result = {"revision": card["revision"]}
        elif action["do"] == "detach_clients":
            for client in host.clients():
                host.tmux("detach-client", "-t", client, check_result=False)
        elif action["do"] == "restart_bridge":
            host.stop_bridge()
            time.sleep(action.get("down", 0.5))
            host.start_bridge()
        elif action["do"] == "restart_daemon":
            host.restart_daemon(action["epoch"])
        elif action["do"] == "commands":
            result = {"log": [r["requestId"] for r in host.owner.log], "epoch": host.owner.epoch}
        elif action["do"] == "revoke":
            host.admin("revoke", {"deviceId": host.devices["v3"]})
        (control / f"done-{handled}.json").write_text(json.dumps(result))
        handled += 1
    text = "".join(output)
    check(child.returncode == 0, "GUI two-PC checks passed", text[-6000:])
    for line in text.splitlines():
        # The test harness prints its own "test … " prefix without a newline.
        if "TWO-PC " in line:
            row, _, message = line.split("TWO-PC ", 1)[1].partition(": ")
            passed(row, message)


# --------------------------------------------------------------------------

def locate_binaries(args):
    if args.binary:
        binary = str(pathlib.Path(args.binary).resolve())
        test_binary = str(pathlib.Path(args.test_binary).resolve()) if args.test_binary else None
        return binary, test_binary
    log("building target/debug/super-desktop and its test executable")
    subprocess.run(["cargo", "build", "--bin", "super-desktop"], cwd=REPO, check=True)
    test_binary = None
    if not args.no_gui:
        built = subprocess.run(["cargo", "test", "--bin", "super-desktop", "--no-run",
                                "--message-format=json"], cwd=REPO, check=True,
                               capture_output=True, text=True)
        for line in built.stdout.splitlines():
            message = json.loads(line)
            if message.get("reason") == "compiler-artifact" and message.get("executable") \
                    and message["profile"]["test"] and message["target"]["name"] == "super-desktop":
                test_binary = message["executable"]
        check(test_binary, "found the unit test executable")
    return str(REPO / "target/debug/super-desktop"), test_binary


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary", nargs="?")
    parser.add_argument("test_binary", nargs="?")
    parser.add_argument("--no-gui", action="store_true", help="skip the GTK phase")
    args = parser.parse_args()
    binary, test_binary = locate_binaries(args)
    # Unix socket paths must stay short, so the instances live under /tmp.
    root = pathlib.Path(tempfile.mkdtemp(prefix="sd2pc-", dir="/tmp"))
    os.chmod(root, 0o700)
    env = {"root": root, "binary": binary}
    try:
        env["a"] = Host(binary, root, "a", seed_cards("a", 2))
        env["b"] = Host(binary, root, "b", seed_cards("b", 2))
        env["c"] = Host(binary, root, "c", seed_cards("c", 1))
        env["v1"] = Viewer(binary, root, "v1")
        env["v2"] = Viewer(binary, root, "v2")
        log(f"hosts A:{env['a'].port} B:{env['b'].port} C:{env['c'].port} in {root}")
        scenario_pairing(env)
        scenario_switching(env)
        scenario_concurrent(env)
        scenario_bridge_restart(env)
        scenario_daemon_restart(env)
        scenario_network_drop(env)
        scenario_legacy(env)
        scenario_revocation(env)
        if test_binary and not args.no_gui:
            gui_phase(env, test_binary)
        elif not args.no_gui:
            log("GUI phase skipped: no test executable given")
    finally:
        for process in reversed(PROCESSES):
            stop(process, timeout=5)
        for key in ("proxy", "legacy"):
            if key in env:
                env[key].close()
        for key in ("a", "b", "c"):
            if key in env:
                try:
                    env[key].close()
                except Exception as error:  # cleanup must reach every host
                    log(f"cleanup {key}: {error}")
        shutil.rmtree(root, ignore_errors=True)
    rows = sorted({row for row, _ in CHECKS})
    log(f"two-PC matrix passed: {len(CHECKS)} checks across {len(rows)} rows "
        f"in {time.monotonic() - STARTED:.0f}s")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    main()
