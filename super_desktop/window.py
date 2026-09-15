#!/usr/bin/env python3
"""
Main Application Window for SUPER DESKTOP.
Implements the Wayland LayerShell overlay, HUD command bar, slide-in/out animations,
canvas event handling, and sticky notes / terminal lifecycle.
"""

import time
from typing import Callable, Dict, List, Optional
from gi.repository import Gtk, Gtk4LayerShell, GLib, Gdk

from .styles import apply_styles
from .state_manager import StateManager
from .tmux_manager import TmuxManager, AGENT_CONFIGS
from .sticky_note import StickyNote
from .mini_terminal import MiniTerminalCard


class SuperDesktopWindow(Gtk.ApplicationWindow):
    """The fullscreen overlay desktop for notes and mini-terminals."""

    def __init__(self, app: Gtk.Application, on_request_close: Optional[Callable[[], None]] = None):
        super().__init__(application=app)
        self.on_request_close_cb = on_request_close
        self.state_mgr = StateManager()
        self.tmux_mgr = TmuxManager()

        self.note_widgets: List[StickyNote] = []
        self.terminal_widgets: List[MiniTerminalCard] = []

        self._animating: bool = False
        self._anim_start_time: float = 0.0
        self._anim_duration: float = 0.26  # 260ms
        self._anim_mode: str = "in"  # "in" or "out"
        self._anim_on_finish: Optional[Callable[[], None]] = None
        self._anim_trajectories: Dict[Gtk.Widget, Dict[str, float]] = {}

        self.screen_width: int = 2560
        self.screen_height: int = 1600

        # Frame-tick synchronized drag tracking (prevents event flood at 1000Hz mouse polling)
        self._drag_pending: Dict[Gtk.Widget, tuple] = {}
        self._drag_tick_active: bool = False

        self._init_layer_shell()
        self._init_ui()
        self._load_saved_items()

        # Periodic status refresh
        self._periodic_source_id = GLib.timeout_add(1500, self._on_periodic_tick)

    def _init_layer_shell(self) -> None:
        """Configure GTK4 Layer Shell for full-screen overlay."""
        Gtk4LayerShell.init_for_window(self)
        Gtk4LayerShell.set_layer(self, Gtk4LayerShell.Layer.OVERLAY)
        Gtk4LayerShell.set_namespace(self, "super-desktop")

        for edge in (
            Gtk4LayerShell.Edge.TOP,
            Gtk4LayerShell.Edge.BOTTOM,
            Gtk4LayerShell.Edge.LEFT,
            Gtk4LayerShell.Edge.RIGHT,
        ):
            Gtk4LayerShell.set_anchor(self, edge, True)

        Gtk4LayerShell.set_keyboard_mode(self, Gtk4LayerShell.KeyboardMode.ON_DEMAND)
        self.add_css_class("super-desktop-window")

        # Detect monitor resolution
        display = Gdk.Display.get_default()
        if display:
            monitors = display.get_monitors()
            if monitors and monitors.get_n_items() > 0:
                geo = monitors.get_item(0).get_geometry()
                self.screen_width = max(geo.width, 1920)
                self.screen_height = max(geo.height, 1080)

    def _init_ui(self) -> None:
        """Construct canvas, HUD bar, and event controllers."""
        self.root_overlay = Gtk.Overlay()

        # Fixed Canvas
        self.canvas = Gtk.Fixed()
        self.canvas.set_hexpand(True)
        self.canvas.set_vexpand(True)
        self.root_overlay.set_child(self.canvas)

        # Double click on background creates new sticky note
        backdrop_click = Gtk.GestureClick()
        backdrop_click.connect("released", self._on_backdrop_click)
        self.canvas.add_controller(backdrop_click)

        # Key controller
        key_ctrl = Gtk.EventControllerKey()
        key_ctrl.connect("key-pressed", self._on_key_pressed)
        self.add_controller(key_ctrl)

        # HUD Bar
        self.hud_box = self._build_hud_bar()
        self.hud_box.set_halign(Gtk.Align.CENTER)
        self.hud_box.set_valign(Gtk.Align.START)
        self.hud_box.set_margin_top(18)
        self.root_overlay.add_overlay(self.hud_box)

        self.set_child(self.root_overlay)

    def _build_hud_bar(self) -> Gtk.Box:
        hud = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=10)
        hud.add_css_class("hud-bar")

        brand_label = Gtk.Label(label="⚡ SUPER DESKTOP")
        brand_label.add_css_class("hud-title")
        hud.append(brand_label)

        self.hud_badge = Gtk.Label(label="0 Notes • 0 Agents")
        self.hud_badge.add_css_class("hud-badge")
        hud.append(self.hud_badge)

        sep1 = Gtk.Separator(orientation=Gtk.Orientation.VERTICAL)
        hud.append(sep1)

        btn_note = Gtk.Button(label="📝 + Note")
        btn_note.set_tooltip_text("Create a new Sticky Note (or double-click background)")
        btn_note.add_css_class("hud-button")
        btn_note.connect("clicked", lambda _: self.create_new_note())
        hud.append(btn_note)

        agents_to_offer = [
            ("antigravity", "🌌 Antigravity"),
            ("claude", "⚡ Claude"),
            ("codex", "🤖 Codex"),
            ("opencode", "🔮 OpenCode"),
            ("grok", "🚀 Grok"),
            ("shell", "💻 Shell"),
        ]

        for agent_key, agent_name in agents_to_offer:
            btn = Gtk.Button(label=agent_name)
            btn.add_css_class("hud-button")
            btn.set_tooltip_text(f"Launch {agent_name} in background tmux session")
            btn.connect("clicked", lambda _, a=agent_key: self.create_new_terminal(a))
            hud.append(btn)

        sep2 = Gtk.Separator(orientation=Gtk.Orientation.VERTICAL)
        hud.append(sep2)

        btn_arrange = Gtk.Button(label="✨ Arrange")
        btn_arrange.set_tooltip_text("Organize notes on left, terminals on right")
        btn_arrange.add_css_class("hud-button")
        btn_arrange.connect("clicked", lambda _: self.auto_arrange_items())
        hud.append(btn_arrange)

        btn_close = Gtk.Button(label="✕ Hide")
        btn_close.set_tooltip_text("Hide Super Desktop [SUPER + SHIFT + Q or Esc]")
        btn_close.add_css_class("hud-button")
        btn_close.add_css_class("hud-button-danger")
        btn_close.connect("clicked", self._trigger_close)
        hud.append(btn_close)

        lbl_hint = Gtk.Label(label="[SUPER + SHIFT + Q]")
        lbl_hint.add_css_class("hud-shortcut")
        hud.append(lbl_hint)

        return hud

    def _trigger_close(self, _btn=None) -> None:
        if self.on_request_close_cb:
            self.on_request_close_cb()
        else:
            self.close()

    # ================= Item Management ================= #

    def _load_saved_items(self) -> None:
        for note_data in self.state_mgr.get_notes():
            self._add_note_widget(note_data, save=False)

        saved_terms = self.state_mgr.get_terminals()
        active_tmux_sessions = set(self.tmux_mgr.list_sd_sessions())

        for term_data in saved_terms:
            sess = term_data.get("session_name")
            if sess in active_tmux_sessions:
                self._add_terminal_widget(term_data, save=False)
            else:
                self._add_terminal_widget(term_data, save=False)

        self._update_counts()

    def create_new_note(self, x: Optional[int] = None, y: Optional[int] = None, text: str = "") -> StickyNote:
        if x is None or y is None:
            idx = len(self.note_widgets)
            x = 80 + (idx % 4) * 50
            y = 140 + (idx % 3) * 60

        note_data = {
            "id": f"note_{int(time.time() * 1000)}",
            "text": text or "New sticky note...",
            "x": int(x),
            "y": int(y),
            "width": 260,
            "height": 200,
            "color": "yellow",
            "updated_at": time.time(),
        }
        widget = self._add_note_widget(note_data, save=True)
        self._update_counts()
        return widget

    def _add_note_widget(self, note_data: Dict, save: bool = True) -> StickyNote:
        note = StickyNote(
            note_data=note_data,
            on_drag_update=self._on_item_drag_update,
            on_drag_end=self._on_item_drag_end,
            on_delete=self._on_note_delete,
            on_change=self._on_note_change,
        )
        self.note_widgets.append(note)
        self.canvas.put(note, note.x, note.y)
        if save:
            self.state_mgr.upsert_note(note_data)
        return note

    def create_new_terminal(
        self,
        agent_type: str = "shell",
        custom_command: Optional[str] = None,
        x: Optional[int] = None,
        y: Optional[int] = None,
    ) -> MiniTerminalCard:
        session_name, cmd_run = self.tmux_mgr.create_session(
            agent_type=agent_type,
            custom_command=custom_command,
        )

        if x is None or y is None:
            idx = len(self.terminal_widgets)
            x = max(400, self.screen_width - 340 - (idx % 3) * 310)
            y = 140 + (idx % 3) * 200

        term_data = {
            "id": session_name,
            "session_name": session_name,
            "agent_type": agent_type,
            "command": cmd_run,
            "x": int(x),
            "y": int(y),
            "created_at": time.time(),
        }

        widget = self._add_terminal_widget(term_data, save=True)
        self._update_counts()
        return widget

    def _add_terminal_widget(self, term_data: Dict, save: bool = True) -> MiniTerminalCard:
        term = MiniTerminalCard(
            term_data=term_data,
            tmux_mgr=self.tmux_mgr,
            on_drag_update=self._on_item_drag_update,
            on_drag_end=self._on_item_drag_end,
            on_double_click=self._on_terminal_double_click,
            on_close=self._on_terminal_close,
        )
        self.terminal_widgets.append(term)
        self.canvas.put(term, term.x, term.y)
        if save:
            self.state_mgr.upsert_terminal(term_data)
        return term

    # ================= Drag & State Updates ================= #

    def _on_item_drag_update(self, widget: Gtk.Widget, new_x: float, new_y: float) -> None:
        clamped_x = max(10.0, min(float(self.screen_width - 80), new_x))
        clamped_y = max(70.0, min(float(self.screen_height - 60), new_y))
        self._drag_pending[widget] = (clamped_x, clamped_y)
        if not self._drag_tick_active:
            self._drag_tick_active = True
            self.canvas.add_tick_callback(self._on_drag_tick)

    def _on_drag_tick(self, widget: Gtk.Widget, frame_clock: Gdk.FrameClock) -> bool:
        if not self._drag_pending:
            self._drag_tick_active = False
            return False
        pending = list(self._drag_pending.items())
        self._drag_pending.clear()
        for w, (px, py) in pending:
            self.canvas.move(w, px, py)
        return True

    def _on_item_drag_end(self, widget: Gtk.Widget) -> None:
        # Flush any pending drag position immediately
        if widget in self._drag_pending:
            px, py = self._drag_pending.pop(widget)
            self.canvas.move(widget, px, py)

        if isinstance(widget, StickyNote):
            self.state_mgr.upsert_note(widget.note_data)
        elif isinstance(widget, MiniTerminalCard):
            self.state_mgr.upsert_terminal(widget.term_data)

    def _on_note_change(self, widget: StickyNote) -> None:
        self.state_mgr.upsert_note(widget.note_data)

    def _on_note_delete(self, widget: StickyNote) -> None:
        if widget in self.note_widgets:
            self.note_widgets.remove(widget)
            self.canvas.remove(widget)
            self.state_mgr.remove_note(widget.note_id)
            self._update_counts()

    def _on_terminal_close(self, widget: MiniTerminalCard) -> None:
        if widget in self.terminal_widgets:
            self.terminal_widgets.remove(widget)
            self.canvas.remove(widget)
            self.tmux_mgr.kill_session(widget.session_name)
            self.state_mgr.remove_terminal(widget.term_id)
            self._update_counts()

    def _on_terminal_double_click(self, widget: MiniTerminalCard) -> None:
        cfg = AGENT_CONFIGS.get(widget.agent_type, AGENT_CONFIGS["shell"])
        title = f"{cfg['name']} ({widget.session_name})"
        self.tmux_mgr.launch_fullscreen(widget.session_name, title=title)
        self._trigger_close()

    def _on_backdrop_click(self, gesture: Gtk.GestureClick, n_press: int, x: float, y: float) -> None:
        if n_press == 2:
            self.create_new_note(x=int(x), y=int(y))

    def _on_key_pressed(
        self,
        controller: Gtk.EventControllerKey,
        keyval: int,
        keycode: int,
        state: Gdk.ModifierType,
    ) -> bool:
        if keyval == Gdk.KEY_Escape:
            self._trigger_close()
            return True
        elif (state & Gdk.ModifierType.CONTROL_MASK) and keyval in (Gdk.KEY_n, Gdk.KEY_N):
            self.create_new_note()
            return True
        elif (state & Gdk.ModifierType.CONTROL_MASK) and keyval in (Gdk.KEY_t, Gdk.KEY_T):
            self.create_new_terminal("shell")
            return True
        return False

    def _update_counts(self) -> None:
        n_notes = len(self.note_widgets)
        n_terms = len(self.terminal_widgets)
        self.hud_badge.set_label(f"{n_notes} Notes • {n_terms} Terminals")

    def _on_periodic_tick(self) -> bool:
        for term in self.terminal_widgets:
            try:
                term.refresh_state()
            except Exception:
                pass
        self._update_counts()
        return True

    # ================= Layout / Auto Arrange ================= #

    def auto_arrange_items(self) -> None:
        start_y = 110
        gap = 20

        col_x = 60
        curr_y = start_y
        for note in self.note_widgets:
            if curr_y + note.height > self.screen_height - 60:
                col_x += note.width + gap
                curr_y = start_y
            note.x = col_x
            note.y = curr_y
            note.note_data["x"] = col_x
            note.note_data["y"] = curr_y
            self.canvas.move(note, col_x, curr_y)
            self.state_mgr.upsert_note(note.note_data)
            curr_y += note.height + gap

        col_x = self.screen_width - 320
        curr_y = start_y
        for term in self.terminal_widgets:
            if curr_y + term.height > self.screen_height - 60:
                col_x -= term.width + gap
                curr_y = start_y
            term.x = col_x
            term.y = curr_y
            term.term_data["x"] = col_x
            term.term_data["y"] = curr_y
            self.canvas.move(term, col_x, curr_y)
            self.state_mgr.upsert_terminal(term.term_data)
            curr_y += term.height + gap

    # ================= Slide-In / Out Animations ================= #

    def _calculate_entry_vectors(self) -> None:
        self._anim_trajectories.clear()
        all_items: List[Gtk.Widget] = list(self.note_widgets) + list(self.terminal_widgets)

        for item in all_items:
            tx = float(item.x)
            ty = float(item.y)
            w = float(item.width)
            h = float(item.height)

            d_left = tx
            d_right = self.screen_width - (tx + w)
            d_top = ty
            d_bottom = self.screen_height - (ty + h)

            min_d = min(d_left, d_right, d_top, d_bottom)
            if min_d == d_left:
                sx = -w - 40.0
                sy = ty
            elif min_d == d_right:
                sx = self.screen_width + 40.0
                sy = ty
            elif min_d == d_top:
                sx = tx
                sy = -h - 40.0
            else:
                sx = tx
                sy = self.screen_height + 40.0

            self._anim_trajectories[item] = {
                "sx": sx,
                "sy": sy,
                "tx": tx,
                "ty": ty,
            }

    def start_slide_in_animation(self) -> None:
        self._calculate_entry_vectors()
        for item, coords in self._anim_trajectories.items():
            self.canvas.move(item, coords["sx"], coords["sy"])

        self._anim_mode = "in"
        self._anim_start_time = time.time()
        self._anim_duration = 0.26
        self._animating = True
        self.canvas.add_tick_callback(self._on_animation_tick)

    def start_slide_out_animation(self, on_finish: Callable[[], None]) -> None:
        if not self._anim_trajectories:
            self._calculate_entry_vectors()

        self._anim_mode = "out"
        self._anim_start_time = time.time()
        self._anim_duration = 0.14
        self._anim_on_finish = on_finish
        self._animating = True
        self.canvas.add_tick_callback(self._on_animation_tick)

    def _on_animation_tick(self, widget: Gtk.Widget, frame_clock: Gdk.FrameClock) -> bool:
        if not self._animating:
            return False

        elapsed = time.time() - self._anim_start_time
        progress = min(1.0, elapsed / self._anim_duration)

        if self._anim_mode == "in":
            factor = 1.0 - pow(1.0 - progress, 3)
            for item, c in self._anim_trajectories.items():
                cx = c["sx"] + (c["tx"] - c["sx"]) * factor
                cy = c["sy"] + (c["ty"] - c["sy"]) * factor
                self.canvas.move(item, cx, cy)

            if progress >= 1.0:
                self._animating = False
                for item, c in self._anim_trajectories.items():
                    self.canvas.move(item, c["tx"], c["ty"])
                return False
        else:
            factor = pow(progress, 2)
            for item, c in self._anim_trajectories.items():
                cx = c["tx"] + (c["sx"] - c["tx"]) * factor
                cy = c["ty"] + (c["sy"] - c["ty"]) * factor
                self.canvas.move(item, cx, cy)

            if progress >= 1.0:
                self._animating = False
                if self._anim_on_finish:
                    self._anim_on_finish()
                return False

        return True

    def close(self) -> None:
        if hasattr(self, "_periodic_source_id") and self._periodic_source_id:
            GLib.source_remove(self._periodic_source_id)
            self._periodic_source_id = None
        super().close()
