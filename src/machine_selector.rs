//! Paired-PC selector and the live remote workspace view.
//! Workers own only network data; generation checks guard every GTK update.
use crate::{
    desktop_protocol::{Capabilities, MachineSelection, WorkspaceSnapshot},
    peer_client::{self, Peer},
    peer_events::{Action, EventCursor, Update, WorkspaceEvents},
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

/// The selected host's live workspace subscription (`workspace-events-v1`).
/// Held only while that host is selected and the overlay shows it.
struct LiveEvents {
    /// Which subscription this is; a late update from a released one is
    /// recognised by it and dropped.
    serial: u64,
    generation: u64,
    id: String,
    _events: WorkspaceEvents,
}

pub struct MachineView {
    pub stack: gtk4::Stack,
    pub local_button: gtk4::MenuButton,
    remote_button: gtk4::MenuButton,
    remote: gtk4::Box,
    remote_toolbar: gtk4::Box,
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
    /// Fit / 100% for the remote workspace, left of Hide.
    fit_button: gtk4::ToggleButton,
    actual_button: gtk4::ToggleButton,
    busy: Cell<bool>,
    /// A refresh was asked for while one was in flight (an event resync, a
    /// command's answer): run one more when it finishes, never in parallel.
    again: Cell<bool>,
    /// The host and capabilities of the last verified snapshot, which is
    /// what an event's snapshot is drawn with.
    host: RefCell<Option<(Peer, Capabilities)>>,
    /// Sequence, epoch and revision of what this view shows, shared by the
    /// event stream and the snapshot fetch.
    cursor: RefCell<EventCursor>,
    events: RefCell<Option<LiveEvents>>,
    events_serial: Cell<u64>,
    /// The subscription is connected: the two-second poll stands down.
    live: Cell<bool>,
    on_switch: Rc<dyn Fn()>,
    on_add_pc: RefCell<Option<Rc<dyn Fn()>>>,
}
impl MachineView {
    pub fn new(local: &gtk4::Fixed, on_switch: Rc<dyn Fn()>, on_hide: Rc<dyn Fn()>) -> Rc<Self> {
        let stack = gtk4::Stack::new();
        stack.set_transition_type(gtk4::StackTransitionType::None);
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        stack.add_named(local, Some("local"));
        let remote = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        // The same top bar as the local workspace: the machine selector and the
        // brand on the left, the host's own harness buttons centered, the
        // connection state and Hide on the right.
        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        toolbar.add_css_class("hud-bar");
        // The bar's inner viewport expands; the bar itself must not take a
        // share of the height the workspace canvas needs for pan and zoom.
        toolbar.set_vexpand(false);
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
        let right = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        right.set_valign(gtk4::Align::Center);
        // Only "Connected" or "Disconnected": the launcher icons already say
        // what that PC can run, and the reason for a disconnect is in the
        // tooltip and in the view itself.
        let status = gtk4::Label::new(Some(connection_label(false)));
        status.add_css_class("remote-connection");
        status.set_xalign(0.0);
        // Optional text: it shrinks before the view toggle and Hide do.
        status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        right.append(&status);
        // Fit the host's whole workspace, or show it at 100% (one host pixel
        // per logical pixel) and pan/zoom over it. Outside every scroller, with
        // Hide, so it stays visible and clickable at any width.
        let view_toggle = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        view_toggle.add_css_class("linked");
        view_toggle.add_css_class("remote-view-toggle");
        view_toggle.set_valign(gtk4::Align::Center);
        let fit_button = gtk4::ToggleButton::with_label("Fit");
        fit_button.add_css_class("hud-button");
        fit_button.set_tooltip_text(Some("Fit that PC's whole workspace in this screen"));
        fit_button.set_active(true);
        let actual_button = gtk4::ToggleButton::with_label("100%");
        actual_button.add_css_class("hud-button");
        actual_button.set_group(Some(&fit_button));
        actual_button.set_tooltip_text(Some(
            "Show that PC at 100% · drag empty space or scroll to pan · \
             Ctrl+scroll or pinch to zoom · click again for 100%",
        ));
        view_toggle.append(&fit_button);
        view_toggle.append(&actual_button);
        right.append(&view_toggle);
        let hide = gtk4::Button::with_label("✕ Hide");
        hide.add_css_class("hud-button");
        hide.add_css_class("hud-button-danger");
        hide.connect_clicked(move |_| on_hide());
        right.append(&hide);

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
        toolbar.append(&crate::window::top_bar_content(&chrome, &bar.group, &right));
        remote.append(&toolbar);
        remote.append(&bar.note);
        let canvas = RemoteCanvas::new();
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
            fit_button,
            actual_button,
            busy: Cell::new(false),
            again: Cell::new(false),
            host: RefCell::new(None),
            cursor: RefCell::new(EventCursor::default()),
            events: RefCell::new(None),
            events_serial: Cell::new(0),
            live: Cell::new(false),
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
        // The toggle follows the canvas: a PC's remembered mode, Ctrl+scroll
        // and pinch all change it without a click.
        view.canvas.set_on_mode_changed(Rc::new({
            let fit = view.fit_button.clone();
            let actual = view.actual_button.clone();
            move |mode| paint_view_mode(&fit, &actual, mode)
        }));
        view.fit_button.connect_clicked({
            let weak = Rc::downgrade(&view);
            move |_| {
                if let Some(view) = weak.upgrade() {
                    view.canvas.set_mode(remote_workspace::ViewMode::Fit, None);
                }
            }
        });
        view.actual_button.connect_clicked({
            let weak = Rc::downgrade(&view);
            move |_| {
                if let Some(view) = weak.upgrade() {
                    view.canvas
                        .set_mode(remote_workspace::ViewMode::Actual { zoom: 1.0 }, None);
                }
            }
        });
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
        // The fallback for hosts without `workspace-events-v1`, and for the
        // time a subscription is down: a live subscription replaces it. While
        // it is live, a quiet host sends no snapshot that would retry a
        // console whose stream dropped, so the tick does that locally.
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if view.remote.is_mapped() {
                if view.live.get() {
                    view.canvas.retry_streams();
                } else {
                    view.refresh();
                }
            }
            glib::ControlFlow::Continue
        });
        view
    }
    pub fn keyboard_cards(&self) -> Vec<Rc<crate::mini_terminal::MiniTerminalCard>> {
        self.canvas.keyboard_cards()
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
    pub fn paint_top_bar_size(&self, size: crate::state::TopBarSize) {
        crate::window::paint_top_bar_size(&self.remote_toolbar, size);
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
            use crate::command_feedback::{for_workspace, Outcome, WorkspaceAction};
            // Applied or not, the folder this view shows is the host's: a
            // refused change has to come back from the host, not from a guess
            // this view made on the way out. An unknown outcome is never
            // retried; the refresh is how the user finds out.
            if let Some(notice) = for_workspace(WorkspaceAction::Folder, Outcome::of(&reply)) {
                view.bar.flash(notice);
            }
            view.refresh();
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
            use crate::command_feedback::{for_workspace, Outcome, WorkspaceAction};
            match for_workspace(WorkspaceAction::Create, Outcome::of(&reply)) {
                // The new card belongs to the host and is already in its next
                // snapshot. The card appearing is the success the user sees,
                // so this line goes back to describing it.
                None => view.bar.note(""),
                Some(notice) => view.bar.flash(notice),
            }
            // A refusal usually means this view is behind that host (its folder
            // or epoch moved on), and "try again" only works once the bar shows
            // what it reports now; an unknown outcome is checked by looking,
            // never by launching again. On success this is how the new card
            // shows up at once.
            if !matches!(&reply, Ok(Err(error)) if error.0 == "connection_failed_or_pin_mismatch") {
                view.refresh();
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
        // terminal streams and its event subscription before anything else.
        // The host keeps its sessions.
        self.release_events();
        *self.host.borrow_mut() = None;
        *self.cursor.borrow_mut() = EventCursor::default();
        self.canvas.clear();
        *self.target.borrow_mut() = None;
        self.bar.apply(&crate::harness_bar::HarnessState::none());
        self.bar.note("");
        self.folder_bar.clear();
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
                self.set_connection(false, "Connecting to this PC…");
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
        self.release_events();
        self.canvas.suspend();
    }

    /// Drop the event subscription; the host releases its slot when the socket
    /// closes, and the poll takes over until the next subscription connects.
    fn release_events(&self) {
        self.events.borrow_mut().take();
        self.live.set(false);
    }

    /// Subscribe to the selected host's workspace events, once per visit,
    /// while the remote view is on screen and the host offers them.
    fn ensure_events(
        self: &Rc<Self>,
        generation: u64,
        id: &str,
        peer: &Peer,
        capabilities: &Capabilities,
    ) {
        if !capabilities.supports_workspace_events() || !self.remote.is_mapped() {
            return;
        }
        if self
            .events
            .borrow()
            .as_ref()
            .is_some_and(|live| live.generation == generation && live.id == id)
        {
            return;
        }
        let serial = self.events_serial.get() + 1;
        self.events_serial.set(serial);
        let (events, mut updates) = WorkspaceEvents::open(peer.clone());
        *self.events.borrow_mut() = Some(LiveEvents {
            serial,
            generation,
            id: id.to_string(),
            _events: events,
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            use futures_util::StreamExt;
            while let Some(update) = updates.next().await {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                let current = view.events.borrow().as_ref().is_some_and(|live| {
                    live.serial == serial
                        && view.selection.borrow().accepts(live.generation, &live.id)
                });
                if !current {
                    return;
                }
                view.on_event(update);
            }
        });
    }

    fn on_event(self: &Rc<Self>, update: Update) {
        match update {
            Update::Connected => {
                self.cursor.borrow_mut().connected();
                self.live.set(true);
            }
            Update::Event(event) => {
                let action = self.cursor.borrow_mut().accept(event);
                match action {
                    Action::Apply(snapshot) => {
                        let host = self.host.borrow().clone();
                        if let Some((peer, capabilities)) = host {
                            self.show_snapshot(&peer, &capabilities, &snapshot);
                        }
                    }
                    Action::Ignore => {}
                    // Never a replay: one fetch of the host's current state.
                    Action::Resync => self.refresh(),
                    Action::Unavailable(_) => self.show_failure("remote_desktop_unavailable"),
                }
            }
            // Reconnecting with backoff; the poll covers the gap.
            Update::Down => self.live.set(false),
            // Revoked, re-identified or downgraded: the poll reports why, and
            // a later verified snapshot may subscribe again.
            Update::Ended(reason) => {
                self.release_events();
                self.set_connection(false, remote_status(reason));
            }
        }
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
            self.again.set(true);
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
            let again = view.again.replace(false);
            if !view.selection.borrow().accepts(generation, &id) {
                if view.remote.is_mapped() {
                    view.refresh();
                }
                return;
            }
            match result {
                Ok(Ok((peer, capabilities, snapshot))) => {
                    *view.host.borrow_mut() = Some((peer.clone(), capabilities.clone()));
                    // A fetch that left before a newer event arrived must not
                    // take the view back to the older state.
                    if view.cursor.borrow_mut().admit(&snapshot) {
                        view.show_snapshot(&peer, &capabilities, &snapshot);
                    }
                    view.ensure_events(generation, &id, &peer, &capabilities);
                }
                failure => {
                    let error = match failure {
                        Ok(Err(e)) => e.0,
                        _ => "connection_failed",
                    };
                    view.show_failure(error);
                }
            }
            if again && view.remote.is_mapped() {
                view.refresh();
            }
        });
    }

    /// Draw one verified snapshot of the selected host, from a fetch or from
    /// its event stream.
    fn show_snapshot(&self, peer: &Peer, capabilities: &Capabilities, snapshot: &WorkspaceSnapshot) {
        let view = self;
        let consoles = snapshot.local.cards.len();
        view.set_connection(
            true,
            &format!("{consoles} console{}", if consoles == 1 { "" } else { "s" }),
        );
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
            writable.then(|| crate::harness_bar::RemoteTarget::of(peer, snapshot));
        view.bar.apply(&crate::harness_bar::HarnessState::of_snapshot(
            snapshot, writable,
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
                .apply(peer, snapshot, capabilities.supports_remote_desktop());
        } else {
            view.canvas.show_message(
                "Update SUPER DESKTOP on the host for live consoles · layout only for now",
            );
        }
    }

    fn show_failure(&self, error: &str) {
        let view = self;
        view.set_connection(false, remote_status(error));
        // A late failure must not leave another PC's consoles on
        // screen: disconnected content is not current content.
        view.canvas.clear();
        *view.target.borrow_mut() = None;
        view.bar.apply(&crate::harness_bar::HarnessState::none());
        view.bar.note("");
        view.folder_bar.clear();
        view.canvas.show_message(remote_status(error));
    }

    /// The bar's whole status: "Connected" or "Disconnected", with the detail
    /// (console count, or why it is not connected) as the tooltip.
    fn set_connection(&self, connected: bool, detail: &str) {
        self.status.set_text(connection_label(connected));
        self.status.set_tooltip_text(Some(detail));
        let (on, off) = ("remote-connected", "remote-disconnected");
        self.status.add_css_class(if connected { on } else { off });
        self.status.remove_css_class(if connected { off } else { on });
    }
}

/// Show the view mode on the toggle: which half is on, and the zoom.
fn paint_view_mode(
    fit: &gtk4::ToggleButton,
    actual: &gtk4::ToggleButton,
    mode: remote_workspace::ViewMode,
) {
    let label = view_mode_label(mode);
    let is_fit = matches!(mode, remote_workspace::ViewMode::Fit);
    if fit.is_active() != is_fit {
        fit.set_active(is_fit);
    }
    if actual.is_active() == is_fit {
        actual.set_active(!is_fit);
    }
    if actual.label().as_deref() != Some(label.as_str()) {
        actual.set_label(&label);
    }
}

/// The 100% half of the toggle names the current zoom.
fn view_mode_label(mode: remote_workspace::ViewMode) -> String {
    match mode {
        remote_workspace::ViewMode::Fit => "100%".to_string(),
        remote_workspace::ViewMode::Actual { zoom } => format!("{:.0}%", zoom * 100.0),
    }
}

fn connection_label(connected: bool) -> &'static str {
    if connected { "Connected" } else { "Disconnected" }
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

    fn requeue_sizes(widget: &gtk4::Widget) {
        widget.queue_resize();
        let mut child = widget.first_child();
        while let Some(current) = child {
            requeue_sizes(&current);
            child = current.next_sibling();
        }
    }

    #[test]
    fn toolbar_remote_fits_after_workspace_switches() {
        crate::gtk_test::run_in_child_process("machine_selector::tests::toolbar_remote_inner");
    }

    #[test]
    fn toolbar_remote_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let local = gtk4::Fixed::new();
        let card = gtk4::Label::new(Some("saved on a larger output"));
        local.put(&card, 6000.0, 0.0);
        let view = MachineView::new(&local, Rc::new(|| {}), Rc::new(|| {}));
        // Select a PC first (no network in this layout test), then fill the
        // bar with the longest contents it can carry.
        view.busy.set(true);
        view.select(Some(("a".repeat(32), "Host laptop".into())));
        view.remote_button
            .set_label(&"A very long paired computer name ".repeat(12));
        view.folder_bar
            .apply(&format!("/home/user/{}", "project/".repeat(60)), &[]);
        view.set_connection(true, "123 consoles");
        // A large host with one card parked far past its own edge, drawn
        // live: neither the 100% workspace nor the off-screen card may
        // enlarge the remote workspace (and so the overlay window).
        let mut snapshot = remote_workspace::fixture();
        snapshot.local.canvas.width = 3840;
        snapshot.local.canvas.height = 2160;
        snapshot.local.cards[0].session_alive = Some(false);
        snapshot.local.cards[0].layout.x = 9000;
        snapshot.local.cards[0].layout.y = 5000;
        view.canvas
            .apply(&peer_client::test_peer('a'), &snapshot, true);
        fn find(widget: &gtk4::Widget, test: &dyn Fn(&gtk4::Widget) -> bool) -> Option<gtk4::Widget> {
            if test(widget) {
                return Some(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(found) = find(&widget, test) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        let hide: gtk4::Button = find(view.remote_toolbar.upcast_ref(), &|w| {
            w.has_css_class("hud-button-danger")
        })
        .unwrap()
        .downcast()
        .unwrap();
        // Every right-side action: the view toggle and Hide.
        let actions: Vec<gtk4::Widget> = vec![
            view.fit_button.clone().upcast(),
            view.actual_button.clone().upcast(),
            hide.clone().upcast(),
        ];
        // The toggle is outside every scroller, like Hide.
        for action in &actions {
            assert!(action.ancestor(gtk4::ScrolledWindow::static_type()).is_none());
        }
        let all_keys: Vec<String> = crate::tmux::HARNESS_KEYS
            .iter()
            .map(|key| key.to_string())
            .collect();
        let modes = [
            remote_workspace::ViewMode::Fit,
            remote_workspace::ViewMode::Actual { zoom: 1.0 },
            remote_workspace::ViewMode::Actual { zoom: 3.0 },
        ];
        for keys in [Vec::new(), all_keys.clone()] {
            view.bar.apply(&crate::harness_bar::HarnessState {
                keys,
                custom: Vec::new(),
                ready: true,
            });
            for mode in modes {
                view.canvas.set_mode(mode, None);
                for size in [
                    crate::state::TopBarSize::Small,
                    crate::state::TopBarSize::Medium,
                    crate::state::TopBarSize::Large,
                ] {
                    view.paint_top_bar_size(size);
                    // The size is a CSS class. This part lays out without a
                    // window, so no frame restyles the bar and GTK would reuse
                    // sizes measured under the previous size; drop them, as a
                    // frame does between a size change and layout.
                    requeue_sizes(view.remote_toolbar.upcast_ref());
                    // Shrink, grow and shrink again, narrow and wide.
                    for width in [1280, 320, 3440, 640, 480, 1024, 360, 2560] {
                        view.stack.set_visible_child_name("local");
                        assert!(view.stack.measure(gtk4::Orientation::Horizontal, -1).0 > 6000);
                        view.stack.set_visible_child_name("remote");
                        assert!(
                            view.stack.measure(gtk4::Orientation::Horizontal, -1).0 <= width,
                            "remote workspace minimum must fit {width} logical pixels in {mode:?}"
                        );
                        assert!(
                            view.stack.measure(gtk4::Orientation::Vertical, width).0 <= 600,
                            "the remote canvas must not demand height in {mode:?}"
                        );
                        view.stack.allocate(width, 600, -1, None);
                        assert!(
                            view.remote_toolbar.height() <= 80,
                            "the bar must stay a bar: {}",
                            view.remote_toolbar.height()
                        );
                        let inset = match size {
                            crate::state::TopBarSize::Small => 10,
                            crate::state::TopBarSize::Medium => 12,
                            crate::state::TopBarSize::Large => 16,
                        };
                        for action in &actions {
                            let bounds = action.compute_bounds(&view.stack).unwrap();
                            assert!(action.is_visible() && action.is_sensitive());
                            assert!(bounds.width() > 0.0 && bounds.x() >= 0.0);
                            assert!(
                                bounds.x() + bounds.width() <= (width - inset) as f32 + 0.5,
                                "remote action must fit {width} at {size:?} in {mode:?}: {bounds:?}"
                            );
                        }
                        let bounds = hide.compute_bounds(&view.stack).unwrap();
                        assert!((bounds.x() + bounds.width() - (width - inset) as f32).abs() < 1.0);
                        // The toggle sits left of Hide, never over it.
                        let toggle = view.actual_button.compute_bounds(&view.stack).unwrap();
                        assert!(toggle.x() + toggle.width() <= bounds.x());
                    }
                }
            }
        }
        view.canvas.set_mode(remote_workspace::ViewMode::Fit, None);
        view.paint_top_bar_size(crate::state::TopBarSize::Medium);
        // Mapped, through the production overlay that sizes the workspace.
        let window = gtk4::Window::new();
        window.set_default_size(640, 600);
        window.set_resizable(false);
        window.set_child(Some(&crate::window::desktop_overlay(&view.stack)));
        window.present();
        let hittable = |window: &gtk4::Window, action: &gtk4::Widget| {
            let Some(bounds) = action.compute_bounds(window) else {
                return false;
            };
            bounds.width() > 0.0
                && bounds.x() >= 0.0
                && bounds.x() + bounds.width() <= window.width() as f32
                && window
                    .pick(
                        (bounds.x() + bounds.width() / 2.0) as f64,
                        (bounds.y() + bounds.height() / 2.0) as f64,
                        gtk4::PickFlags::DEFAULT,
                    )
                    .is_some_and(|picked| picked == *action || picked.is_ancestor(action))
        };
        for (step, (width, mode)) in [
            (640, remote_workspace::ViewMode::Fit),
            (480, remote_workspace::ViewMode::Actual { zoom: 1.0 }),
            (1280, remote_workspace::ViewMode::Actual { zoom: 3.0 }),
            (800, remote_workspace::ViewMode::Fit),
            (480, remote_workspace::ViewMode::Actual { zoom: 1.0 }),
        ]
        .into_iter()
        .enumerate()
        {
            if step == 3 {
                // The disconnected state.
                view.set_connection(false, "Connecting to this PC…");
            }
            window.set_default_size(width, 600);
            view.canvas.set_mode(mode, None);
            let until = std::time::Instant::now() + Duration::from_secs(3);
            let mut ok = false;
            while std::time::Instant::now() < until {
                while glib::MainContext::default().iteration(false) {}
                // Right-aligned inside the medium bar's 12 px padding.
                let aligned = hide.compute_bounds(&window).is_some_and(|bounds| {
                    (bounds.x() + bounds.width() - (width - 12) as f32).abs() < 1.0
                });
                ok = window.width() == width
                    && aligned
                    && actions.iter().all(|a| hittable(&window, a));
                if ok {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(window.width(), width, "the canvas must not enlarge the window");
            assert!(ok, "every remote action must stay hittable at {width} in {mode:?}");
        }
        // The toggle really switches the view, and follows it back.
        view.fit_button.emit_clicked();
        assert_eq!(view.canvas.mode(), remote_workspace::ViewMode::Fit);
        assert!(view.fit_button.is_active() && !view.actual_button.is_active());
        view.actual_button.emit_clicked();
        assert_eq!(
            view.canvas.mode(),
            remote_workspace::ViewMode::Actual { zoom: 1.0 }
        );
        assert!(view.actual_button.is_active() && !view.fit_button.is_active());
        view.canvas
            .set_mode(remote_workspace::ViewMode::Actual { zoom: 1.5 }, None);
        assert_eq!(view.actual_button.label().as_deref(), Some("150%"));
        // Clicking 100% again returns to exactly 100%.
        view.actual_button.emit_clicked();
        assert_eq!(view.actual_button.label().as_deref(), Some("100%"));
        window.close();
    }

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
    fn live_events_drive_the_view() {
        crate::gtk_test::run_in_child_process("machine_selector::tests::live_events_inner");
    }

    #[test]
    fn live_events_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        use crate::desktop_protocol::{Capabilities, WorkspaceEvent};
        gtk4::init().unwrap();
        let view = MachineView::new(&gtk4::Fixed::new(), Rc::new(|| {}), Rc::new(|| {}));
        // No socket in this test: a resync must ask for a fetch, and the fetch
        // is held back by `busy` exactly as a fetch already in flight would be.
        view.busy.set(true);
        let peer = peer_client::test_peer('a');
        let generation = view
            .selection
            .borrow_mut()
            .select(MachineSelection::Remote(peer.machine_id.clone()));
        assert!(view.selection.borrow().accepts(generation, &peer.machine_id));
        *view.host.borrow_mut() =
            Some((peer.clone(), Capabilities::current(peer.machine_id.clone())));
        let mut first = remote_workspace::fixture();
        // A card whose session is gone is drawn but never streamed.
        first.local.cards[0].session_alive = Some(false);
        let snapshot = |sequence, revision, x| {
            let mut workspace = first.clone();
            workspace.local.revision = revision;
            workspace.local.cards[0].revision = revision;
            workspace.local.cards[0].layout.x = x;
            Update::Event(WorkspaceEvent::Snapshot { sequence, workspace })
        };

        view.on_event(Update::Connected);
        assert!(view.live.get(), "a connected subscription stands the poll down");
        view.on_event(snapshot(1, 1, 100));
        assert_eq!(view.canvas.card_count(), 1);
        assert_eq!(view.status.text(), "Connected");
        assert_eq!(view.status.tooltip_text().as_deref(), Some("1 console"));
        // A host-side move arrives as the next event and is drawn at once.
        view.on_event(snapshot(2, 2, 700));
        assert_eq!(view.canvas.card_position("card-one"), Some((700, 200)));
        assert_eq!(view.canvas.card_revision("card-one"), Some(2));
        // A fetch that left before that event cannot take the view back.
        let mut older = first.clone();
        older.local.revision = 1;
        assert!(!view.cursor.borrow_mut().admit(&older));
        // A lost event is never guessed across: it asks for a snapshot.
        assert!(!view.again.get());
        view.on_event(snapshot(4, 3, 900));
        assert!(view.again.get(), "a sequence gap asks for a fresh snapshot");
        assert_eq!(view.canvas.card_position("card-one"), Some((700, 200)));
        // A dropped subscription hands back to the poll; hiding releases it.
        view.on_event(Update::Down);
        assert!(!view.live.get());
        view.on_event(Update::Connected);
        view.suspend_streams();
        assert!(!view.live.get() && view.events.borrow().is_none());
        // An ended subscription says why and is not kept.
        view.on_event(Update::Ended("peer_revoked_or_expired"));
        assert_eq!(view.status.text(), "Disconnected");
        assert_eq!(
            view.status.tooltip_text().as_deref(),
            Some("Pairing required · Add this PC again")
        );
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

    /// Sockets this process holds to a local port, in any TCP state.
    fn own_connections(port: u16) -> usize {
        let inodes: std::collections::HashSet<String> = std::fs::read_dir("/proc/self/fd")
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                    .filter_map(|link| {
                        let link = link.to_string_lossy().to_string();
                        link.strip_prefix("socket:[")
                            .and_then(|rest| rest.strip_suffix(']'))
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let wanted = format!(":{port:04X}");
        ["/proc/self/net/tcp", "/proc/self/net/tcp6"]
            .iter()
            .filter_map(|table| std::fs::read_to_string(table).ok())
            .flat_map(|table| table.lines().skip(1).map(str::to_string).collect::<Vec<_>>())
            .filter(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                fields.len() > 9 && fields[2].ends_with(&wanted) && inodes.contains(fields[9])
            })
            .count()
    }

    /// Driven by tests/two_pc_matrix.py: the real selector and remote canvas
    /// against isolated hosts (real bridges, private tmux servers, a stateful
    /// owner). Host-side actions are requested through files in `control`.
    #[test]
    fn two_pc_inner() {
        let Ok(spec) = std::env::var("SUPER_DESKTOP_TWO_PC_TEST_INPUT") else {
            return;
        };
        use std::time::Instant;
        let spec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(spec).unwrap()).unwrap();
        let control = std::path::PathBuf::from(spec["control"].as_str().unwrap());
        struct Host {
            id: String,
            port: u16,
            tmux: String,
            cards: Vec<String>,
        }
        let host = |name: &str| {
            let host = &spec["hosts"][name];
            Host {
                id: host["machineId"].as_str().unwrap().to_string(),
                port: host["port"].as_u64().unwrap() as u16,
                tmux: host["tmuxTmpdir"].as_str().unwrap().to_string(),
                cards: host["cards"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|card| card.as_str().unwrap().to_string())
                    .collect(),
            }
        };
        let (a, b, c) = (host("a"), host("b"), host("c"));
        let tmux = |host: &Host, args: &[&str]| {
            let output = std::process::Command::new("tmux")
                .args(args)
                .env("TMUX_TMPDIR", &host.tmux)
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).to_string()
        };
        let clients = |host: &Host| {
            tmux(host, &["list-clients", "-F", "#{client_name}"])
                .lines()
                .filter(|line| !line.is_empty())
                .count()
        };

        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let view = MachineView::new(&gtk4::Fixed::new(), Rc::new(|| {}), Rc::new(|| {}));
        let window = gtk4::Window::new();
        window.set_default_size(1280, 800);
        window.set_child(Some(&view.stack));
        window.present();

        // Every pump checks that no card of another PC is ever on screen.
        let allowed: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let pump = || {
            while glib::MainContext::default().iteration(false) {}
            let ids = view.canvas.card_ids();
            let allowed = allowed.borrow();
            assert!(
                ids.iter().all(|id| allowed.iter().any(|prefix| id.starts_with(prefix.as_str()))),
                "cards of another PC on screen: {ids:?}, allowed {allowed:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let until = |seconds: f64, what: &str, done: &dyn Fn() -> bool| -> f64 {
            let started = Instant::now();
            while !done() {
                assert!(
                    started.elapsed().as_secs_f64() < seconds,
                    "timed out after {seconds}s: {what} (status {:?}, cards {:?}, streams {}, live {})",
                    view.status.text(),
                    view.canvas.card_ids(),
                    view.canvas.live_streams(),
                    view.live.get()
                );
                pump();
            }
            started.elapsed().as_secs_f64()
        };
        let step = Cell::new(0);
        // `pumping: false` holds GTK still while the host acts, so an event the
        // action causes cannot be drawn before the next line runs.
        let ask = |action: serde_json::Value, pumping: bool| -> serde_json::Value {
            let n = step.get();
            step.set(n + 1);
            std::fs::write(
                control.join(format!("request-{n}.json")),
                serde_json::to_vec(&action).unwrap(),
            )
            .unwrap();
            let done = control.join(format!("done-{n}.json"));
            let started = Instant::now();
            while !done.exists() {
                assert!(started.elapsed() < Duration::from_secs(60), "host action {action}");
                if pumping {
                    pump();
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            std::thread::sleep(Duration::from_millis(20));
            serde_json::from_slice(&std::fs::read(done).unwrap()).unwrap()
        };
        let report = |row: &str, message: String| println!("TWO-PC {row}: {message}");
        let showing = |host: &Host| {
            view.canvas.card_ids() == {
                let mut ids = host.cards.clone();
                ids.sort();
                ids
            }
        };
        let streaming = |host: &Host| {
            showing(host)
                && view.live.get()
                && view.canvas.live_streams() == host.cards.len()
                && clients(host) == host.cards.len()
        };
        let released = |host: &Host| clients(host) == 0 && own_connections(host.port) == 0;
        let select = |host: &Host, label: &str| {
            *allowed.borrow_mut() = vec![format!("{}-", label.to_lowercase())];
            view.select(Some((host.id.clone(), format!("PC {label}"))));
        };

        // 1. Select A: its cards, one event subscription, one stream per card.
        select(&a, "A");
        let took = until(20.0, "A shown and streaming", &|| streaming(&a));
        report("Select a PC (GUI)", format!(
            "A's {} consoles drawn and streaming with a live subscription {took:.1}s after selecting it",
            a.cards.len()));

        // 2. A move made on A arrives as an event: the poll stays down.
        let moved = Instant::now();
        ask(serde_json::json!({"do": "host_move", "host": "a", "card": "a-card-1", "x": 900}), true);
        until(5.0, "host move drawn", &|| {
            assert!(view.live.get(), "the subscription stayed up");
            view.canvas.card_position("a-card-1").is_some_and(|(x, _)| x == 900)
        });
        report("Live events (GUI)", format!(
            "a move made on A was drawn {:.2}s later from the event stream, without polling",
            moved.elapsed().as_secs_f64()));

        // 3. Switch to B: A's cards go at once, A's streams and subscription
        //    are released, and nothing of A ever comes back on screen.
        select(&b, "B");
        assert_eq!(view.canvas.card_count(), 0, "A's cards left with the switch");
        let took = until(20.0, "B shown and streaming", &|| streaming(&b));
        let freed = until(3.0, "A released", &|| released(&a));
        report("Switch A→B (GUI)", format!(
            "B streaming {took:.1}s after the switch; A's tmux clients and sockets released \
             {freed:.2}s later; no A card ever drawn"));

        // 4. Keys typed into B's console reach B, never A.
        let marker = format!("SD_GUI_KEYS_{}", std::process::id());
        let card = view.canvas.card_widget("b-card-1").unwrap();
        card.remote_session()
            .unwrap()
            .input(format!("echo {marker}\r").as_bytes());
        until(5.0, "keys reached B", &|| {
            tmux(&b, &["capture-pane", "-p", "-t", "sd_term_b_1"]).contains(&marker)
        });
        for session in ["sd_term_a_1", "sd_term_a_2"] {
            assert!(!tmux(&a, &["capture-pane", "-p", "-t", session]).contains(&marker));
        }
        report("Keys reach only the selected PC", "typed bytes appeared in B's pane and in none of A's".into());

        // 5. Concurrent edit: B's own user moves the card while this view
        //    drags the revision it drew. The drop is refused with B's geometry
        //    and the card glides there, saying why.
        let drawn = view.canvas.card_revision("b-card-2").unwrap();
        ask(serde_json::json!({"do": "host_move", "host": "b", "card": "b-card-2", "x": 1100, "y": 420}),
            false);
        let before = view.canvas.card_position("b-card-2").unwrap();
        view.canvas.drag_card_in_view("b-card-2", (10.0, 10.0), (60.0, 90.0));
        assert_eq!(view.canvas.card_revision("b-card-2"), Some(drawn), "the drop used the drawn revision");
        until(5.0, "conflict shown on the card", &|| {
            view.canvas.card_widget("b-card-2").and_then(|card| card.notice_text()).as_deref()
                == Some("Changed on that PC · showing its layout")
        });
        until(3.0, "card at B's geometry", &|| {
            view.canvas.card_position("b-card-2") == Some((1100, 420))
        });
        assert!(view.canvas.card_revision("b-card-2").unwrap() > drawn);
        report("Drag from either machine (GUI)", format!(
            "a drop from {before:?} at revision {drawn} after B moved the card: conflict notice on \
             the card, card at B's (1100, 420), B's edit kept"));

        // 6. A console stream that drops while the subscription stays up is
        //    retried, not left saying "Reconnecting…" until the host changes.
        ask(serde_json::json!({"do": "detach_clients", "host": "b"}), true);
        until(5.0, "streams dropped", &|| view.canvas.live_streams() < b.cards.len());
        let took = until(15.0, "streams back without a host change", &|| {
            assert!(view.live.get(), "the subscription stayed up");
            streaming(&b)
        });
        report("Dropped console reconnects (GUI)", format!(
            "both of B's consoles re-attached {took:.1}s after their tmux clients were dropped, \
             with the event subscription up and no host change"));

        // 7. Back to A, then a burst of switches: only the last PC remains.
        select(&a, "A");
        until(20.0, "A again", &|| streaming(&a));
        until(3.0, "B released", &|| released(&b));
        view.select(Some((b.id.clone(), "PC B".into())));
        view.select(Some((a.id.clone(), "PC A".into())));
        view.select(Some((b.id.clone(), "PC B".into())));
        select(&a, "A");
        let took = until(20.0, "A after rapid switching", &|| streaming(&a));
        until(3.0, "B released after rapid switching", &|| released(&b));
        report("Rapid switching (GUI)", format!(
            "A→B→A→B→A in one frame settled on A {took:.1}s later; B never drawn, its clients and \
             sockets released"));

        // 8. Hide releases everything; show brings it back. (The overlay's
        //    hide unmaps the window and suspends the streams; its show maps it.)
        window.set_visible(false);
        view.suspend_streams();
        let freed = until(3.0, "hidden: A released", &|| {
            clients(&a) == 0 && own_connections(a.port) == 0
        });
        window.set_visible(true);
        let took = until(15.0, "shown again", &|| streaming(&a));
        report("Hide/show (GUI)", format!(
            "hide released A's clients and sockets in {freed:.2}s; show re-attached in {took:.1}s"));

        // 9. A's bridge restarts: the subscription and every console come back,
        //    and no command is sent again.
        let commands = ask(serde_json::json!({"do": "commands", "host": "a"}), true);
        let restarted = Instant::now();
        ask(serde_json::json!({"do": "restart_bridge", "host": "a", "down": 1.0}), true);
        until(30.0, "A back after its bridge restarted", &|| streaming(&a));
        let after = ask(serde_json::json!({"do": "commands", "host": "a"}), true);
        assert_eq!(commands["log"], after["log"], "no command replayed");
        report("Bridge restart (GUI)", format!(
            "subscription and both consoles back {:.1}s after the bridge went down; no command resent",
            restarted.elapsed().as_secs_f64()));

        // 10. A's daemon restarts: the new epoch is drawn, consoles return,
        //     and the new daemon never receives a replayed command.
        let restarted = Instant::now();
        ask(serde_json::json!({"do": "restart_daemon", "host": "a", "epoch": "a-epoch-gui"}), true);
        until(30.0, "A's new epoch", &|| {
            view.canvas.snapshot_epoch().as_deref() == Some("a-epoch-gui") && streaming(&a)
        });
        let after = ask(serde_json::json!({"do": "commands", "host": "a"}), true);
        assert_eq!(after["log"], serde_json::json!([]), "the new daemon got no command");
        report("Daemon restart (GUI)", format!(
            "new epoch drawn and consoles streaming {:.1}s after the daemon restarted; it received no command",
            restarted.elapsed().as_secs_f64()));

        // 11. An old host without workspace-events-v1 is polled instead.
        select(&c, "C");
        until(20.0, "old host shown", &|| showing(&c) && view.canvas.live_streams() == c.cards.len());
        assert!(!view.live.get() && view.events.borrow().is_none(), "no subscription to an old host");
        let moved = Instant::now();
        ask(serde_json::json!({"do": "host_move", "host": "c", "card": "c-card-1", "x": 777}), true);
        until(5.0, "old host polled", &|| {
            view.canvas.card_position("c-card-1").is_some_and(|(x, _)| x == 777)
        });
        assert!(view.events.borrow().is_none());
        report("Old host fallback (GUI)", format!(
            "no subscription; a move on the old host was drawn {:.1}s later by the two-second poll",
            moved.elapsed().as_secs_f64()));

        // 12. Revocation ends everything promptly and says why.
        select(&a, "A");
        until(20.0, "A before revocation", &|| streaming(&a));
        let revoked = Instant::now();
        ask(serde_json::json!({"do": "revoke", "host": "a"}), true);
        let ended = until(2.0, "revoked streams released on A", &|| clients(&a) == 0);
        until(6.0, "revocation explained", &|| {
            view.status.text() == "Disconnected"
                && view.status.tooltip_text().as_deref() == Some("Pairing required · Add this PC again")
                && view.canvas.card_count() == 0
        });
        until(3.0, "no socket kept to a revoked host", &|| own_connections(a.port) == 0);
        report("Revoke (GUI)", format!(
            "A reaped this viewer's consoles {ended:.2}s after revocation; the view cleared and said \
             “Pairing required” {:.1}s after", revoked.elapsed().as_secs_f64()));
        window.close();
    }
}
