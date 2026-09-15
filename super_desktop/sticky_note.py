#!/usr/bin/env python3
"""
Sticky Note Widget for SUPER DESKTOP.
Provides editable, draggable, color-customizable sticky notes with auto-saving.
"""

import time
from typing import Any, Callable, Dict, Optional
from gi.repository import Gtk, GLib, Gdk


COLOR_PALETTE = ["yellow", "mint", "sky", "rose", "purple", "dark"]


class StickyNote(Gtk.Box):
    """A floating, draggable, editable sticky note card."""

    def __init__(
        self,
        note_data: Dict[str, Any],
        on_drag_update: Callable[["StickyNote", float, float], None],
        on_drag_end: Callable[["StickyNote"], None],
        on_delete: Callable[["StickyNote"], None],
        on_change: Callable[["StickyNote"], None],
    ):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=0)
        self.note_data = note_data
        self.on_drag_update_cb = on_drag_update
        self.on_drag_end_cb = on_drag_end
        self.on_delete_cb = on_delete
        self.on_change_cb = on_change

        self.note_id: str = note_data.get("id", str(time.time()))
        self.x: int = int(note_data.get("x", 100))
        self.y: int = int(note_data.get("y", 100))
        self.width: int = int(note_data.get("width", 260))
        self.height: int = int(note_data.get("height", 200))
        self.color: str = note_data.get("color", "yellow")
        if self.color not in COLOR_PALETTE:
            self.color = "yellow"

        self._save_timer_id: Optional[int] = None
        self._drag_start_x = 0.0
        self._drag_start_y = 0.0

        self.set_size_request(self.width, self.height)
        self.add_css_class("sticky-note")
        self.add_css_class(f"note-{self.color}")

        self._build_ui()

    def _build_ui(self) -> None:
        # Header / Drag Handle
        self.header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.header.add_css_class("note-header")

        # Grip & Title
        grip_label = Gtk.Label(label="⋮⋮")
        grip_label.add_css_class("note-header-grip")
        self.header.append(grip_label)

        title_label = Gtk.Label(label="Note")
        title_label.set_hexpand(True)
        title_label.set_halign(Gtk.Align.START)
        title_label.add_css_class("note-header-title")
        self.header.append(title_label)

        # Color cycle button
        self.color_btn = Gtk.Button(label="🎨")
        self.color_btn.set_tooltip_text("Change Color")
        self.color_btn.add_css_class("note-header-btn")
        self.color_btn.connect("clicked", self._on_cycle_color)
        self.header.append(self.color_btn)

        # Delete button
        self.delete_btn = Gtk.Button(label="✕")
        self.delete_btn.set_tooltip_text("Delete Note")
        self.delete_btn.add_css_class("note-header-btn")
        self.delete_btn.connect("clicked", lambda _: self.on_delete_cb(self))
        self.header.append(self.delete_btn)

        self.append(self.header)

        # Content Area (TextView)
        content_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        content_box.add_css_class("note-content-area")
        content_box.set_vexpand(True)

        scrolled = Gtk.ScrolledWindow()
        scrolled.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_vexpand(True)

        self.text_view = Gtk.TextView()
        self.text_view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        self.text_view.add_css_class("note-textview")
        self.text_view.set_vexpand(True)

        buffer = self.text_view.get_buffer()
        buffer.set_text(self.note_data.get("text", ""))
        buffer.connect("changed", self._on_text_changed)

        scrolled.set_child(self.text_view)
        content_box.append(scrolled)
        self.append(content_box)

        # Attach Drag Gesture to header
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._on_drag_begin)
        drag.connect("drag-update", self._on_drag_update)
        drag.connect("drag-end", self._on_drag_end)
        self.header.add_controller(drag)

    def _on_cycle_color(self, _btn: Gtk.Button) -> None:
        idx = (COLOR_PALETTE.index(self.color) + 1) % len(COLOR_PALETTE)
        old_color = self.color
        self.color = COLOR_PALETTE[idx]
        self.remove_css_class(f"note-{old_color}")
        self.add_css_class(f"note-{self.color}")
        self.note_data["color"] = self.color
        self.on_change_cb(self)

    def _on_text_changed(self, buffer: Gtk.TextBuffer) -> None:
        # Debounce auto-save
        if self._save_timer_id is not None:
            GLib.source_remove(self._save_timer_id)

        def _do_save():
            start_iter = buffer.get_start_iter()
            end_iter = buffer.get_end_iter()
            text = buffer.get_text(start_iter, end_iter, False)
            self.note_data["text"] = text
            self.note_data["updated_at"] = time.time()
            self.on_change_cb(self)
            self._save_timer_id = None
            return False

        self._save_timer_id = GLib.timeout_add(300, _do_save)

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
        self.note_data["x"] = self.x
        self.note_data["y"] = self.y
        self.on_drag_end_cb(self)
