#!/usr/bin/env python3
"""
State Manager for SUPER DESKTOP.
Handles JSON persistence of sticky notes, mini-terminal cards, positions, colors, and content.
Stored in ~/.config/super-desktop/state.json with atomic writes.
"""

import json
import os
import tempfile
import time
from typing import Any, Dict, List


CONFIG_DIR = os.path.expanduser("~/.config/super-desktop")
STATE_FILE = os.path.join(CONFIG_DIR, "state.json")


DEFAULT_WELCOME_NOTE = {
    "id": "welcome_note",
    "text": "✨ Welcome to SUPER DESKTOP!\n\n• Shortcut: SUPER + SHIFT + Q to show / hide.\n• Drag: Grab any header to reposition.\n• Double-click background to create a new note.\n• Double-click terminal cards to open in fullscreen!\n• Click '+' buttons in top bar to spawn AI agents.",
    "x": 80,
    "y": 140,
    "width": 300,
    "height": 220,
    "color": "yellow",
    "updated_at": time.time(),
}


class StateManager:
    """Manages persistent application state."""

    def __init__(self, state_path: str = STATE_FILE):
        self.state_path = state_path
        self.config_dir = os.path.dirname(state_path)
        os.makedirs(self.config_dir, exist_ok=True)
        self.data: Dict[str, Any] = self.load()
        self.ensure_saved()

    def load(self) -> Dict[str, Any]:
        """Load state from JSON file or return defaults."""
        if os.path.exists(self.state_path):
            try:
                with open(self.state_path, "r", encoding="utf-8") as f:
                    data = json.load(f)
                    if isinstance(data, dict):
                        return {
                            "notes": data.get("notes", []),
                            "terminals": data.get("terminals", []),
                        }
            except Exception as e:
                print(f"[SUPER DESKTOP] Warning: failed to parse {self.state_path}: {e}")

        # Fresh default setup
        initial = {
            "notes": [DEFAULT_WELCOME_NOTE.copy()],
            "terminals": [],
        }
        return initial

    def ensure_saved(self) -> None:
        """Ensure state file exists on disk."""
        if not os.path.exists(self.state_path):
            self.save()

    def save(self) -> bool:
        """Atomically persist state to disk."""
        try:
            temp_fd, temp_path = tempfile.mkstemp(
                prefix="sd_state_",
                dir=self.config_dir,
                text=True,
            )
            with os.fdopen(temp_fd, "w", encoding="utf-8") as f:
                json.dump(self.data, f, indent=2, ensure_ascii=False)
            os.replace(temp_path, self.state_path)
            return True
        except Exception as e:
            print(f"[SUPER DESKTOP] Error saving state: {e}")
            return False

    # ------------------ Notes ------------------ #

    def get_notes(self) -> List[Dict[str, Any]]:
        return self.data.setdefault("notes", [])

    def upsert_note(self, note: Dict[str, Any]) -> None:
        notes = self.get_notes()
        for idx, item in enumerate(notes):
            if item.get("id") == note.get("id"):
                notes[idx] = note
                self.save()
                return
        notes.append(note)
        self.save()

    def remove_note(self, note_id: str) -> None:
        notes = self.get_notes()
        self.data["notes"] = [n for n in notes if n.get("id") != note_id]
        self.save()

    # ---------------- Terminals ---------------- #

    def get_terminals(self) -> List[Dict[str, Any]]:
        return self.data.setdefault("terminals", [])

    def upsert_terminal(self, term: Dict[str, Any]) -> None:
        terminals = self.get_terminals()
        for idx, item in enumerate(terminals):
            if item.get("id") == term.get("id") or item.get("session_name") == term.get("session_name"):
                terminals[idx] = term
                self.save()
                return
        terminals.append(term)
        self.save()

    def remove_terminal(self, term_id: str) -> None:
        terminals = self.get_terminals()
        self.data["terminals"] = [
            t for t in terminals
            if t.get("id") != term_id and t.get("session_name") != term_id
        ]
        self.save()
