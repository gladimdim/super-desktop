//! Paired-PC selector and the live remote workspace view.
//! Workers own only network data; generation checks guard every GTK update.
use crate::{
    desktop_protocol::MachineSelection,
    peer_client,
    peer_store::PeerStore,
    remote_terminal::RemoteCanvas,
    remote_workspace::{self, Selection},
};
use gtk4::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

/// Set after construction: a bar cannot hold a handle to the view it belongs to
/// while that view is still being built.
type OnLaunch = Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>;
/// The same, for picking one of the host's folders.
type OnPickFolder = Rc<RefCell<Option<Rc<dyn Fn(String)>>>>;

pub struct MachineView {
    pub stack: gtk4::Stack,
    pub local_button: gtk4::MenuButton,
    remote_button: gtk4::MenuButton,
    remote: gtk4::Box,
    remote_toolbar: gtk4::Overlay,
    selection: RefCell<Selection>,
    canvas: Rc<RemoteCanvas>,
    /// The host's own harness list, in the shared launch bar.
    bar: Rc<crate::harness_bar::HarnessBar>,
    /// The host's folders, in the shared folder control.
    folder_bar: crate::workspace_bar::RemoteFolderBar,
    /// Where a launch on the selected host goes, from the last snapshot.
    target: RefCell<Option<crate::harness_bar::RemoteTarget>>,
    /// One launch at a time: a slow host must not make two cards per click.
    launching: Cell<bool>,
    status: gtk4::Label,
    details: gtk4::Label,
    busy: Cell<bool>,
    on_switch: Rc<dyn Fn()>,
    on_add_pc: RefCell<Option<Rc<dyn Fn()>>>,
}
impl MachineView {
    pub fn new(local: &gtk4::Fixed, on_switch: Rc<dyn Fn()>, on_hide: Rc<dyn Fn()>) -> Rc<Self> {
        let stack = gtk4::Stack::new();
        stack.set_transition_type(gtk4::StackTransitionType::None);
        stack.add_named(local, Some("local"));
        let remote = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        // The same top bar as the local workspace: the machine selector and the
        // brand on the left, the host's own harness buttons centered, the
        // connection state and Hide on the right.
        let toolbar = gtk4::Overlay::new();
        toolbar.add_css_class("hud-bar");
        let chrome = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        chrome.set_hexpand(true);
        chrome.set_valign(gtk4::Align::Fill);
        let local_button = gtk4::MenuButton::new();
        local_button.set_label("This PC");
        local_button.add_css_class("machine-selector");
        let remote_button = gtk4::MenuButton::new();
        remote_button.add_css_class("machine-selector");
        chrome.append(&remote_button);
        let brand = gtk4::Label::new(Some("⚡ SUPER DESKTOP"));
        brand.add_css_class("hud-title");
        chrome.append(&brand);

        // The folder new harnesses start in on that PC, in the same control the
        // local workspace shows; only the folders are that host's to choose.
        let on_folder: OnPickFolder = Rc::new(RefCell::new(None));
        let folder_bar = crate::workspace_bar::build_remote_folder_bar(Rc::new({
            let on_folder = Rc::clone(&on_folder);
            move |dir: String| {
                if let Some(pick) = on_folder.borrow().as_ref() {
                    pick(dir);
                }
            }
        }));
        chrome.append(&folder_bar.widget);
        let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        chrome.append(&spacer);
        let details = gtk4::Label::new(None);
        details.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        chrome.append(&details);
        let status = gtk4::Label::new(Some("Connecting…"));
        status.set_xalign(0.0);
        chrome.append(&status);
        let hide = gtk4::Button::with_label("✕ Hide");
        hide.add_css_class("hud-button");
        hide.add_css_class("hud-button-danger");
        hide.connect_clicked(move |_| on_hide());
        chrome.append(&hide);
        toolbar.set_child(Some(&chrome));

        // The launcher refreshes this view the moment a card is created on the
        // host, instead of leaving the user to wait for the next poll.
        let on_launch: OnLaunch = Rc::new(RefCell::new(None));
        let bar = crate::harness_bar::HarnessBar::new(
            Rc::new({
                let on_launch = Rc::clone(&on_launch);
                move |key: &str| {
                    if let Some(launch) = on_launch.borrow().as_ref() {
                        launch(key);
                    }
                }
            }),
            // No usage card: those numbers come from this PC's own provider
            // state and would describe the wrong machine.
            Rc::new(|_: &gtk4::Button, _: &str| false),
        );
        toolbar.add_overlay(&bar.group);
        remote.append(&toolbar);
        remote.append(&bar.note);
        let canvas = RemoteCanvas::new();
        canvas.area.set_tooltip_text(Some(
            "The host's consoles, streamed live and scaled to fit. Click a console and type, or drag its header to move it on that PC. This view refreshes every two seconds.",
        ));
        remote.append(&canvas.area);
        stack.add_named(&remote, Some("remote"));
        stack.set_visible_child_name("local");
        let view = Rc::new(Self {
            stack,
            local_button,
            remote_button,
            remote,
            remote_toolbar: toolbar,
            selection: RefCell::new(Selection::default()),
            canvas,
            bar,
            folder_bar,
            target: RefCell::new(None),
            launching: Cell::new(false),
            status,
            details,
            busy: Cell::new(false),
            on_switch,
            on_add_pc: RefCell::new(None),
        });
        *on_launch.borrow_mut() = Some(Rc::new({
            let weak = Rc::downgrade(&view);
            move |key: &str| {
                if let Some(view) = weak.upgrade() {
                    view.launch_on_host(key);
                }
            }
        }));
        *on_folder.borrow_mut() = Some(Rc::new({
            let weak = Rc::downgrade(&view);
            move |dir: String| {
                if let Some(view) = weak.upgrade() {
                    view.choose_folder(&dir);
                }
            }
        }));
        // A command whose effect the answer cannot describe — a close, an
        // expand — asks for a fresh snapshot instead of waiting out the poll.
        view.canvas.set_on_changed(Rc::new({
            let weak = Rc::downgrade(&view);
            move || {
                if let Some(view) = weak.upgrade() {
                    view.refresh();
                }
            }
        }));
        for button in [&view.local_button, &view.remote_button] {
            let popover = gtk4::Popover::new();
            popover.add_css_class("ws-pop");
            popover.set_has_arrow(false);
            // Wayland layer-shell keyboard changes invalidate GTK's modal
            // popup grab. Dismiss explicitly, as the workspace dropdown does.
            popover.set_autohide(false);
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

    pub fn set_add_pc_action(&self, action: Rc<dyn Fn()>) {
        *self.on_add_pc.borrow_mut() = Some(action);
    }

    pub fn select_saved_peer(self: &Rc<Self>, peer: peer_client::PeerSummary) {
        self.select(Some((peer.machine_id, peer.label)));
    }

    /// The remote bar's harness logos, for the window's theme swap.
    pub fn brand_images(&self) -> Vec<(gtk4::Image, String)> {
        self.bar.brand_images()
    }

    /// Match the local dock's top-bar size, so the toolbar does not change
    /// height when the user switches between this PC and another one.
    pub fn paint_top_bar_size(&self, size: crate::state::TopBarSize, screen_width: i32) {
        crate::window::paint_top_bar_size(&self.remote_toolbar, size, screen_width);
    }
    /// Ask the selected PC to work in another of its own folders.
    ///
    /// The folder came from that host's own list, so this names nothing the
    /// host did not offer; the change is the host's, visible on its own bar and
    /// persisted there, exactly like typing it on that machine.
    fn choose_folder(self: &Rc<Self>, directory: &str) {
        let Some(target) = self.target.borrow().clone() else {
            return;
        };
        let revision = self
            .canvas
            .snapshot_revision()
            .unwrap_or_default();
        let request = peer_client::request(
            &target.peer,
            &target.epoch,
            crate::desktop_protocol::WorkspaceCommand::SetWorkspace {
                workspace: directory.to_string(),
                expected_revision: revision,
            },
        );
        // The next launch must use the folder that was just picked, even if the
        // snapshot of the change has not come back yet.
        if let Some(target) = self.target.borrow_mut().as_mut() {
            target.workspace = directory.to_string();
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reply = gtk4::gio::spawn_blocking(move || {
                peer_client::command(&target.peer, &request)
            })
            .await;
            let Some(view) = weak.upgrade() else {
                return;
            };
            match reply {
                Ok(Ok(_)) => view.refresh(),
                Ok(Err(failure)) => {
                    view.bar.fail(crate::harness_bar::launch_error(failure.0));
                    // The folder this view shows is the host's, so a refused
                    // change has to come back from the host, not from a guess
                    // this view made on the way out.
                    view.refresh();
                }
                Err(_) => {
                    view.bar.fail("That PC did not answer · try again");
                    view.refresh();
                }
            }
        });
    }

    /// Launch one harness on the selected PC, through the same bar the local
    /// workspace uses.
    ///
    /// One command per click, never retried: the host deduplicates on the
    /// request id, so a repeat cannot make two cards, and an answer whose
    /// outcome is unknown is reported instead of guessed at.
    fn launch_on_host(self: &Rc<Self>, agent_key: &str) {
        let Some(target) = self.target.borrow().clone() else {
            return;
        };
        if self.launching.replace(true) {
            return;
        }
        let (name, _) = crate::harness_bar::harness_label(agent_key);
        self.bar.set_busy(true);
        self.bar.starting(&format!("Launching {name} on that PC…"));
        let request = crate::harness_bar::create_request(&target, agent_key);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reply = gtk4::gio::spawn_blocking(move || {
                peer_client::command(&target.peer, &request)
            })
            .await;
            let Some(view) = weak.upgrade() else {
                return;
            };
            view.launching.set(false);
            view.bar.set_busy(false);
            match reply {
                Ok(Ok(reply)) => {
                    match reply.result {
                        // The new card belongs to the host and is already in its
                        // next snapshot. The card appearing is the success the
                        // user sees, so this line goes back to describing it.
                        crate::desktop_protocol::CommandResult::Applied { .. } => {
                            view.bar.note("")
                        }
                        crate::desktop_protocol::CommandResult::Conflict { .. } => {
                            view.bar.fail("That PC changed · try launching again")
                        }
                        crate::desktop_protocol::CommandResult::Rejected { error } => {
                            view.bar.fail(crate::harness_bar::launch_error(&error))
                        }
                    }
                    // A refusal usually means this view is behind that host
                    // (its folder or epoch moved on), and "try again" only works
                    // once the bar shows what it reports now. On success this is
                    // how the new card shows up at once.
                    view.refresh();
                }
                Ok(Err(failure)) => view
                    .bar
                    .fail(crate::harness_bar::launch_error(failure.0)),
                Err(_) => view.bar.fail("That PC did not answer · try again"),
            }
        });
    }

    pub fn dismiss(&self) {
        self.local_button.popdown();
        self.remote_button.popdown();
    }
    pub fn dismiss_if_open(&self) -> bool {
        let open = [&self.local_button, &self.remote_button]
            .iter()
            .any(|button| button.popover().is_some_and(|pop| pop.is_visible()));
        if open {
            self.dismiss();
        }
        open
    }
    fn contains_menu_widget(&self, widget: &gtk4::Widget) -> bool {
        [&self.local_button, &self.remote_button]
            .iter()
            .any(|button| {
                widget == button.upcast_ref::<gtk4::Widget>() || widget.is_ancestor(*button)
            })
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
                    let open = view.on_add_pc.borrow().clone();
                    pop.popdown();
                    if let Some(open) = open {
                        // Let the popover release its Wayland keyboard grab
                        // before the centered wizard claims keyboard focus.
                        glib::idle_add_local_once(move || open());
                    }
                }
            });
            list.append(&add);
        });
    }
    pub fn bind_keyboard(self: &Rc<Self>, window: &gtk4::ApplicationWindow) {
        // Observe outside clicks without consuming them: the clicked local
        // control should still work. Events in the popup have their own surface.
        let outside = gtk4::GestureClick::new();
        outside.set_button(0);
        outside.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let win = window.downgrade();
        outside.connect_pressed(move |gesture, _, x, y| {
            if gesture
                .current_event()
                .and_then(|event| event.surface())
                .is_some_and(|surface| surface.is::<gtk4::gdk::Popup>())
            {
                return;
            }
            if let (Some(view), Some(window)) = (weak.upgrade(), win.upgrade()) {
                let inside = window
                    .pick(x, y, gtk4::PickFlags::DEFAULT)
                    .is_some_and(|widget| view.contains_menu_widget(&widget));
                if !inside {
                    view.dismiss_if_open();
                }
            }
        });
        window.add_controller(outside);
        let weak = Rc::downgrade(self);
        window.connect_unmap(move |_| {
            if let Some(view) = weak.upgrade() {
                view.dismiss();
            }
        });
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
        // Leaving a PC — or picking another one — must release this viewer's
        // terminal streams before anything else. The host keeps its sessions.
        self.canvas.clear();
        *self.target.borrow_mut() = None;
        self.bar.apply(&crate::harness_bar::HarnessState::none());
        self.bar.note("");
        self.folder_bar.clear();
        self.details.set_text("");
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
                self.canvas.show_message("Connecting to this PC…");
                self.stack.set_visible_child_name("remote");
                self.refresh();
            }
        }
    }

    /// Stop every remote terminal stream while the overlay is hidden.
    ///
    /// The host keeps its sessions, the cards keep their last frame for the
    /// hide animation, and the next show reconnects. The selection, the saved
    /// layout and the local workspace are untouched.
    pub fn suspend_streams(&self) {
        self.canvas.suspend();
    }

    /// Remote cards that should slide out with the overlay, as
    /// `(widget, x, y, width, offset)` in the viewer's own coordinates.
    pub fn slide_cards(&self) -> Vec<(gtk4::Widget, f64, f64, f64, f64)> {
        self.canvas.slide_cards()
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
                let (capabilities, snapshot) = peer_client::verified_workspace(&peer)?;
                remote_workspace::validate(&snapshot).map_err(peer_client::PeerError)?;
                Ok::<_, peer_client::PeerError>((peer, capabilities, snapshot))
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
                Ok(Ok((peer, capabilities, snapshot))) => {
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
                    // The folder has its own control in the bar; this line is
                    // what that PC can run.
                    view.details.set_text(&harnesses);
                    view.folder_bar.apply(
                        &peer_client::label(&snapshot.local.workspace),
                        // Only a host that accepts commands can be moved to
                        // another of its folders, so only that host's list is
                        // worth offering.
                        if capabilities.supports_remote_desktop() {
                            &snapshot.local.folders
                        } else {
                            &[]
                        },
                    );
                    // The host's own harness list, offered by the same bar the
                    // local workspace uses; only a host that accepts commands
                    // becomes a launch target.
                    let writable = capabilities.supports_remote_desktop();
                    *view.target.borrow_mut() =
                        writable.then(|| crate::harness_bar::RemoteTarget::of(&peer, &snapshot));
                    view.bar.apply(&crate::harness_bar::HarnessState::of_snapshot(
                        &snapshot, writable,
                    ));
                    view.bar.note(match (writable, snapshot.local.visible_harnesses.len()) {
                        (false, _) => "Update SUPER DESKTOP on that PC to launch harnesses there",
                        (true, 0) => "That PC offers no harnesses to launch",
                        (true, _) => "",
                    });
                    // Live output is only carried for a host that advertises
                    // the terminal transport; anything else is a layout-only
                    // preview, never a snapshot emulation. Layout commands and
                    // live output are separate capabilities: a host may accept
                    // neither, either, or both.
                    if capabilities.supports_terminal_stream() {
                        view.canvas
                            .apply(&peer, &snapshot, capabilities.supports_remote_desktop());
                    } else {
                        view.canvas.show_message(
                            "Update SUPER DESKTOP on the host for live consoles · layout only for now",
                        );
                    }
                }
                failure => {
                    let error = match failure {
                        Ok(Err(e)) => e.0,
                        _ => "connection_failed",
                    };
                    view.status.set_text(remote_status(error));
                    // A late failure must not leave another PC's consoles on
                    // screen: disconnected content is not current content.
                    view.canvas.clear();
                    *view.target.borrow_mut() = None;
                    view.bar.apply(&crate::harness_bar::HarnessState::none());
                    view.bar.note("");
                    view.folder_bar.clear();
                    view.canvas.show_message(remote_status(error));
                    view.details.set_text("");
                }
            }
        });
    }
}

/// One place for the viewer-visible wording of a peer failure.
fn remote_status(error: &str) -> &'static str {
    match error {
        "peer_revoked_or_expired" | "peer_not_found" => "Pairing required · Add this PC again",
        "update_remote_super_desktop" | "peer_endpoint_unavailable" => {
            "Update SUPER DESKTOP on the host"
        }
        "peer_identity_changed" | "connection_failed_or_pin_mismatch" => {
            "Cannot verify or reach this PC · Check its bridge and pairing"
        }
        _ => "Remote desktop unavailable · Retrying…",
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
    #[ignore = "requires a real Wayland compositor with layer-shell"]
    fn layer_popup_stays_open() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "machine_selector::tests::layer_popup_inner",
                "--nocapture",
            ])
            .env(crate::gtk_test::CHILD_ENV, "1")
            .env("LD_PRELOAD", "/usr/lib/libgtk4-layer-shell.so")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[test]
    fn layer_popup_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
        gtk4::init().unwrap();
        let directory = std::env::temp_dir().join(format!("sd-popup-{}", std::process::id()));
        std::env::set_var("SUPER_DESKTOP_PEERS_STATE_DIR", &directory);
        let app = gtk4::Application::new(
            Some("com.superdesktop.PopupTest"),
            gtk4::gio::ApplicationFlags::NON_UNIQUE,
        );
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("sd-selector-regression"));
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_default_size(640, 480);
        let local = gtk4::Fixed::new();
        let view = MachineView::new(&local, Rc::new(|| {}), Rc::new(|| {}));
        local.put(&view.local_button, 10.0, 10.0);
        view.bind_keyboard(&window);
        window.set_child(Some(&view.stack));
        window.present();
        fn pump() {
            let until = std::time::Instant::now() + Duration::from_millis(250);
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        pump();
        let pop = view.local_button.popover().unwrap();
        assert!(!pop.is_autohide());
        view.local_button.popup();
        pump();
        assert!(
            pop.is_visible(),
            "selector closed during layer keyboard transition"
        );
        assert_eq!(window.keyboard_mode(), KeyboardMode::Exclusive);
        let opened = Rc::new(Cell::new(false));
        let opened_on_click = Rc::clone(&opened);
        view.set_add_pc_action(Rc::new(move || opened_on_click.set(true)));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let add = loop {
            pump();
            if let Some(add) = pop.child().and_then(|list| list.last_child())
                .and_then(|child| child.downcast::<gtk4::Button>().ok())
            {
                if add.label().as_deref() == Some("＋ Add a PC") { break add; }
            }
            assert!(std::time::Instant::now() < deadline, "Add a PC did not appear");
        };
        add.emit_clicked();
        pump();
        assert!(opened.get(), "Add a PC did not open the centered wizard");
        assert!(!pop.is_visible(), "selector remained over the wizard");
        view.local_button.popup();
        pump();
        assert!(pop.is_visible(), "selector could not reopen");
        assert!(view.contains_menu_widget(&pop.child().unwrap()));
        assert!(!view.contains_menu_widget(local.upcast_ref()));
        let controllers = window.observe_controllers();
        let outside = (0..controllers.n_items())
            .find_map(|i| {
                controllers
                    .item(i)
                    .and_then(|object| object.downcast::<gtk4::GestureClick>().ok())
            })
            .unwrap();
        // Same capture handler as a pointer press outside the selector.
        outside.emit_by_name::<()>("pressed", &[&1i32, &600.0f64, &400.0f64]);
        pump();
        assert!(!pop.is_visible(), "outside click did not dismiss selector");
        assert_eq!(window.keyboard_mode(), KeyboardMode::OnDemand);
        view.local_button.popup();
        pump();
        assert!(pop.is_visible(), "selector could not reopen");
        assert!(view.dismiss_if_open()); // Escape uses this same method.
        pump();
        view.local_button.popup();
        pump();
        window.set_visible(false);
        pump();
        assert!(!pop.is_visible(), "selector survived hiding the window");
        window.close();
        std::fs::remove_dir_all(directory).unwrap();
    }
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
        // Before a host snapshot arrives the canvas states what it is doing
        // instead of leaving an empty area.
        assert_eq!(
            view.canvas.area.visible_child_name().as_deref(),
            Some("message")
        );
        // Reconciliation and geometry need no network: a host whose session is
        // gone renders as a card but is never streamed.
        let mut snapshot = remote_workspace::fixture();
        snapshot.local.cards[0].session_alive = Some(false);
        view.canvas
            .apply(&peer_client::test_peer('a'), &snapshot, true);
        assert_eq!(view.canvas.card_count(), 1);
        // The remote workspace carries the host's own launch bar in its top
        // bar, and it is inert until that host reports what it can run.
        assert!(view.bar.group.parent().is_some());
        assert!(view.bar.note.parent().is_some());
        assert!(!view.bar.sensitive("shell"));
        assert_eq!(
            view.canvas.area.visible_child_name().as_deref(),
            Some("canvas")
        );
        view.select(Some(("b".repeat(32), "Other laptop".into())));
        assert!(view.canvas.card_count() == 0, "another PC's cards must not stay");
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

    #[test]
    fn remote_canvas_replaces_cards_the_host_removed() {
        crate::gtk_test::run_in_child_process("machine_selector::tests::reconcile_inner");
    }

    #[test]
    fn reconcile_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let canvas = crate::remote_terminal::RemoteCanvas::new();
        let peer = peer_client::test_peer('a');
        let mut snapshot = remote_workspace::fixture();
        // Both cards stay unstreamed, so this check never opens a socket.
        for card in snapshot.local.cards.iter_mut() {
            card.session_alive = Some(false);
        }
        let second = snapshot.local.cards[0].clone();
        canvas.apply(&peer, &snapshot, true);
        assert_eq!(canvas.card_count(), 1);
        snapshot.local.cards.clear();
        canvas.apply(&peer, &snapshot, true);
        assert_eq!(canvas.card_count(), 0);
        snapshot.local.cards.push(second);
        canvas.apply(&peer, &snapshot, true);
        assert_eq!(canvas.card_count(), 1);
        // Iconified and expanded states are part of the host's geometry.
        snapshot.local.cards[0].layout.iconified = true;
        canvas.apply(&peer, &snapshot, true);
        assert_eq!(canvas.card_count(), 1);
    }

    #[test]
    fn a_slow_poll_cannot_take_back_a_revision_we_already_adopted() {
        crate::gtk_test::run_in_child_process("machine_selector::tests::stale_poll_inner");
    }

    #[test]
    fn stale_poll_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let canvas = crate::remote_terminal::RemoteCanvas::new();
        let peer = peer_client::test_peer('a');
        // A host whose session is gone renders as a card but is never streamed,
        // so this check opens no socket.
        let mut snapshot = remote_workspace::fixture();
        snapshot.local.cards[0].session_alive = Some(false);
        canvas.apply(&peer, &snapshot, true);
        assert!(canvas.layout_is_writable());
        assert_eq!(canvas.card_revision("card-one"), Some(1));

        // A command was accepted: the host published revision 2 with the new
        // position, and this view adopted both.
        let mut moved = snapshot.clone();
        moved.local.revision = 2;
        moved.local.cards[0].revision = 2;
        moved.local.cards[0].layout.x = 700;
        canvas.apply(&peer, &moved, true);
        assert_eq!(canvas.card_position("card-one"), Some((700, 200)));
        assert_eq!(canvas.card_revision("card-one"), Some(2));

        // A poll that started before that command still reports revision 1.
        // Drawing it would make the next drop be refused as stale, so the
        // newer revision has to win.
        canvas.apply(&peer, &snapshot, true);
        assert_eq!(canvas.card_position("card-one"), Some((700, 200)));
        assert_eq!(canvas.card_revision("card-one"), Some(2));

        // A host that does not accept layout commands keeps its own geometry.
        canvas.apply(&peer, &snapshot, false);
        assert!(!canvas.layout_is_writable());

        // A restarting host owns its revisions again: nothing carries over.
        let mut restarted = snapshot.clone();
        restarted.local.epoch = "host-two".into();
        canvas.apply(&peer, &restarted, true);
        assert_eq!(canvas.card_revision("card-one"), Some(1));
        assert_eq!(canvas.card_position("card-one"), Some((100, 200)));
    }
}
