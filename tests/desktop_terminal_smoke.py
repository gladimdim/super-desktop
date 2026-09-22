"""Live remote terminal streaming against a real bridge and a private tmux server.

Runs the real `harness-bridge`, the real pinned-WSS viewer CLI and a disposable
tmux server, so a failure here is a real transport, authentication or PTY
failure. No production daemon, session, credential or socket is touched: the
owner IPC endpoint is a stub, the tmux server is private, and the bridge binds
an ephemeral port.
"""
import copy
import json
import os
import pathlib
import re
import socket
import subprocess
import sys
import tempfile
import time
from desktop_bridge_checks import DesktopStub

BINARY = str(pathlib.Path(sys.argv[1]).resolve())

CARD = "card-smoke"
SESSION = "sd_term_smoke"
FOREIGN_CARD = "card-foreign"
# A red foreground, however tmux chooses to encode it for the client's TERM.
COLOR_SGR = re.compile(rb"\x1b\[(3[0-9]|9[0-9]|38;5;\d+|38;2;\d+;\d+;\d+)m")


def scenario():
    with tempfile.TemporaryDirectory(prefix="sd-terminal-smoke-") as root_dir:
        root = pathlib.Path(root_dir)
        host = root / "host"
        host.mkdir(mode=0o700)
        tmux_dir = root / "tmux"
        tmux_dir.mkdir(mode=0o700)
        env = {
            **os.environ,
            "SUPER_DESKTOP_BRIDGE_STATE_DIR": str(host),
            "XDG_RUNTIME_DIR": str(host),
            "TMUX_TMPDIR": str(tmux_dir),
        }
        # Hermetic tmux: bare `tmux` commands inherit the caller's server via
        # $TMUX and ignore $TMUX_TMPDIR, so without this a `kill-server` in the
        # cleanup below would murder the user's real sessions. Dropping
        # $TMUX/$TMUX_PANE makes every call (here and in the bridge under
        # test, which uses the default socket) land on a private server in
        # $TMUX_TMPDIR. No `-L`/`-S` flag: the bridge has no such option, so
        # the test must use the same default socket the bridge will use.
        env.pop("TMUX", None)
        env.pop("TMUX_PANE", None)
        client_env = {
            **env,
            "SUPER_DESKTOP_BRIDGE_STATE_DIR": str(root / "viewer"),
            "SUPER_DESKTOP_PEERS_STATE_DIR": str(root / "peers"),
        }

        def tmux(*args, check=True):
            result = subprocess.run(["tmux", *args], env=env, capture_output=True,
                                    text=True, timeout=15)
            assert result.returncode == 0 or not check, (args, result.stdout, result.stderr)
            return result.stdout.strip()

        # A private tmux server with a deterministic grid: no client ever
        # attaches to it except the ones under test.
        tmux("-f", "/dev/null", "new-session", "-d", "-s", SESSION, "-x", "100", "-y", "30",
             "bash", "--noprofile", "--norc")
        tmux("set-option", "-g", "status", "off")
        tmux("set-option", "-t", SESSION, "detach-on-destroy", "on")
        assert tmux("display-message", "-p", "-t", SESSION, "#{window_width}x#{window_height}") == "100x30"

        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        bridge = subprocess.Popen([BINARY, "harness-bridge", str(port)], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        stub = DesktopStub(host)
        live = None
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
                    client.sendall(
                        (f"{'POST' if body is not None else 'GET'} /api/v1/pair/{path} HTTP/1.1\r\n"
                         f"Host: local\r\nContent-Length: {len(payload)}\r\n\r\n").encode() + payload)
                    response = b""
                    while chunk := client.recv(65536):
                        response += chunk
                    assert response.startswith(b"HTTP/1.1 200"), response[:100]
                    return json.loads(response.split(b"\r\n\r\n", 1)[1])

            def cli(*args, expected=0, timeout=30):
                result = subprocess.run([BINARY, *args], env=client_env, capture_output=True,
                                        timeout=timeout)
                assert result.returncode == expected, (args, result.stderr)
                return result

            def host_cards(cards):
                reply = copy.deepcopy(stub.reply)
                reply["workspace"]["cards"] = cards
                stub.replace(reply)

            def card(card_id, session_name):
                return {
                    "cardId": card_id, "sessionName": session_name, "agentType": "shell",
                    "title": card_id, "status": "RUNNING", "sessionAlive": True,
                    "workspace": "/remote/project", "revision": 1, "stackingOrder": 0,
                    "expanded": False, "terminalSize": {"columns": 100, "rows": 30},
                    "layout": {"x": 100, "y": 200, "width": 640, "height": 480,
                               "restoredWidth": 640, "restoredHeight": 480,
                               "iconified": False, "iconX": 32, "iconY": 64, "tag": 3},
                }
            host_cards([card(CARD, SESSION), card(FOREIGN_CARD, "not_owned_session")])

            # Pair a real viewer through the CLI and the host's approval flow.
            invitation = admin("invitation", {})
            pairing = subprocess.Popen(
                [BINARY, "peer-add", "--host", "127.0.0.1", "--port", str(port)],
                env=client_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True)
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
            admin("approve", {"requestId": pending[0]["requestId"]})
            stdout, stderr = pairing.communicate(timeout=15)
            assert pairing.returncode == 0, stderr
            machine = json.loads(stdout)["machineId"]

            # The host's own pane content, produced before the viewer attaches:
            # a full redraw must carry it, in color, to the live stream.
            tmux("send-keys", "-t", SESSION, r"printf '\033[31mSD_COLOR_MARK\033[0m\n'", "Enter")
            deadline = time.time() + 5
            while "SD_COLOR_MARK" not in tmux("capture-pane", "-p", "-t", SESSION):
                assert time.time() < deadline, "host pane never printed the marker"
                time.sleep(.05)

            result = cli("peer-attach", machine, CARD, "--seconds", "4")
            assert b"attached 100x30" in result.stderr, result.stderr
            # Raw bytes, colors included: the viewer paints what the host paints.
            assert b"SD_COLOR_MARK" in result.stdout, result.stdout
            assert COLOR_SGR.search(result.stdout), result.stdout

            # Duplex: piped stdin types into the host shell and the echo comes
            # back over the same stream. Ctrl+C is just another byte.
            marker = "SD_DUPLEX_%d" % os.getpid()
            sender = subprocess.run(
                [BINARY, "peer-attach", machine, CARD, "--seconds", "8"],
                input=("echo %s\n" % marker).encode(), env=client_env,
                capture_output=True, timeout=30)
            assert sender.returncode == 0, sender.stderr
            assert marker.encode() in sender.stdout, sender.stderr

            # Only an owned card can be addressed, and only its own session.
            refused = cli("peer-attach", machine, "no-such-card", expected=1).stderr
            assert b"unknown_card" in refused, refused
            foreign = cli("peer-attach", machine, FOREIGN_CARD, expected=1).stderr
            assert b"unknown_card" in foreign, foreign
            # Revoked access cannot keep streaming, let alone keep typing into it.
            live = subprocess.Popen([BINARY, "peer-attach", machine, CARD], env=client_env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            time.sleep(3)
            assert live.poll() is None, "a live attach must stay open"
            admin("revoke", {"deviceId": admin("devices")["devices"][0]["id"]})
            live.wait(timeout=15)
            reason = live.stderr.read()
            assert live.returncode != 0, reason
            assert b"connection_failed" in reason or b"revoked" in reason, reason
            # The host session itself is untouched by detach and revocation.
            assert tmux("display-message", "-p", "-t", SESSION, "#{pane_pid}")
        finally:
            if live is not None and live.poll() is None:
                live.kill()
                live.wait()
            stub.close()
            bridge.terminate()
            bridge.wait(timeout=10)
            tmux("kill-server", check=False)


scenario()
print("Desktop terminal smoke passed: pinned WSS attach, live colored output, "
      "typed stdin, card ownership, revocation teardown, host session survival")
