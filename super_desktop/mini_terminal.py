#!/usr/bin/env python3
"""
Mini Terminal Card Widget for SUPER DESKTOP.
Displays a compact (~280x185px) live preview card representing an AI Agent or Shell session.
Shows agent icon, real-time status (active/thinking/idle), mini console preview, and opens fullscreen on double click.
"""

from typing import Any, Callable, Dict
from gi.repository import Gtk, GLib

from .tmux_manager import AGENT_CONFIGS, TmuxManager


class MiniTerminalCard(Gtk.Box):
    """A compact, draggable terminal card representing a background tmux session."""

    def __init__(
        self,
        term_data: Dict[str, Any],
        tmux_mgr: TmuxManager,
        on_drag_update: Callable[["MiniTerminalCard", float, float], None],
        on_drag_end: Callable[["MiniTerminalCard"], None],
        on_double_click: Callable[["MiniTerminalCard"], None],
        on_close: Callable[["MiniTerminalCard"], None],
    ):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=0)
        self.term_data = term_data
        self.tmux_mgr = tmux_mgr
        self.on_drag_update_cb = on_drag_update
        self.on_drag_end_cb = on_drag_end
        self.on_double_click_cb = on_double_click
        self.on_close_cb = on_close

        self.term_id: str = term_data.get("id", "")
        self.session_name: str = term_data.get("session_name", "")
        self.agent_type: str = term_data.get("agent_type", "shell")
        self.x: int = int(term_data.get("x", 400))
        self.y: int = int(term_data.get("y", 140))
        self.width: int = 290
        self.height: int = 185

        self._drag_start_x = 0.0
        self._drag_start_y = 0.0

        self.set_size_request(self.width, self.height)
        self.add_css_class("mini-terminal")
        self.add_css_class(f"agent-card-{self.agent_type}")

        self._build_ui()
        self.refresh_state()

    def _build_ui(self) -> None:
        cfg = AGENT_CONFIGS.get(self.agent_type, AGENT_CONFIGS["shell"])

        # Header Bar
        self.header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.header.add_css_class("term-header")

        # Grip
        grip = Gtk.Label(label="⋮⋮")
        grip.add_css_class("note-header-grip")
        self.header.append(grip)

        # Agent Icon + Name
        agent_label_text = f"{cfg['icon']} {cfg['name']}"
        self.title_label = Gtk.Label(label=agent_label_text)
        self.title_label.add_css_class("term-title")
        self.header.append(self.title_label)

        # Status badge
        self.status_badge = Gtk.Label(label="● ACTIVE")
        self.status_badge.add_css_class("term-status-badge")
        self.status_badge.add_css_class("status-active")
        self.status_badge.set_hexpand(True)
        self.status_badge.set_halign(Gtk.Align.END)
        self.header.append(self.status_badge)

        # Fullscreen expand button
        self.expand_btn = Gtk.Button(label="⛶")
        self.expand_btn.set_tooltip_text("Open Fullscreen (or Double-Click)")
        self.expand_btn.add_css_class("term-btn")
        self.expand_btn.connect("clicked", lambda _: self.on_double_click_cb(self))
        self.header.append(self.expand_btn)

        # Close/Kill button
        self.kill_btn = Gtk.Button(label="✕")
        self.kill_btn.set_tooltip_text("Kill Session")
        self.kill_btn.add_css_class("term-btn")
        self.kill_btn.connect("clicked", lambda _: self.on_close_cb(self))
        self.header.append(self.kill_btn)

        self.append(self.header)

        # Mini Console Preview Box
        self.preview_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.preview_box.add_css_class("term-preview-box")
        self.preview_box.set_vexpand(True)

        self.preview_label = Gtk.Label(label="Connecting to session...")
        self.preview_label.set_wrap(True)
        self.preview_label.set_wrap_mode(Gtk.WrapMode.CHAR)
        self.preview_label.set_halign(Gtk.Align.START)
        self.preview_label.set_valign(Gtk.Align.START)
        self.preview_label.set_vexpand(True)
        self.preview_label.set_hexpand(True)
        self.preview_label.add_css_class("term-preview-text")
        self.preview_box.append(self.preview_label)

        self.append(self.preview_box)

        # Footer
        footer = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        footer.add_css_class("term-footer")

        self.meta_label = Gtk.Label(label="PID: - • Foot/Tmux")
        self.meta_label.add_css_class("term-meta")
        self.meta_label.set_halign(Gtk.Align.START)
        footer.append(self.meta_label)

        hint_label = Gtk.Label(label="Double-click to expand")
        hint_label.add_css_class("term-hint")
        hint_label.set_hexpand(True)
        hint_label.set_halign(Gtk.Align.END)
        footer.append(hint_label)

        self.append(footer)

        # Attach Drag Gesture to header
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._on_drag_begin)
        drag.connect("drag-update", self._on_drag_update)
        drag.connect("drag-end", self._on_drag_end)
        self.header.add_controller(drag)

        # Attach Click Gesture to entire card for double-click
        click = Gtk.GestureClick()
        click.connect("released", self._on_card_click)
        self.add_controller(click)

    def _on_card_click(self, gesture: Gtk.GestureClick, n_press: int, _x: float, _y: float) -> None:
        if n_press == 2:
            self.on_double_click_cb(self)

    def _on_drag_begin(self, _gesture: Gtk.GestureDrag, _start_x: float, _start_y: float) -> None:
        self._drag_start_x = float(self.x)
        self._drag_start_y = float(self.y)

    def _on_drag_update(self, _gesture: Gtk.GestureDrag, offset_x: float, offset_y: float) -> None:
        new_x = self._drag_start_x + offset_x
        new_y = self._drag_start_y + offset_y
        self.on_drag_update_cb(self, new_x, new_y)

    def _on_drag_end(self, _gesture: Gtk.GestureDrag, offset_x: float, offset_y: float) -> None:
        self.x = int(self._drag_start_x + offset_x)
        self.y = int(self._drag_start_y + offset_y)
        self.term_data["x"] = self.x
        self.term_data["y"] = self.y
        self.on_drag_end_cb(self)

    def refresh_state(self) -> None:
        """Query tmux session for status and preview update."""
        if not self.session_name:
            return

        status_info = self.tmux_mgr.inspect_status(self.session_name, self.agent_type)
        status_text = status_info.get("status", "ACTIVE")
        status_label = status_info.get("label", "Active")
        pid = status_info.get("pid", "-")
        cmd = status_info.get("cmd", self.agent_type)

        # Update badge
        for cls in ["status-active", "status-busy", "status-exited"]:
            self.status_badge.remove_css_class(cls)

        if status_text == "BUSY":
            self.status_badge.add_css_class("status-busy")
            self.status_badge.set_label("● WORKING")
        elif status_text == "EXITED":
            self.status_badge.add_css_class("status-exited")
            self.status_badge.set_label("○ EXITED")
        else:
            self.status_badge.add_css_class("status-active")
            self.status_badge.set_label("● ACTIVE")

        # Update preview
        preview_text = self.tmux_mgr.get_preview(self.session_name, max_lines=6)
        self.preview_label.set_label(preview_text)

        # Update footer
        self.meta_label.set_label(f"PID: {pid} • {cmd[:18]}")
