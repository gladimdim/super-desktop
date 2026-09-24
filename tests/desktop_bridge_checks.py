"""Desktop wire checks used by bridge_security_smoke; no production daemon."""
import copy
import json
import pathlib
import socket
import struct
import threading
import time


class DesktopStub:
    """Only models owner IPC. TLS, authentication and WSS use the real bridge."""
    def __init__(self, directory):
        self.path = pathlib.Path(directory) / "super-desktop.sock"
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.reply = {
            "ok": True,
            "workspace": {
                "epoch": "daemon-one", "revision": 1,
                "canvas": {"x": 0, "y": 0, "width": 1920, "height": 1080,
                           "scale": 1.0, "topInset": 56},
                "workspace": "/remote/project with spaces",
                "folders": ["/remote/project with spaces", "/remote/other"],
                "homeDirectory": "/remote",
                "visibleHarnesses": ["shell"],
                "harnessTypes": [{"id": "shell", "name": "Shell", "available": True}],
                "cards": [{
                    "cardId": "saved-card", "sessionName": "sd_term_saved",
                    "agentType": "shell", "title": "Remote shell", "status": "RUNNING",
                    "sessionAlive": True, "workspace": "/remote/project with spaces",
                    "revision": 1, "stackingOrder": 0, "expanded": False,
                    "terminalSize": {"columns": 120, "rows": 40},
                    "layout": {"x": 100, "y": 200, "width": 640, "height": 480,
                               "restoredWidth": 640, "restoredHeight": 480,
                               "iconified": False, "iconX": 32, "iconY": 64, "tag": 3}
                }],
                # Unknown internal data must not cross the typed DTO boundary.
                "privateToken": "DO_NOT_EXPORT", "machineId": "untrusted-local-id"
            }
        }
        self.commands = []
        # Owner answers for `desktop-command`; the snapshot above answers every
        # other route. `silent` models an owner that never answers at all, which
        # is the only way to reach the bridge's uncertain-outcome path quickly.
        self.command_reply = None
        self.silent = False
        # Open `desktop-watch` feeds, like the daemon's change notification.
        # `watch=False` models an older owner without the feed.
        self.watchers = []
        self.watch = True
        self.change = 0
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(self.path))
        self.listener.listen()
        self.listener.settimeout(.1)
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        while not self.stop.is_set():
            try:
                client, _ = self.listener.accept()
            except socket.timeout:
                continue
            with client:
                client.settimeout(2)
                command = b""
                while b"\n" not in command:
                    part = client.recv(1024)
                    if not part:
                        break
                    command += part
                if command.strip() == b"desktop-watch" and self.watch:
                    # The feed holds its socket; the stub keeps serving others.
                    with self.lock:
                        self.watchers.append(client.dup())
                        watcher = self.watchers[-1]
                        change = self.change
                    try:
                        watcher.sendall(f'{{"ok":true,"watch":{change}}}\n'.encode())
                    except OSError:
                        pass
                    continue
                with self.lock:
                    self.commands.append(command.decode().strip())
                    if self.silent and command.startswith(b"desktop-command "):
                        continue
                    reply = self.reply
                    if command.startswith(b"desktop-command ") and self.command_reply:
                        reply = self.command_reply
                    response = json.dumps(reply).encode()
                try:
                    client.sendall(response)
                except BrokenPipeError:
                    pass

    def replace(self, reply):
        with self.lock:
            self.reply = copy.deepcopy(reply)
            self.change += 1
            for watcher in list(self.watchers):
                try:
                    watcher.sendall(f"changed {self.change}\n".encode())
                except OSError:
                    self.watchers.remove(watcher)

    def answer_commands_with(self, reply=None, silent=False):
        with self.lock:
            self.command_reply = copy.deepcopy(reply)
            self.silent = silent

    def close(self):
        with self.lock:
            for watcher in self.watchers:
                watcher.close()
            self.watchers.clear()
        self.stop.set()
        self.thread.join(timeout=3)
        assert not self.thread.is_alive(), "owner IPC test worker did not stop"
        self.listener.close()
        self.path.unlink(missing_ok=True)


def receive_exact(stream, length):
    data = b""
    while len(data) < length:
        part = stream.recv(length - len(data))
        assert part, "unexpected WSS EOF"
        data += part
    return data


def receive_event(stream):
    first, second = receive_exact(stream, 2)
    assert first == 0x81 and second & 0x80 == 0, "expected unmasked server text"
    length = second & 0x7f
    if length == 126:
        length = struct.unpack("!H", receive_exact(stream, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", receive_exact(stream, 8))[0]
    assert length <= 1024 * 1024
    return json.loads(receive_exact(stream, length))


def command_outcome(ok, **fields):
    """One typed owner answer, shaped exactly like CommandOutcome."""
    return dict({"ok": ok, "epoch": "daemon-one", "revision": 2,
                 "cardId": None, "cardRevision": None, "layout": None,
                 "expanded": None, "error": None}, **fields)


def set_layout_command(layout, request_id="r1", machine_id=None, epoch="daemon-one",
                       card_id="saved-card", revision=1):
    return {
        "requestId": request_id,
        "machineId": machine_id if machine_id is not None else "placeholder",
        "expectedEpoch": epoch,
        "command": {"type": "setLayout", "cardId": card_id,
                    "expectedRevision": revision, "layout": layout},
    }


def check_desktop_commands(request, token, machine_id, directory):
    """The mutation route: envelope validation, dedup and typed refusals."""
    stub = DesktopStub(directory)
    layout = copy.deepcopy(stub.reply["workspace"]["cards"][0]["layout"])
    try:
        # Nothing reaches the owner before the envelope is valid, so a hostile
        # or malformed command can never move a card.
        assert request("/api/v1/desktop/commands", set_layout_command(layout))[0] == 401
        for bad in [
            {"type": "setLayout", "cardId": "saved-card", "expectedRevision": 1},
            set_layout_command(copy.deepcopy(layout), machine_id="other-machine"),
            set_layout_command(copy.deepcopy(layout), request_id="with space"),
            set_layout_command(copy.deepcopy(layout), revision=0),
            set_layout_command({**layout, "width": 0}),
            # Inside i32, outside the accepted bounds.
            set_layout_command({**layout, "y": 2 ** 30}),
            set_layout_command({**layout, "tag": 99}),
            set_layout_command({**layout, "shellCommand": "rm -rf /"}),
            {"requestId": "r0", "machineId": "placeholder", "expectedEpoch": "daemon-one",
             "command": {"type": "createTerminal", "agentType": "", "workspace": "/remote"}},
            # A create cannot smuggle a command, a flag or an extra field in.
            {"requestId": "r0", "machineId": "placeholder", "expectedEpoch": "daemon-one",
             "command": {"type": "createTerminal", "agentType": "shell", "workspace": "/remote",
                         "command": "rm -rf /"}},
        ]:
            status, body = request("/api/v1/desktop/commands", bad, token=token)
            assert status in (400, 409), (status, body)
            assert "result" not in body and body["error"] in (
                "invalid_command", "invalid_layout", "wrong_machine"), body
        assert stub.commands == [], stub.commands

        # An accepted move answers with the owner's published revision and
        # geometry, and the owner sees the typed envelope, not a shell string.
        moved = copy.deepcopy(layout)
        moved["x"] = 700
        stub.answer_commands_with(command_outcome(
            True, revision=2, cardId="saved-card", cardRevision=2, layout=moved))
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(copy.deepcopy(layout), machine_id=machine_id), token=token)
        assert status == 200, body
        assert body == {"requestId": "r1", "machineId": machine_id, "epoch": "daemon-one",
                        "revision": 2,
                        "result": {"type": "applied", "cardId": "saved-card",
                                   "cardRevision": 2, "expanded": None,
                                   "layout": moved}}, body
        assert len(stub.commands) == 1
        sent = json.loads(stub.commands[0].split(" ", 1)[1])
        assert sent["command"]["type"] == "setLayout"
        assert sent["command"]["cardId"] == "saved-card"
        assert sent["command"]["expectedRevision"] == 1
        assert sent["expectedEpoch"] == "daemon-one"

        # The same request id replays the recorded answer instead of applying
        # the same mutation twice.
        status, replayed = request("/api/v1/desktop/commands",
            set_layout_command(copy.deepcopy(layout), machine_id=machine_id), token=token)
        assert status == 200 and replayed == body, replayed
        assert len(stub.commands) == 1, stub.commands
        assert json.loads(stub.commands[0].split(" ", 1)[1])["requestId"] == "r1"

        # A stale revision is refused with the owner's own geometry, so the
        # viewer can redraw the real state instead of overwriting it.
        stub.answer_commands_with(command_outcome(
            False, error="conflict", cardId="saved-card", cardRevision=2, layout=moved))
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r2", machine_id=machine_id), token=token)
        assert status == 409, body
        assert body["result"] == {"type": "conflict", "cardId": "saved-card",
                                  "cardRevision": 2, "expanded": None,
                                  "layout": moved}, body

        # A refused card and an expired epoch are typed refusals too.
        stub.answer_commands_with(command_outcome(False, error="unknown_card"))
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r3", machine_id=machine_id), token=token)
        assert status == 404 and body["result"] == {"type": "rejected",
                                                    "error": "unknown_card"}, body
        stub.answer_commands_with(command_outcome(False, error="epoch_changed"))
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r4", machine_id=machine_id), token=token)
        assert status == 409 and body["result"]["error"] == "epoch_changed", body

        # An owner that never answers leaves the outcome unknown. The retry is
        # refused rather than applied a second time, and neither is replayed.
        stub.answer_commands_with(silent=True)
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r5", machine_id=machine_id), token=token)
        assert status == 504 and body["error"] == "desktop_timeout", body
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r5", machine_id=machine_id), token=token)
        assert status == 409 and body["error"] == "unknown_outcome", body
        attempts = [json.loads(c.split(" ", 1)[1])["requestId"] for c in stub.commands]
        assert attempts.count("r5") == 1, attempts

        # An owner that answers without a typed outcome applied nothing: its
        # status is still reported, and a code that is not one of ours is never
        # passed on as free-form text.
        stub.answer_commands_with({"ok": False, "error": "desktop_not_ready"})
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r6", machine_id=machine_id), token=token)
        assert status == 503 and body == {"error": "desktop_not_ready"}, body
        stub.answer_commands_with({"ok": False, "error": "rm -rf / #"})
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r7", machine_id=machine_id), token=token)
        assert status == 502 and body == {"error": "invalid_desktop_response"}, body

        # A create names the harness and the folder the host itself reported,
        # and the answer carries the card the host made.
        created = copy.deepcopy(layout)
        stub.answer_commands_with(command_outcome(
            True, revision=3, cardId="sd_term_new", cardRevision=3, layout=created))
        status, body = request("/api/v1/desktop/commands",
            {"requestId": "r9", "machineId": machine_id, "expectedEpoch": "daemon-one",
             "command": {"type": "createTerminal", "agentType": "shell",
                         "workspace": "/remote/project with spaces"}}, token=token)
        assert status == 200, body
        assert body["result"] == {"type": "applied", "cardId": "sd_term_new",
                                  "cardRevision": 3, "expanded": None,
                                  "layout": created}, body
        assert json.loads(stub.commands[-1].split(" ", 1)[1])["command"] == {
            "type": "createTerminal", "agentType": "shell",
            "workspace": "/remote/project with spaces"}

        # A folder change names a folder from the owner's own list and the
        # workspace revision it saw.
        stub.answer_commands_with(command_outcome(
            True, revision=5, expanded=None))
        status, body = request("/api/v1/desktop/commands",
            {"requestId": "r11", "machineId": machine_id, "expectedEpoch": "daemon-one",
             "command": {"type": "setWorkspace", "workspace": "/remote/project with spaces",
                         "expectedRevision": 1}}, token=token)
        assert status == 200 and body["result"]["type"] == "applied", body
        assert json.loads(stub.commands[-1].split(" ", 1)[1])["command"] == {
            "type": "setWorkspace", "workspace": "/remote/project with spaces",
            "expectedRevision": 1}
        # Whether a folder is one the host offers is the owner's rule, but the
        # envelope's shape is the bridge's: an empty folder never reaches it.
        before = len(stub.commands)
        status, body = request("/api/v1/desktop/commands",
            {"requestId": "r12", "machineId": machine_id, "expectedEpoch": "daemon-one",
             "command": {"type": "setWorkspace", "workspace": "",
                         "expectedRevision": 1}}, token=token)
        assert status == 400 and body["error"] == "invalid_command", body
        assert len(stub.commands) == before, stub.commands

        # Expanding and collapsing travel as their own command, because the
        # owner's `expanded` is presentation and is not part of saved geometry.
        stub.answer_commands_with(command_outcome(
            True, revision=4, cardId="saved-card", cardRevision=4, layout=layout))
        status, body = request("/api/v1/desktop/commands",
            {"requestId": "r10", "machineId": machine_id, "expectedEpoch": "daemon-one",
             "command": {"type": "setExpanded", "cardId": "saved-card",
                         "expectedRevision": 3, "expanded": True}}, token=token)
        assert status == 200 and body["result"]["type"] == "applied", body
        assert json.loads(stub.commands[-1].split(" ", 1)[1])["command"] == {
            "type": "setExpanded", "cardId": "saved-card", "expectedRevision": 3,
            "expanded": True}

        # An owner that is simply not running never received the command, so its
        # outcome is known: the retry after the owner returns is applied instead
        # of being refused as uncertain.
        stub.close()
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r8", machine_id=machine_id), token=token)
        assert status == 503 and body["error"] == "desktop_unavailable", body
        stub = DesktopStub(directory)
        stub.answer_commands_with(command_outcome(
            True, revision=3, cardId="saved-card", cardRevision=3, layout=layout))
        status, body = request("/api/v1/desktop/commands",
            set_layout_command(layout, request_id="r8", machine_id=machine_id), token=token)
        assert status == 200 and body["result"]["type"] == "applied", body
    finally:
        stub.close()


def check_desktop_routes(request, context, port, token, directory):
    status, missing = request("/api/v1/desktop/workspace", token=token)
    assert status == 503 and missing["error"] == "desktop_unavailable"
    assert request("/api/v1/desktop/events", token=token)[0] == 400
    stub = DesktopStub(directory)
    live = None
    try:
        status, snapshot = request("/api/v1/desktop/workspace", token=token)
        assert status == 200, snapshot
        assert snapshot["machineId"] == request("/api/v1/ping")[1]["bridgeId"]
        assert snapshot["epoch"] == "daemon-one" and snapshot["revision"] == 1
        assert snapshot["cards"][0]["layout"]["x"] == 100
        assert "DO_NOT_EXPORT" not in json.dumps(snapshot)
        assert "untrusted-local-id" not in json.dumps(snapshot)

        live = context.wrap_socket(socket.create_connection(("127.0.0.1", port)),
                                   server_hostname="super-desktop.local")
        live.settimeout(5)
        live.sendall((f"GET /api/v1/desktop/events HTTP/1.1\r\nHost: localhost\r\n"
                      f"Authorization: Bearer {token}\r\nUpgrade: websocket\r\n"
                      "Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
                      "Sec-WebSocket-Version: 13\r\n\r\n").encode())
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            header += receive_exact(live, 1)
            assert len(header) < 16384
        assert header.startswith(b"HTTP/1.1 101")
        assert receive_event(live) == {"type": "snapshot", "sequence": 1, "workspace": snapshot}

        # The bridge learns about the change from the owner's feed, not by
        # polling: one notification, one snapshot read, one event.
        deadline = time.time() + 5
        while not stub.watchers:
            assert time.time() < deadline, "the bridge never opened the owner's change feed"
            time.sleep(.02)
        time.sleep(.3)
        reads = stub.commands.count("desktop-workspace")
        time.sleep(1.2)
        assert stub.commands.count("desktop-workspace") == reads, "the idle stream polled the owner"
        moved = copy.deepcopy(stub.reply)
        moved["workspace"]["revision"] = 2
        moved["workspace"]["cards"][0]["revision"] = 2
        moved["workspace"]["cards"][0]["layout"]["x"] = 700
        stub.replace(moved)
        event = receive_event(live)
        assert event["type"] == "snapshot" and event["sequence"] == 2, event
        assert event["workspace"]["revision"] == 2
        assert event["workspace"]["cards"][0]["layout"]["x"] == 700
        assert stub.commands.count("desktop-workspace") == reads + 1, stub.commands

        restarted = copy.deepcopy(moved)
        restarted["workspace"].update(epoch="daemon-two", revision=1, cards=[])
        stub.replace(restarted)
        event = receive_event(live)
        assert event["sequence"] == 3
        assert event["workspace"]["epoch"] == "daemon-two"
        assert event["workspace"]["revision"] == 1 and event["workspace"]["cards"] == []

        stub.replace({"ok": False, "error": "desktop_not_ready"})
        assert receive_event(live) == {"type": "unavailable", "sequence": 4, "error": "desktop_not_ready"}
        # Quiet streams carry sequenced heartbeats (no revision while down).
        live.settimeout(8)
        heartbeat = receive_event(live)
        live.settimeout(5)
        assert heartbeat == {"type": "heartbeat", "sequence": 5}, heartbeat
        assert request("/api/v1/desktop/workspace", token=token)[0] == 503
        stub.replace({"ok": True, "workspace": {}})
        assert request("/api/v1/desktop/workspace", token=token)[0] == 502
        assert stub.commands and set(stub.commands) == {"desktop-workspace"}, stub.commands
    except BaseException:
        if live is not None:
            live.close()
        raise
    finally:
        stub.close()
    assert request("/api/v1/desktop/workspace", token=token)[0] == 503
    # Caller revokes the credential and verifies this live stream also closes.
    return live
