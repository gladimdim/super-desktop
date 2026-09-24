//! Connections pages of the settings card in the overlay HUD.
//!
//! The overlay is a single layer-shell surface (never a separate Hyprland
//! window), and the ⚙ card is a settings hub. Connections is a short overview
//! (this computer's bridge, a waiting request, and one entry per kind of
//! connection) with a focused sub-page for each:
//!
//! * Add a device — pair a phone, share this PC, or view another PC;
//! * the invitation flow (`pairing_invite`) that pairing and sharing use;
//! * PCs — remote PCs this one can open, and PCs with access to it;
//! * Phones & other devices — mobile clients with access to this PC;
//! * Rejected devices — blocked from asking again until removed;
//! * Network & firewall — addresses, port and the UFW rule.
//!
//! Requests are never decided here: every "Review" opens the approval panel
//! (`pairing_request_ui`). The card chrome, the header and the navigation live
//! in `harness_settings`. Every colour/radius/padding lives in `styles.rs` —
//! this file only builds the widgets.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation, PolicyType, ScrolledWindow, Separator};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::bridge;
use crate::harness_settings::settings_entry_with_summary;
pub use crate::pairing_invite::InviteKind;
use crate::pairing_invite::InviteFlow;
use crate::pairing_request_ui::{bridge_error_message, device_kind, RequestHooks};

/// The Connections destinations of the settings card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPage {
    Overview,
    AddDevice,
    Invite,
    Pcs,
    Phones,
    Rejected,
    Network,
}

impl ConnectionPage {
    #[cfg(test)]
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::AddDevice,
        Self::Invite,
        Self::Pcs,
        Self::Phones,
        Self::Rejected,
        Self::Network,
    ];

    /// Where ← goes; `None` is the settings hub.
    pub fn parent(self) -> Option<Self> {
        match self {
            Self::Overview => None,
            Self::Invite => Some(Self::AddDevice),
            _ => Some(Self::Overview),
        }
    }

    /// Badge, title and subtitle for the card header. The invitation page is
    /// titled by what it invites (see `ConnectionPages::header`).
    fn header(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Overview => ("⇄", "Connections", "Devices · encrypted connections"),
            Self::AddDevice => ("＋", "Add a device", "Every new device needs your approval here"),
            Self::Invite => ("＋", "Pair a device", "Single-use invitation"),
            Self::Pcs => ("▭", "PCs", "Remote workspaces and PCs with access"),
            Self::Phones => ("▯", "Phones & other devices", "Devices with access to this PC"),
            Self::Rejected => ("⊘", "Rejected devices", "They can't ask to connect again"),
            Self::Network => ("⇅", "Network & firewall", "Addresses, port and firewall"),
        }
    }
}

/// What the Connections pages need from the rest of the overlay.
#[derive(Clone)]
pub struct ConnectionHooks {
    pub requests: RequestHooks,
    /// Open the Add-a-PC wizard on its "connect to another PC" page.
    pub connect_to_pc: Rc<dyn Fn()>,
}

impl ConnectionHooks {
    /// For pages built without an overlay.
    #[cfg(test)]
    pub fn inert() -> Self {
        Self {
            requests: RequestHooks::inert(),
            connect_to_pc: Rc::new(|| {}),
        }
    }
}

/// The Connections destinations, each a scrolling page for the settings card.
pub struct ConnectionPages {
    pages: Vec<(ConnectionPage, gtk4::Widget)>,
    /// Re-read the bridge, devices and network on worker threads. Runs on
    /// every navigation here and every 2 s while one of these pages is shown.
    pub refresh: Rc<dyn Fn()>,
    pub invite: Rc<InviteFlow>,
}

impl ConnectionPages {
    #[cfg(test)]
    pub fn widget(&self, page: ConnectionPage) -> &gtk4::Widget {
        &self.pages.iter().find(|(p, _)| *p == page).expect("every page is built").1
    }

    pub fn widgets(&self) -> impl Iterator<Item = (ConnectionPage, &gtk4::Widget)> {
        self.pages.iter().map(|(page, widget)| (*page, widget))
    }

    pub fn header(&self, page: ConnectionPage) -> (&'static str, &'static str, &'static str) {
        match page {
            ConnectionPage::Invite => {
                let kind = self.invite.kind();
                ("＋", kind.title(), kind.subtitle())
            }
            _ => page.header(),
        }
    }
}

struct Snapshot {
    online: bool,
    harnesses: usize,
    firewall: (String, bool),
    host: String,
    lan: String,
    tailscale: Option<String>,
    mdns: String,
    pending: Vec<Value>,
    devices: Vec<Value>,
    pcs: Vec<Value>,
    rejected: Vec<Value>,
}

impl Snapshot {
    fn collect() -> Self {
        let online = bridge::bridge_running(bridge::BRIDGE_PORT);
        Self {
            online,
            harnesses: if online {
                bridge::collect_harnesses().len()
            } else {
                0
            },
            firewall: bridge::firewall_summary(),
            host: bridge::hostname(),
            lan: bridge::lan_ip(),
            tailscale: bridge::tailscale_ip(),
            mdns: bridge::mdns_summary(online),
            pending: if online {
                bridge::pending_requests()
            } else {
                vec![]
            },
            devices: bridge::paired_devices(),
            pcs: crate::peer_store::PeerStore::default_store()
                .and_then(|store| store.peers()).unwrap_or_default().iter()
                .map(|peer| serde_json::to_value(peer.summary()).unwrap_or_default()).collect(),
            rejected: bridge::rejected_devices(),
        }
    }
}

/// At most one probe runs at a time. Requests arriving during it trigger a
/// follow-up, so a slow old probe cannot leave a completed action stale.
pub(crate) fn background_refresh<T: Send + 'static>(
    collect: impl Fn() -> T + Send + Sync + 'static,
    apply: impl Fn(T) + 'static,
) -> Rc<dyn Fn()> {
    let collect = std::sync::Arc::new(collect);
    let apply = Rc::new(apply);
    let running = Rc::new(Cell::new(false));
    let requested = Rc::new(Cell::new(false));
    Rc::new(move || {
        requested.set(true);
        if running.replace(true) {
            return;
        }
        let collect = std::sync::Arc::clone(&collect);
        let apply = Rc::clone(&apply);
        let running = Rc::clone(&running);
        let requested = Rc::clone(&requested);
        glib::MainContext::default().spawn_local(async move {
            while requested.replace(false) {
                let collect = std::sync::Arc::clone(&collect);
                if let Ok(snapshot) = gtk4::gio::spawn_blocking(move || collect()).await {
                    apply(snapshot);
                }
            }
            running.set(false);
        });
    })
}

fn set_text(label: &Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
    }
}

fn set_state_class(label: &Label, active: bool, yes: &str, no: &str) {
    let (add, remove) = if active { (yes, no) } else { (no, yes) };
    if !label.has_css_class(add) {
        label.remove_css_class(remove);
        label.add_css_class(add);
    }
}

/// Each destination scrolls on its own; horizontal scrolling would only clip
/// the panels, so labels wrap.
fn page_scroll(content: &Box) -> ScrolledWindow {
    let scroll = ScrolledWindow::new();
    scroll.add_css_class("launcher-scroll");
    scroll.add_css_class("harness-page");
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(content));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll
}

fn page_root() -> Box {
    let root = Box::new(Orientation::Vertical, 10);
    root.add_css_class("launcher-body");
    root.add_css_class("android-page");
    root
}

fn hint(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("launcher-hint");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}

/// Where a page reports the outcome of its last action (revoke, remove).
struct ActionNote(Label);

impl ActionNote {
    fn new(parent: &Box) -> Rc<Self> {
        let label = Label::new(None);
        label.add_css_class("launcher-note");
        label.add_css_class("connections-action-note");
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_visible(false);
        parent.append(&label);
        Rc::new(Self(label))
    }

    fn say(&self, text: &str, error: bool) {
        self.0.set_text(text);
        self.0.set_visible(true);
        if error {
            self.0.add_css_class("launcher-note-error");
        } else {
            self.0.remove_css_class("launcher-note-error");
        }
    }
}

type Slot = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

fn call(slot: &Slot) {
    let f = slot.borrow().clone();
    if let Some(f) = f {
        f();
    }
}

/// A button that runs `op` on a worker thread, reports the outcome in `note`
/// and refreshes the pages. The label names the action; `busy` while it runs.
fn action_button(
    label: &'static str,
    busy: &'static str,
    danger: bool,
    op: impl Fn() -> Result<(), String> + Send + Sync + 'static,
    done: String,
    failed: String,
    note: &Rc<ActionNote>,
    refresh: &Slot,
) -> Button {
    let button = Button::with_label(label);
    button.add_css_class("launcher-btn");
    if danger {
        button.add_css_class("launcher-btn-danger");
    }
    let op = std::sync::Arc::new(op);
    let note = Rc::clone(note);
    let refresh = Rc::clone(refresh);
    button.connect_clicked(move |button| {
        button.set_sensitive(false);
        button.set_label(busy);
        let button = button.clone();
        let op = std::sync::Arc::clone(&op);
        let note = Rc::clone(&note);
        let refresh = Rc::clone(&refresh);
        let (done, failed) = (done.clone(), failed.clone());
        glib::MainContext::default().spawn_local(async move {
            match gtk4::gio::spawn_blocking(move || op()).await {
                Ok(Ok(())) => {
                    note.say(&done, false);
                    call(&refresh);
                }
                Ok(Err(code)) => {
                    note.say(&format!("{failed} {}", bridge_error_message(&code)), true);
                    button.set_label(label);
                    button.set_sensitive(true);
                }
                Err(_) => {
                    note.say(&format!("{failed} {}", bridge_error_message("bridge_not_responding")), true);
                    button.set_label(label);
                    button.set_sensitive(true);
                }
            }
        });
    });
    button
}

/// Builds every Connections destination. `navigate` moves the settings card
/// to another one of them.
pub fn build_connection_pages(
    hooks: ConnectionHooks,
    navigate: Rc<dyn Fn(ConnectionPage)>,
) -> ConnectionPages {
    let refresh_slot: Slot = Rc::new(RefCell::new(None));
    let invite = InviteFlow::new(hooks.requests.clone());
    let start_invite: Rc<dyn Fn(InviteKind)> = {
        let invite = Rc::clone(&invite);
        let navigate = Rc::clone(&navigate);
        Rc::new(move |kind| {
            invite.start(kind);
            navigate(ConnectionPage::Invite);
        })
    };

    // ================= Overview =================
    let overview = page_root();
    overview.add_css_class("connections-overview");
    let (head, body) = section_card(&overview, "", "This computer");
    let v_bridge_state = chip("Checking…");
    head.append(&v_bridge_state);

    let status = Label::new(Some("Checking the bridge in the background…"));
    status.add_css_class("launcher-status-text");
    status.set_xalign(0.0);
    status.set_wrap(true);
    body.append(&status);

    let controls = Box::new(Orientation::Horizontal, 8);
    controls.add_css_class("launcher-actions");
    let btn_start = Button::with_label("Start bridge");
    btn_start.set_tooltip_text(Some("Launch super-desktop harness-bridge on :8759"));
    btn_start.add_css_class("launcher-btn");
    btn_start.add_css_class("launcher-btn-primary");
    let btn_stop = Button::with_label("Stop");
    btn_start.set_visible(false);
    btn_stop.set_visible(false);
    btn_stop.set_tooltip_text(Some("Stop the local harness bridge"));
    btn_stop.add_css_class("launcher-btn");
    btn_stop.add_css_class("launcher-btn-danger");
    controls.append(&btn_start);
    controls.append(&btn_stop);
    body.append(&controls);

    body.append(&hint("Encrypted end to end · Wi-Fi or Tailscale · Automatically recovers while SUPER DESKTOP is running"));

    // Outcome of the last start/stop. Failures used to go to the stderr of a
    // daemon nobody reads, which reads as "the button does nothing".
    let bridge_note = Label::new(None);
    bridge_note.add_css_class("launcher-note");
    bridge_note.set_xalign(0.0);
    bridge_note.set_wrap(true);
    bridge_note.set_selectable(true);
    bridge_note.set_visible(false);
    body.append(&bridge_note);

    // A waiting request is decided in the approval panel, never down here.
    let pending_banner = Box::new(Orientation::Horizontal, 10);
    pending_banner.add_css_class("connections-pending");
    pending_banner.set_visible(false);
    let pending_text = Label::new(None);
    pending_text.add_css_class("connections-pending-text");
    pending_text.set_xalign(0.0);
    pending_text.set_wrap(true);
    pending_text.set_hexpand(true);
    pending_banner.append(&pending_text);
    let btn_review = Button::with_label("Review");
    btn_review.add_css_class("launcher-btn");
    btn_review.add_css_class("launcher-btn-primary");
    btn_review.set_valign(Align::Center);
    let review = Rc::clone(&hooks.requests.review);
    btn_review.connect_clicked(move |_| review());
    pending_banner.append(&btn_review);
    overview.append(&pending_banner);

    let (btn_add, _, _) = settings_entry_with_summary(
        "＋",
        "Add a device",
        "Pair a phone, share this PC with another PC, or view another PC from here.",
        "connections-add-entry",
    );
    overview.append(&btn_add);
    let (btn_pcs, _, pcs_trailing) = settings_entry_with_summary(
        "▭",
        "PCs",
        "Remote PCs you can open, and PCs with access to this one.",
        "connections-pcs-entry",
    );
    let pcs_count = chip("…");
    pcs_count.add_css_class("android-connection-count");
    pcs_trailing.prepend(&pcs_count);
    overview.append(&btn_pcs);
    let (btn_phones, _, phones_trailing) = settings_entry_with_summary(
        "▯",
        "Phones & other devices",
        "Phones and other devices with access to this PC.",
        "connections-phones-entry",
    );
    let phones_count = chip("…");
    phones_count.add_css_class("android-connection-count");
    phones_count.set_tooltip_text(Some(
        "Active / registered. Active means connected or seen in the last 60 seconds.",
    ));
    phones_trailing.prepend(&phones_count);
    overview.append(&btn_phones);
    let (btn_rejected, _, rejected_trailing) = settings_entry_with_summary(
        "⊘",
        "Rejected devices",
        "Devices you rejected. They can't ask to connect again until you remove them.",
        "connections-rejected-entry",
    );
    let rejected_count = chip("…");
    rejected_count.add_css_class("android-connection-count");
    rejected_trailing.prepend(&rejected_count);
    overview.append(&btn_rejected);
    let (btn_network, network_summary, network_trailing) = settings_entry_with_summary(
        "⇅",
        "Network & firewall",
        "Addresses, port and firewall.",
        "connections-network-entry",
    );
    let network_warning = chip("Needs attention");
    network_warning.add_css_class("connections-warning-chip");
    network_warning.set_visible(false);
    network_trailing.prepend(&network_warning);
    overview.append(&btn_network);

    for (button, page) in [
        (&btn_add, ConnectionPage::AddDevice),
        (&btn_pcs, ConnectionPage::Pcs),
        (&btn_phones, ConnectionPage::Phones),
        (&btn_rejected, ConnectionPage::Rejected),
        (&btn_network, ConnectionPage::Network),
    ] {
        let navigate = Rc::clone(&navigate);
        button.connect_clicked(move |_| navigate(page));
    }

    let footer = Label::new(Some("Only devices you approve can access your terminals."));
    footer.add_css_class("launcher-footer");
    footer.set_xalign(0.5);
    footer.set_wrap(true);
    overview.append(&footer);

    // ================= Add a device =================
    let add = page_root();
    let add_intro = Label::new(Some(
        "What do you want to connect? Every new device asks for your approval on this PC, with a code to compare.",
    ));
    add_intro.add_css_class("launcher-status-text");
    add_intro.set_xalign(0.0);
    add_intro.set_wrap(true);
    add.append(&add_intro);
    let (btn_phone, _, _) = settings_entry_with_summary(
        "▯",
        "Pair a phone",
        "Show a QR code for SUPER DESKTOP on Android, so the phone can use this PC's harnesses.",
        "connections-invite-phone",
    );
    add.append(&btn_phone);
    let (btn_share, _, _) = settings_entry_with_summary(
        "▭",
        "Share this PC with another PC",
        "Create a link for another SUPER DESKTOP PC, so it can use this PC's harnesses.",
        "connections-invite-pc",
    );
    add.append(&btn_share);
    let (btn_view, _, _) = settings_entry_with_summary(
        "↗",
        "View another PC from here",
        "Paste a link from another PC to use its harnesses on this one.",
        "connections-connect-pc",
    );
    add.append(&btn_view);
    add.append(&hint("Devices you reject stay blocked until you remove them from Rejected devices."));
    for (button, kind) in [(&btn_phone, InviteKind::Phone), (&btn_share, InviteKind::Pc)] {
        let start_invite = Rc::clone(&start_invite);
        button.connect_clicked(move |_| start_invite(kind));
    }
    let connect_to_pc = Rc::clone(&hooks.connect_to_pc);
    btn_view.connect_clicked(move |_| connect_to_pc());

    // ================= Invitation =================
    let invite_page = page_root();
    invite_page.append(&invite.widget);

    // ================= PCs =================
    let pcs_page = page_root();
    let (outgoing_head, outgoing_body) = section_card(&pcs_page, "", "Open from this PC");
    let outgoing_count = chip("0");
    outgoing_head.append(&outgoing_count);
    let outgoing_rows = Box::new(Orientation::Vertical, 0);
    outgoing_body.append(&outgoing_rows);
    let outgoing_empty = empty_state(
        &outgoing_body,
        "No remote PCs yet. Get a link from the other PC, then paste it here.",
        "View another PC",
    );
    let connect_to_pc = Rc::clone(&hooks.connect_to_pc);
    outgoing_empty.1.connect_clicked(move |_| connect_to_pc());

    let (incoming_head, incoming_body) = section_card(&pcs_page, "", "Can open this PC");
    let incoming_count = chip("0");
    incoming_head.append(&incoming_count);
    let incoming_rows = Box::new(Orientation::Vertical, 0);
    incoming_body.append(&incoming_rows);
    let incoming_empty = empty_state(
        &incoming_body,
        "No other PC has access to this one.",
        "Share this PC",
    );
    let share = Rc::clone(&start_invite);
    incoming_empty.1.connect_clicked(move |_| share(InviteKind::Pc));
    pcs_page.append(&hint("Revoking disconnects that PC now. It needs a new invitation to connect again."));
    let pcs_note = ActionNote::new(&pcs_page);

    // ================= Phones & other devices =================
    let phones_page = page_root();
    let mut phone_groups = Vec::new();
    for (key, title) in [
        ("android", "Android devices"),
        ("mobile", "Other mobile devices"),
        ("unknown", "Other devices"),
    ] {
        let (head, body) = section_card(&phones_page, "", title);
        let count = chip("0");
        head.append(&count);
        let rows = Box::new(Orientation::Vertical, 0);
        body.append(&rows);
        let card = body.parent().expect("section body sits in its card");
        phone_groups.push((key, card, count, rows));
    }
    let phones_empty = empty_state(&phones_page, "No phones are paired with this PC yet.", "Pair a phone");
    let pair_phone = Rc::clone(&start_invite);
    phones_empty.1.connect_clicked(move |_| pair_phone(InviteKind::Phone));
    phones_page.append(&hint("Removing a PC on the phone does not revoke it here. Revoke it on this PC to cut its access."));
    let phones_note = ActionNote::new(&phones_page);

    // ================= Rejected devices =================
    let rejected_page = page_root();
    rejected_page.append(&hint(
        "A device you reject can't ask to connect again. SUPER DESKTOP PCs are recognised by their installation; other devices by name and network address. Remove a device to let it send a new request.",
    ));
    let (rejected_head, rejected_body) = section_card(&rejected_page, "", "Rejected devices");
    let rejected_list_count = chip("0");
    rejected_head.append(&rejected_list_count);
    let rejected_rows = Box::new(Orientation::Vertical, 0);
    rejected_body.append(&rejected_rows);
    let rejected_empty = Label::new(Some("No rejected devices."));
    rejected_empty.add_css_class("android-empty");
    rejected_empty.set_xalign(0.0);
    rejected_body.append(&rejected_empty);
    let rejected_note = ActionNote::new(&rejected_page);

    // ================= Network & firewall =================
    let network = page_root();
    let (_, body) = section_card(&network, "", "Firewall");
    let fw_row = Box::new(Orientation::Horizontal, 10);
    fw_row.add_css_class("launcher-row");
    let v_fw = Label::new(None);
    v_fw.add_css_class("launcher-status-text");
    v_fw.set_xalign(0.0);
    v_fw.set_hexpand(true);
    v_fw.set_wrap(true);
    fw_row.append(&v_fw);
    let btn_fw = Button::with_label("Allow connection");
    btn_fw.set_tooltip_text(Some("Allow 8759/tcp via a password prompt"));
    btn_fw.add_css_class("launcher-btn");
    btn_fw.add_css_class("launcher-btn-primary");
    btn_fw.set_valign(Align::Center);
    fw_row.append(&btn_fw);
    let btn_fw_weak = btn_fw.downgrade();
    body.append(&fw_row);
    let v_fw_note = Label::new(None);
    v_fw_note.add_css_class("launcher-note");
    v_fw_note.set_xalign(0.0);
    v_fw_note.set_wrap(true);
    v_fw_note.set_selectable(true);
    body.append(&v_fw_note);

    let (_, body) = section_card(&network, "", "Network addresses");
    let v_host = kv(&body, "Computer");
    body.append(&row_sep());
    let v_lan = kv(&body, "LAN IP");
    body.append(&row_sep());
    let v_tail = kv(&body, "Tailscale IP");
    body.append(&row_sep());
    let v_port = kv(&body, "Port");
    body.append(&row_sep());
    let v_mdns = kv(&body, "mDNS");

    let pages: Vec<(ConnectionPage, gtk4::Widget)> = [
        (ConnectionPage::Overview, &overview),
        (ConnectionPage::AddDevice, &add),
        (ConnectionPage::Invite, &invite_page),
        (ConnectionPage::Pcs, &pcs_page),
        (ConnectionPage::Phones, &phones_page),
        (ConnectionPage::Rejected, &rejected_page),
        (ConnectionPage::Network, &network),
    ]
    .into_iter()
    .map(|(page, root)| (page, page_scroll(root).upcast()))
    .collect();

    // System commands, sockets and disk reads never run on the GTK thread.
    let firewall_busy = Rc::new(Cell::new(false));
    let firewall_refresh_busy = Rc::clone(&firewall_busy);
    let start_weak = btn_start.downgrade();
    let stop_weak = btn_stop.downgrade();
    let alive = overview.downgrade();
    let previous_pcs: RefCell<Option<(Vec<Value>, Vec<Value>)>> = RefCell::new(None);
    let previous_phones: RefCell<Option<Vec<Value>>> = RefCell::new(None);
    let previous_rejected: RefCell<Option<Vec<Value>>> = RefCell::new(None);
    let rows_refresh = Rc::clone(&refresh_slot);
    let refresh = background_refresh(Snapshot::collect, move |snapshot| {
        if alive.upgrade().is_none() {
            return;
        }
        // ---- overview ----
        set_state_class(
            &v_bridge_state,
            snapshot.online,
            "launcher-online",
            "launcher-offline",
        );
        if let Some(start) = start_weak.upgrade() {
            start.set_visible(!snapshot.online);
        }
        if let Some(stop) = stop_weak.upgrade() {
            stop.set_visible(snapshot.online);
        }
        if snapshot.online {
            let n = snapshot.harnesses;
            set_text(&v_bridge_state, "● ONLINE");
            set_text(
                &status,
                &format!(
                    "{} · {n} terminal{} available",
                    snapshot.host,
                    if n == 1 { "" } else { "s" }
                ),
            );
        } else {
            set_text(&v_bridge_state, "○ OFFLINE");
            set_text(&status, "Start the bridge to let paired devices connect.");
        }
        let waiting = snapshot.pending.len();
        pending_banner.set_visible(waiting > 0);
        set_text(
            &pending_text,
            &match waiting {
                1 => "1 connection request is waiting for your decision.".to_string(),
                n => format!("{n} connection requests are waiting for your decision."),
            },
        );
        let (fw_text, fw_can_unlock) = snapshot.firewall.clone();
        network_warning.set_visible(fw_can_unlock);
        set_text(
            &network_summary,
            &if fw_can_unlock {
                format!("{fw_text}. Other devices can't reach this PC.")
            } else {
                format!("{} · port {} · {}", snapshot.lan, bridge::BRIDGE_PORT, fw_text.trim_end_matches(" ✓"))
            },
        );
        let is_type = |d: &&Value, key: &str| connection_group(d) == key;
        let pc_devices: Vec<Value> = snapshot.devices.iter().filter(|d| is_type(d, "pc")).cloned().collect();
        let phone_devices: Vec<Value> = snapshot.devices.iter().filter(|d| !is_type(d, "pc")).cloned().collect();
        set_text(&pcs_count, &(snapshot.pcs.len() + pc_devices.len()).to_string());
        let active = phone_devices.iter().filter(|d| d["active"] == true).count();
        set_text(&phones_count, &format!("{active}/{}", phone_devices.len()));
        set_text(&rejected_count, &snapshot.rejected.len().to_string());

        // ---- network ----
        set_text(&v_fw, &fw_text);
        if let Some(b) = btn_fw_weak.upgrade() {
            b.set_visible(fw_can_unlock);
            b.set_sensitive(fw_can_unlock && !firewall_refresh_busy.get());
        }
        set_text(&v_host, &snapshot.host);
        set_text(&v_lan, &snapshot.lan);
        set_text(
            &v_tail,
            snapshot.tailscale.as_deref().unwrap_or("— (Tailscale off)"),
        );
        set_text(&v_port, &bridge::BRIDGE_PORT.to_string());
        set_text(&v_mdns, &snapshot.mdns);

        // ---- PCs ----
        let pcs_data = (snapshot.pcs.clone(), pc_devices.clone());
        if previous_pcs.borrow().as_ref() != Some(&pcs_data) {
            clear(&outgoing_rows);
            set_text(&outgoing_count, &snapshot.pcs.len().to_string());
            outgoing_empty.0.set_visible(snapshot.pcs.is_empty());
            for pc in &snapshot.pcs {
                let host = pc["endpoint"]["host"].as_str().unwrap_or("");
                let port = pc["endpoint"]["port"].as_u64().unwrap_or(0);
                let (row, actions) = device_row(
                    pc["label"].as_str().unwrap_or("PC"),
                    &format!("Remote workspace · {host}:{port} · open it from the machine selector"),
                );
                let expired = pc["expired"] == true;
                actions.append(&state_chip(if expired { "Expired" } else { "Paired" }, !expired));
                outgoing_rows.append(&row);
            }
            clear(&incoming_rows);
            set_text(&incoming_count, &pc_devices.len().to_string());
            incoming_empty.0.set_visible(pc_devices.is_empty());
            for device in &pc_devices {
                incoming_rows.append(&access_row(device, &pcs_note, &rows_refresh));
            }
            *previous_pcs.borrow_mut() = Some(pcs_data);
        }

        // ---- phones & other devices ----
        if previous_phones.borrow().as_ref() != Some(&phone_devices) {
            for (key, card, count, rows) in &phone_groups {
                clear(rows);
                let devices: Vec<_> = phone_devices.iter().filter(|d| connection_group(d) == *key).collect();
                // Android always has its group; uncommon types appear when present.
                card.set_visible(!devices.is_empty() || (*key == "android" && !phone_devices.is_empty()));
                set_text(count, &devices.len().to_string());
                for device in devices {
                    rows.append(&access_row(device, &phones_note, &rows_refresh));
                }
            }
            phones_empty.0.set_visible(phone_devices.is_empty());
            *previous_phones.borrow_mut() = Some(phone_devices);
        }

        // ---- rejected ----
        if previous_rejected.borrow().as_ref() != Some(&snapshot.rejected) {
            clear(&rejected_rows);
            set_text(&rejected_list_count, &snapshot.rejected.len().to_string());
            rejected_empty.set_visible(snapshot.rejected.is_empty());
            for device in &snapshot.rejected {
                rejected_rows.append(&rejected_row(device, &rejected_note, &rows_refresh));
            }
            *previous_rejected.borrow_mut() = Some(snapshot.rejected);
        }
    });
    *refresh_slot.borrow_mut() = Some(Rc::clone(&refresh));

    // Start/stop wait for the child (up to ~5s) and are therefore run on a
    // worker thread, with the outcome shown in `bridge_note`. Calling them
    // straight from the click handler froze the overlay AND hid every error.
    let bridge_busy = Rc::new(Cell::new(false));
    let run_bridge_action = {
        let refresh = Rc::clone(&refresh);
        let note = bridge_note.clone();
        let btn_start = btn_start.clone();
        let btn_stop = btn_stop.clone();
        let busy = Rc::clone(&bridge_busy);
        move |progress: &str, op: fn() -> Result<(), String>| {
            if busy.get() {
                return;
            }
            busy.set(true);
            btn_start.set_sensitive(false);
            btn_stop.set_sensitive(false);
            note.remove_css_class("launcher-note-error");
            note.set_text(progress);
            note.set_visible(true);

            let refresh = Rc::clone(&refresh);
            let note = note.clone();
            let btn_start = btn_start.clone();
            let btn_stop = btn_stop.clone();
            let busy = Rc::clone(&busy);
            glib::MainContext::default().spawn_local(async move {
                match gtk4::gio::spawn_blocking(op).await {
                    Ok(Ok(())) => {
                        note.set_visible(false);
                    }
                    Ok(Err(e)) => {
                        note.add_css_class("launcher-note-error");
                        note.set_text(&format!("⚠ {e}"));
                    }
                    Err(_) => {
                        note.add_css_class("launcher-note-error");
                        note.set_text("⚠ bridge action did not finish");
                    }
                }
                busy.set(false);
                btn_start.set_sensitive(true);
                btn_stop.set_sensitive(true);
                refresh();
            });
        }
    };

    btn_start.connect_clicked({
        let run = run_bridge_action.clone();
        move |_| {
            run(
                &format!("… starting the bridge on :{}", bridge::BRIDGE_PORT),
                bridge::start_bridge,
            )
        }
    });
    btn_stop.connect_clicked({
        let run = run_bridge_action.clone();
        move |_| run("… stopping the bridge", bridge::stop_bridge)
    });
    btn_fw.connect_clicked({
        let refresh = Rc::clone(&refresh);
        let note = v_fw_note.clone();
        move |btn| {
            if firewall_busy.replace(true) {
                return;
            }
            btn.set_sensitive(false);
            note.set_text("Waiting for the password prompt…");
            let note2 = note.clone();
            let refresh2 = Rc::clone(&refresh);
            let busy = Rc::clone(&firewall_busy);
            glib::MainContext::default().spawn_local(async move {
                let msg = match gtk4::gio::spawn_blocking(bridge::unlock_firewall).await {
                    Ok(Ok(msg)) => msg,
                    Ok(Err(error)) => format!("Unlock failed: {error}"),
                    Err(_) => "Unlock failed".into(),
                };
                busy.set(false);
                note2.set_text(&msg);
                refresh2();
            });
        }
    });

    // Deliberately no eager `refresh()` here: it probes tmux (one exec per live
    // session), `tailscale` (~100ms) and `hostname`, and these pages are built
    // on the settings card's first open. The content is filled when the card
    // navigates here (`refresh`) and then every 2s while a page stays on screen.
    //
    // `is_mapped` is false both when the card navigated away from these pages
    // and when the card (or the whole overlay) is hidden, so nothing probes
    // tmux / `tailscale` / `hostname` for a page nobody is looking at.
    let watched: Vec<glib::WeakRef<gtk4::Widget>> =
        pages.iter().map(|(_, widget)| widget.downgrade()).collect();
    let tick = Rc::clone(&refresh);
    glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        let mut alive = false;
        let mut shown = false;
        for page in &watched {
            if let Some(page) = page.upgrade() {
                alive = true;
                shown |= page.is_mapped();
            }
        }
        if !alive {
            return glib::ControlFlow::Break;
        }
        if shown {
            tick();
        }
        glib::ControlFlow::Continue
    });

    ConnectionPages {
        pages,
        refresh,
        invite,
    }
}

fn clear(rows: &Box) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
}

/// An explanation for an empty list, and the button that fills it.
fn empty_state(parent: &Box, text: &str, action: &str) -> (Box, Button) {
    let area = Box::new(Orientation::Vertical, 8);
    area.add_css_class("connections-empty");
    let label = Label::new(Some(text));
    label.add_css_class("android-empty");
    label.set_xalign(0.0);
    label.set_wrap(true);
    area.append(&label);
    let button = Button::with_label(action);
    button.add_css_class("launcher-btn");
    button.add_css_class("launcher-btn-primary");
    button.set_halign(Align::Start);
    area.append(&button);
    area.set_visible(false);
    parent.append(&area);
    (area, button)
}

fn connection_group(device: &Value) -> &str {
    match device["deviceType"].as_str() {
        Some("pc") => "pc",
        Some("android") => "android",
        Some("mobile") => "mobile",
        _ => "unknown",
    }
}

fn state_chip(state: &str, active: bool) -> Label {
    let badge = chip(state);
    badge.add_css_class(if active { "launcher-online" } else { "launcher-offline" });
    badge
}

/// A device with access to this PC: its state and a Revoke button.
fn access_row(device: &Value, note: &Rc<ActionNote>, refresh: &Slot) -> Box {
    let name = device["name"].as_str().unwrap_or("Device").to_string();
    let active = device["active"] == true;
    let expires = device["expires"]
        .as_f64()
        .map(|at| format!(" · access until {}", short_date(at)))
        .unwrap_or_default();
    let detail = if connection_group(device) == "unknown" {
        format!("Access to this PC · type not reported{expires}")
    } else {
        format!("Access to this PC{expires}")
    };
    let (row, actions) = device_row(&name, &detail);
    actions.append(&state_chip(if active { "● Active" } else { "Offline" }, active));
    let id = device["id"].as_str().unwrap_or("").to_string();
    let revoke = action_button(
        "Revoke",
        "Revoking…",
        true,
        move || bridge::revoke_device(&id),
        format!("“{name}” can no longer connect to this PC."),
        format!("Could not revoke “{name}”."),
        note,
        refresh,
    );
    revoke.set_tooltip_text(Some("Revoke this device’s access to this computer"));
    actions.append(&revoke);
    row
}

/// A rejected device: what it reported, when, and a Remove button.
fn rejected_row(device: &Value, note: &Rc<ActionNote>, refresh: &Slot) -> Box {
    let name = device["name"].as_str().unwrap_or("Device").to_string();
    let (_, kind) = device_kind(device["deviceType"].as_str().unwrap_or(""));
    let address = device["address"]
        .as_str()
        .filter(|a| !a.is_empty())
        .unwrap_or("unknown address");
    let when = device["rejectedAt"]
        .as_f64()
        .map(|at| format!(" · rejected {}", short_date(at)))
        .unwrap_or_default();
    let (row, actions) = device_row(&name, &format!("{kind} · {address}{when}"));
    let id = device["id"].as_str().unwrap_or("").to_string();
    let remove = action_button(
        "Remove",
        "Removing…",
        false,
        move || bridge::forget_rejected(&id),
        format!("“{name}” can ask to connect again."),
        format!("Could not remove “{name}”."),
        note,
        refresh,
    );
    remove.set_tooltip_text(Some("Let this device ask to connect again"));
    actions.append(&remove);
    row
}

fn short_date(epoch: f64) -> String {
    chrono::DateTime::from_timestamp(epoch as i64, 0)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%-d %b %Y, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "an unknown date".into())
}

fn device_row(name: &str, detail: &str) -> (Box, Box) {
    let row = Box::new(Orientation::Horizontal, 12);
    row.add_css_class("connections-device");
    let labels = Box::new(Orientation::Vertical, 4);
    labels.set_hexpand(true);
    let title = Label::new(Some(name));
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    title.add_css_class("connections-device-name");
    labels.append(&title);
    let subtitle = Label::new(Some(detail));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    subtitle.add_css_class("connections-device-detail");
    labels.append(&subtitle);
    row.append(&labels);
    let actions = Box::new(Orientation::Horizontal, 8);
    actions.set_valign(Align::Center);
    row.append(&actions);
    (row, actions)
}

/// Bordered section panel with a numbered head.
///
/// Returns `(head, body)`: callers may drop a status chip into `head` and
/// append rows to `body` — the panel itself is already inside `parent`.
/// Shared chrome for both overlay panels (launcher + harness settings).
pub(crate) fn section_card(parent: &Box, num: &str, title: &str) -> (Box, Box) {
    let card = Box::new(Orientation::Vertical, 6);
    card.add_css_class("launcher-section");

    let head = Box::new(Orientation::Horizontal, 8);
    head.add_css_class("launcher-section-head");
    let n = Label::new(Some(num));
    n.add_css_class("launcher-section-num");
    n.set_valign(Align::Center);
    if !num.is_empty() {
        head.append(&n);
    }
    let t = Label::new(Some(title));
    t.add_css_class("launcher-section-title");
    t.set_xalign(0.0);
    t.set_hexpand(true);
    head.append(&t);
    card.append(&head);

    let body = Box::new(Orientation::Vertical, 4);
    body.add_css_class("launcher-section-body");
    card.append(&body);

    parent.append(&card);
    (head, body)
}

/// Small pill label for status chips (`term-status-badge` sizing).
pub(crate) fn chip(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("term-status-badge");
    l.set_valign(Align::Center);
    l
}

/// Hairline between two rows of the same section.
fn row_sep() -> Separator {
    let s = Separator::new(Orientation::Horizontal);
    s.add_css_class("launcher-sep");
    s
}

/// Selectable key: value row (values are copy-pasteable). Returns the value label.
pub(crate) fn kv(parent: &Box, key: &str) -> Label {
    let row = Box::new(Orientation::Horizontal, 10);
    row.add_css_class("launcher-row");
    let k = Label::new(Some(key));
    k.add_css_class("launcher-key");
    k.set_xalign(0.0);
    k.set_size_request(110, -1);
    let v = Label::new(None);
    v.add_css_class("launcher-value");
    v.set_xalign(0.0);
    v.set_hexpand(true);
    v.set_selectable(true);
    v.set_tooltip_text(Some("Select to copy (Ctrl+C)"));
    row.append(&k);
    row.append(&v);
    parent.append(&row);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "interactive visual preview; run explicitly with one test thread"]
    fn connections_overview_visual_preview() {
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let pages = build_connection_pages(ConnectionHooks::inert(), Rc::new(|_| {}));
        let page = pages.widget(ConnectionPage::Overview).clone();
        let window = gtk4::Window::new();
        window.set_title(Some("SUPER DESKTOP · Connections preview"));
        window.set_default_size(660, 620);
        window.add_css_class("mini-terminal");
        window.set_child(Some(&page));
        window.present();
        (pages.refresh)();
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let snapshot = gtk4::Snapshot::new();
        let paintable = gtk4::WidgetPaintable::new(Some(&page));
        paintable.snapshot(&snapshot, page.width() as f64, page.height() as f64);
        let node = snapshot.to_node().unwrap();
        window
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png("/tmp/sd-connections-preview.png")
            .unwrap();
        window.close();
    }

    /// Every widget carrying `class` in the subtree rooted at `w`.
    fn find(w: &gtk4::Widget, class: &str) -> Vec<gtk4::Widget> {
        let mut out = Vec::new();
        if w.has_css_class(class) {
            out.push(w.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            out.extend(find(&c, class));
            child = c.next_sibling();
        }
        out
    }

    fn count_class(w: &gtk4::Widget, class: &str) -> usize {
        find(w, class).len()
    }

    fn button(w: &gtk4::Widget, class: &str) -> Button {
        find(w, class)
            .into_iter()
            .find_map(|w| w.downcast::<Button>().ok())
            .unwrap_or_else(|| panic!("no button .{class}"))
    }

    #[test]
    fn test_connection_pages_structure() {
        // GTK may only be used from one thread per process, so the widget
        // assertions run in a child process (see `crate::gtk_test`).
        crate::gtk_test::run_in_child_process("launcher_settings::tests::pages_gtk_structure");
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn pages_gtk_structure() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();
        check_background_refresh();
        let visited = Rc::new(RefCell::new(Vec::new()));
        let connects = Rc::new(Cell::new(0));
        let reviews = Rc::new(Cell::new(0));
        let hooks = ConnectionHooks {
            requests: RequestHooks {
                review: Rc::new({
                    let reviews = Rc::clone(&reviews);
                    move || reviews.set(reviews.get() + 1)
                }),
                decisions: Default::default(),
            },
            connect_to_pc: Rc::new({
                let connects = Rc::clone(&connects);
                move || connects.set(connects.get() + 1)
            }),
        };
        let pages = build_connection_pages(hooks, {
            let visited = Rc::clone(&visited);
            Rc::new(move |page| visited.borrow_mut().push(page))
        });

        // One scrolling page per destination; the settings card owns the
        // chrome (`mini-terminal`/`harness-panel`).
        assert_eq!(pages.widgets().count(), ConnectionPage::ALL.len());
        for (page, widget) in pages.widgets() {
            assert!(widget.downcast_ref::<ScrolledWindow>().is_some(), "{page:?}");
            assert!(widget.has_css_class("launcher-scroll") && widget.has_css_class("harness-page"));
            assert!(!widget.has_css_class("mini-terminal"));
        }

        // The overview is short: this computer, then one entry per kind of
        // connection. No approve/deny buttons live in settings any more.
        let overview = pages.widget(ConnectionPage::Overview);
        assert_eq!(count_class(overview, "launcher-section"), 1);
        assert_eq!(count_class(overview, "settings-entry"), 5);
        for (class, page) in [
            ("connections-add-entry", ConnectionPage::AddDevice),
            ("connections-pcs-entry", ConnectionPage::Pcs),
            ("connections-phones-entry", ConnectionPage::Phones),
            ("connections-rejected-entry", ConnectionPage::Rejected),
            ("connections-network-entry", ConnectionPage::Network),
        ] {
            button(overview, class).emit_clicked();
            assert_eq!(visited.borrow().last(), Some(&page));
        }
        assert!(find(&pages.widget(ConnectionPage::Overview).clone(), "launcher-btn")
            .iter()
            .filter_map(|w| w.downcast_ref::<Button>().and_then(|b| b.label()))
            .all(|label| label != "Approve" && label != "Deny"));
        // A waiting request is reviewed in the approval panel.
        let review = find(overview, "connections-pending")[0]
            .last_child()
            .and_downcast::<Button>()
            .unwrap();
        review.emit_clicked();
        assert_eq!(reviews.get(), 1);

        // Add a device offers the three directions; viewing another PC is the
        // wizard's job. (The invitation entries start the real bridge, so they
        // are covered by `pairing_invite`'s tests instead.)
        let add = pages.widget(ConnectionPage::AddDevice);
        assert_eq!(count_class(add, "settings-entry"), 3);
        button(add, "connections-connect-pc").emit_clicked();
        assert_eq!(connects.get(), 1);

        // Back always leads towards the overview, then the settings hub.
        assert_eq!(ConnectionPage::Invite.parent(), Some(ConnectionPage::AddDevice));
        assert_eq!(ConnectionPage::Rejected.parent(), Some(ConnectionPage::Overview));
        assert_eq!(ConnectionPage::Overview.parent(), None);
        assert_eq!(pages.header(ConnectionPage::Invite).1, "Pair a phone");

        // Filled on worker threads: the bridge chip ends in one of its two
        // styled states, and every list says what to do when it is empty.
        (pages.refresh)();
        let context = glib::MainContext::default();
        let chip = find(overview, "term-status-badge")[0].clone().downcast::<Label>().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while chip.text() == "Checking…" {
            context.iteration(false);
            assert!(std::time::Instant::now() < deadline, "the snapshot never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(chip.has_css_class("launcher-online") || chip.has_css_class("launcher-offline"));
        let rejected = pages.widget(ConnectionPage::Rejected);
        assert_eq!(count_class(rejected, "launcher-section"), 1);
        let network = pages.widget(ConnectionPage::Network);
        assert_eq!(count_class(network, "launcher-value"), 5);
    }

    fn check_background_refresh() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc::channel,
            Arc, Mutex,
        };
        use std::time::{Duration, Instant};

        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let release_rx = Mutex::new(release_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&calls);
        let applied = Rc::new(Cell::new(0));
        let result = Rc::clone(&applied);
        let refresh = background_refresh(
            move || {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    started_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                n
            },
            move |n| result.set(n),
        );
        let context = glib::MainContext::default();
        refresh();
        while context.pending() {
            context.iteration(false);
        }
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..1000 {
            refresh();
        }
        let responsive = Rc::new(Cell::new(false));
        let ran = Rc::clone(&responsive);
        context.spawn_local(async move {
            ran.set(true);
        });
        while context.pending() {
            context.iteration(false);
        }
        assert!(
            responsive.get(),
            "main loop must run while the probe is blocked"
        );
        assert_eq!(applied.get(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no concurrent probes");
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while applied.get() != 2 && Instant::now() < deadline {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            applied.get(),
            2,
            "requests coalesce into one fresh follow-up"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
