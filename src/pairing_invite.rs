//! Guided invitation for pairing a device with this PC. It checks the secure
//! bridge, the firewall and the network, creates a single-use invitation (a
//! QR code for a phone, a link for another PC), then hands the device's
//! request to the approval panel and reports what became of it. A step that
//! fails says why and offers the fix. Bridge and system work stay off GTK.

use crate::pairing_request_ui::{bridge_error_message, Decision, RequestHooks};
use gtk4::{gdk, glib, prelude::*};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteKind {
    Phone,
    Pc,
}

impl InviteKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Phone => "Pair a phone",
            Self::Pc => "Share this PC",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Phone => "Scan a code in SUPER DESKTOP for Android",
            Self::Pc => "Let another PC use this PC's harnesses",
        }
    }

    fn intro(self) -> &'static str {
        match self {
            Self::Phone => "On the phone, open SUPER DESKTOP → Settings → Connections → Add connection and scan the code below. Then compare the code both devices show and approve the request here.",
            Self::Pc => "On the other PC, open Settings → Connections → Add a device → View another PC (or Add a PC in its machine selector) and paste the link below. Then compare the code both PCs show and approve the request here.",
        }
    }
}

/// Everything the flow needs to know before it can show an invitation.
struct Preflight {
    bridge: Result<(), String>,
    firewall: (String, bool),
    lan: String,
    tailscale: Option<String>,
    /// Requests already waiting before this invitation existed: not ours.
    earlier: Vec<String>,
    invitation: Option<Result<String, String>>,
}

fn preflight() -> Preflight {
    let bridge = crate::bridge::start_bridge();
    let earlier = if bridge.is_ok() { request_ids(crate::bridge::pending_requests()) } else { vec![] };
    let invitation = bridge.is_ok().then(crate::bridge::pairing_invitation);
    Preflight {
        bridge,
        firewall: crate::bridge::firewall_summary(),
        lan: crate::bridge::lan_ip(),
        tailscale: crate::bridge::tailscale_ip(),
        earlier,
        invitation,
    }
}

fn request_ids(requests: Vec<Value>) -> Vec<String> {
    requests
        .iter()
        .filter_map(|r| r["requestId"].as_str().map(str::to_string))
        .collect()
}

/// `superdesktop://pair?data=…`, the form both the Android app and the
/// Add-a-PC wizard accept.
pub fn invitation_link(payload: &str) -> String {
    let encoded = crate::ws::base64(payload.as_bytes())
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    format!("superdesktop://pair?data={encoded}")
}

type RunPreflight = Arc<dyn Fn() -> Preflight + Send + Sync>;
type ListRequests = Arc<dyn Fn() -> Vec<Value> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Preparing,
    Failed,
    Ready,
    Requested,
    Approved,
    Rejected,
    Expired,
}

#[derive(Clone, Copy)]
enum Mark {
    Working,
    Ok,
    Warn,
    Fail,
}

/// One checklist line: mark, title, what was found, and the fix if any.
struct Step {
    mark: gtk4::Label,
    detail: gtk4::Label,
    action: gtk4::Button,
}

impl Step {
    fn new(parent: &gtk4::Box, title: &str, action: &str) -> Self {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        row.add_css_class("invite-step");
        let mark = gtk4::Label::new(None);
        mark.add_css_class("invite-step-mark");
        mark.set_valign(gtk4::Align::Start);
        row.append(&mark);
        let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
        words.set_hexpand(true);
        let heading = gtk4::Label::new(Some(title));
        heading.add_css_class("invite-step-title");
        heading.set_xalign(0.0);
        words.append(&heading);
        let detail = gtk4::Label::new(None);
        detail.add_css_class("invite-step-detail");
        detail.set_xalign(0.0);
        detail.set_wrap(true);
        detail.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        detail.set_selectable(true);
        words.append(&detail);
        row.append(&words);
        let action = gtk4::Button::with_label(action);
        action.add_css_class("launcher-btn");
        action.add_css_class("launcher-btn-primary");
        action.set_valign(gtk4::Align::Center);
        action.set_visible(false);
        row.append(&action);
        parent.append(&row);
        Self { mark, detail, action }
    }

    fn set(&self, mark: Mark, detail: &str, action: bool) {
        let (text, class) = match mark {
            Mark::Working => ("…", "invite-working"),
            Mark::Ok => ("✓", "invite-ok"),
            Mark::Warn => ("!", "invite-warn"),
            Mark::Fail => ("✕", "invite-fail"),
        };
        self.mark.set_text(text);
        for other in ["invite-working", "invite-ok", "invite-warn", "invite-fail"] {
            self.mark.remove_css_class(other);
        }
        self.mark.add_css_class(class);
        self.detail.set_text(detail);
        self.action.set_visible(action);
        self.action.set_sensitive(true);
    }
}

pub struct InviteFlow {
    pub widget: gtk4::Box,
    hooks: RequestHooks,
    run_preflight: RunPreflight,
    list: ListRequests,
    kind: Cell<InviteKind>,
    stage: Cell<Stage>,
    intro: gtk4::Label,
    bridge: Step,
    firewall: Step,
    network: Step,
    card: gtk4::Box,
    expiry: gtk4::Label,
    qr: gtk4::DrawingArea,
    qr_code: Rc<RefCell<Option<qrcode::QrCode>>>,
    link: gtk4::Entry,
    link_note: gtk4::Label,
    invite_error: gtk4::Label,
    outcome: gtk4::Box,
    outcome_text: gtk4::Label,
    review: gtk4::Button,
    renew: gtk4::Button,
    generation: Cell<u64>,
    expires_at: Cell<Option<Instant>>,
    earlier: RefCell<Vec<String>>,
    /// The request that used this invitation: (request ID, device name).
    request: RefCell<Option<(String, String)>>,
    /// Polls in a row that no longer list our request. A decision is
    /// reported by the panel; only a request that stays gone expired.
    missing: Cell<u8>,
    poll_busy: Cell<bool>,
}

impl InviteFlow {
    pub fn new(hooks: RequestHooks) -> Rc<Self> {
        Self::with_source(
            hooks,
            Arc::new(preflight),
            Arc::new(crate::bridge::pending_requests),
        )
    }

    fn with_source(hooks: RequestHooks, run_preflight: RunPreflight, list: ListRequests) -> Rc<Self> {
        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
        widget.add_css_class("invite-flow");

        let intro = gtk4::Label::new(None);
        intro.add_css_class("settings-entry-summary");
        intro.set_wrap(true);
        intro.set_xalign(0.0);
        widget.append(&intro);

        let checklist = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        checklist.add_css_class("launcher-section");
        checklist.add_css_class("invite-checklist");
        let bridge = Step::new(&checklist, "Secure bridge", "Try again");
        let firewall = Step::new(&checklist, "Firewall", "Allow");
        firewall.action.set_tooltip_text(Some(
            "Allow 8759/tcp in UFW. Asks for your password.",
        ));
        let network = Step::new(&checklist, "Network", "Try again");
        widget.append(&checklist);

        // ---- the invitation itself ----
        let card = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        card.add_css_class("launcher-section");
        card.add_css_class("invite-card");
        card.set_visible(false);
        let card_head = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let card_title = gtk4::Label::new(Some("Invitation"));
        card_title.add_css_class("launcher-section-title");
        card_title.set_xalign(0.0);
        card_title.set_hexpand(true);
        card_head.append(&card_title);
        let expiry = crate::launcher_settings::chip("");
        expiry.add_css_class("invite-expiry");
        card_head.append(&expiry);
        card.append(&card_head);
        let qr_code: Rc<RefCell<Option<qrcode::QrCode>>> = Rc::new(RefCell::new(None));
        let qr = gtk4::DrawingArea::new();
        qr.set_content_width(240);
        qr.set_content_height(240);
        qr.set_halign(gtk4::Align::Start);
        qr.add_css_class("invite-qr");
        qr.set_draw_func({
            let qr_code = Rc::clone(&qr_code);
            move |_, cr, width, height| {
                let Some(code) = qr_code.borrow().clone() else {
                    return;
                };
                let size = code.width();
                let unit = (width.min(height) as f64 / (size + 8) as f64).floor().max(1.0);
                let offset_x = (width as f64 - (size + 8) as f64 * unit) / 2.0;
                let offset_y = (height as f64 - (size + 8) as f64 * unit) / 2.0;
                cr.set_source_rgb(1.0, 1.0, 1.0);
                let _ = cr.paint();
                cr.set_source_rgb(0.0, 0.0, 0.0);
                for y in 0..size {
                    for x in 0..size {
                        if code[(x, y)] == qrcode::Color::Dark {
                            cr.rectangle(
                                offset_x + (x + 4) as f64 * unit,
                                offset_y + (y + 4) as f64 * unit,
                                unit,
                                unit,
                            );
                        }
                    }
                }
                let _ = cr.fill();
            }
        });
        card.append(&qr);
        let link_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let link = gtk4::Entry::new();
        link.add_css_class("ws-entry");
        link.set_editable(false);
        link.set_hexpand(true);
        link.set_tooltip_text(Some("Single-use connection link"));
        link_row.append(&link);
        let copy = gtk4::Button::with_label("Copy link");
        copy.add_css_class("launcher-btn");
        copy.add_css_class("launcher-btn-primary");
        link_row.append(&copy);
        card.append(&link_row);
        let link_note = gtk4::Label::new(Some(
            "Single use: it stops working once a device uses it, or after five minutes.",
        ));
        link_note.add_css_class("launcher-hint");
        link_note.set_xalign(0.0);
        link_note.set_wrap(true);
        card.append(&link_note);
        widget.append(&card);

        let invite_error = gtk4::Label::new(None);
        invite_error.add_css_class("launcher-note");
        invite_error.add_css_class("launcher-note-error");
        invite_error.set_xalign(0.0);
        invite_error.set_wrap(true);
        invite_error.set_selectable(true);
        invite_error.set_visible(false);
        widget.append(&invite_error);

        // ---- what happened to it ----
        let outcome = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        outcome.add_css_class("pc-wizard-request");
        outcome.add_css_class("invite-outcome");
        outcome.set_visible(false);
        let outcome_text = gtk4::Label::new(None);
        outcome_text.add_css_class("launcher-status-text");
        outcome_text.set_xalign(0.0);
        outcome_text.set_wrap(true);
        outcome.append(&outcome_text);
        let outcome_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let review = gtk4::Button::with_label("Review request");
        review.add_css_class("launcher-btn");
        review.add_css_class("launcher-btn-primary");
        outcome_actions.append(&review);
        let renew = gtk4::Button::with_label("Create a new invitation");
        renew.add_css_class("launcher-btn");
        outcome_actions.append(&renew);
        outcome.append(&outcome_actions);
        widget.append(&outcome);

        let flow = Rc::new(Self {
            widget,
            hooks,
            run_preflight,
            list,
            kind: Cell::new(InviteKind::Phone),
            stage: Cell::new(Stage::Idle),
            intro,
            bridge,
            firewall,
            network,
            card,
            expiry,
            qr,
            qr_code,
            link,
            link_note,
            invite_error,
            outcome,
            outcome_text,
            review,
            renew,
            generation: Cell::new(0),
            expires_at: Cell::new(None),
            earlier: RefCell::new(Vec::new()),
            request: RefCell::new(None),
            missing: Cell::new(0),
            poll_busy: Cell::new(false),
        });

        for button in [&flow.bridge.action, &flow.network.action, &flow.renew] {
            let weak = Rc::downgrade(&flow);
            button.connect_clicked(move |_| {
                if let Some(flow) = weak.upgrade() {
                    flow.start(flow.kind.get());
                }
            });
        }
        let weak = Rc::downgrade(&flow);
        flow.firewall.action.connect_clicked(move |_| {
            if let Some(flow) = weak.upgrade() {
                flow.allow_firewall();
            }
        });
        let weak = Rc::downgrade(&flow);
        copy.connect_clicked(move |_| {
            let Some(flow) = weak.upgrade() else {
                return;
            };
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&flow.link.text());
                flow.link_note.set_text(match flow.kind.get() {
                    InviteKind::Pc => "Link copied. Paste it into SUPER DESKTOP on the other PC.",
                    InviteKind::Phone => "Link copied. Paste it into SUPER DESKTOP on the phone.",
                });
            }
        });
        let review_hook = Rc::clone(&flow.hooks.review);
        flow.review.connect_clicked(move |_| review_hook());
        let weak = Rc::downgrade(&flow);
        flow.hooks.decisions.subscribe(Rc::new(move |decision| {
            if let Some(flow) = weak.upgrade() {
                flow.decided(decision);
            }
        }));
        // Countdown and request watch, only while the flow is on screen.
        let weak = Rc::downgrade(&flow);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(flow) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if flow.widget.is_mapped() {
                flow.tick();
            }
            glib::ControlFlow::Continue
        });
        flow
    }

    pub fn kind(&self) -> InviteKind {
        self.kind.get()
    }

    /// Check everything and create a fresh invitation. Any earlier one is
    /// replaced: the bridge keeps a single invitation.
    pub fn start(self: &Rc<Self>, kind: InviteKind) {
        self.kind.set(kind);
        let generation = self.reset();
        self.stage.set(Stage::Preparing);
        self.intro.set_text(kind.intro());
        self.bridge.set(Mark::Working, "Starting…", false);
        self.firewall.set(Mark::Working, "Checking…", false);
        self.network.set(Mark::Working, "Checking…", false);
        let run = Arc::clone(&self.run_preflight);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(move || run()).await;
            let Some(flow) = weak.upgrade() else {
                return;
            };
            if flow.generation.get() != generation {
                return;
            }
            match result {
                Ok(checked) => flow.prepared(checked),
                Err(_) => {
                    flow.stage.set(Stage::Failed);
                    flow.bridge.set(Mark::Fail, "The check did not finish.", true);
                }
            }
        });
    }

    /// Leave the flow. The link's secret is cleared from the page; the
    /// bridge's invitation still expires on its own.
    pub fn stop(&self) {
        self.reset();
        self.stage.set(Stage::Idle);
    }

    fn reset(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.poll_busy.set(false);
        self.expires_at.set(None);
        self.earlier.borrow_mut().clear();
        self.request.borrow_mut().take();
        self.missing.set(0);
        self.link.set_text("");
        self.qr_code.borrow_mut().take();
        self.card.set_visible(false);
        self.invite_error.set_visible(false);
        self.outcome.set_visible(false);
        generation
    }

    fn prepared(&self, checked: Preflight) {
        let port = crate::bridge::BRIDGE_PORT;
        match &checked.bridge {
            Ok(()) => self.bridge.set(Mark::Ok, &format!("Running on port {port}"), false),
            Err(error) => self.bridge.set(
                Mark::Fail,
                &format!("Could not start: {error}"),
                true,
            ),
        }
        let (firewall, can_allow) = checked.firewall;
        let firewall = firewall.trim_end_matches(" ✓").to_string();
        if can_allow {
            self.firewall.set(
                Mark::Warn,
                &format!("{firewall}. Devices on your network can't reach this PC until port {port}/tcp is allowed."),
                true,
            );
        } else {
            self.firewall.set(Mark::Ok, &firewall, false);
        }
        let tailscale = checked
            .tailscale
            .as_ref()
            .map(|ip| format!(" · Tailscale {ip}"))
            .unwrap_or_default();
        let no_network = checked.lan == "127.0.0.1";
        match (no_network, checked.tailscale.as_ref()) {
            (false, _) => self.network.set(
                Mark::Ok,
                &format!("Invitation address {}:{port}{tailscale}", checked.lan),
                false,
            ),
            (true, Some(ip)) => self.network.set(
                Mark::Warn,
                &format!("No LAN address. On the other device, enter this PC's Tailscale address {ip} by hand."),
                false,
            ),
            (true, None) => self.network.set(
                Mark::Fail,
                "No network connection. Connect this PC to Wi-Fi, Ethernet or Tailscale, then try again.",
                true,
            ),
        }
        *self.earlier.borrow_mut() = checked.earlier;
        let payload = match checked.invitation {
            None => {
                self.stage.set(Stage::Failed);
                return;
            }
            Some(Err(code)) => {
                self.stage.set(Stage::Failed);
                self.show_error(&format!(
                    "Could not create an invitation. {}",
                    bridge_error_message(&code)
                ));
                return;
            }
            Some(Ok(payload)) => payload,
        };
        let expires_in = serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|v| v["expiresIn"].as_u64())
            .unwrap_or(300);
        let link = invitation_link(&payload);
        let phone = self.kind.get() == InviteKind::Phone;
        *self.qr_code.borrow_mut() = if phone {
            qrcode::QrCode::new(link.as_bytes()).ok()
        } else {
            None
        };
        self.qr.set_visible(phone && self.qr_code.borrow().is_some());
        self.qr.queue_draw();
        self.link.set_text(&link);
        self.link.set_position(0);
        self.link_note.set_text(
            "Single use: it stops working once a device uses it, or after five minutes.",
        );
        self.card.set_visible(true);
        self.expires_at
            .set(Some(Instant::now() + Duration::from_secs(expires_in)));
        self.stage.set(Stage::Ready);
        self.paint_expiry();
        self.show_outcome(
            match self.kind.get() {
                InviteKind::Phone => "Waiting for the phone to scan the code…",
                InviteKind::Pc => "Waiting for the other PC to use the link…",
            },
            false,
            false,
        );
    }

    fn show_error(&self, text: &str) {
        self.invite_error.set_text(text);
        self.invite_error.set_visible(true);
        self.show_outcome("", false, true);
    }

    fn show_outcome(&self, text: &str, review: bool, renew: bool) {
        self.outcome.set_visible(!text.is_empty() || review || renew);
        self.outcome_text.set_text(text);
        self.outcome_text.set_visible(!text.is_empty());
        self.review.set_visible(review);
        self.renew.set_visible(renew);
    }

    fn paint_expiry(&self) {
        let Some(at) = self.expires_at.get() else {
            return;
        };
        let left = at.saturating_duration_since(Instant::now()).as_secs();
        self.expiry
            .set_text(&format!("Expires in {}:{:02}", left / 60, left % 60));
    }

    fn tick(self: &Rc<Self>) {
        match self.stage.get() {
            Stage::Ready => {
                self.paint_expiry();
                if self
                    .expires_at
                    .get()
                    .is_some_and(|at| Instant::now() >= at)
                {
                    self.expired("The invitation expired before a device used it. Create a new one when the device is ready.");
                    return;
                }
                self.poll();
            }
            Stage::Requested => self.poll(),
            _ => {}
        }
    }

    fn expired(&self, text: &str) {
        self.stage.set(Stage::Expired);
        self.expires_at.set(None);
        self.link.set_text("");
        self.qr_code.borrow_mut().take();
        self.card.set_visible(false);
        self.show_outcome(text, false, true);
    }

    fn poll(self: &Rc<Self>) {
        if self.poll_busy.replace(true) {
            return;
        }
        let list = Arc::clone(&self.list);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let requests = gtk4::gio::spawn_blocking(move || list()).await.unwrap_or_default();
            let Some(flow) = weak.upgrade() else {
                return;
            };
            if flow.generation.get() != generation {
                return;
            }
            flow.poll_busy.set(false);
            flow.saw(requests);
        });
    }

    fn saw(&self, requests: Vec<Value>) {
        match self.stage.get() {
            Stage::Ready => {
                let earlier = self.earlier.borrow().clone();
                let Some(ours) = requests.iter().find(|r| {
                    r["requestId"]
                        .as_str()
                        .is_some_and(|id| !earlier.iter().any(|e| e == id))
                }) else {
                    return;
                };
                let id = ours["requestId"].as_str().unwrap_or("").to_string();
                let name = ours["deviceName"].as_str().unwrap_or("A device").to_string();
                self.requested(id, name);
                (self.hooks.review)();
            }
            Stage::Requested => {
                let Some((id, name)) = self.request.borrow().clone() else {
                    return;
                };
                if requests.iter().any(|r| r["requestId"] == id.as_str()) {
                    self.missing.set(0);
                    return;
                }
                self.missing.set(self.missing.get().saturating_add(1));
                if self.missing.get() >= 3 {
                    self.expired(&format!(
                        "The request from “{name}” is no longer waiting: it expired or was decided elsewhere. Create a new invitation to try again."
                    ));
                }
            }
            _ => {}
        }
    }

    fn requested(&self, id: String, name: String) {
        self.stage.set(Stage::Requested);
        self.expires_at.set(None);
        // Used up: the bridge accepts an invitation once.
        self.link.set_text("");
        self.qr_code.borrow_mut().take();
        self.card.set_visible(false);
        self.show_outcome(
            &format!("“{name}” asked to connect. Compare the code on both devices, then approve or reject it in the request panel."),
            true,
            false,
        );
        *self.request.borrow_mut() = Some((id, name));
        self.missing.set(0);
    }

    fn decided(&self, decision: &Decision) {
        let ours = match self.stage.get() {
            Stage::Requested => self
                .request
                .borrow()
                .as_ref()
                .is_some_and(|(id, _)| *id == decision.request_id),
            // Decided before this page noticed the request: anything that
            // was not already waiting came from this invitation.
            Stage::Ready => !self
                .earlier
                .borrow()
                .iter()
                .any(|id| *id == decision.request_id),
            _ => false,
        };
        if !ours {
            return;
        }
        self.request.borrow_mut().take();
        self.expires_at.set(None);
        self.link.set_text("");
        self.qr_code.borrow_mut().take();
        self.card.set_visible(false);
        if decision.approved {
            self.stage.set(Stage::Approved);
            self.renew.set_label("Invite another device");
            self.show_outcome(
                &format!("✓ “{}” can now connect to this PC.", decision.device),
                false,
                true,
            );
        } else {
            self.stage.set(Stage::Rejected);
            self.renew.set_label("Create a new invitation");
            self.show_outcome(
                &format!("“{}” was rejected and can't ask again. Remove it from Rejected devices to allow it later.", decision.device),
                false,
                true,
            );
        }
    }

    fn allow_firewall(self: &Rc<Self>) {
        self.firewall.action.set_sensitive(false);
        self.firewall
            .set(Mark::Working, "Waiting for the password prompt…", true);
        self.firewall.action.set_sensitive(false);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(crate::bridge::unlock_firewall).await;
            let Some(flow) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(Ok(message)) => flow
                    .firewall
                    .set(Mark::Ok, message.trim_end_matches(" ✓"), false),
                Ok(Err(error)) => flow.firewall.set(
                    Mark::Warn,
                    &format!("Could not allow port {}: {error}", crate::bridge::BRIDGE_PORT),
                    true,
                ),
                Err(_) => flow.firewall.set(Mark::Warn, "The firewall change did not finish.", true),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn invitation_links_are_the_url_safe_form_the_clients_accept() {
        let payload = json!({"v":3,"host":"10.0.0.2","port":8759,"fingerprint":"a".repeat(64),
            "secret":"b".repeat(48),"expiresIn":300}).to_string();
        let link = invitation_link(&payload);
        let data = link.strip_prefix("superdesktop://pair?data=").unwrap();
        assert!(!data.contains(['=', '+', '/']), "URL-safe and unpadded: {data}");
        let invitation = crate::peer_client::Invitation::parse(&link).unwrap();
        assert_eq!(invitation.endpoint.host, "10.0.0.2");
    }

    #[test]
    fn invite_flow() {
        crate::gtk_test::run_in_child_process("pairing_invite::tests::invite_flow_child");
    }

    fn pump_until(what: &str, done: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            while context.iteration(false) {}
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn checks(bridge: Result<(), String>, lan: &str, invitation: Option<Result<String, String>>) -> Preflight {
        Preflight {
            bridge,
            firewall: ("UFW is blocking port 8759".into(), true),
            lan: lan.into(),
            tailscale: None,
            earlier: vec!["old".into()],
            invitation,
        }
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn invite_flow_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let payload = json!({"v":3,"host":"10.0.0.2","port":8759,"fingerprint":"a".repeat(64),
            "secret":"b".repeat(48),"expiresIn":300}).to_string();
        let next = Arc::new(Mutex::new(checks(Ok(()), "10.0.0.2", Some(Ok(payload.clone())))));
        let waiting = Arc::new(Mutex::new(vec![json!({"requestId":"old","deviceName":"Earlier"})]));
        let run = {
            let next = Arc::clone(&next);
            Arc::new(move || {
                let checked = next.lock().unwrap();
                Preflight {
                    bridge: checked.bridge.clone(),
                    firewall: checked.firewall.clone(),
                    lan: checked.lan.clone(),
                    tailscale: checked.tailscale.clone(),
                    earlier: checked.earlier.clone(),
                    invitation: checked.invitation.clone(),
                }
            }) as RunPreflight
        };
        let list = {
            let waiting = Arc::clone(&waiting);
            Arc::new(move || waiting.lock().unwrap().clone()) as ListRequests
        };
        let reviews = Rc::new(Cell::new(0));
        let hooks = RequestHooks {
            review: Rc::new({
                let reviews = Rc::clone(&reviews);
                move || reviews.set(reviews.get() + 1)
            }),
            decisions: Default::default(),
        };
        // Never presented: the timer only runs on screen, so the test drives
        // the request watch itself.
        let flow = InviteFlow::with_source(hooks.clone(), run, list);

        // A phone gets a QR code and a link; a blocked firewall is a warning
        // with its fix, not a failure.
        flow.start(InviteKind::Phone);
        pump_until("the invitation", || flow.stage.get() == Stage::Ready);
        assert!(flow.card.property::<bool>("visible"));
        assert!(flow.qr.property::<bool>("visible"));
        assert!(flow.link.text().starts_with("superdesktop://pair?data="));
        assert!(flow.expiry.text().starts_with("Expires in 4:") || flow.expiry.text() == "Expires in 5:00");
        assert_eq!(flow.firewall.mark.text(), "!");
        assert!(flow.firewall.action.property::<bool>("visible"));
        assert_eq!(flow.network.detail.text(), "Invitation address 10.0.0.2:8759");

        // A request that was already waiting is not this invitation's; a new
        // one is, and it goes to the approval panel.
        flow.saw(waiting.lock().unwrap().clone());
        assert_eq!(flow.stage.get(), Stage::Ready);
        waiting.lock().unwrap().push(json!({"requestId":"new","deviceName":"Pixel"}));
        flow.saw(waiting.lock().unwrap().clone());
        assert_eq!(flow.stage.get(), Stage::Requested);
        assert_eq!(reviews.get(), 1);
        assert!(flow.link.text().is_empty(), "a used invitation is not left on screen");
        assert!(flow.review.property::<bool>("visible"));

        // Decisions on other requests are not ours; ours is reported.
        hooks.decisions.emit(&Decision { request_id: "old".into(), device: "Earlier".into(), approved: true });
        assert_eq!(flow.stage.get(), Stage::Requested);
        hooks.decisions.emit(&Decision { request_id: "new".into(), device: "Pixel".into(), approved: false });
        assert_eq!(flow.stage.get(), Stage::Rejected);
        assert!(flow.outcome_text.text().contains("was rejected"));
        assert!(flow.renew.property::<bool>("visible"));

        // A request that vanishes without a decision is reported as such,
        // not left "waiting" forever.
        flow.start(InviteKind::Pc);
        pump_until("the second invitation", || flow.stage.get() == Stage::Ready);
        assert!(!flow.qr.property::<bool>("visible"), "a PC gets a link, not a QR code");
        flow.saw(vec![json!({"requestId":"third","deviceName":"Laptop"})]);
        for _ in 0..3 {
            flow.saw(vec![]);
        }
        assert_eq!(flow.stage.get(), Stage::Expired);
        assert!(flow.outcome_text.text().contains("no longer waiting"));

        // Each failing step says why and offers its fix.
        *next.lock().unwrap() = checks(
            Err("port 8759 is held by another process".into()), "10.0.0.2", None);
        flow.start(InviteKind::Pc);
        pump_until("the bridge failure", || flow.stage.get() == Stage::Failed);
        assert_eq!(flow.bridge.mark.text(), "✕");
        assert!(flow.bridge.detail.text().contains("held by another process"));
        assert!(flow.bridge.action.property::<bool>("visible"));
        assert!(!flow.card.property::<bool>("visible"));

        *next.lock().unwrap() = checks(Ok(()), "127.0.0.1", Some(Err("bridge_not_responding".into())));
        flow.start(InviteKind::Pc);
        pump_until("the invitation failure", || flow.stage.get() == Stage::Failed);
        assert_eq!(flow.network.mark.text(), "✕");
        assert!(flow.invite_error.text().contains("did not answer"));
        assert!(flow.renew.property::<bool>("visible"));

        // Leaving the page clears the secret from it.
        *next.lock().unwrap() = checks(Ok(()), "10.0.0.2", Some(Ok(payload)));
        flow.start(InviteKind::Phone);
        pump_until("the third invitation", || flow.stage.get() == Stage::Ready);
        flow.stop();
        assert!(flow.link.text().is_empty());
        assert_eq!(flow.stage.get(), Stage::Idle);
    }
}
