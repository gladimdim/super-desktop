//! Paired-PC selector and read-only remote workspace preview.
//! Workers own only network data; generation checks guard every GTK update.
use crate::{
    desktop_protocol::{MachineSelection, WorkspaceSnapshot},
    peer_client,
    peer_store::PeerStore,
    remote_workspace::{self, Selection},
};
use gtk4::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

pub struct MachineView {
    pub stack: gtk4::Stack,
    pub local_button: gtk4::MenuButton,
    remote_button: gtk4::MenuButton,
    remote: gtk4::Box,
    selection: RefCell<Selection>,
    snapshot: RefCell<Option<WorkspaceSnapshot>>,
    drawing: gtk4::DrawingArea,
    status: gtk4::Label,
    details: gtk4::Label,
    busy: Cell<bool>,
    on_switch: Rc<dyn Fn()>,
}
impl MachineView {
    pub fn new(local: &gtk4::Fixed, on_switch: Rc<dyn Fn()>, on_hide: Rc<dyn Fn()>) -> Rc<Self> {
        let stack = gtk4::Stack::new();
        stack.set_transition_type(gtk4::StackTransitionType::None);
        stack.add_named(local, Some("local"));
        let remote = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
        toolbar.add_css_class("hud-bar");
        let local_button = gtk4::MenuButton::new();
        local_button.set_label("This PC");
        local_button.add_css_class("machine-selector");
        let remote_button = gtk4::MenuButton::new();
        remote_button.add_css_class("machine-selector");
        toolbar.append(&remote_button);
        let status = gtk4::Label::new(Some("Connecting…"));
        status.set_hexpand(true);
        status.set_xalign(0.0);
        toolbar.append(&status);
        let hide = gtk4::Button::with_label("✕ Hide");
        hide.add_css_class("hud-button");
        hide.add_css_class("hud-button-danger");
        hide.connect_clicked(move |_| on_hide());
        toolbar.append(&hide);
        remote.append(&toolbar);
        let notice = gtk4::Label::new(Some(
            "Remote layout preview · Console input and remote controls are not available yet",
        ));
        notice.set_wrap(true);
        remote.append(&notice);
        let details = gtk4::Label::new(None);
        details.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        details.set_margin_start(16);
        details.set_margin_end(16);
        remote.append(&details);
        let drawing = gtk4::DrawingArea::new();
        drawing.set_hexpand(true);
        drawing.set_vexpand(true);
        drawing.set_tooltip_text(Some("Host card positions and sizes, scaled to fit. This preview refreshes every two seconds."));
        remote.append(&drawing);
        stack.add_named(&remote, Some("remote"));
        stack.set_visible_child_name("local");
        let view = Rc::new(Self {
            stack,
            local_button,
            remote_button,
            remote,
            selection: RefCell::new(Selection::default()),
            snapshot: RefCell::new(None),
            drawing,
            status,
            details,
            busy: Cell::new(false),
            on_switch,
        });
        for button in [&view.local_button, &view.remote_button] {
            let popover = gtk4::Popover::new();
            popover.add_css_class("ws-pop");
            popover.set_has_arrow(false);
            popover.set_offset(0, 6);
            button.set_popover(Some(&popover));
            let weak = Rc::downgrade(&view);
            popover.connect_show(move |popover| {
                if let Some(view) = weak.upgrade() {
                    view.populate(popover);
                }
            });
        }
        let weak = Rc::downgrade(&view);
        view.drawing.set_draw_func(move |_, cr, width, height| {
            if let Some(view) = weak.upgrade() {
                view.draw(cr, width, height);
            }
        });
        let weak = Rc::downgrade(&view);
        view.remote.connect_map(move |_| {
            if let Some(view) = weak.upgrade() {
                view.refresh();
            }
        });
        let weak = Rc::downgrade(&view);
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if view.remote.is_mapped() {
                view.refresh();
            }
            glib::ControlFlow::Continue
        });
        view
    }
    pub fn is_remote(&self) -> bool {
        self.selection.borrow().request().is_some()
    }
    pub fn dismiss(&self) {
        self.local_button.popdown();
        self.remote_button.popdown();
    }
    fn populate(self: &Rc<Self>, popover: &gtk4::Popover) {
        let list = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        list.add_css_class("ws-pop-box");
        let local = gtk4::Button::with_label("This PC");
        style_peer_button(&local, !self.is_remote());
        let weak = Rc::downgrade(self);
        let pop = popover.downgrade();
        local.connect_clicked(move |_| {
            if let Some(pop) = pop.upgrade() {
                pop.popdown();
            }
            if let Some(view) = weak.upgrade() {
                view.select(None);
            }
        });
        list.append(&local);
        let loading = gtk4::Label::new(Some("Loading paired PCs…"));
        loading.add_css_class("ws-row-path");
        list.append(&loading);
        popover.set_child(Some(&list));
        let weak = Rc::downgrade(self);
        let pop = popover.downgrade();
        // Registry I/O stays off the GTK thread, just like TLS requests.
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(|| {
                PeerStore::default_store()?
                    .peers()
                    .map(|peers| peers.into_iter().map(|p| p.summary()).collect::<Vec<_>>())
            })
            .await;
            let (Some(view), Some(pop)) = (weak.upgrade(), pop.upgrade()) else {
                return;
            };
            // A reopened menu owns a new list; never write into its replacement.
            if pop.child().as_ref() != Some(list.upcast_ref()) {
                return;
            }
            list.remove(&loading);
            match result {
                Ok(Ok(peers)) => {
                    for peer in peers {
                        let title = format!(
                            "{}{}",
                            peer.label,
                            if peer.expired { " · Pair again" } else { "" }
                        );
                        let button = gtk4::Button::with_label(&title);
                        style_peer_button(
                            &button,
                            view.selection.borrow().machine
                                == MachineSelection::Remote(peer.machine_id.clone()),
                        );
                        button.set_tooltip_text(Some(&format!(
                            "{}:{} · {}",
                            peer.endpoint.host, peer.endpoint.port, peer.machine_id
                        )));
                        let weak = Rc::downgrade(&view);
                        let pop = pop.downgrade();
                        button.connect_clicked(move |_| {
                            if let Some(pop) = pop.upgrade() {
                                pop.popdown();
                            }
                            if let Some(view) = weak.upgrade() {
                                view.select(Some((peer.machine_id.clone(), peer.label.clone())));
                            }
                        });
                        list.append(&button);
                    }
                }
                _ => list.append(&gtk4::Label::new(Some(
                    "Could not read paired PCs. Check peer-list.",
                ))),
            }
            let add = gtk4::Button::with_label("＋ Add a PC");
            style_peer_button(&add, false);
            add.add_css_class("hud-action-primary");
            let weak = Rc::downgrade(&view);
            let pop = pop.downgrade();
            add.connect_clicked(move |_| {
                if let (Some(view), Some(pop)) = (weak.upgrade(), pop.upgrade()) {
                    view.pairing_form(&pop);
                }
            });
            list.append(&add);
        });
    }
    fn pairing_form(self: &Rc<Self>, popover: &gtk4::Popover) {
        let weak = Rc::downgrade(self);
        let pop = popover.downgrade();
        let back = Rc::new(move || {
            if let (Some(view), Some(pop)) = (weak.upgrade(), pop.upgrade()) {
                view.populate(&pop);
            }
        });
        let weak = Rc::downgrade(self);
        let pop = popover.downgrade();
        let saved = Rc::new(move |peer: peer_client::PeerSummary| {
            if let (Some(view), Some(pop)) = (weak.upgrade(), pop.upgrade()) {
                pop.popdown();
                view.select(Some((peer.machine_id, peer.label)));
            }
        });
        popover.set_child(Some(&crate::peer_pairing_ui::build(back, saved)));
    }
    pub fn bind_keyboard(&self, window: &gtk4::ApplicationWindow) {
        use gtk4_layer_shell::{KeyboardMode, LayerShell};
        for button in [&self.local_button, &self.remote_button] {
            let popover = button.popover().unwrap();
            let previous = Rc::new(Cell::new(KeyboardMode::OnDemand));
            let mode = previous.clone();
            let weak = window.downgrade();
            popover.connect_show(move |_| {
                if let Some(window) = weak.upgrade() {
                    mode.set(window.keyboard_mode());
                    window.set_keyboard_mode(KeyboardMode::Exclusive);
                }
            });
            let weak = window.downgrade();
            popover.connect_closed(move |_| {
                if let Some(window) = weak.upgrade() {
                    // A PC switch may already have reset keyboard ownership.
                    if window.keyboard_mode() == KeyboardMode::Exclusive {
                        window.set_keyboard_mode(previous.get());
                    }
                }
            });
        }
    }
    fn select(self: &Rc<Self>, peer: Option<(String, String)>) {
        (self.on_switch)();
        *self.snapshot.borrow_mut() = None;
        self.details.set_text("");
        self.drawing.queue_draw();
        match peer {
            None => {
                self.selection.borrow_mut().select(MachineSelection::Local);
                self.stack.set_visible_child_name("local");
            }
            Some((id, label)) => {
                self.selection
                    .borrow_mut()
                    .select(MachineSelection::Remote(id));
                self.remote_button.set_label(&label);
                self.status.set_text("Connecting…");
                self.stack.set_visible_child_name("remote");
                self.refresh();
            }
        }
    }
    fn refresh(self: &Rc<Self>) {
        let Some((generation, id)) = self.selection.borrow().request() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let requested_id = id.clone();
            let result = gtk4::gio::spawn_blocking(move || {
                let peer = PeerStore::default_store()?.get(&requested_id)?;
                let snapshot = peer_client::workspace(&peer)?;
                remote_workspace::validate(&snapshot).map_err(peer_client::PeerError)?;
                Ok::<_, peer_client::PeerError>(snapshot)
            })
            .await;
            let Some(view) = weak.upgrade() else {
                return;
            };
            view.busy.set(false);
            if !view.selection.borrow().accepts(generation, &id) {
                if view.remote.is_mapped() {
                    view.refresh();
                }
                return;
            }
            match result {
                Ok(Ok(snapshot)) => {
                    view.status.set_text(&format!(
                        "Connected · {} consoles",
                        snapshot.local.cards.len()
                    ));
                    let harnesses = snapshot
                        .local
                        .harness_types
                        .iter()
                        .filter(|h| snapshot.local.visible_harnesses.contains(&h.id))
                        .map(|h| peer_client::label(&h.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    view.details
                        .set_text(&format!("{} · {}", snapshot.local.workspace, harnesses));
                    *view.snapshot.borrow_mut() = Some(snapshot);
                }
                failure => {
                    let error = match failure {
                        Ok(Err(e)) => e.0,
                        _ => "connection_failed",
                    };
                    view.status.set_text(match error {
                        "peer_revoked_or_expired" | "peer_not_found" => {
                            "Pairing required · Add this PC again"
                        }
                        "update_remote_super_desktop" | "peer_endpoint_unavailable" => {
                            "Update SUPER DESKTOP on the host"
                        }
                        "peer_identity_changed" | "connection_failed_or_pin_mismatch" => {
                            "Cannot verify or reach this PC · Check its bridge and pairing"
                        }
                        _ => "Remote desktop unavailable · Retrying…",
                    });
                    // Don't present disconnected or revoked content as current.
                    *view.snapshot.borrow_mut() = None;
                    view.details.set_text("");
                }
            }
            view.drawing.queue_draw();
        });
    }
    fn draw(&self, cr: &gtk4::cairo::Context, width: i32, height: i32) {
        cr.set_source_rgb(0.07, 0.08, 0.10);
        let _ = cr.paint();
        let snapshot = self.snapshot.borrow();
        let Some(snapshot) = snapshot.as_ref() else {
            return;
        };
        let canvas = &snapshot.local.canvas;
        let (scale, x, y) =
            remote_workspace::fit(canvas.width, canvas.height, width as f64, height as f64);
        let _ = cr.save();
        cr.translate(x, y);
        cr.scale(scale, scale);
        cr.rectangle(0.0, 0.0, canvas.width as f64, canvas.height as f64);
        cr.clip();
        cr.set_source_rgb(0.12, 0.14, 0.18);
        let _ = cr.paint();
        cr.set_source_rgb(0.18, 0.21, 0.27);
        cr.rectangle(0.0, 0.0, canvas.width as f64, canvas.top_inset as f64);
        let _ = cr.fill();
        let mut cards: Vec<_> = snapshot.local.cards.iter().collect();
        cards.sort_by_key(|card| (card.expanded, card.stacking_order));
        for card in cards {
            let rect = remote_workspace::card_rect(card, canvas.width, canvas.height);
            let _ = cr.save();
            cr.rectangle(rect.x, rect.y, rect.width, rect.height);
            cr.clip();
            cr.set_source_rgb(0.19, 0.23, 0.30);
            let _ = cr.paint();
            cr.set_source_rgb(0.42, 0.65, 0.85);
            cr.set_line_width(2.0);
            cr.rectangle(
                rect.x + 1.0,
                rect.y + 1.0,
                rect.width - 2.0,
                rect.height - 2.0,
            );
            let _ = cr.stroke();
            cr.set_font_size(18.0);
            cr.set_source_rgb(0.94, 0.95, 0.98);
            cr.move_to(rect.x + 12.0, rect.y + 28.0);
            let _ = cr.show_text(&peer_client::label(&card.title));
            cr.set_font_size(14.0);
            cr.move_to(rect.x + 12.0, rect.y + 53.0);
            let _ = cr.show_text(&format!(
                "{} · {}",
                peer_client::label(&card.agent_type),
                peer_client::label(&card.status)
            ));
            if !card.layout.iconified {
                cr.move_to(rect.x + 12.0, rect.y + 83.0);
                let _ = cr.show_text("Console preview — input unavailable");
            }
            let _ = cr.restore();
        }
        let _ = cr.restore();
    }
}

fn style_peer_button(button: &gtk4::Button, selected: bool) {
    button.add_css_class("hud-button");
    button.add_css_class("machine-peer");
    if selected {
        button.add_css_class("machine-peer-selected");
    }
    if let Some(label) = button
        .child()
        .and_then(|child| child.downcast::<gtk4::Label>().ok())
    {
        label.set_xalign(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_widgets_survive_remote_selection() {
        crate::gtk_test::run_in_child_process("machine_selector::tests::switching_inner");
    }
    #[test]
    fn switching_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let local = gtk4::Fixed::new();
        let entry = gtk4::Entry::new();
        entry.set_text("unsaved local input");
        local.put(&entry, 42.0, 84.0);
        let switches = Rc::new(Cell::new(0));
        let count = switches.clone();
        let view = MachineView::new(
            &local,
            Rc::new(move || count.set(count.get() + 1)),
            Rc::new(|| {}),
        );
        assert!(!view.is_remote());
        assert_eq!(
            view.stack.visible_child().unwrap(),
            local.clone().upcast::<gtk4::Widget>()
        );
        // No socket access in this GTK lifecycle test. Network behavior has
        // its own real-TLS peer_pairing_smoke regression.
        view.busy.set(true);
        let directory = std::env::temp_dir().join(format!("sd-selector-{}", std::process::id()));
        std::env::set_var("SUPER_DESKTOP_PEERS_STATE_DIR", &directory);
        PeerStore::default_store()
            .unwrap()
            .upsert(peer_client::test_peer('a'))
            .unwrap();
        let popover = view.local_button.popover().unwrap();
        view.populate(&popover);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            let list = popover.child().unwrap();
            if let Some(button) = list
                .first_child()
                .and_then(|w| w.next_sibling())
                .and_then(|w| w.downcast::<gtk4::Button>().ok())
            {
                assert_eq!(button.label().as_deref(), Some("Laptop"));
                button.emit_clicked();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "peer menu did not load"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(view.is_remote());
        std::fs::remove_dir_all(directory).unwrap();
        view.select(Some(("a".repeat(32), "Host laptop".into())));
        assert!(view.is_remote());
        assert_eq!(view.stack.visible_child_name().as_deref(), Some("remote"));
        *view.snapshot.borrow_mut() = Some(remote_workspace::fixture());
        let surface =
            gtk4::cairo::ImageSurface::create(gtk4::cairo::Format::ARgb32, 960, 540).unwrap();
        let cr = gtk4::cairo::Context::new(&surface).unwrap();
        view.draw(&cr, 960, 540);
        cr.status().unwrap();
        view.select(Some(("b".repeat(32), "Other laptop".into())));
        assert!(view.snapshot.borrow().is_none());
        view.select(None);
        assert!(!view.is_remote());
        assert_eq!(
            view.stack.visible_child().unwrap(),
            local.clone().upcast::<gtk4::Widget>()
        );
        assert_eq!(entry.text(), "unsaved local input");
        assert_eq!(
            local.child_transform(&entry).unwrap().to_translate(),
            (42.0, 84.0)
        );
        assert_eq!(switches.get(), 4);
    }
}
