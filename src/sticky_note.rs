use gtk4::prelude::*;
use gtk4::{glib, Align, Button, GestureDrag, Label, Orientation, PolicyType, ScrolledWindow, TextView, WrapMode};
use std::cell::RefCell;
use std::rc::Rc;

use crate::state::NoteData;

pub const COLOR_PALETTE: &[&str] = &["yellow", "mint", "sky", "rose", "purple", "dark"];

pub struct StickyNote {
    pub container: gtk4::Box,
    pub data: Rc<RefCell<NoteData>>,
}

impl StickyNote {
    pub fn new<FDragUpdate, FDragEnd, FDelete, FChange>(
        note_data: NoteData,
        on_drag_update: FDragUpdate,
        on_drag_end: FDragEnd,
        on_delete: FDelete,
        on_change: FChange,
    ) -> Self
    where
        FDragUpdate: Fn(gtk4::Widget, f64, f64) + 'static,
        FDragEnd: Fn(gtk4::Widget, &NoteData) + 'static,
        FDelete: Fn(String) + 'static,
        FChange: Fn(&NoteData) + 'static,
    {
        let data = Rc::new(RefCell::new(note_data));
        let container = gtk4::Box::new(Orientation::Vertical, 0);

        let initial_color = data.borrow().color.clone();
        container.set_size_request(data.borrow().width, data.borrow().height);
        container.add_css_class("sticky-note");
        container.add_css_class(&format!("note-{}", initial_color));

        // Header
        let header = gtk4::Box::new(Orientation::Horizontal, 6);
        header.add_css_class("note-header");

        let grip = Label::new(Some("⋮⋮"));
        grip.add_css_class("note-header-grip");
        header.append(&grip);

        let title = Label::new(Some("Note"));
        title.set_hexpand(true);
        title.set_halign(Align::Start);
        title.add_css_class("note-header-title");
        header.append(&title);

        // Color button
        let color_btn = Button::with_label("🎨");
        color_btn.set_tooltip_text(Some("Change Color"));
        color_btn.add_css_class("note-header-btn");

        let container_weak = container.downgrade();
        let data_color = Rc::clone(&data);
        let on_change_rc = Rc::new(on_change);
        let on_change_color = Rc::clone(&on_change_rc);

        color_btn.connect_clicked(move |_| {
            if let Some(c) = container_weak.upgrade() {
                let current_color = data_color.borrow().color.clone();
                let next_idx = (COLOR_PALETTE.iter().position(|&col| col == current_color).unwrap_or(0) + 1)
                    % COLOR_PALETTE.len();
                let next_color = COLOR_PALETTE[next_idx];

                c.remove_css_class(&format!("note-{}", current_color));
                c.add_css_class(&format!("note-{}", next_color));

                data_color.borrow_mut().color = next_color.to_string();
                on_change_color(&data_color.borrow());
            }
        });
        header.append(&color_btn);

        // Delete button
        let delete_btn = Button::with_label("✕");
        delete_btn.set_tooltip_text(Some("Delete Note"));
        delete_btn.add_css_class("note-header-btn");

        let note_id = data.borrow().id.clone();
        delete_btn.connect_clicked(move |_| {
            on_delete(note_id.clone());
        });
        header.append(&delete_btn);

        container.append(&header);

        // Content
        let content_box = gtk4::Box::new(Orientation::Vertical, 0);
        content_box.add_css_class("note-content-area");
        content_box.set_vexpand(true);

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(PolicyType::Automatic, PolicyType::Automatic);
        scrolled.set_vexpand(true);

        let text_view = TextView::new();
        text_view.set_wrap_mode(WrapMode::WordChar);
        text_view.add_css_class("note-textview");
        text_view.set_vexpand(true);

        let buffer = text_view.buffer();
        buffer.set_text(&data.borrow().text);

        let data_text = Rc::clone(&data);
        let on_change_text = Rc::clone(&on_change_rc);
        let timer_id: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

        buffer.connect_changed(move |buf| {
            if let Some(source) = timer_id.borrow_mut().take() {
                source.remove();
            }

            let start = buf.start_iter();
            let end = buf.end_iter();
            let text = buf.text(&start, &end, false).to_string();

            let data_clone = Rc::clone(&data_text);
            let on_change_clone = Rc::clone(&on_change_text);
            let timer_clone = Rc::clone(&timer_id);

            let source = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                data_clone.borrow_mut().text = text.clone();
                on_change_clone(&data_clone.borrow());
                *timer_clone.borrow_mut() = None;
                glib::ControlFlow::Break
            });

            *timer_id.borrow_mut() = Some(source);
        });

        scrolled.set_child(Some(&text_view));
        content_box.append(&scrolled);
        container.append(&content_box);

        // Drag gesture
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

        Self { container, data }
    }
}
