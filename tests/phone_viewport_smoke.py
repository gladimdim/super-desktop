"""Phone viewport negotiation against a real isolated protocol-3 bridge.

Usage: python3 tests/phone_viewport_smoke.py BINARY [--legacy]
The legacy mode verifies a released PC ignores the new opt-in query. No user
tmux server, desktop daemon, credential or phone configuration is touched.
"""
import http.client
import json
import os
import pathlib
import queue
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time


def main():
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    legacy = "--legacy" in sys.argv[2:]
    with tempfile.TemporaryDirectory(prefix="sd-phone-viewport-") as directory:
        root = pathlib.Path(directory)
        env = {**os.environ, "SUPER_DESKTOP_BRIDGE_STATE_DIR": directory,
               "XDG_RUNTIME_DIR": directory, "TMUX_TMPDIR": directory,
               "XDG_STATE_HOME": str(root / "state"), "XDG_CONFIG_HOME": str(root / "config")}
        for name in ("TMUX", "TMUX_PANE", "WAYLAND_DISPLAY", "DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE"):
            env.pop(name, None)
        session = "sd_term_viewport_smoke"
        bridge = desktop = None
        sockets = []

        def tmux(*args):
            return subprocess.check_output(["tmux", *args], env=env, text=True).strip()

        def grid():
            return tmux("display-message", "-p", "-t", session, "#{pane_width}x#{pane_height}")

        def wait_grid(expected, timeout=5):
            end = time.monotonic() + timeout
            while grid() != expected:
                assert time.monotonic() < end, (expected, grid())
                time.sleep(.03)

        def admin(path, body=None):
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(5)
                client.connect(str(root / "control.sock"))
                data = json.dumps(body or {}).encode()
                client.sendall((f"{'POST' if body is not None else 'GET'} {path} HTTP/1.1\r\n"
                                f"Host: local\r\nContent-Length: {len(data)}\r\n\r\n").encode() + data)
                response = b""
                while chunk := client.recv(65536): response += chunk
                assert response.startswith(b"HTTP/1.1 200"), response[:100]
                return json.loads(response.split(b"\r\n\r\n", 1)[1])

        try:
            tmux("-f", "/dev/null", "new-session", "-d", "-s", session,
                 "-x", "120", "-y", "40", "/usr/bin/sleep", "1000")
            desktop = subprocess.Popen(["tmux", "-C", "attach-session", "-f", "no-output", "-t", session],
                                       env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            replies = queue.Queue()
            def read_desktop():
                for line in desktop.stdout: replies.put(line)
            threading.Thread(target=read_desktop, daemon=True).start()
            while not replies.get(timeout=5).startswith("%end "): pass
            desktop.stdin.write("refresh-client -C 120x40\n")
            desktop.stdin.flush()
            while not replies.get(timeout=5).startswith("%end "): pass
            wait_grid("120x40")
            pid = tmux("display-message", "-p", "-t", session, "#{pane_pid}")
            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                port = probe.getsockname()[1]
            bridge = subprocess.Popen([binary, "harness-bridge", str(port)], env=env,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(100):
                if (root / "control.sock").exists(): break
                assert bridge.poll() is None
                time.sleep(.05)
            identity = json.loads((root / "tls-identity.json").read_text())
            context = ssl.create_default_context(cadata=ssl.DER_cert_to_PEM_cert(bytes(identity["certificate"])))
            context.check_hostname = False

            def request(path, body=None):
                connection = http.client.HTTPSConnection("127.0.0.1", port, context=context, timeout=5)
                try:
                    connection.request("POST" if body is not None else "GET", path,
                                       json.dumps(body) if body is not None else None)
                    response = connection.getresponse()
                    return response.status, json.loads(response.read())
                finally: connection.close()

            assert request("/api/v1/ping")[1]["protocolVersion"] == 3
            invite = admin("/api/v1/pair/invitation", {})
            status, pending = request("/api/v1/pair", {"secret": invite["secret"], "deviceName": "Viewport smoke"})
            assert status == 202, pending
            rid = {"requestId": pending["requestId"]}
            admin("/api/v1/pair/approve", rid)
            token = request("/api/v1/pair/poll", rid)[1]["token"]

            def connect(query):
                raw = socket.create_connection(("127.0.0.1", port), timeout=5)
                sock = context.wrap_socket(raw, server_hostname="localhost")
                sockets.append(sock)
                sock.sendall((f"GET /api/v1/harnesses/{session}/stream?{query} HTTP/1.1\r\n"
                              f"Host: localhost\r\nAuthorization: Bearer {token}\r\n"
                              "Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n"
                              "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").encode())
                response = b""
                while not response.endswith(b"\r\n\r\n"): response += sock.recv(1)
                assert response.startswith(b"HTTP/1.1 101"), response
                return sock

            def exact(sock, count):
                out = b""
                while len(out) < count:
                    part = sock.recv(count - len(out))
                    assert part, "unexpected socket close"
                    out += part
                return out

            def frame(sock):
                opcode, length = exact(sock, 2)
                length &= 127
                if length == 126: length = struct.unpack("!H", exact(sock, 2))[0]
                elif length == 127: length = struct.unpack("!Q", exact(sock, 8))[0]
                assert opcode & 15 == 1, opcode
                return json.loads(exact(sock, length))

            def send(sock, body):
                data = json.dumps(body).encode()
                assert len(data) < 126
                mask = b"abcd"
                sock.sendall(bytes([0x81, 0x80 | len(data)]) + mask +
                             bytes(value ^ mask[i % 4] for i, value in enumerate(data)))

            old = connect("ansiOnly=1")
            old_frame = frame(old)
            assert "viewport" not in old_frame
            assert (old_frame["columns"], old_frame["rows"]) == (120, 40)
            # Unnegotiated messages must never acquire a lease.
            send(old, {"type": "viewport", "columns": 42, "rows": 30})
            time.sleep(1.2)
            assert grid() == "120x40"
            phone = connect("ansiOnly=1&viewport=1")
            initial = frame(phone)
            if legacy:
                assert "viewport" not in initial
                assert grid() == "120x40"
                print("PASS: released protocol-3 PC ignores viewport opt-in; no capability, no resize")
                return
            assert initial["viewport"] == {"version": 1, "active": False}
            for columns, rows in [(42, 30), (80, 20), (52, 12)]:
                send(phone, {"type": "viewport", "columns": columns, "rows": rows, "future": True})
                wait_grid(f"{columns}x{rows}")
            send(phone, {"type": "viewport", "columns": 1, "rows": 900})
            time.sleep(1.2)
            assert grid() == "52x12"
            send(phone, {"type": "viewport", "release": True})
            wait_grid("120x40")
            send(phone, {"type": "viewport", "columns": 42, "rows": 30})
            wait_grid("42x30")
            phone.close()
            wait_grid("120x40")
            phone = connect("viewport=1")
            frame(phone)
            send(phone, {"type": "viewport", "columns": 42, "rows": 30})
            wait_grid("42x30")
            # A connected but silent phone cannot retain sizing indefinitely.
            wait_grid("120x40", timeout=33)
            send(phone, {"type": "viewport", "columns": 42, "rows": 30})
            wait_grid("42x30")
            # Bridge crash closes its private control clients; tmux restores
            # the existing desktop client's grid without a cleanup command.
            bridge.kill()
            bridge.wait(timeout=5)
            wait_grid("120x40")
            assert tmux("display-message", "-p", "-t", session, "#{pane_pid}") == pid
            print("PASS: legacy frames, capability opt-in, resize, bounds, release, disconnect, lease expiry, crash recovery; session survives")
        finally:
            for sock in sockets: sock.close()
            if bridge and bridge.poll() is None:
                bridge.terminate()
                bridge.wait(timeout=10)
            if desktop and desktop.poll() is None:
                desktop.stdin.close()
                desktop.wait(timeout=5)
            subprocess.run(["tmux", "kill-server"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == "__main__": main()
