"""Isolated v3 wire/security regression test. Never uses production credentials."""
import http.client
import json
import os
import pathlib
import socket
import ssl
import subprocess
import sys
import tempfile
import time


def main():
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix="sd-security-test-") as directory:
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        process = subprocess.Popen([binary, "harness-bridge", str(port)],
            env={**os.environ, "SUPER_DESKTOP_BRIDGE_STATE_DIR": directory},
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            control = str(pathlib.Path(directory) / "control.sock")
            for _ in range(100):
                if pathlib.Path(control).exists():
                    break
                assert process.poll() is None, "bridge exited during startup"
                time.sleep(.05)

            def admin(path, body=None):
                with socket.socket(socket.AF_UNIX) as client:
                    client.connect(control)
                    client.settimeout(5)
                    payload = json.dumps(body or {}).encode()
                    client.sendall((f"{'POST' if body is not None else 'GET'} {path} HTTP/1.1\r\nHost: local\r\nContent-Length: {len(payload)}\r\n\r\n").encode() + payload)
                    response = b""
                    while chunk := client.recv(65536):
                        response += chunk
                    assert response.startswith(b"HTTP/1.1 200"), response[:100]
                    return json.loads(response.split(b"\r\n\r\n", 1)[1])

            identity = json.loads((pathlib.Path(directory) / "tls-identity.json").read_text())
            cert = bytes(identity["certificate"])
            context = ssl.create_default_context(cadata=ssl.DER_cert_to_PEM_cert(cert))
            context.check_hostname = False  # Exact isolated certificate is the trust anchor.

            def request(path, body=None, token=None, headers=None):
                connection = http.client.HTTPSConnection("127.0.0.1", port, context=context, timeout=5)
                try:
                    hdr = headers or {}
                    if token:
                        hdr["Authorization"] = "Bearer " + token
                    connection.request("POST" if body is not None else "GET", path,
                        json.dumps(body) if body is not None else None, hdr)
                    response = connection.getresponse()
                    return response.status, json.loads(response.read())
                finally:
                    connection.close()

            assert request("/api/v1/ping")[0] == 200
            for path in ["/api/v1/desktop/capabilities", "/api/v1/harnesses", "/api/v1/theme", "/api/v1/workspaces", "/api/v1/harnesses/stream", "/api/v1/harnesses/sd_term_probe/input",
                         "/api/v1/harnesses/sd_term_probe/assets", "/api/v1/harnesses/sd_term_probe/assets/id/content",
                         "/api/v1/harnesses/sd_term_probe/assets/id/pages/1"]:
                assert request(path)[0] == 401, path
            assert request("/api/v1/pair/state")[0] == 403
            assert request("/api/v1/completions", {"sessions": []})[0] == 401
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {})[0] == 401
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {}, headers={"Content-Length": "99999999"})[0] == 401
            assert request("/api/v1/pair/invitation", {})[0] == 403
            assert request("/api/v1/pair", {"deviceName": "stranger"})[0] == 403
            assert request("/api/v1/ping", headers={"Origin": "https://untrusted.example"})[0] == 403

            invitation = admin("/api/v1/pair/invitation", {})
            status, pending = request("/api/v1/pair", {"deviceName": "Isolated test phone", "secret": invitation["secret"]})
            assert status == 202 and "token" not in pending
            assert request("/api/v1/pair", {"secret": invitation["secret"]})[0] == 403
            rid = {"requestId": pending["requestId"]}
            assert request("/api/v1/pair/poll", rid)[1]["status"] == "pending"
            assert request("/api/v1/pair/approve", rid)[0] == 403
            admin("/api/v1/pair/approve", rid)
            token = request("/api/v1/pair/poll", rid)[1]["token"]
            status, desktop = request("/api/v1/desktop/capabilities", token=token)
            assert status == 200
            assert desktop == {"machineId": request("/api/v1/ping")[1]["bridgeId"],
                               "desktopApiVersion": 1, "capabilities": []}
            assert request("/api/v1/ping")[1]["protocolVersion"] == 3
            assert request("/api/v1/desktop/capabilities", token=token,
                           headers={"Origin": "https://untrusted.example"})[0] == 403
            assert request("/api/v1/theme", token=token)[0] == 200
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {}, token=token, headers={"Origin": "https://untrusted.example"})[0] == 403
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {}, token=token, headers={"Content-Length": "99999999"})[0] == 413
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {"requestId": "a" * 32, "text": "hello", "imageBase64": "invalid"}, token=token)[0] == 409
            # Authorized image route accepts >16 KiB, but the general request limit stays unchanged.
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {"requestId": "a" * 32, "text": "hello", "imageBase64": "A" * 20000}, token=token)[0] == 409
            assert request("/api/v1/completions", {"sessions": ["../invalid"]}, token=token)[0] == 400
            assert request("/api/v1/completions", {"sessions": ["x"] * 33}, token=token)[0] == 400
            status, completion = request("/api/v1/completions", {"sessions": ["sd_term_missing"]}, token=token)
            assert status == 200 and completion["terminals"][0]["supported"] is False
            assert request("/api/v1/harnesses/sd_term_nonexistent_asset_probe/assets", token=token)[0] == 400
            assert request("/api/v1/harnesses/sd_term_nonexistent_asset_probe/assets", {"path": "../../etc/passwd"}, token=token)[0] == 400
            assert request("/api/v1/pair/approve", rid, token=token)[0] == 403
            assert token not in (pathlib.Path(directory) / "config.json").read_text()

            live = context.wrap_socket(socket.create_connection(("127.0.0.1", port)), server_hostname="super-desktop.local")
            live.settimeout(5)
            live.sendall((f"GET /api/v1/harnesses/stream HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
            assert b"101 Switching" in live.recv(4096)
            device = admin("/api/v1/pair/devices")["devices"][0]
            assert device["active"] is True, "authorized phone must count as active"
            admin("/api/v1/pair/revoke", {"deviceId": device["id"]})
            assert admin("/api/v1/pair/devices")["devices"] == []
            assert request("/api/v1/theme", token=token)[0] == 401
            assert request("/api/v1/desktop/capabilities", token=token)[0] == 401
            assert request("/api/v1/completions", {"sessions": []}, token=token)[0] == 401
            assert request("/api/v1/harnesses/sd_term_probe/image-prompt", {}, token=token)[0] == 401
            assert request("/api/v1/harnesses/sd_term_probe/assets/id/content", token=token)[0] == 401
            assert request("/api/v1/pair/poll", rid)[0] == 404
            while live.recv(65536):
                pass
            live.close()

            def raw(payload):
                with context.wrap_socket(socket.create_connection(("127.0.0.1", port)), server_hostname="super-desktop.local") as client:
                    client.settimeout(7)
                    client.sendall(payload)
                    return client.recv(4096)
            assert not raw(b"POST /api/v1/pair HTTP/1.1\r\nHost: local\r\nContent-Length: 999999999\r\n\r\n")
            assert not raw(b"GET /api/v1/ping HTTP/1.1\r\nHost: local\r\nHost: duplicate\r\n\r\n")
            malformed = raw(b"GET /api/v1/theme HTTP/1.1\r\nHost: local\r\nAuthorization: \xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9\r\n\r\n")
            assert b"401" in malformed
            assert request("/api/v1/ping")[0] == 200
            with socket.create_connection(("127.0.0.1", port)) as plain:
                plain.settimeout(7)
                plain.sendall(b"GET /api/v1/ping HTTP/1.1\r\nHost: localhost\r\n\r\n")
                try:
                    assert b"HTTP/1.1 200" not in plain.recv(1024)
                except ConnectionResetError:
                    pass
            try:
                with ssl.create_default_context().wrap_socket(socket.create_connection(("127.0.0.1", port)), server_hostname="localhost"):
                    raise AssertionError("Untrusted certificate accepted")
            except ssl.SSLCertVerificationError:
                pass
            print("PASS: TLS, no plaintext/localhost bypass, origin rejection, invitation/approval, hashed tokens, live revocation, request limits, malformed headers")
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
