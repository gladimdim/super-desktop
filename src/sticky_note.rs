use gtk4::prelude::*;
use gtk4::{
    glib, Align, Button, GestureClick, GestureDrag, Label, Orientation, Overlay, PolicyType,
    ScrolledWindow, TextView, WrapMode,
};
use std::cell::RefCell;
use std::rc::Rc;

use crate::state::NoteData;

pub const MIN_NOTE_WIDTH: i32 = 180;
pub const MIN_NOTE_HEIGHT: i32 = 120;

pub struct StickyNote {
    pub container: Overlay,
    pub data: Rc<RefCell<NoteData>>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub text_view: TextView,
}

/// CSS class on every note's root widget.
pub const NOTE_CLASS: &str = "sticky-note";

/// True while keyboard focus in `widget`'s window is inside a sticky note.
///
/// Terminal cards follow the mouse (hover raises them and takes focus). While
/// a note is being edited they must not: otherwise the pointer drifting over a
/// nearby card steals the keyboard mid-edit, so Ctrl+A, Delete, Backspace and
/// Ctrl+C land in that terminal and a finished mouse selection vanishes under
/// the raised card. Clicking a terminal still switches to it.
pub fn note_has_focus(widget: &impl IsA<gtk4::Widget>) -> bool {
    let Some(root) = widget.as_ref().root() else { return false };
    let mut current = gtk4::prelude::RootExt::focus(&root);
    while let Some(w) = current {
        if w.has_css_class(NOTE_CLASS) {
            return true;
        }
        current = w.parent();
    }
    false
}

impl StickyNote {
    pub fn new<FDragUpdate, FDragEnd, FDelete, FChange, FRaise, FResizeGhost, FResizeEnd>(
        mut note_data: NoteData,
        on_drag_update: FDragUpdate,
        on_drag_end: FDragEnd,
        on_delete: FDelete,
        on_change: FChange,
        on_raise: FRaise,
        on_resize_ghost: FResizeGhost,
        on_resize_end: FResizeEnd,
        screen_w: i32,
        screen_h: i32,
    ) -> Self
    where
        FDragUpdate: Fn(gtk4::Widget, f64, f64) + 'static,
        FDragEnd: Fn(gtk4::Widget, &NoteData) + 'static,
        FDelete: Fn(String) + 'static,
        FChange: Fn(&NoteData) + 'static,
        FRaise: Fn(gtk4::Widget) + 'static,
        FResizeGhost: Fn(f64, f64, i32, i32) + 'static,
        FResizeEnd: Fn() + 'static,
    {
        note_data.width = note_data.width.clamp(
            MIN_NOTE_WIDTH,
            ((screen_w as f64) * 0.70).round() as i32,
        );
        note_data.height = note_data.height.clamp(
            MIN_NOTE_HEIGHT,
            ((screen_h as f64) * 0.75).round() as i32,
        );
        let data = Rc::new(RefCell::new(note_data));
        let on_drag_end = Rc::new(on_drag_end);
        let on_change_rc = Rc::new(on_change);
        let on_raise_rc = Rc::new(on_raise);
        let on_resize_ghost = Rc::new(on_resize_ghost);
        let on_resize_end = Rc::new(on_resize_end);
        let container = Overlay::new();
        let body = gtk4::Box::new(Orientation::Vertical, 0);

        container.set_size_request(data.borrow().width, data.borrow().height);
        container.add_css_class(NOTE_CLASS);
        container.set_child(Some(&body));

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

        // Group color tag dot (click -> 8-color picker, no text)
        let data_tag = Rc::clone(&data);
        let on_change_tag = Rc::clone(&on_change_rc);
        let tag_dot = crate::tag::make_tag_dot(data.borrow().tag, move |next| {
            data_tag.borrow_mut().tag = next;
            on_change_tag(&data_tag.borrow());
        });
        header.append(&tag_dot);

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

        body.append(&header);

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
        let buffer_weak = buffer.downgrade();

        // Perf: never touch buffer contents on the keystroke path itself.
        // Just restart a 300ms trailing timer; the full O(n) text copy
        // happens once the user pauses, inside the timer callback.
        buffer.connect_changed(move |_| {
            if let Some(source) = timer_id.borrow_mut().take() {
                source.remove();
            }

            let data_clone = Rc::clone(&data_text);
            let on_change_clone = Rc::clone(&on_change_text);
            let timer_clone = Rc::clone(&timer_id);
            let buf_weak = buffer_weak.clone();

            let source = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                if let Some(buf) = buf_weak.upgrade() {
                    let text = buf
                        .text(&buf.start_iter(), &buf.end_iter(), false)
                        .to_string();
                    data_clone.borrow_mut().text = text;
                    on_change_clone(&data_clone.borrow());
                }
                *timer_clone.borrow_mut() = None;
                glib::ControlFlow::Break
            });

            *timer_id.borrow_mut() = Some(source);
        });

        scrolled.set_child(Some(&text_view));
        content_box.append(&scrolled);
        // A press on the padding around the text (or below a short note's last
        // line) still means "edit this note": focus the text so keyboard
        // shortcuts go here instead of wherever focus was.
        let focus_click = GestureClick::new();
        let text_focus = text_view.downgrade();
        focus_click.connect_pressed(move |_, _, _, _| {
            if let Some(text) = text_focus.upgrade() {
                if !text.has_focus() {
                    text.grab_focus();
                }
            }
        });
        content_box.add_controller(focus_click);
        body.append(&content_box);

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
        let on_drag_end_move = Rc::clone(&on_drag_end);
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
                on_drag_end_move(c.upcast(), &data_drag_end.borrow());
            }
        });

        header.add_controller(drag);

        let resize_limits = crate::card_resize::Limits {
            min_width: MIN_NOTE_WIDTH,
            min_height: MIN_NOTE_HEIGHT,
            max_width: ((screen_w as f64) * 0.70).round() as i32,
            max_height: ((screen_h as f64) * 0.75).round() as i32,
            left: 10.0,
            top: 70.0,
            right: (screen_w - 10) as f64,
            bottom: (screen_h - 10) as f64,
        };
        let data_start = Rc::clone(&data);
        let get_start: Rc<dyn Fn() -> Option<crate::card_resize::Rect>> = Rc::new(move || {
            let d = data_start.borrow();
            Some(crate::card_resize::Rect {
                x: d.x as f64,
                y: d.y as f64,
                width: d.width,
                height: d.height,
            })
        });
        let root_begin = container.clone();
        let data_begin = Rc::clone(&data);
        let on_raise_resize = Rc::clone(&on_raise_rc);
        let ghost_begin = Rc::clone(&on_resize_ghost);
        let on_begin: Rc<dyn Fn()> = Rc::new(move || {
            root_begin.add_css_class("resizing");
            on_raise_resize(root_begin.clone().upcast());
            let d = data_begin.borrow();
            ghost_begin(d.x as f64, d.y as f64, d.width, d.height);
        });
        let ghost_preview = Rc::clone(&on_resize_ghost);
        let on_preview: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new(move |rect| {
            ghost_preview(rect.x, rect.y, rect.width, rect.height);
        });
        let root_commit = container.clone();
        let data_commit = Rc::clone(&data);
        let on_drag_end_resize = Rc::clone(&on_drag_end);
        let ghost_end = Rc::clone(&on_resize_end);
        let on_commit: Rc<dyn Fn(crate::card_resize::Rect)> = Rc::new(move |rect| {
            ghost_end();
            {
                let mut d = data_commit.borrow_mut();
                d.x = rect.x as i32;
                d.y = rect.y as i32;
                d.width = rect.width;
                d.height = rect.height;
            }
            root_commit.set_size_request(rect.width, rect.height);
            root_commit.remove_css_class("resizing");
            on_drag_end_resize(root_commit.clone().upcast(), &data_commit.borrow());
        });
        crate::card_resize::attach_resize_borders(
            &container,
            resize_limits,
            get_start,
            on_begin,
            on_preview,
            on_commit,
        );

        Self { container, data, text_view }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_note_has_all_eight_resize_targets() {
        crate::gtk_test::run_in_child_process("sticky_note::tests::note_resize_targets_child");
    }

    #[test]
    fn note_resize_targets_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();
        let note = StickyNote::new(
            NoteData {
                id: "test".into(),
                text: "text".into(),
                x: 100,
                y: 100,
                width: 260,
                height: 200,
                color: "omarchy".into(),
                updated_at: 0.0,
                tag: 0,
            },
            |_, _, _| {},
            |_, _| {},
            |_| {},
            |_| {},
            |_| {},
            |_, _, _, _| {},
            || {},
            1920,
            1080,
        );
        assert_eq!(count_class(&note.container, "card-resize-zone"), 8);
    }

    #[test]
    fn test_note_focus_is_detected_for_hover_guard() {
        crate::gtk_test::run_in_child_process("sticky_note::tests::note_focus_child");
    }

    #[test]
    fn note_focus_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();
        let note = StickyNote::new(
            NoteData { id: "focus".into(), text: "select me".into(), x: 0, y: 0, width: 260, height: 200,
                color: "omarchy".into(), updated_at: 0.0, tag: 0 },
            |_, _, _| {}, |_, _| {}, |_| {}, |_| {}, |_| {}, |_, _, _, _| {}, || {}, 1920, 1080,
        );
        let terminal_stand_in = gtk4::Entry::new();
        let layout = gtk4::Box::new(Orientation::Horizontal, 0);
        layout.append(&note.container);
        layout.append(&terminal_stand_in);
        let window = gtk4::Window::new();
        window.set_child(Some(&layout));
        window.present();
        let pump = || while glib::MainContext::default().iteration(false) {};
        pump();
        // GTK focuses the first focusable widget on present; start from none.
        gtk4::prelude::GtkWindowExt::set_focus(&window, None::<&gtk4::Widget>);
        pump();
        assert!(!note_has_focus(&terminal_stand_in), "nothing focused yet");
        note.text_view.grab_focus();
        pump();
        assert!(note_has_focus(&terminal_stand_in), "editing a note must suspend terminal hover focus");
        terminal_stand_in.grab_focus();
        pump();
        assert!(!note_has_focus(&note.container), "hover focus resumes once the note is left");
        window.close();
    }

    fn count_class<W: IsA<gtk4::Widget>>(root: &W, class: &str) -> usize {
        let widget = root.as_ref();
        let mut count = usize::from(widget.has_css_class(class));
        let mut child = widget.first_child();
        while let Some(current) = child {
            count += count_class(&current, class);
            child = current.next_sibling();
        }
        count
    }
}
