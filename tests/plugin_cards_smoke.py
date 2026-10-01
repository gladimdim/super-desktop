#!/usr/bin/env python3
"""Plugins on terminal cards, end to end, on a real isolated daemon.

Window Controls (the example) replaces the card controls: half-screen moves,
to-icon-at-the-edge, restore from the icon, and the built-in controls back
when it is off. Card Kit (tests/fixtures) sets titles and chips on every card
and types into a terminal through its card button; the title other devices
see (the desktop-workspace snapshot) never changes.
"""
import json
import shutil
import socket
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from plugin_isolated import ROOT, Isolated, wait_for  # noqa: E402

BIN = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/debug/super-desktop"
failures = []


def check(ok, what, detail=""):
    print(("ok   " if ok else "FAIL ") + what + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(what)


def main():
    if not shutil.which("gtk4-broadwayd") or not BIN.exists():
        print("SKIP: needs gtk4-broadwayd and a built binary")
        return 0
    box = Isolated(BIN)

    def raw(command):
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(str(box.runtime / "super-desktop.sock"))
            s.sendall(f"{command}\n".encode())
            data = b""
            while chunk := s.recv(1 << 16):
                data += chunk
            return json.loads(data.decode())

    def cards():
        reply = box.ipc({"op": "cards"})
        return reply["screen"], {c["id"]: c for c in reply["cards"]}

    def press(card, control):
        return box.cli("press", card, control).returncode == 0

    def events():
        path = box.home / ".local/state/super-desktop/plugins/cardkit/events.log"
        return path.read_text().splitlines() if path.exists() else []

    try:
        check(box.start(), "isolated daemon is listening")
        for _ in range(2):
            raw("add-term shell")
        listed = wait_for(lambda: len(cards()[1]) == 2 and cards())
        check(listed is not None, "two shell cards")
        screen, all_cards = listed or ({}, {})
        first, second = sorted(all_cards)[:2] if len(all_cards) >= 2 else (None, None)
        check(all_cards.get(first, {}).get("controls") == ["builtin:iconify", "builtin:expand", "builtin:close"], "built-in controls without plugins")
        base_title = all_cards.get(first, {}).get("title", {}).get("published")

        controls = box.copy_plugin(ROOT / "skills/super-desktop-plugin/examples/window-controls", "window-controls")
        check(box.cli("link", str(controls), "--yes").returncode == 0 and box.cli("activate", "window-controls").returncode == 0, "Window Controls on")
        wanted = ["window-controls.left-btn", "window-controls.right-btn", "window-controls.edge-btn", "builtin:expand", "builtin:close"]
        check(wait_for(lambda: cards()[1][first]["controls"] == wanted) is not None, "its control set replaces the built-in one", cards()[1].get(first, {}).get("controls"))
        w, top = screen.get("w", 0), screen.get("top", 0)
        half = (w - 24) / 2
        check(press(first, "window-controls.left-btn"), "press ◧")
        # The host clamps like a drag (x >= 10, y >= 70): the left edge, half the width.
        left = wait_for(lambda: cards()[1][first]["rect"]["x"] <= 16 and cards()[1][first]["rect"])
        check(left is not None and abs(left["w"] - half) <= 1 and left["y"] <= max(top + 8, 70), "the card takes the left half", cards()[1][first]["rect"])
        check(press(first, "window-controls.right-btn"), "press ◨")
        right = wait_for(lambda: cards()[1][first]["rect"]["x"] > w / 2 and cards()[1][first]["rect"])
        check(right is not None and abs(right["x"] + right["w"] - (w - 8)) <= 1, "the card takes the right half", cards()[1][first]["rect"])
        check(press(first, "window-controls.edge-btn"), "press ⇥")
        icon = wait_for(lambda: cards()[1][first]["iconified"] and cards()[1][first])
        check(icon is not None and icon["icon"]["x"] > w / 2, "it becomes an icon at the nearest (right) edge", cards()[1][first])
        check(not press(first, "window-controls.left-btn"), "a header control cannot be pressed on an icon")
        check(press(first, "builtin:restore"), "restore from the icon's controls")
        check(wait_for(lambda: not cards()[1][first]["iconified"]) is not None, "the card is open again")
        check(box.cli("activate", "window-controls").returncode == 0, "activating again is harmless")
        check(box.cli("deactivate", "window-controls").returncode == 0, "Window Controls off")
        check(wait_for(lambda: cards()[1][first]["controls"] == ["builtin:iconify", "builtin:expand", "builtin:close"]) is not None, "built-in controls are back")
        check(box.ipc({"op": "footprint", "id": "window-controls"}).get("footprint") == [], "nothing of Window Controls is left")

        kit = box.copy_plugin(ROOT / "tests/fixtures/plugins/cardkit", "cardkit")
        check(box.cli("link", str(kit), "--yes").returncode == 0 and box.cli("activate", "cardkit").returncode == 0, "Card Kit on")
        titled = wait_for(lambda: cards()[1][first]["title"]["drawn"].startswith("🧪 shell"))
        check(titled is not None, "a plugin title is drawn", cards()[1][first]["title"])
        state = cards()[1][first]
        check(state["title"]["chipsBefore"] == ["kit"], "with its chip", state["title"])
        check(state["title"]["published"] == base_title, "the published title is the card's own", state["title"])
        snapshot = raw("desktop-workspace")
        text = json.dumps(snapshot)
        check(snapshot.get("ok") and "🧪" not in text and "kit" not in text, "the workspace other devices get has no plugin title or chip")
        check(state["buttons"] == ["cardkit.button"], "its card button is in the header", state["buttons"])
        check(press(first, "cardkit.button"), "press the card button")
        check(wait_for(lambda: "read back plugin-was-42" in events(), timeout=20) is not None, "it typed into the terminal and read the output back", events())
        check(any(e.startswith("cards 2 screen") for e in events()), "workspace.cards saw both cards")
        # harness.launch with a first prompt: the harness gets it once, as one
        # argument; the saved command and the phone's list never contain it.
        argv_file = box.base / "claude-argv.json"
        box.stub("claude", f"""#!/usr/bin/env python3
import json, sys, time
json.dump(sys.argv[1:], open({str(argv_file)!r}, "w"))
time.sleep(600)
""")
        prompt = "Flush these repos: 'a b', $HOME and `x`"
        folder = box.base / "work"
        folder.mkdir()
        check(box.cli("run", "cardkit", "cardkit.launch", json.dumps({"agent": "claude", "folder": str(folder), "prompt": prompt})).returncode == 0, "launch claude with a prompt")
        check(wait_for(lambda: any(e.startswith("launched sd_") for e in events())) is not None, "a card was opened", events())
        argv = wait_for(lambda: argv_file.exists() and json.loads(argv_file.read_text()))
        check(argv is not None and prompt in argv, "the harness received the prompt as one argument", argv)
        state = (box.home / ".config/super-desktop/state.json").read_text()
        check(prompt not in state and "Flush these repos" not in state, "the saved command has no prompt")
        listed = subprocess.run([str(BIN), "harnesses"], env=box.env, capture_output=True, text=True, timeout=30).stdout
        check("Flush these repos" not in listed, "the phone's harness list has no prompt")
        refused = box.cli("run", "cardkit", "cardkit.launch", json.dumps({"agent": "shell", "folder": str(folder), "prompt": "x"}))
        check(wait_for(lambda: any("cannot be started with a prompt" in l for l in (box.home / ".local/state/super-desktop/plugins/cardkit/plugin.log").read_text().splitlines())) is not None, "a harness without a prompt argument is refused with a reason")
        check(box.cli("deactivate", "cardkit").returncode == 0, "Card Kit off")
        back = wait_for(lambda: cards()[1][first]["title"]["drawn"] == cards()[1][first]["title"]["published"] and cards()[1][first])
        check(back is not None and back["title"]["chipsBefore"] == [] and back["buttons"] == [], "title, chip and button are gone", cards()[1].get(first))
        check(box.ipc({"op": "footprint", "id": "cardkit"}).get("footprint") == [], "nothing of Card Kit is left")
    finally:
        if failures:
            print("daemon stderr:\n" + box.daemon_log()[-3000:])
            for plugin in ("window-controls", "cardkit"):
                log = box.home / f".local/state/super-desktop/plugins/{plugin}/plugin.log"
                if log.exists():
                    print(f"{plugin} log:\n" + log.read_text()[-2000:])
        box.close()
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
