"""Desktop wire checks used by bridge_security_smoke; no production daemon."""
import copy
import json
import pathlib
import socket
import struct
import threading


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
                "workspace": "/remote/project with spaces", "homeDirectory": "/remote",
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
                with self.lock:
                    self.commands.append(command.decode().strip())
                    response = json.dumps(self.reply).encode()
                try:
                    client.sendall(response)
                except BrokenPipeError:
                    pass

    def replace(self, reply):
        with self.lock:
            self.reply = copy.deepcopy(reply)

    def close(self):
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
        assert receive_event(live) == {"type": "snapshot", "workspace": snapshot}

        moved = copy.deepcopy(stub.reply)
        moved["workspace"]["revision"] = 2
        moved["workspace"]["cards"][0]["revision"] = 2
        moved["workspace"]["cards"][0]["layout"]["x"] = 700
        stub.replace(moved)
        event = receive_event(live)
        assert event["workspace"]["revision"] == 2
        assert event["workspace"]["cards"][0]["layout"]["x"] == 700

        restarted = copy.deepcopy(moved)
        restarted["workspace"].update(epoch="daemon-two", revision=1, cards=[])
        stub.replace(restarted)
        event = receive_event(live)
        assert event["workspace"]["epoch"] == "daemon-two"
        assert event["workspace"]["revision"] == 1 and event["workspace"]["cards"] == []

        stub.replace({"ok": False, "error": "desktop_not_ready"})
        assert receive_event(live) == {"type": "unavailable", "error": "desktop_not_ready"}
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
