#!/usr/bin/env python3
"""
Tmux Manager for SUPER DESKTOP.
Manages background tmux sessions for AI agents (Antigravity, Claude, Codex, OpenCode, Grok) and Shells.
Provides status inspection, output capture for mini-window previews, and fullscreen attachment via foot.
"""

import os
import shutil
import subprocess
import time
from typing import Dict, List, Optional, Tuple


AGENT_CONFIGS = {
    "antigravity": {
        "name": "Antigravity",
        "icon": "🌌",
        "color": "#6366F1",  # Indigo
        "badge_class": "agent-antigravity",
        "commands": ["agy", "antigravity"],
        "default_title": "Antigravity CLI",
    },
    "claude": {
        "name": "Claude Code",
        "icon": "⚡",
        "color": "#F97316",  # Amber/Orange
        "badge_class": "agent-claude",
        "commands": ["claude"],
        "default_title": "Claude Code",
    },
    "codex": {
        "name": "OpenAI Codex",
        "icon": "🤖",
        "color": "#10B981",  # Emerald
        "badge_class": "agent-codex",
        "commands": ["codex"],
        "default_title": "Codex CLI",
    },
    "opencode": {
        "name": "OpenCode",
        "icon": "🔮",
        "color": "#06B6D4",  # Cyan
        "badge_class": "agent-opencode",
        "commands": ["opencode"],
        "default_title": "OpenCode",
    },
    "grok": {
        "name": "Grok CLI",
        "icon": "🚀",
        "color": "#EC4899",  # Neon Pink
        "badge_class": "agent-grok",
        "commands": ["grok"],
        "default_title": "Grok",
    },
    "aider": {
        "name": "Aider",
        "icon": "🧠",
        "color": "#8B5CF6",  # Violet
        "badge_class": "agent-aider",
        "commands": ["aider"],
        "default_title": "Aider Chat",
    },
    "shell": {
        "name": "Terminal",
        "icon": "💻",
        "color": "#64748B",  # Slate
        "badge_class": "agent-shell",
        "commands": [os.environ.get("SHELL", "/bin/bash"), "bash"],
        "default_title": "Interactive Shell",
    },
}


def find_agent_binary(agent_type: str) -> Optional[str]:
    """Resolve executable path for a given agent type."""
    cfg = AGENT_CONFIGS.get(agent_type)
    if not cfg:
        return shutil.which(agent_type)
    for cmd in cfg["commands"]:
        resolved = shutil.which(cmd)
        if resolved:
            return resolved
    # Fallback to shell if agent binary is missing
    return shutil.which("bash")


def detect_agent_from_command(cmdline: str) -> str:
    """Infer agent type from command line string."""
    lower = cmdline.lower()
    if "antigravity" in lower or "agy" in lower:
        return "antigravity"
    elif "claude" in lower:
        return "claude"
    elif "codex" in lower:
        return "codex"
    elif "opencode" in lower:
        return "opencode"
    elif "grok" in lower:
        return "grok"
    elif "aider" in lower:
        return "aider"
    return "shell"


class TmuxManager:
    """Handles creating, monitoring, and attaching to tmux sessions."""

    def __init__(self):
        self.ensure_tmux_available()

    @staticmethod
    def ensure_tmux_available() -> bool:
        return shutil.which("tmux") is not None

    def list_sd_sessions(self) -> List[str]:
        """List all existing tmux sessions managed by super-desktop."""
        try:
            res = subprocess.run(
                ["tmux", "list-sessions", "-F", "#{session_name}"],
                capture_output=True,
                text=True,
                check=False,
            )
            if res.returncode != 0:
                return []
            sessions = []
            for line in res.stdout.splitlines():
                name = line.strip()
                if name.startswith("sd_term_"):
                    sessions.append(name)
            return sessions
        except Exception:
            return []

    def create_session(
        self,
        agent_type: str = "shell",
        custom_command: Optional[str] = None,
        cwd: Optional[str] = None,
    ) -> Tuple[str, str]:
        """
        Creates a new detached tmux session for the requested agent or command.
        Returns (session_name, command_run).
        """
        session_id = f"sd_term_{int(time.time() * 1000) % 1000000}"
        working_dir = cwd or os.path.expanduser("~")

        if custom_command:
            cmd = custom_command
        else:
            bin_path = find_agent_binary(agent_type)
            if bin_path:
                cmd = bin_path
            else:
                cmd = os.environ.get("SHELL", "/bin/bash")

        # Launch session in background with standard geometry
        subprocess.run(
            [
                "tmux",
                "new-session",
                "-d",
                "-s",
                session_id,
                "-c",
                working_dir,
                "-x",
                "120",
                "-y",
                "35",
                cmd,
            ],
            check=False,
        )

        return session_id, cmd

    def kill_session(self, session_name: str) -> bool:
        """Kill a tmux session."""
        try:
            res = subprocess.run(
                ["tmux", "kill-session", "-t", session_name],
                capture_output=True,
                check=False,
            )
            return res.returncode == 0
        except Exception:
            return False

    def get_pane_info(self, session_name: str) -> Dict[str, str]:
        """Fetch PID, current command, title, and alive state for session."""
        try:
            res = subprocess.run(
                [
                    "tmux",
                    "list-panes",
                    "-t",
                    session_name,
                    "-F",
                    "#{pane_pid}::#{pane_current_command}::#{pane_title}::#{pane_dead}",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            if res.returncode != 0 or not res.stdout.strip():
                return {"pid": "", "command": "", "title": "", "dead": "1"}

            parts = res.stdout.strip().splitlines()[0].split("::")
            return {
                "pid": parts[0] if len(parts) > 0 else "",
                "command": parts[1] if len(parts) > 1 else "",
                "title": parts[2] if len(parts) > 2 else "",
                "dead": parts[3] if len(parts) > 3 else "0",
            }
        except Exception:
            return {"pid": "", "command": "", "title": "", "dead": "1"}

    def get_preview(self, session_name: str, max_lines: int = 8) -> str:
        """Capture the most recent lines of terminal output."""
        try:
            res = subprocess.run(
                ["tmux", "capture-pane", "-p", "-t", session_name, "-S", f"-{max_lines * 3}"],
                capture_output=True,
                text=True,
                check=False,
            )
            if res.returncode != 0:
                return "Session offline or ended."

            raw_lines = res.stdout.splitlines()
            # Trim trailing empty lines
            while raw_lines and not raw_lines[-1].strip():
                raw_lines.pop()

            if not raw_lines:
                return "Ready. Waiting for input..."

            snippet = raw_lines[-max_lines:]
            return "\n".join(snippet)
        except Exception as e:
            return f"Preview unavailable ({e})"

    def inspect_status(self, session_name: str, agent_type: str) -> Dict[str, str]:
        """
        Inspect the session to determine AI state:
        Status: 'ACTIVE', 'BUSY' (running/thinking), 'IDLE', 'EXITED'
        """
        info = self.get_pane_info(session_name)
        if info["dead"] == "1" or not info["pid"]:
            return {
                "status": "EXITED",
                "label": "Ended",
                "badge": "status-exited",
                "dot_color": "#94A3B8",
                "pid": info.get("pid", "-"),
                "cmd": info.get("command", "-"),
            }

        pid_str = info["pid"]
        child_cmd = self._get_active_child_cmd(pid_str)
        command_name = child_cmd or info.get("command", "")

        # Inspect recent output text to detect prompt vs busy thinking
        preview = self.get_preview(session_name, max_lines=4).lower()

        is_busy = False
        if any(term in preview for term in ["thinking", "generating", "working", "building", "running", "streaming"]):
            is_busy = True
        elif any(term in command_name.lower() for term in ["python", "node", "cargo", "go", "make", "git"]):
            is_busy = True

        if is_busy:
            return {
                "status": "BUSY",
                "label": "Thinking / Working",
                "badge": "status-busy",
                "dot_color": "#FACC15",  # Yellow pulse
                "pid": pid_str,
                "cmd": command_name,
            }

        return {
            "status": "ACTIVE",
            "label": "Active",
            "badge": "status-active",
            "dot_color": "#22C55E",  # Green
            "pid": pid_str,
            "cmd": command_name or agent_type,
        }

    def _get_active_child_cmd(self, parent_pid: str) -> Optional[str]:
        """Check if pane has active child process (e.g. claude under bash)."""
        try:
            pid = int(parent_pid)
            children_file = f"/proc/{pid}/task/{pid}/children"
            if os.path.exists(children_file):
                with open(children_file, "r") as f:
                    children = f.read().split()
                if children:
                    last_child = children[-1]
                    cmd_path = f"/proc/{last_child}/cmdline"
                    if os.path.exists(cmd_path):
                        with open(cmd_path, "rb") as f:
                            raw = f.read().decode(errors="ignore").replace("\x00", " ").strip()
                            if raw:
                                return os.path.basename(raw.split()[0])
        except Exception:
            pass
        return None

    def launch_fullscreen(self, session_name: str, title: str = "SUPER DESKTOP") -> bool:
        """
        Launch foot terminal in fullscreen attached to the tmux session.
        When user detaches or exits foot, the session remains safely in tmux.
        """
        foot_bin = shutil.which("foot")
        if not foot_bin:
            foot_bin = "foot"

        try:
            subprocess.Popen(
                [
                    foot_bin,
                    "-F",
                    "-a",
                    "super-desktop-terminal",
                    "-T",
                    f"SUPER DESKTOP - {title}",
                    "tmux",
                    "attach-session",
                    "-t",
                    session_name,
                ],
                start_new_session=True,
            )
            return True
        except Exception:
            return False
