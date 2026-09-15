use gtk4::prelude::*;
use gtk4::{Align, Button, GestureClick, GestureDrag, Label, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

use crate::state::TerminalData;
use crate::tmux::{get_agent_config, get_preview, inspect_status};

pub struct MiniTerminalCard {
    pub container: gtk4::Box,
    pub data: Rc<RefCell<TerminalData>>,
    status_badge: Label,
    preview_label: Label,
    meta_label: Label,
}

impl MiniTerminalCard {
    pub fn new<FDragUpdate, FDragEnd, FDoubleClick, FClose>(
        term_data: TerminalData,
        on_drag_update: FDragUpdate,
        on_drag_end: FDragEnd,
        on_double_click: FDoubleClick,
        on_close: FClose,
    ) -> Self
    where
        FDragUpdate: Fn(gtk4::Widget, f64, f64) + 'static,
        FDragEnd: Fn(gtk4::Widget, &TerminalData) + 'static,
        FDoubleClick: Fn(&TerminalData) + 'static,
        FClose: Fn(String) + 'static,
    {
        let data = Rc::new(RefCell::new(term_data));
        let container = gtk4::Box::new(Orientation::Vertical, 0);

        let agent_type = data.borrow().agent_type.clone();
        let cfg = get_agent_config(&agent_type);

        container.set_size_request(290, 185);
        container.add_css_class("mini-terminal");
        container.add_css_class(&format!("agent-card-{}", agent_type));

        // Header
        let header = gtk4::Box::new(Orientation::Horizontal, 6);
        header.add_css_class("term-header");

        let grip = Label::new(Some("⋮⋮"));
        grip.add_css_class("note-header-grip");
        header.append(&grip);

        let title = Label::new(Some(&format!("{} {}", cfg.icon, cfg.name)));
        title.add_css_class("term-title");
        header.append(&title);

        let status_badge = Label::new(Some("● ACTIVE"));
        status_badge.add_css_class("term-status-badge");
        status_badge.add_css_class("status-active");
        status_badge.set_hexpand(true);
        status_badge.set_halign(Align::End);
        header.append(&status_badge);

        // Expand button
        let expand_btn = Button::with_label("⛶");
        expand_btn.set_tooltip_text(Some("Open Fullscreen foot"));
        expand_btn.add_css_class("term-btn");

        let on_double_click_rc = Rc::new(on_double_click);
        let on_expand_click = Rc::clone(&on_double_click_rc);
        let data_expand = Rc::clone(&data);
        expand_btn.connect_clicked(move |_| {
            on_expand_click(&data_expand.borrow());
        });
        header.append(&expand_btn);

        // Kill button
        let kill_btn = Button::with_label("✕");
        kill_btn.set_tooltip_text(Some("Kill Session"));
        kill_btn.add_css_class("term-btn");

        let sess_name = data.borrow().session_name.clone();
        kill_btn.connect_clicked(move |_| {
            on_close(sess_name.clone());
        });
        header.append(&kill_btn);

        container.append(&header);

        // Preview box
        let preview_box = gtk4::Box::new(Orientation::Vertical, 0);
        preview_box.add_css_class("term-preview-box");
        preview_box.set_vexpand(true);

        let preview_label = Label::new(Some("Connecting to session..."));
        preview_label.set_wrap(true);
        preview_label.set_wrap_mode(gtk4::pango::WrapMode::Char);
        preview_label.set_halign(Align::Start);
        preview_label.set_valign(Align::Start);
        preview_label.set_vexpand(true);
        preview_label.set_hexpand(true);
        preview_label.add_css_class("term-preview-text");
        preview_box.append(&preview_label);

        container.append(&preview_box);

        // Footer
        let footer = gtk4::Box::new(Orientation::Horizontal, 6);
        footer.add_css_class("term-footer");

        let meta_label = Label::new(Some("PID: - • Foot/Tmux"));
        meta_label.add_css_class("term-meta");
        meta_label.set_halign(Align::Start);
        footer.append(&meta_label);

        let hint_label = Label::new(Some("Double-click to expand"));
        hint_label.add_css_class("term-hint");
        hint_label.set_hexpand(true);
        hint_label.set_halign(Align::End);
        footer.append(&hint_label);

        container.append(&footer);

        // Double click on card
        let click = GestureClick::new();
        let data_click = Rc::clone(&data);
        let on_double_click_card = Rc::clone(&on_double_click_rc);
        click.connect_released(move |_, n_press, _, _| {
            if n_press == 2 {
                on_double_click_card(&data_click.borrow());
            }
        });
        container.add_controller(click);

        // Drag on header
        let drag = GestureDrag::new();
        let start_pos = Rc::new(RefCell::new((0.0, 0.0)));

        let data_drag = Rc::clone(&data);
        let start_pos_begin = Rc::clone(&start_pos);
        drag.connect_drag_begin(move |_, _, _| {
            *start_pos_begin.borrow_mut() = (data_drag.borrow().x as f64, data_drag.borrow().y as f64);
        });

        let container_weak = container.downgrade();
        let start_pos_update = Rc::clone(&start_pos);
        drag.connect_drag_update(move |_, offset_x, offset_y| {
            if let Some(c) = container_weak.upgrade() {
                let (sx, sy) = *start_pos_update.borrow();
                on_drag_update(c.upcast(), sx + offset_x, sy + offset_y);
            }
        });

        let container_weak = container.downgrade();
        let data_drag_end = Rc::clone(&data);
        let start_pos_end = Rc::clone(&start_pos);
        drag.connect_drag_end(move |_, offset_x, offset_y| {
            if let Some(c) = container_weak.upgrade() {
                let (sx, sy) = *start_pos_end.borrow();
                let nx = (sx + offset_x) as i32;
                let ny = (sy + offset_y) as i32;
                data_drag_end.borrow_mut().x = nx;
                data_drag_end.borrow_mut().y = ny;
                on_drag_end(c.upcast(), &data_drag_end.borrow());
            }
        });

        header.add_controller(drag);

        let card = Self {
            container,
            data,
            status_badge,
            preview_label,
            meta_label,
        };

        card.refresh_status();
        card
    }

    pub fn refresh_status(&self) {
        let sess_name = self.data.borrow().session_name.clone();
        let agent_type = self.data.borrow().agent_type.clone();

        let status_info = inspect_status(&sess_name, &agent_type);

        for cls in &["status-active", "status-busy", "status-exited"] {
            self.status_badge.remove_css_class(cls);
        }

        match status_info.status {
            "BUSY" => {
                self.status_badge.add_css_class("status-busy");
                self.status_badge.set_label("● WORKING");
            }
            "EXITED" => {
                self.status_badge.add_css_class("status-exited");
                self.status_badge.set_label("○ EXITED");
            }
            _ => {
                self.status_badge.add_css_class("status-active");
                self.status_badge.set_label("● ACTIVE");
            }
        }

        let preview = get_preview(&sess_name, 6);
        self.preview_label.set_label(&preview);

        self.meta_label.set_label(&format!("PID: {} • {}", status_info.pid, status_info.cmd));
    }
}
