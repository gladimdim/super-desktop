"""Actual desktop CLI against an isolated TLS bridge; no production credentials."""
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
from desktop_bridge_checks import DesktopStub

BINARY = str(pathlib.Path(sys.argv[1]).resolve())


def scenario(approve, gui_test_binary=None, cancel=False, unmap=False):
    with tempfile.TemporaryDirectory(prefix="sd-peer-smoke-") as root:
        root = pathlib.Path(root)
        host = root / "host"
        host.mkdir(mode=0o700)
        env = {**os.environ, "SUPER_DESKTOP_BRIDGE_STATE_DIR": str(host), "XDG_RUNTIME_DIR": str(host)}
        client_env = {**env, "SUPER_DESKTOP_BRIDGE_STATE_DIR": str(root / "viewer"),
                      "SUPER_DESKTOP_PEERS_STATE_DIR": str(root / "peers")}
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        bridge = subprocess.Popen([BINARY, "harness-bridge", str(port)], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        stub = DesktopStub(host)
        pairing = None
        try:
            for _ in range(100):
                if (host / "control.sock").exists():
                    break
                assert bridge.poll() is None
                time.sleep(.05)

            def admin(path, body=None):
                with socket.socket(socket.AF_UNIX) as client:
                    client.settimeout(5)
                    client.connect(str(host / "control.sock"))
                    payload = json.dumps(body or {}).encode()
                    client.sendall((f"{'POST' if body is not None else 'GET'} /api/v1/pair/{path} HTTP/1.1\r\nHost: local\r\nContent-Length: {len(payload)}\r\n\r\n").encode() + payload)
                    response = b""
                    while chunk := client.recv(65536):
                        response += chunk
                    assert response.startswith(b"HTTP/1.1 200"), response[:100]
                    return json.loads(response.split(b"\r\n\r\n", 1)[1])

            def cli(*args, input=None, expected=0, environment=client_env):
                result = subprocess.run([BINARY, *args], input=input, text=True,
                                        capture_output=True, env=environment, timeout=20)
                assert result.returncode == expected, (args, result.stderr)
                return result

            invitation = admin("invitation", {})
            options = ["peer-add", "--host", "127.0.0.1", "--port", str(port)]
            wrong = {**invitation, "fingerprint": "0" * 64}
            assert "pin_mismatch" in cli(*options, input=json.dumps(wrong), expected=1).stderr
            assert "itself" in cli(*options, input=json.dumps(invitation), expected=1,
                                    environment={**client_env, "SUPER_DESKTOP_BRIDGE_STATE_DIR": str(host)}).stderr
            assert not admin("state")["requests"]
            if gui_test_binary:
                form_input = root / "form-input.json"
                form_result = root / "form-result.json"
                form_input.write_text(json.dumps({"invitation": invitation, "port": port, "approve": approve, "cancel": cancel, "unmap": unmap}))
                gui_env = {**client_env, "XDG_RUNTIME_DIR": os.environ["XDG_RUNTIME_DIR"],
                           "SUPER_DESKTOP_PAIRING_TEST_INPUT": str(form_input),
                           "SUPER_DESKTOP_PAIRING_TEST_RESULT": str(form_result)}
                pairing = subprocess.Popen([gui_test_binary, "--exact", "peer_pairing_ui::tests::wire_inner", "--nocapture"],
                                           env=gui_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            else:
                pairing = subprocess.Popen([BINARY, *options], env=client_env, stdin=subprocess.PIPE,
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                pairing.stdin.write(json.dumps(invitation) + "\n")
                pairing.stdin.close()
                pairing.stdin = None
            pending = []
            for _ in range(100):
                pending = admin("state")["requests"]
                if pending:
                    break
                assert pairing.poll() is None, pairing.communicate()
                time.sleep(.05)
            assert len(pending) == 1
            if cancel:
                for _ in range(100):
                    if form_result.exists():
                        break
                    assert pairing.poll() is None, pairing.communicate()
                    time.sleep(.05)
                assert form_result.read_text() == "cancelled"
            admin("approve" if approve else "deny", {"requestId": pending[0]["requestId"]})
            stdout, stderr = pairing.communicate(timeout=15)
            assert pending[0]["code"] in stderr
            if cancel:
                assert pairing.returncode == 0, (stdout, stderr)
                assert json.loads(cli("peer-list").stdout) == []
                return
            if not approve:
                assert pairing.returncode == (0 if gui_test_binary else 1) and "pairing_denied" in stderr
                assert json.loads(cli("peer-list").stdout) == []
                return
            assert pairing.returncode == 0, stderr
            summary = json.loads(form_result.read_text() if gui_test_binary else stdout)
            machine = summary["machineId"]
            registry = root / "peers" / "peers.json"
            saved = registry.read_bytes()
            peer = json.loads(saved)["peers"][0]
            assert peer["token"] not in stdout + stderr
            assert peer["expiresAt"] > time.time()
            assert registry.stat().st_mode & 0o777 == 0o600
            assert registry.parent.stat().st_mode & 0o777 == 0o700
            listing = cli("peer-list").stdout
            assert peer["token"] not in listing and "fingerprint" not in listing
            workspace = json.loads(cli("peer-workspace", machine).stdout)
            assert workspace["machineId"] == machine and workspace["cards"][0]["layout"]["x"] == 100
            damaged = json.loads(saved)
            damaged["peers"][0]["machineId"] = "0" * 32
            registry.write_text(json.dumps(damaged))
            assert "identity_changed" in cli("peer-workspace", "0" * 32, expected=1).stderr
            damaged["peers"][0] = {**peer, "expiresAt": 1}
            registry.write_text(json.dumps(damaged))
            assert "expired" in cli("peer-workspace", machine, expected=1).stderr
            registry.write_bytes(saved)
            cli("peer-forget", machine)
            assert json.loads(cli("peer-list").stdout) == []
            devices = admin("devices")["devices"]
            assert len(devices) == 1  # Forget is local, not host revocation.
            registry.write_bytes(saved)
            admin("revoke", {"deviceId": devices[0]["id"]})
            assert "revoked" in cli("peer-workspace", machine, expected=1).stderr
        finally:
            if pairing is not None and pairing.poll() is None:
                pairing.kill()
                pairing.wait()
            stub.close()
            bridge.terminate()
            bridge.wait(timeout=10)


gui_test_binary = str(pathlib.Path(sys.argv[2]).resolve()) if len(sys.argv) > 2 else None
scenario(True, gui_test_binary)
scenario(False, gui_test_binary)
if gui_test_binary:
    scenario(True, gui_test_binary, cancel=True)
    scenario(True, gui_test_binary, unmap=True)
print("Desktop peer pairing smoke passed: pin, self-pair, approval, denial, private storage, layout, identity, expiry, forget, revocation")
