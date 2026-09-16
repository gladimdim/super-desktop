use gtk4::prelude::*;
use gtk4::{glib, Align, Button, GestureClick, GestureDrag, Label, Orientation, PolicyType, ScrolledWindow, TextView, WrapMode};
use std::cell::RefCell;
use std::rc::Rc;

use crate::state::NoteData;

pub struct StickyNote {
    pub container: gtk4::Box,
    pub data: Rc<RefCell<NoteData>>,
}

impl StickyNote {
    pub fn new<FDragUpdate, FDragEnd, FDelete, FChange, FRaise>(
        note_data: NoteData,
        on_drag_update: FDragUpdate,
        on_drag_end: FDragEnd,
        on_delete: FDelete,
        on_change: FChange,
        on_raise: FRaise,
    ) -> Self
    where
        FDragUpdate: Fn(gtk4::Widget, f64, f64) + 'static,
        FDragEnd: Fn(gtk4::Widget, &NoteData) + 'static,
        FDelete: Fn(String) + 'static,
        FChange: Fn(&NoteData) + 'static,
        FRaise: Fn(gtk4::Widget) + 'static,
    {
        let data = Rc::new(RefCell::new(note_data));
        let on_drag_end = Rc::new(on_drag_end);
        let on_change_rc = Rc::new(on_change);
        let on_raise_rc = Rc::new(on_raise);
        let container = gtk4::Box::new(Orientation::Vertical, 0);

        container.set_size_request(data.borrow().width, data.borrow().height);
        container.add_css_class("sticky-note");

        // Click to raise note above all other widgets
        let click = GestureClick::new();
        click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let container_weak_click = container.downgrade();
        let on_raise_click = Rc::clone(&on_raise_rc);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(c) = container_weak_click.upgrade() {
                on_raise_click(c.upcast());
            }
        });
        container.add_controller(click);

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
        let grab_offset: Rc<RefCell<Option<(f64, f64)>>> = Rc::new(RefCell::new(None));

        let data_drag = Rc::clone(&data);
        let start_pos_begin = Rc::clone(&start_pos);
        let grab_offset_begin = Rc::clone(&grab_offset);
        let container_weak_drag = container.downgrade();
        let on_raise_drag = Rc::clone(&on_raise_rc);
        drag.connect_drag_begin(move |gesture, _, _| {
            if let Some(c) = container_weak_drag.upgrade() {
                on_raise_drag(c.upcast());
            }
            let dx = data_drag.borrow().x as f64;
            let dy = data_drag.borrow().y as f64;
            *start_pos_begin.borrow_mut() = (dx, dy);
            *grab_offset_begin.borrow_mut() = gesture
                .current_event()
                .and_then(|e| e.position())
                .map(|(mx, my)| (mx - dx, my - dy));
        });

        let container_weak = container.downgrade();
        let start_pos_update = Rc::clone(&start_pos);
        let grab_offset_update = Rc::clone(&grab_offset);
        let data_drag_update = Rc::clone(&data);
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            if offset_x.abs() > 2.0 || offset_y.abs() > 2.0 {
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
            if let Some(c) = container_weak.upgrade() {
                let (nx, ny) = match (
                    *grab_offset_update.borrow(),
                    gesture.current_event().and_then(|e| e.position()),
                ) {
                    (Some((gx, gy)), Some((mx, my))) => (mx - gx, my - gy),
                    _ => {
                        let (sx, sy) = *start_pos_update.borrow();
                        (sx + offset_x, sy + offset_y)
                    }
                };
                data_drag_update.borrow_mut().x = nx.round() as i32;
                data_drag_update.borrow_mut().y = ny.round() as i32;
                on_drag_update(c.upcast(), nx, ny);
            }
        });

        let container_weak = container.downgrade();
        let data_drag_end = Rc::clone(&data);
        let start_pos_end = Rc::clone(&start_pos);
        let grab_offset_end = Rc::clone(&grab_offset);
        let on_drag_end = Rc::clone(&on_drag_end);
        drag.connect_drag_end(move |gesture, offset_x, offset_y| {
            if let Some(c) = container_weak.upgrade() {
                let (nx, ny) = match (
                    *grab_offset_end.borrow(),
                    gesture.current_event().and_then(|e| e.position()),
                ) {
                    (Some((gx, gy)), Some((mx, my))) => (mx - gx, my - gy),
                    _ => {
                        let (sx, sy) = *start_pos_end.borrow();
                        (sx + offset_x, sy + offset_y)
                    }
                };
                let rx = nx.round() as i32;
                let ry = ny.round() as i32;
                data_drag_end.borrow_mut().x = rx;
                data_drag_end.borrow_mut().y = ry;
                on_drag_end(c.upcast(), &data_drag_end.borrow());
            }
        });

        header.add_controller(drag);

        Self { container, data }
    }
}
