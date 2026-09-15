#!/usr/bin/env python3
"""
Entry point and CLI controller for SUPER DESKTOP.
Implements Unix domain socket IPC for instantaneous toggling via Hyprland shortcuts (SUPER + SHIFT + Q).
"""

import os
import sys

# Ensure LD_PRELOAD is active before GTK is imported
LAYER_SHELL_LIB = "/usr/lib/libgtk4-layer-shell.so"
if os.path.exists(LAYER_SHELL_LIB) and LAYER_SHELL_LIB not in os.environ.get("LD_PRELOAD", ""):
    new_env = dict(os.environ)
    new_env["LD_PRELOAD"] = f"{LAYER_SHELL_LIB}:{os.environ.get('LD_PRELOAD', '')}".strip(":")
    os.execve(sys.executable, [sys.executable] + sys.argv, new_env)

import argparse
import json
import socket
import threading
import time
from typing import Optional

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Gtk4LayerShell", "1.0")
from gi.repository import Gtk, GLib

from .styles import apply_styles
from .window import SuperDesktopWindow


def get_socket_path() -> str:
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    return os.path.join(runtime_dir, "super-desktop.sock")


# ================= IPC Client ================= #

def send_ipc_command(cmd: str, timeout: float = 1.0) -> Optional[str]:
    """Send a command string to running super-desktop daemon."""
    sock_path = get_socket_path()
    if not os.path.exists(sock_path):
        return None

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout)
            s.connect(sock_path)
            s.sendall((cmd.strip() + "\n").encode("utf-8"))
            resp = s.recv(4096).decode("utf-8").strip()
            return resp
    except (socket.error, socket.timeout):
        return None


# ================= Daemon Application ================= #

class SuperDesktopApp(Gtk.Application):
    """The main GTK Application holding the SuperDesktopWindow and IPC server."""

    def __init__(self, start_visible: bool = False):
        super().__init__(
            application_id="org.omarchy.superdesktop",
            flags=0,
        )
        self.start_visible = start_visible
        self.window: Optional[SuperDesktopWindow] = None
        self.sock_path = get_socket_path()
        self._server_sock: Optional[socket.socket] = None
        self._running = True
        self._last_toggle_time: float = 0.0

    def do_activate(self) -> None:
        # Keep daemon alive even when overlay window is closed
        self.hold()
        apply_styles()
        self._start_ipc_server()

        if self.start_visible:
            self.show_overlay()

    def show_overlay(self) -> None:
        """Create and display overlay window."""
        if self.window is not None:
            return

        self.window = SuperDesktopWindow(self, on_request_close=self.hide_overlay)
        self.window.present()
        self.window.start_slide_in_animation()

    def hide_overlay(self) -> None:
        """Animate out and destroy overlay window to free compositor layer."""
        if self.window is None:
            return

        win_to_close = self.window

        def _on_finish():
            if self.window == win_to_close:
                self.window.close()
                self.window = None

        win_to_close.start_slide_out_animation(on_finish=_on_finish)

    def toggle_overlay(self) -> bool:
        """Toggle overlay between visible and hidden with debounce protection."""
        now = time.time()
        if now - self._last_toggle_time < 0.45:  # 450ms debounce
            return self.window is not None
        self._last_toggle_time = now

        if self.window is not None:
            self.hide_overlay()
            return False
        else:
            self.show_overlay()
            return True

    def _start_ipc_server(self) -> None:
        """Start background Unix socket listener thread."""
        if os.path.exists(self.sock_path):
            try:
                os.unlink(self.sock_path)
            except OSError:
                pass

        self._server_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server_sock.bind(self.sock_path)
        self._server_sock.listen(5)

        thread = threading.Thread(target=self._ipc_listener_loop, daemon=True)
        thread.start()

    def _ipc_listener_loop(self) -> None:
        while self._running:
            try:
                conn, _ = self._server_sock.accept()
                with conn:
                    data = conn.recv(2048).decode("utf-8").strip()
                    if not data:
                        continue

                    response_container = []
                    event = threading.Event()

                    def handle_in_gtk(cmd_str=data):
                        resp = self._handle_command(cmd_str)
                        response_container.append(resp)
                        event.set()
                        return False

                    GLib.idle_add(handle_in_gtk)
                    event.wait(timeout=2.0)

                    resp_str = response_container[0] if response_container else '{"error": "timeout"}'
                    conn.sendall((resp_str + "\n").encode("utf-8"))
            except Exception:
                if not self._running:
                    break

    def _handle_command(self, cmd_line: str) -> str:
        parts = cmd_line.split()
        cmd = parts[0].lower() if parts else "toggle"

        if cmd == "toggle":
            is_vis = self.toggle_overlay()
            return json.dumps({"ok": True, "visible": is_vis})
        elif cmd == "show":
            self.show_overlay()
            return json.dumps({"ok": True, "visible": True})
        elif cmd == "hide":
            self.hide_overlay()
            return json.dumps({"ok": True, "visible": False})
        elif cmd == "status":
            is_vis = self.window is not None
            notes_count = len(self.window.note_widgets) if self.window else 0
            terminals_count = len(self.window.terminal_widgets) if self.window else 0
            terms = [
                {"id": t.term_id, "agent": t.agent_type, "session": t.session_name}
                for t in (self.window.terminal_widgets if self.window else [])
            ]
            return json.dumps({
                "ok": True,
                "visible": is_vis,
                "notes_count": notes_count,
                "terminals_count": terminals_count,
                "terminals": terms,
            })
        elif cmd == "add-note":
            self.show_overlay()
            text = " ".join(parts[1:]) if len(parts) > 1 else "New note..."
            note = self.window.create_new_note(text=text)
            return json.dumps({"ok": True, "note_id": note.note_id})
        elif cmd == "add-term":
            self.show_overlay()
            agent = parts[1] if len(parts) > 1 else "shell"
            term = self.window.create_new_terminal(agent_type=agent)
            return json.dumps({"ok": True, "terminal_id": term.term_id, "session": term.session_name})
        elif cmd in ("quit", "kill"):
            self.release()
            GLib.idle_add(lambda: self.quit())
            return json.dumps({"ok": True, "action": "quitting"})

        return json.dumps({"ok": False, "error": f"unknown_command: {cmd}"})

    def do_shutdown(self) -> None:
        self._running = False
        if self._server_sock:
            try:
                self._server_sock.close()
            except Exception:
                pass
        if os.path.exists(self.sock_path):
            try:
                os.unlink(self.sock_path)
            except Exception:
                pass
        Gtk.Application.do_shutdown(self)


# ================= CLI Dispatcher ================= #

def main() -> None:
    parser = argparse.ArgumentParser(description="SUPER DESKTOP - Sticky Notes and AI Terminals for Hyprland/Omarchy")
    parser.add_argument("action", nargs="?", default="toggle", choices=[
        "toggle", "show", "hide", "status", "start", "daemon", "add-note", "add-term", "kill"
    ], help="Action to perform (default: toggle)")
    parser.add_argument("--text", type=str, default="", help="Text for new note")
    parser.add_argument("--agent", type=str, default="shell", help="Agent type for new terminal")

    args = parser.parse_args()

    if args.action in ("daemon", "start"):
        app = SuperDesktopApp(start_visible=(args.action == "start"))
        app.run([])
        return

    # Try sending to running daemon
    cmd_to_send = args.action
    if args.action == "add-note" and args.text:
        cmd_to_send = f"add-note {args.text}"
    elif args.action == "add-term":
        cmd_to_send = f"add-term {args.agent}"

    resp = send_ipc_command(cmd_to_send)
    if resp is not None:
        try:
            parsed = json.loads(resp)
            if args.action == "status":
                print(f"SUPER DESKTOP Status: {'Visible' if parsed.get('visible') else 'Hidden'}")
                print(f"Notes: {parsed.get('notes_count', 0)}, Terminals: {parsed.get('terminals_count', 0)}")
                for t in parsed.get("terminals", []):
                    print(f"  • {t.get('agent', 'terminal')} ({t.get('session')})")
            elif args.action == "toggle":
                print(f"SUPER DESKTOP: {'Shown' if parsed.get('visible') else 'Hidden'}")
            else:
                print(f"SUPER DESKTOP: {resp}")
        except Exception:
            print(resp)
        return

    # Daemon not running
    if args.action in ("toggle", "show"):
        import subprocess
        script_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        bin_script = os.path.join(script_dir, "bin", "super-desktop")

        subprocess.Popen(
            [bin_script, "daemon"],
            cwd=script_dir,
            start_new_session=True,
        )

        for _ in range(30):
            time.sleep(0.08)
            resp = send_ipc_command("show")
            if resp:
                print("SUPER DESKTOP: Started and Shown")
                return

        print("Error: Failed to launch SUPER DESKTOP daemon", file=sys.stderr)
        sys.exit(1)
    elif args.action == "status":
        print("SUPER DESKTOP: Daemon not running.")
    elif args.action == "hide":
        print("SUPER DESKTOP: Daemon not running.")


if __name__ == "__main__":
    main()
