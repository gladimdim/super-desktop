//! The approval panel for pairing requests: who is asking, from where, the
//! code to compare, and Reject / Approve. It is the one place a request is
//! decided. A clicked notification, a request that arrives while the overlay
//! is visible, and every "Review" button open it. Bridge work stays off GTK.
//!
//! Rejecting also blocks: the bridge remembers the device and refuses its
//! later requests until it is removed from Settings → Connections → Rejected
//! devices.

use gtk4::{glib, prelude::*};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A request decided in the panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub request_id: String,
    pub device: String,
    pub approved: bool,
}

/// Listeners for decisions, so a page that invited a device can report what
/// became of its request.
#[derive(Clone, Default)]
pub struct Decisions(Rc<RefCell<Vec<Rc<dyn Fn(&Decision)>>>>);

impl Decisions {
    pub fn subscribe(&self, listener: Rc<dyn Fn(&Decision)>) {
        self.0.borrow_mut().push(listener);
    }

    pub(crate) fn emit(&self, decision: &Decision) {
        // A listener may subscribe or open panels: never hold the borrow
        // across one.
        let listeners = self.0.borrow().clone();
        for listener in listeners {
            listener(decision);
        }
    }
}

/// What other surfaces need from the approval panel.
#[derive(Clone)]
pub struct RequestHooks {
    /// Open the approval panel.
    pub review: Rc<dyn Fn()>,
    pub decisions: Decisions,
}

impl RequestHooks {
    /// For surfaces built without a panel.
    #[cfg(test)]
    pub fn inert() -> Self {
        Self {
            review: Rc::new(|| {}),
            decisions: Decisions::default(),
        }
    }
}

/// Icon and label for a device type as the device reported it.
pub fn device_kind(device_type: &str) -> (&'static str, &'static str) {
    match device_type {
        "pc" => ("computer-symbolic", "SUPER DESKTOP PC"),
        "android" => ("phone-symbolic", "Android device"),
        "mobile" => ("phone-symbolic", "Mobile device"),
        _ => ("network-workgroup-symbolic", "Device type not reported"),
    }
}

/// What a failed bridge action means for the person at this PC.
pub fn bridge_error_message(code: &str) -> String {
    let state_dir = "~/.local/state/omarchy/harness-bridge";
    match code {
        "bridge_offline" => "The secure bridge on this PC is not running. Start it on the Connections page.".into(),
        "bridge_not_responding" => "The secure bridge did not answer. Try again in a moment.".into(),
        "invalid_bridge_response" => "The secure bridge sent an unexpected answer. Stop and start it on the Connections page.".into(),
        "request_expired_or_already_decided" => {
            "This request expired or was already decided. Ask the device to connect again.".into()
        }
        "revoke_old_devices_first" => "This PC already has 64 paired devices. Revoke the ones you no longer use, then ask the device to connect again.".into(),
        "could_not_save_pairing" => format!("Could not save the pairing. Check the free space and permissions of {state_dir}, then ask the device to connect again."),
        "could_not_save_revocation" => format!("Could not save the revocation. Check the free space and permissions of {state_dir}."),
        "could_not_save_rejected_list" => format!("Could not save the rejected list. Check the free space and permissions of {state_dir}."),
        "not_rejected" => "This device is no longer in the rejected list.".into(),
        other => format!("The secure bridge refused this action ({other})."),
    }
}

/// A pending request as the bridge reports it. Everything but the request ID
/// and code comes from the device and is shown as plain text only.
#[derive(Clone, Debug, PartialEq)]
struct Request {
    id: String,
    name: String,
    device_type: String,
    address: String,
    code: String,
    identified: bool,
    expires_in: u64,
}

impl Request {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["requestId"].as_str()?.to_string(),
            name: value["deviceName"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Unnamed device")
                .to_string(),
            device_type: value["deviceType"].as_str().unwrap_or("unknown").to_string(),
            address: value["address"].as_str().unwrap_or("").to_string(),
            code: value["code"].as_str().unwrap_or("").to_string(),
            identified: value["identified"] == true,
            // Older bridges do not report it; the countdown is hidden then.
            expires_in: value["expiresIn"].as_u64().unwrap_or(0),
        })
    }
}

/// "123 456": two groups are easier to compare than six digits.
fn grouped_code(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

fn countdown(remaining: Duration) -> String {
    let secs = remaining.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

type ListRequests = Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync>;
type Decide = Arc<dyn Fn(&str, bool) -> Result<bool, String> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Loading,
    Request,
    Result,
    Empty,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Request => "request",
            Self::Result => "result",
            Self::Empty => "empty",
        }
    }
}

pub struct PairingRequestPanel {
    pub widget: gtk4::Box,
    decisions: Decisions,
    list: ListRequests,
    decide_with: Decide,
    open: Cell<bool>,
    view: Cell<View>,
    pages: gtk4::Stack,
    icon: gtk4::Image,
    name: gtk4::Label,
    kind: gtk4::Label,
    address: gtk4::Label,
    identity: gtk4::Label,
    expires: gtk4::Label,
    code: gtk4::Label,
    compare: gtk4::Label,
    status: gtk4::Label,
    queue: gtk4::Label,
    approve: gtk4::Button,
    reject: gtk4::Button,
    result_mark: gtk4::Label,
    result_title: gtk4::Label,
    result_text: gtk4::Label,
    next: gtk4::Button,
    empty_title: gtk4::Label,
    empty_text: gtk4::Label,
    current: RefCell<Option<Request>>,
    waiting: Cell<usize>,
    expires_at: Cell<Option<Instant>>,
    busy: Cell<bool>,
    poll_busy: Cell<bool>,
    /// Bumped on close, so a late answer never lands on a closed panel.
    generation: Cell<u64>,
}

impl PairingRequestPanel {
    pub fn new() -> Rc<Self> {
        Self::with_source(
            Arc::new(crate::bridge::pairing_requests),
            Arc::new(crate::bridge::decide_request),
        )
    }

    fn with_source(list: ListRequests, decide_with: Decide) -> Rc<Self> {
        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        for class in ["mini-terminal", "harness-panel", "pc-wizard", "pairing-panel"] {
            widget.add_css_class(class);
        }
        widget.set_size_request(560, -1);
        widget.set_halign(gtk4::Align::Center);
        widget.set_valign(gtk4::Align::Center);
        widget.set_visible(false);

        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        header.add_css_class("term-header");
        let badge = gtk4::Label::new(Some("⇄"));
        badge.add_css_class("launcher-head-badge");
        header.append(&badge);
        let heading = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        heading.set_hexpand(true);
        let title = gtk4::Label::new(Some("Connection request"));
        title.add_css_class("term-title");
        title.set_xalign(0.0);
        let subtitle = gtk4::Label::new(Some("A device wants to connect to this PC"));
        subtitle.add_css_class("launcher-subtitle");
        subtitle.set_xalign(0.0);
        heading.append(&title);
        heading.append(&subtitle);
        header.append(&heading);
        let close = gtk4::Button::with_label("✕");
        close.set_tooltip_text(Some("Close. The request keeps waiting until it expires."));
        close.add_css_class("term-btn");
        header.append(&close);
        widget.append(&header);

        let pages = gtk4::Stack::new();
        pages.set_transition_type(gtk4::StackTransitionType::Crossfade);
        pages.set_transition_duration(120);
        pages.set_vhomogeneous(false);
        pages.set_hexpand(true);
        widget.append(&pages);

        let loading = page_box();
        loading.append(&help("Checking for connection requests…"));
        pages.add_named(&loading, Some(View::Loading.name()));

        // ---- the request ----
        let request = page_box();
        let who = gtk4::Box::new(gtk4::Orientation::Horizontal, 14);
        let icon = gtk4::Image::from_icon_name("network-workgroup-symbolic");
        icon.set_pixel_size(40);
        icon.add_css_class("pairing-device-icon");
        icon.set_valign(gtk4::Align::Center);
        who.append(&icon);
        let words = gtk4::Box::new(gtk4::Orientation::Vertical, 3);
        words.set_hexpand(true);
        let name = gtk4::Label::new(None);
        name.add_css_class("pairing-device-name");
        name.set_xalign(0.0);
        name.set_wrap(true);
        name.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        words.append(&name);
        let kind = help("");
        words.append(&kind);
        who.append(&words);
        request.append(&who);

        let details = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
        details.add_css_class("launcher-section");
        details.add_css_class("pairing-details");
        let address = crate::launcher_settings::kv(&details, "Network address");
        let identity = crate::launcher_settings::kv(&details, "Identity");
        identity.set_wrap(true);
        let expires = crate::launcher_settings::kv(&details, "Expires in");
        request.append(&details);

        let code_title = headline("Verification code");
        code_title.set_margin_top(6);
        request.append(&code_title);
        let code = gtk4::Label::new(None);
        code.add_css_class("pc-wizard-code");
        code.set_xalign(0.0);
        code.set_selectable(true);
        request.append(&code);
        let compare = help("");
        request.append(&compare);

        let status = gtk4::Label::new(None);
        status.add_css_class("pc-wizard-status");
        status.set_wrap(true);
        status.set_xalign(0.0);
        status.set_selectable(true);
        status.set_visible(false);
        request.append(&status);

        let decisions_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        decisions_row.add_css_class("pairing-decisions");
        let reject = gtk4::Button::with_label("Reject");
        reject.add_css_class("hud-button");
        reject.add_css_class("hud-button-danger");
        reject.set_tooltip_text(Some(
            "Reject and block this device. Undo it in Settings → Connections → Rejected devices.",
        ));
        decisions_row.append(&reject);
        let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        decisions_row.append(&spacer);
        let approve = gtk4::Button::with_label("Approve");
        approve.add_css_class("hud-button");
        approve.add_css_class("hud-action-primary");
        approve.set_tooltip_text(Some("Approve only if both devices show the same code"));
        decisions_row.append(&approve);
        request.append(&decisions_row);
        request.append(&help(
            "Rejecting also blocks the device: it can't ask again until you remove it from Settings → Connections → Rejected devices.",
        ));
        let queue = gtk4::Label::new(None);
        queue.add_css_class("pairing-queue");
        queue.set_xalign(0.0);
        queue.set_visible(false);
        request.append(&queue);
        pages.add_named(&request, Some(View::Request.name()));

        // ---- after a decision ----
        let result = page_box();
        let result_mark = gtk4::Label::new(None);
        result_mark.add_css_class("pairing-result-mark");
        result_mark.set_xalign(0.0);
        result.append(&result_mark);
        let result_title = headline("");
        result_title.set_wrap(true);
        result.append(&result_title);
        let result_text = help("");
        result.append(&result_text);
        let result_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let done = primary_button("Done");
        result_actions.append(&done);
        let next = gtk4::Button::with_label("Review next request");
        next.add_css_class("hud-button");
        next.set_visible(false);
        result_actions.append(&next);
        result.append(&result_actions);
        pages.add_named(&result, Some(View::Result.name()));

        // ---- nothing to decide ----
        let empty = page_box();
        let empty_title = headline("No requests waiting");
        empty.append(&empty_title);
        let empty_text = help("");
        empty.append(&empty_text);
        let empty_close = primary_button("Close");
        empty.append(&empty_close);
        pages.add_named(&empty, Some(View::Empty.name()));
        pages.set_visible_child_name(View::Loading.name());

        let panel = Rc::new(Self {
            widget,
            decisions: Decisions::default(),
            list,
            decide_with,
            open: Cell::new(false),
            view: Cell::new(View::Loading),
            pages,
            icon,
            name,
            kind,
            address,
            identity,
            expires,
            code,
            compare,
            status,
            queue,
            approve,
            reject,
            result_mark,
            result_title,
            result_text,
            next,
            empty_title,
            empty_text,
            current: RefCell::new(None),
            waiting: Cell::new(0),
            expires_at: Cell::new(None),
            busy: Cell::new(false),
            poll_busy: Cell::new(false),
            generation: Cell::new(0),
        });

        for button in [&close, &done, &empty_close] {
            let weak = Rc::downgrade(&panel);
            button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.close();
                }
            });
        }
        for (button, approve) in [(&panel.approve, true), (&panel.reject, false)] {
            let weak = Rc::downgrade(&panel);
            button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.decide(approve);
                }
            });
        }
        let weak = Rc::downgrade(&panel);
        panel.next.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.show_view(View::Loading);
                panel.poll();
            }
        });
        // One request to the bridge per second, and only while open.
        let weak = Rc::downgrade(&panel);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if panel.open.get() {
                panel.paint_expiry();
                panel.poll();
            }
            glib::ControlFlow::Continue
        });
        panel
    }

    pub fn hooks(self: &Rc<Self>) -> RequestHooks {
        let weak = Rc::downgrade(self);
        RequestHooks {
            review: Rc::new(move || {
                if let Some(panel) = weak.upgrade() {
                    panel.open();
                }
            }),
            decisions: self.decisions.clone(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Show the panel. A request under review stays put; an answered or empty
    /// panel looks again, since this is usually a new request arriving.
    pub fn open(self: &Rc<Self>) {
        self.widget.set_visible(true);
        let was_open = self.open.replace(true);
        if !was_open || matches!(self.view.get(), View::Result | View::Empty) {
            self.show_view(View::Loading);
        }
        self.poll();
    }

    /// Hide the panel without deciding: the request waits until it expires.
    pub fn close(&self) {
        self.open.set(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        self.poll_busy.set(false);
        self.current.borrow_mut().take();
        self.expires_at.set(None);
        self.show_view(View::Loading);
        self.widget.set_visible(false);
    }

    fn show_view(&self, view: View) {
        self.view.set(view);
        self.pages.set_visible_child_name(view.name());
    }

    fn poll(self: &Rc<Self>) {
        if !self.open.get() || self.busy.get() || self.poll_busy.replace(true) {
            return;
        }
        let list = Arc::clone(&self.list);
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(move || list()).await;
            let Some(panel) = weak.upgrade() else {
                return;
            };
            if panel.generation.get() != generation {
                return;
            }
            panel.poll_busy.set(false);
            if !panel.open.get() || panel.busy.get() {
                return;
            }
            match result {
                Ok(Ok(items)) => panel.apply(items.iter().filter_map(Request::parse).collect()),
                Ok(Err(code)) => panel.unavailable(&code),
                Err(_) => panel.unavailable("bridge_not_responding"),
            }
        });
    }

    fn apply(&self, requests: Vec<Request>) {
        let others = requests.len().saturating_sub(1);
        match self.view.get() {
            View::Request => {
                let current = self.current.borrow().as_ref().map(|r| r.id.clone());
                if let Some(same) = current.and_then(|id| requests.iter().find(|r| r.id == id)) {
                    self.sync_expiry(same.expires_in);
                    self.set_waiting(others);
                    return;
                }
                match requests.first() {
                    Some(next) => {
                        self.show_request(next.clone(), others);
                        self.set_status("The previous request expired or was decided elsewhere.", false);
                    }
                    None => self.show_empty(
                        "The request is no longer waiting",
                        "It expired or was decided elsewhere. Ask the device to connect again if you still want to pair it.",
                    ),
                }
            }
            View::Result => {
                self.waiting.set(requests.len());
                self.paint_next();
            }
            View::Loading | View::Empty => match requests.first() {
                Some(first) => self.show_request(first.clone(), others),
                None => self.show_empty(
                    "No requests waiting",
                    "There is no connection request to review. A request expires two minutes after the device sends it.",
                ),
            },
        }
    }

    fn unavailable(&self, code: &str) {
        match self.view.get() {
            View::Result => {}
            View::Request => self.set_status(&bridge_error_message(code), true),
            View::Loading | View::Empty => {
                self.show_empty("Can't check for requests", &bridge_error_message(code))
            }
        }
    }

    fn show_request(&self, request: Request, others: usize) {
        let (icon, kind) = device_kind(&request.device_type);
        self.icon.set_icon_name(Some(icon));
        self.name.set_text(&request.name);
        self.kind.set_text(&format!(
            "{kind} · wants to use this PC's workspace and harnesses"
        ));
        self.address.set_text(if request.address.is_empty() {
            "Unknown"
        } else {
            &request.address
        });
        self.identity.set_text(if request.identified {
            "Installation ID reported by the PC"
        } else {
            "Name reported by the device, not verified"
        });
        self.code.set_text(&grouped_code(&request.code));
        self.compare.set_text(&format!(
            "Approve only if “{}” shows this same code.",
            request.name
        ));
        self.set_status("", false);
        self.approve.set_sensitive(true);
        self.reject.set_sensitive(true);
        self.expires_at.set(None);
        self.sync_expiry(request.expires_in);
        *self.current.borrow_mut() = Some(request);
        self.set_waiting(others);
        self.show_view(View::Request);
    }

    fn show_empty(&self, title: &str, text: &str) {
        self.current.borrow_mut().take();
        self.expires_at.set(None);
        self.empty_title.set_text(title);
        self.empty_text.set_text(text);
        self.show_view(View::Empty);
    }

    fn show_result(&self, request: &Request, approved: bool, remembered: bool) {
        self.current.borrow_mut().take();
        self.expires_at.set(None);
        if approved {
            self.result_mark.set_text("✓");
            self.result_mark.remove_css_class("pairing-result-rejected");
            self.result_title
                .set_text(&format!("{} can now connect", request.name));
            let list = if request.device_type == "pc" {
                "PCs"
            } else {
                "Phones & other devices"
            };
            self.result_text.set_text(&format!(
                "It can use this PC's workspace and harnesses. Revoke its access at any time in Settings → Connections → {list}."
            ));
        } else {
            self.result_mark.set_text("⊘");
            self.result_mark.add_css_class("pairing-result-rejected");
            self.result_title
                .set_text(&format!("{} was rejected", request.name));
            let mut text = String::from(
                "It can't ask to connect again. To allow it later, remove it from Settings → Connections → Rejected devices.",
            );
            if !remembered {
                text.push_str(" The rejected list could not be saved, so the device can ask again after the secure bridge restarts.");
            }
            self.result_text.set_text(&text);
        }
        self.show_view(View::Result);
        self.paint_next();
    }

    fn set_status(&self, text: &str, error: bool) {
        self.status.set_text(text);
        self.status.set_visible(!text.is_empty());
        if error {
            self.status.add_css_class("launcher-note-error");
        } else {
            self.status.remove_css_class("launcher-note-error");
        }
    }

    fn set_waiting(&self, others: usize) {
        self.waiting.set(others);
        self.queue.set_visible(others > 0);
        self.queue.set_text(&match others {
            1 => "1 more request is waiting.".to_string(),
            n => format!("{n} more requests are waiting."),
        });
    }

    fn paint_next(&self) {
        let waiting = self.waiting.get();
        self.next.set_visible(waiting > 0);
        self.next.set_label(&format!("Review next request ({waiting})"));
    }

    /// The bridge's clock decides; this only keeps the countdown smooth.
    fn sync_expiry(&self, expires_in: u64) {
        if expires_in == 0 && self.expires_at.get().is_none() {
            self.expires.set_text("About two minutes after the request");
            return;
        }
        if expires_in > 0 {
            self.expires_at
                .set(Some(Instant::now() + Duration::from_secs(expires_in)));
        }
        self.paint_expiry();
    }

    fn paint_expiry(&self) {
        let Some(at) = self.expires_at.get() else {
            return;
        };
        if self.view.get() != View::Request {
            return;
        }
        let remaining = at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.expires.set_text("Expired");
            self.approve.set_sensitive(false);
        } else {
            self.expires.set_text(&countdown(remaining));
        }
    }

    pub fn decide(self: &Rc<Self>, approve: bool) {
        if self.view.get() != View::Request {
            return;
        }
        let Some(request) = self.current.borrow().clone() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.approve.set_sensitive(false);
        self.reject.set_sensitive(false);
        self.set_status(if approve { "Approving…" } else { "Rejecting…" }, false);
        let decide = Arc::clone(&self.decide_with);
        let generation = self.generation.get();
        let id = request.id.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(move || decide(&id, approve)).await;
            let Some(panel) = weak.upgrade() else {
                return;
            };
            panel.busy.set(false);
            let here = panel.generation.get() == generation && panel.open.get();
            match result {
                Ok(Ok(remembered)) => {
                    if here {
                        panel.show_result(&request, approve, remembered);
                    }
                    panel.decisions.emit(&Decision {
                        request_id: request.id.clone(),
                        device: request.name.clone(),
                        approved: approve,
                    });
                }
                Ok(Err(code)) if here => {
                    panel.set_status(&bridge_error_message(&code), true);
                    let gone = code == "request_expired_or_already_decided";
                    panel.approve.set_sensitive(!gone);
                    panel.reject.set_sensitive(!gone);
                }
                Err(_) if here => {
                    panel.set_status(&bridge_error_message("bridge_not_responding"), true);
                    panel.approve.set_sensitive(true);
                    panel.reject.set_sensitive(true);
                }
                _ => {}
            }
        });
    }
}

fn page_box() -> gtk4::Box {
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    body.add_css_class("launcher-body");
    body.add_css_class("pc-wizard-page");
    body
}

fn headline(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class("settings-entry-title");
    label.set_xalign(0.0);
    label
}

fn help(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.add_css_class("settings-entry-summary");
    label.set_wrap(true);
    label.set_xalign(0.0);
    label
}

fn primary_button(text: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(text);
    button.add_css_class("hud-button");
    button.add_css_class("hud-action-primary");
    button.set_halign(gtk4::Align::Start);
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn requests_parse_defensively_and_codes_read_in_two_groups() {
        let full = Request::parse(&json!({"requestId":"r","deviceName":"Laptop","deviceType":"pc",
            "address":"10.0.0.2","code":"123456","identified":true,"expiresIn":90}))
        .unwrap();
        assert_eq!((full.device_type.as_str(), full.identified, full.expires_in), ("pc", true, 90));
        // An older bridge reports less; a blank name is not shown as blank.
        let old = Request::parse(&json!({"requestId":"r","deviceName":"  ","code":"1"})).unwrap();
        assert_eq!((old.name.as_str(), old.device_type.as_str(), old.expires_in), ("Unnamed device", "unknown", 0));
        assert!(Request::parse(&json!({"code":"123456"})).is_none());
        assert_eq!(grouped_code("123456"), "123 456");
        assert_eq!(grouped_code("12345"), "12345");
        assert_eq!(countdown(Duration::from_secs(119)), "1:59");
        assert_eq!(device_kind("android").1, "Android device");
        assert_eq!(device_kind("toaster").1, "Device type not reported");
        assert!(bridge_error_message("bridge_offline").contains("not running"));
        assert!(bridge_error_message("mystery").contains("mystery"));
    }

    #[test]
    fn approval_panel_flow() {
        // GTK may only be used from one thread per process.
        crate::gtk_test::run_in_child_process("pairing_request_ui::tests::approval_panel_flow_child");
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

    fn request(id: &str, name: &str, device_type: &str) -> Value {
        json!({"requestId":id,"deviceName":name,"deviceType":device_type,"address":"10.0.0.2",
            "code":"123456","identified":device_type == "pc","expiresIn":100})
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn approval_panel_flow_child() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let waiting = Arc::new(Mutex::new(Ok(vec![
            request("one", "Laptop", "pc"),
            request("two", "Pixel", "android"),
        ])));
        let decided = Arc::new(Mutex::new(Vec::new()));
        let list = {
            let waiting = Arc::clone(&waiting);
            Arc::new(move || waiting.lock().unwrap().clone()) as ListRequests
        };
        let decide = {
            let waiting = Arc::clone(&waiting);
            let decided = Arc::clone(&decided);
            Arc::new(move |id: &str, approve: bool| {
                decided.lock().unwrap().push((id.to_string(), approve));
                if let Ok(items) = waiting.lock().unwrap().as_mut() {
                    items.retain(|item| item["requestId"] != id);
                }
                Ok(true)
            }) as Decide
        };
        let panel = PairingRequestPanel::with_source(list, decide);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let hooks = panel.hooks();
        hooks.decisions.subscribe({
            let seen = Rc::clone(&seen);
            Rc::new(move |decision| seen.borrow_mut().push(decision.clone()))
        });

        // The review hook opens the panel on the oldest request, with its
        // details and the code to compare, and says another one is waiting.
        (hooks.review)();
        assert!(panel.widget.property::<bool>("visible"));
        pump_until("the first request", || panel.view.get() == View::Request);
        assert_eq!(panel.name.text(), "Laptop");
        assert_eq!(panel.code.text(), "123 456");
        assert_eq!(panel.address.text(), "10.0.0.2");
        assert_eq!(panel.identity.text(), "Installation ID reported by the PC");
        assert!(panel.queue.property::<bool>("visible"));
        assert!(panel.approve.is_sensitive() && panel.reject.is_sensitive());

        // Reject: the bridge is told, listeners hear it, the result says the
        // device is blocked and offers the next request.
        panel.reject.emit_clicked();
        assert!(!panel.approve.is_sensitive(), "no second decision while one is in flight");
        pump_until("the rejection", || panel.view.get() == View::Result);
        assert_eq!(*decided.lock().unwrap(), [("one".to_string(), false)]);
        assert_eq!(seen.borrow().last().map(|d| (d.request_id.as_str(), d.approved)), Some(("one", false)));
        assert_eq!(panel.result_title.text(), "Laptop was rejected");
        assert!(panel.result_text.text().contains("Rejected devices"));
        pump_until("the next-request button", || panel.next.property::<bool>("visible"));

        panel.next.emit_clicked();
        pump_until("the second request", || panel.view.get() == View::Request);
        assert_eq!(panel.name.text(), "Pixel");
        assert_eq!(panel.identity.text(), "Name reported by the device, not verified");
        assert!(!panel.queue.property::<bool>("visible"));
        panel.approve.emit_clicked();
        pump_until("the approval", || panel.view.get() == View::Result);
        assert_eq!(panel.result_title.text(), "Pixel can now connect");
        assert!(panel.result_text.text().contains("Phones & other devices"));
        assert_eq!(seen.borrow().len(), 2);
        assert!(!panel.next.property::<bool>("visible"));

        // A request that vanishes while it is on screen (expired, or decided
        // elsewhere) is not left there to be approved.
        panel.close();
        assert!(!panel.widget.property::<bool>("visible"));
        *waiting.lock().unwrap() = Ok(vec![request("three", "Tablet", "mobile")]);
        panel.open();
        pump_until("the third request", || panel.view.get() == View::Request);
        *waiting.lock().unwrap() = Ok(vec![]);
        panel.poll();
        pump_until("the vanished request", || panel.view.get() == View::Empty);
        assert_eq!(panel.empty_title.text(), "The request is no longer waiting");

        // A stopped bridge is reported as such, not as "no requests".
        *waiting.lock().unwrap() = Err("bridge_offline".to_string());
        panel.close();
        panel.open();
        pump_until("the offline report", || panel.view.get() == View::Empty);
        assert_eq!(panel.empty_title.text(), "Can't check for requests");
        assert!(panel.empty_text.text().contains("not running"));
        assert_eq!(seen.borrow().len(), 2, "nothing was decided while offline");
    }

    /// Driven by tests/peer_pairing_smoke.py with a disposable real TLS bridge:
    /// the panel finds the request through the owner-only control socket and
    /// approves it.
    #[test]
    fn approval_inner() {
        let Ok(result_path) = std::env::var("SUPER_DESKTOP_SHARE_TEST_RESULT") else {
            return;
        };
        gtk4::init().unwrap();
        let panel = PairingRequestPanel::new();
        panel.open();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut code = None;
        loop {
            while glib::MainContext::default().iteration(false) {}
            if panel.view.get() == View::Empty {
                // Opened before the viewer asked: look again.
                panel.open();
            }
            if code.is_none() && panel.view.get() == View::Request {
                code = panel.current.borrow().as_ref().map(|r| r.code.clone());
                panel.decide(true);
            }
            if panel.view.get() == View::Result {
                assert!(panel.result_title.text().ends_with("can now connect"));
                std::fs::write(result_path, code.expect("approved without a code")).unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "the panel did not approve the request");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
