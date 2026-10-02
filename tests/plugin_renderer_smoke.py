#!/usr/bin/env python3
"""Window renderers end to end, on a real isolated daemon.

Center Magnify (the example, built by its build.sh) draws the cards; the
saved layout and the layout other devices get never change; an icon at the
edge is drawn docked; turning it off brings every card back to the built-in
layout. A renderer whose frames trap is turned off after three failures, its
plugin is marked failed, and the cards are drawn as before.

Drags cannot be scripted: drops go through the renderer unit tests.
"""
import json
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from plugin_isolated import ROOT, Isolated, wait_for  # noqa: E402

BIN = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/debug/super-desktop"
EXAMPLE = ROOT / "skills/super-desktop-plugin/examples/center-magnify"
failures = []


def check(ok, what, detail=""):
    print(("ok   " if ok else "FAIL ") + what + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(what)


def leb(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value == 0:
            out.append(byte)
            return bytes(out)
        out.append(byte | 0x80)


def trapping_renderer():
    """A valid ABI-1 module whose sd_present traps (`unreachable`)."""
    def section(sid, body):
        return bytes([sid]) + leb(len(body)) + body

    def func(body):
        code = bytes([0]) + body + bytes([0x0B])
        return leb(len(code)) + code

    def name(text):
        return leb(len(text)) + text.encode()

    m = b"\0asm\x01\0\0\0"
    m += section(1, bytes([2, 0x60, 0, 1, 0x7F, 0x60, 1, 0x7F, 1, 0x7F]))
    m += section(3, bytes([4, 0, 0, 0, 1]))
    m += section(5, bytes([1, 0, 1]))
    exports = [("memory", 2, 0), ("sd_abi_version", 0, 0), ("sd_input", 0, 1), ("sd_output", 0, 2), ("sd_present", 0, 3)]
    m += section(7, leb(len(exports)) + b"".join(name(n) + bytes([k, i]) for n, k, i in exports))
    m += section(10, bytes([4]) + func(bytes([0x41, 1])) + func(bytes([0x41, 0])) + func(bytes([0x41, 0x80, 0x80, 0x02])) + func(bytes([0x00])))
    return m


def main():
    if not shutil.which("gtk4-broadwayd") or not BIN.exists():
        print("SKIP: needs gtk4-broadwayd and a built binary")
        return 0
    if not (EXAMPLE / "renderer.wasm").exists():
        built = subprocess.run([str(EXAMPLE / "build.sh")], capture_output=True, text=True)
        if built.returncode != 0:
            print("SKIP: center-magnify/renderer.wasm is not built and build.sh failed (needs the wasm32-unknown-unknown target)")
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

    def published():
        """Each card's published layout: what other devices see of its place."""
        snapshot = raw("desktop-workspace")
        return {c["cardId"]: c["layout"] for c in snapshot.get("workspace", {}).get("cards", [])}

    try:
        check(box.start(), "isolated daemon is listening")
        for _ in range(2):
            raw("add-term shell")
        listed = wait_for(lambda: len(cards()[1]) == 2 and cards())
        check(listed is not None, "two shell cards")
        screen, all_cards = listed
        first, second = sorted(all_cards)
        mover = box.copy_plugin(ROOT / "tests/fixtures/plugins/mover", "mover")
        check(box.cli("link", str(mover), "--yes").returncode == 0 and box.cli("activate", "mover").returncode == 0, "Mover on")
        # The second card becomes an icon at the left edge.
        box.cli("run", "mover", "mover.iconify", json.dumps({"card": second, "at": {"x": 10, "y": 300}}))
        check(wait_for(lambda: cards()[1][second]["iconified"]) is not None, "second card is an icon at the left")
        saved_before = {cid: (c["rect"], c["icon"], c["iconified"]) for cid, c in cards()[1].items()}
        published_before = json.dumps(published(), sort_keys=True)
        check(all(c["drawn"] is None for c in cards()[1].values()), "without a renderer cards are drawn where they are saved")

        magnify = box.copy_plugin(EXAMPLE, "center-magnify")
        check(box.cli("link", str(magnify), "--yes").returncode == 0 and box.cli("activate", "center-magnify").returncode == 0, "Center Magnify on")
        drawn = wait_for(lambda: all(c["drawn"] for c in cards()[1].values()) and cards()[1])
        check(drawn is not None, "the renderer draws every card")
        if drawn:
            open_card, icon_card = drawn[first], drawn[second]
            check(not open_card["drawn"]["icon"], "the open card is drawn full", open_card["drawn"])
            check(open_card["drawn"]["w"] != open_card["rect"]["w"], "drawn at another size than saved (magnified or shrunk)", open_card)
            check(icon_card["drawn"]["icon"] and icon_card["drawn"]["x"] < 20, "the edge icon is drawn docked at the left edge", icon_card["drawn"])
        time.sleep(0.5)
        saved_after = {cid: (c["rect"], c["icon"], c["iconified"]) for cid, c in cards()[1].items()}
        check(saved_after == saved_before, "the saved layout is unchanged", (saved_before, saved_after))
        check(json.dumps(published(), sort_keys=True) == published_before and len(published()) == 2, "the layout other devices get is unchanged")
        status = box.status("center-magnify") or {}
        check(status.get("renderer") == "on" and status.get("toolbarPressed") == ["center-magnify.button"], "its toolbar toggle is drawn on", status)

        # The toggle (toolbar button, shortcut or `plugin run`): the plugin stays
        # on, cards go back to the built-in layout, and the choice is remembered.
        check(box.cli("run", "center-magnify", "center-magnify.toggle").returncode == 0, "toggle the effect off")
        check(wait_for(lambda: all(c["drawn"] is None for c in cards()[1].values()), timeout=5) is not None, "cards are back on the built-in layout")
        status = box.status("center-magnify") or {}
        check(status.get("renderer") == "off" and status.get("toolbarPressed") == [] and status.get("state") != "failed", "the plugin is on, its effect off, the button not pressed", status)
        stored = json.loads((box.home / ".config/super-desktop/plugins.json").read_text())
        entry = next(p for p in stored["plugins"] if p["id"] == "center-magnify")
        check(entry.get("rendererOff") is True and entry.get("active") is True, "the choice is saved", entry)
        check(box.cli("reload", "center-magnify").returncode == 0 and (box.status("center-magnify") or {}).get("renderer") == "off", "it stays off after a reload")
        check(box.cli("run", "center-magnify", "center-magnify.toggle").returncode == 0, "toggle the effect on")
        check(wait_for(lambda: all(c["drawn"] for c in cards()[1].values()), timeout=5) is not None, "the renderer draws again")
        check((box.status("center-magnify") or {}).get("toolbarPressed") == ["center-magnify.button"], "the button is pressed again")

        # Settings reach the renderer as params: 50% at the centre instead of 70%
        # (Broadway is 1024 px wide; 40% would be under the smallest card size).
        box.cli("run", "mover", "mover.place", json.dumps({"card": first, "rect": {"x": screen["w"] // 2 - 320, "y": 100, "w": 640, "h": 300}}))
        wide = wait_for(lambda: abs(cards()[1][first]["drawn"]["w"] - 0.70 * screen["w"]) < 3 and cards()[1][first]["drawn"], timeout=5)
        check(wide is not None, "a card at the centre is 70% of the screen wide", cards()[1][first])
        settings = box.home / ".config/super-desktop/plugins/center-magnify/settings.json"
        settings.parent.mkdir(parents=True, exist_ok=True)
        settings.write_text(json.dumps({"maxWidth": 50}))
        box.cli("reload", "center-magnify")
        narrow = wait_for(lambda: abs(cards()[1][first]["drawn"]["w"] - 0.50 * screen["w"]) < 3 and cards()[1][first]["drawn"], timeout=5)
        check(narrow is not None, "with maxWidth 50 it is 50% wide", cards()[1][first])

        # Moving a card while the renderer draws: it redraws around the new spot.
        box.cli("run", "mover", "mover.place", json.dumps({"card": first, "rect": {"x": 10, "y": 100, "w": 640, "h": 480}}))
        moved = wait_for(lambda: cards()[1][first]["rect"]["x"] <= 16 and cards()[1][first]["drawn"]["x"] < 200 and cards()[1][first])
        check(moved is not None, "a moved card is drawn at its new place", cards()[1][first])

        check(box.cli("deactivate", "center-magnify").returncode == 0, "Center Magnify off")
        back = wait_for(lambda: all(c["drawn"] is None for c in cards()[1].values()), timeout=5)
        check(back is not None, "every card is back on the built-in layout")
        check(box.ipc({"op": "footprint", "id": "center-magnify"}).get("footprint") == [], "nothing of Center Magnify is left")

        bad = box.base / "bad-renderer"
        bad.mkdir()
        (bad / "renderer.wasm").write_bytes(trapping_renderer())
        (bad / "super-desktop-plugin.json").write_text(json.dumps({
            "manifestVersion": 1, "id": "bad-renderer", "name": "Bad Renderer", "version": "0.1.0",
            "description": "Every frame traps.", "engines": {"superDesktop": ">=1.0.0", "pluginApi": "1"},
            "permissions": ["layout.renderer"],
            "contributes": {"renderer": {"id": "bad-renderer.r", "wasm": "renderer.wasm"}},
        }))
        check(box.cli("validate", str(bad)).returncode == 0, "the broken renderer still validates (it fails only when it runs)")
        check(box.cli("link", str(bad), "--yes").returncode == 0 and box.cli("activate", "bad-renderer").returncode == 0, "Bad Renderer on")
        # Frames are asked for when something changes: move a card three times.
        for x in (40, 80, 120):
            box.cli("run", "mover", "mover.place", json.dumps({"card": first, "rect": {"x": x, "y": 100, "w": 640, "h": 480}}))
            time.sleep(0.3)
        failed = wait_for(lambda: (box.status("bad-renderer") or {}).get("state") == "failed", timeout=10)
        check(failed is not None, "a renderer that keeps failing is turned off and its plugin marked failed", box.status("bad-renderer"))
        check(all(c["drawn"] is None for c in cards()[1].values()), "cards are drawn by the built-in layout")
        log = (box.home / ".local/state/super-desktop/plugins/bad-renderer/plugin.log").read_text()
        check("3 failures" in log, "the log says why")
    finally:
        if failures:
            print("daemon stderr:\n" + box.daemon_log()[-3000:])
            for plugin in ("center-magnify", "bad-renderer"):
                log = box.home / f".local/state/super-desktop/plugins/{plugin}/plugin.log"
                if log.exists():
                    print(f"{plugin} log:\n" + log.read_text()[-2000:])
        box.close()
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
