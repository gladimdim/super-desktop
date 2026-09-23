//! Centered PC pairing wizard. Network and owner-socket work stays off GTK.
use crate::{
    peer_client::{PeerError, PeerSummary},
    peer_pairing::{self, Event, Session},
};
use gtk4::{gdk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, LayerShell};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Choose,
    Connect,
    Share,
    Verify,
}

impl Page {
    fn name(self) -> &'static str {
        match self {
            Self::Choose => "choose",
            Self::Connect => "connect",
            Self::Share => "share",
            Self::Verify => "verify",
        }
    }
}

pub struct PairingWizard {
    pub widget: gtk4::Box,
    window: glib::WeakRef<gtk4::ApplicationWindow>,
    pages: gtk4::Stack,
    open: Cell<bool>,
    page: Cell<Page>,
    title: gtk4::Label,
    subtitle: gtk4::Label,
    back: gtk4::Button,
    invitation: gtk4::Entry,
    name: gtk4::Entry,
    host: gtk4::Entry,
    port: gtk4::Entry,
    connect_status: gtk4::Label,
    verify_status: gtk4::Label,
    verify_code: gtk4::Label,
    session: RefCell<Option<Session>>,
    link: gtk4::Entry,
    link_status: gtk4::Label,
    create_link: gtk4::Button,
    copy_link: gtk4::Button,
    link_generation: Cell<u64>,
    request_box: gtk4::Box,
    request_name: gtk4::Label,
    request_code: gtk4::Label,
    request_id: RefCell<Option<String>>,
    code_matches: gtk4::CheckButton,
    approve: gtk4::Button,
    deny: gtk4::Button,
    request_busy: Cell<bool>,
    poll_busy: Cell<bool>,
    on_saved: Rc<dyn Fn(PeerSummary)>,
}

impl PairingWizard {
    pub fn new(window: &gtk4::ApplicationWindow, on_saved: Rc<dyn Fn(PeerSummary)>) -> Rc<Self> {
        let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        widget.add_css_class("mini-terminal");
        widget.add_css_class("harness-panel");
        widget.add_css_class("pc-wizard");
        widget.set_size_request(660, 520);
        widget.set_halign(gtk4::Align::Center);
        widget.set_valign(gtk4::Align::Center);
        widget.set_visible(false);

        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        header.add_css_class("term-header");
        let badge = gtk4::Label::new(Some("⌁"));
        badge.add_css_class("launcher-head-badge");
        header.append(&badge);
        let heading = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        heading.set_hexpand(true);
        let title = gtk4::Label::new(Some("Add a PC"));
        title.add_css_class("term-title");
        title.set_xalign(0.0);
        let subtitle = gtk4::Label::new(Some("Connect SUPER DESKTOPs"));
        subtitle.add_css_class("launcher-subtitle");
        subtitle.set_xalign(0.0);
        heading.append(&title);
        heading.append(&subtitle);
        header.append(&heading);
        let back = gtk4::Button::with_label("← Back");
        back.add_css_class("term-btn");
        back.set_visible(false);
        header.append(&back);
        let close = gtk4::Button::with_label("✕");
        close.set_tooltip_text(Some("Close PC setup"));
        close.add_css_class("term-btn");
        header.append(&close);
        widget.append(&header);

        let pages = gtk4::Stack::new();
        pages.set_transition_type(gtk4::StackTransitionType::SlideLeftRight);
        pages.set_transition_duration(180);
        pages.set_vexpand(true);
        pages.set_hexpand(true);
        widget.append(&pages);

        let choose = page_box();
        let question = headline("What would you like to do?");
        choose.append(&question);
        choose.append(&help(
            "Each PC keeps its own workspace. Choose which direction you want to connect.",
        ));
        let choose_connect = choice_button(
            "↗",
            "I want to connect to another PC and view its harnesses",
            "Get a connection link from that PC, then approve this PC there.",
        );
        choose.append(&choose_connect);
        let choose_share = choice_button(
            "↙",
            "I want this PC's harnesses to be available on another PC",
            "Create a link here and approve the request when the other PC connects.",
        );
        choose.append(&choose_share);
        pages.add_named(&choose, Some(Page::Choose.name()));

        let connect_page = page_box();
        connect_page.append(&headline("Connect to another PC"));
        connect_page.append(&help("1. On the other PC, open Add a PC and choose “Make this PC available.”\n2. Create and copy its connection link. Paste that link below.\n3. Compare the six-digit code and approve on the other PC."));
        let invitation = field(
            &connect_page,
            "Connection link",
            "Paste the other PC's connection link",
        );
        invitation.set_visibility(false);
        invitation.set_max_length(crate::peer_client::MAX_INVITATION as i32);
        let name = field(
            &connect_page,
            "PC name (optional)",
            "Use the other PC's name",
        );
        name.set_max_length(64);
        let advanced = gtk4::Expander::new(Some("Use a different network address"));
        let address = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        let host = field(&address, "Host address", "Use the link's address");
        host.set_max_length(253);
        let port = field(&address, "Port", "Use the link's port");
        port.set_max_length(5);
        advanced.set_child(Some(&address));
        connect_page.append(&advanced);
        connect_page.append(&help("Use the address fields only if the link's address cannot be reached over your LAN or VPN. The certificate is still checked against the link."));
        let connect_status = status_label();
        connect_page.append(&connect_status);
        let connect = primary_button("Connect to PC");
        connect_page.append(&connect);
        pages.add_named(&scroll_page(&connect_page), Some(Page::Connect.name()));

        let share_page = page_box();
        share_page.append(&headline("Make this PC available"));
        share_page.append(&help("1. Create a one-time connection link and copy it.\n2. On the other PC, open Add a PC, choose “Connect to another PC,” and paste the link.\n3. Compare the code shown on both PCs before approving here."));
        let create_link = primary_button("Create connection link");
        share_page.append(&create_link);
        let link = field(&share_page, "Your connection link", "Create a link first");
        link.set_editable(false);
        link.set_tooltip_text(Some("Single-use link. It expires after five minutes."));
        let copy_link = gtk4::Button::with_label("Copy link");
        copy_link.add_css_class("hud-button");
        copy_link.set_sensitive(false);
        share_page.append(&copy_link);
        let link_status = status_label();
        share_page.append(&link_status);

        let request_box = gtk4::Box::new(gtk4::Orientation::Vertical, 9);
        request_box.add_css_class("pc-wizard-request");
        request_box.set_visible(false);
        request_box.append(&headline("Connection request waiting"));
        let request_name = help("Another PC wants to connect.");
        request_box.append(&request_name);
        let request_code = gtk4::Label::new(None);
        request_code.add_css_class("pc-wizard-code");
        request_box.append(&request_code);
        let code_matches = gtk4::CheckButton::with_label("The code matches the other PC");
        request_box.append(&code_matches);
        let decisions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let approve = primary_button("Approve this PC");
        approve.set_sensitive(false);
        let deny = gtk4::Button::with_label("Deny");
        deny.add_css_class("hud-button");
        deny.add_css_class("hud-button-danger");
        decisions.append(&approve);
        decisions.append(&deny);
        request_box.append(&decisions);
        share_page.append(&request_box);
        pages.add_named(&scroll_page(&share_page), Some(Page::Share.name()));

        let verify_page = page_box();
        verify_page.append(&headline("Verify the connection"));
        verify_page.append(&help("Compare this code with the one shown on the other PC. Approve the request there only if both codes match."));
        let verify_code = gtk4::Label::new(Some("…"));
        verify_code.add_css_class("pc-wizard-code");
        verify_page.append(&verify_code);
        let verify_status = status_label();
        verify_status.set_text("Connecting securely…");
        verify_page.append(&verify_status);
        pages.add_named(&verify_page, Some(Page::Verify.name()));
        pages.set_visible_child_name(Page::Choose.name());

        let wizard = Rc::new(Self {
            widget,
            window: window.downgrade(),
            pages,
            open: Cell::new(false),
            page: Cell::new(Page::Choose),
            title,
            subtitle,
            back,
            invitation,
            name,
            host,
            port,
            connect_status,
            verify_status,
            verify_code,
            session: RefCell::new(None),
            link,
            link_status,
            create_link,
            copy_link,
            link_generation: Cell::new(0),
            request_box,
            request_name,
            request_code,
            request_id: RefCell::new(None),
            code_matches,
            approve,
            deny,
            request_busy: Cell::new(false),
            poll_busy: Cell::new(false),
            on_saved,
        });
        let weak = Rc::downgrade(&wizard);
        choose_connect.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.show(Page::Connect);
            }
        });
        let weak = Rc::downgrade(&wizard);
        choose_share.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.show(Page::Share);
            }
        });
        let weak = Rc::downgrade(&wizard);
        wizard.back.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.back();
            }
        });
        let weak = Rc::downgrade(&wizard);
        close.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.close();
            }
        });
        let weak = Rc::downgrade(&wizard);
        connect.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.connect();
            }
        });
        let weak = Rc::downgrade(&wizard);
        wizard.create_link.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.create_link();
            }
        });
        let weak = Rc::downgrade(&wizard);
        wizard.copy_link.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                if let Some(display) = gdk::Display::default() {
                    display.clipboard().set_text(&wizard.link.text());
                    wizard
                        .link_status
                        .set_text("Link copied. Paste it into SUPER DESKTOP on the other PC.");
                }
            }
        });
        let weak = Rc::downgrade(&wizard);
        wizard.code_matches.connect_toggled(move |check| {
            if let Some(wizard) = weak.upgrade() {
                wizard.approve.set_sensitive(
                    check.is_active()
                        && wizard.request_id.borrow().is_some()
                        && !wizard.request_busy.get(),
                );
            }
        });
        let weak = Rc::downgrade(&wizard);
        wizard.approve.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.decide(true);
            }
        });
        let weak = Rc::downgrade(&wizard);
        wizard.deny.connect_clicked(move |_| {
            if let Some(wizard) = weak.upgrade() {
                wizard.decide(false);
            }
        });
        let weak = Rc::downgrade(&wizard);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(wizard) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            wizard.poll_host_requests();
            glib::ControlFlow::Continue
        });
        wizard
    }

    // Explicit state survives a transient Wayland unmap while keyboard focus
    // moves between the selector and the layer surface.
    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn open(&self) {
        if self.is_open() {
            return;
        }
        self.show(Page::Choose);
        self.open.set(true);
        self.widget.set_visible(true);
        if let Some(window) = self.window.upgrade() {
            window.set_keyboard_mode(KeyboardMode::Exclusive);
        }
    }

    pub fn close(&self) {
        self.open.set(false);
        self.session.borrow_mut().take();
        self.link_generation
            .set(self.link_generation.get().wrapping_add(1));
        self.invitation.set_text("");
        self.name.set_text("");
        self.host.set_text("");
        self.port.set_text("");
        self.link.set_text("");
        self.copy_link.set_sensitive(false);
        self.create_link.set_sensitive(true);
        self.connect_status.set_text("");
        self.link_status.set_text("");
        self.request_box.set_visible(false);
        self.request_id.borrow_mut().take();
        self.reset_request_controls();
        self.show(Page::Choose);
        self.widget.set_visible(false);
        if let Some(window) = self.window.upgrade() {
            vte4::GtkWindowExt::set_focus(&window, None::<&gtk4::Widget>);
            window.set_keyboard_mode(KeyboardMode::OnDemand);
        }
    }

    fn show(&self, page: Page) {
        self.page.set(page);
        self.pages.set_visible_child_name(page.name());
        self.back.set_visible(page != Page::Choose);
        self.back.set_label(if page == Page::Verify {
            "Cancel"
        } else {
            "← Back"
        });
        let (title, subtitle) = match page {
            Page::Choose => ("Add a PC", "Connect SUPER DESKTOPs"),
            Page::Connect => ("Connect to another PC", "View its harnesses here"),
            Page::Share => (
                "Make this PC available",
                "Share its harnesses with another PC",
            ),
            Page::Verify => ("Verify the connection", "Approve on the other PC"),
        };
        self.title.set_text(title);
        self.subtitle.set_text(subtitle);
    }

    fn back(&self) {
        match self.page.get() {
            Page::Choose => self.close(),
            Page::Connect => self.show(Page::Choose),
            Page::Share => {
                self.link_generation
                    .set(self.link_generation.get().wrapping_add(1));
                self.link.set_text("");
                self.copy_link.set_sensitive(false);
                self.create_link.set_sensitive(true);
                self.request_id.borrow_mut().take();
                self.request_box.set_visible(false);
                self.reset_request_controls();
                self.show(Page::Choose);
            }
            Page::Verify => {
                self.session.borrow_mut().take();
                self.verify_code.set_text("…");
                self.connect_status
                    .set_text("Request canceled. Get a fresh link from the other PC to try again.");
                self.show(Page::Connect);
            }
        }
    }

    fn connect(self: &Rc<Self>) {
        if self.session.borrow().is_some() {
            return;
        }
        match peer_pairing::begin(
            self.invitation.text().to_string(),
            self.host.text().to_string(),
            self.port.text().to_string(),
            self.name.text().to_string(),
        ) {
            Err(error) => self.connect_status.set_text(peer_pairing::message(error)),
            Ok(job) => {
                self.invitation.set_text("");
                *self.session.borrow_mut() = Some(job);
                self.verify_code.set_text("…");
                self.verify_status.set_text("Connecting securely…");
                self.show(Page::Verify);
                let weak = Rc::downgrade(self);
                glib::timeout_add_local(Duration::from_millis(100), move || {
                    let Some(wizard) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    let event = {
                        let current = wizard.session.borrow();
                        let Some(job) = current.as_ref() else {
                            return glib::ControlFlow::Break;
                        };
                        job.events.try_recv()
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            return glib::ControlFlow::Continue
                        }
                        Err(_) => Event::Failed(PeerError("pairing_worker_unavailable")),
                    };
                    match event {
                        Event::Code(code) => {
                            wizard.verify_code.set_text(&code);
                            wizard.verify_status.set_text(&format!(
                                "Compare code {code} on the other PC, then approve it there."
                            ));
                            glib::ControlFlow::Continue
                        }
                        Event::Saved(peer) => {
                            wizard.session.borrow_mut().take();
                            wizard.close();
                            (wizard.on_saved)(peer);
                            glib::ControlFlow::Break
                        }
                        Event::Failed(error) => {
                            wizard.session.borrow_mut().take();
                            wizard.connect_status.set_text(peer_pairing::message(error));
                            wizard.show(Page::Connect);
                            glib::ControlFlow::Break
                        }
                    }
                });
            }
        }
    }

    fn create_link(self: &Rc<Self>) {
        let generation = self.link_generation.get().wrapping_add(1);
        self.link_generation.set(generation);
        self.link.set_text("");
        self.copy_link.set_sensitive(false);
        self.create_link.set_sensitive(false);
        self.link_status
            .set_text("Starting the secure bridge and creating a link…");
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(|| {
                crate::bridge::start_bridge()?;
                crate::bridge::pairing_invitation()
            }).await;
            let Some(wizard) = weak.upgrade() else { return; };
            if wizard.link_generation.get() != generation { return; }
            wizard.create_link.set_sensitive(true);
            match result {
                Ok(Ok(payload)) => {
                    let encoded = crate::ws::base64(payload.as_bytes()).trim_end_matches('=')
                        .replace('+', "-").replace('/', "_");
                    wizard.link.set_text(&format!("superdesktop://pair?data={encoded}"));
                    wizard.link.set_position(0);
                    wizard.copy_link.set_sensitive(true);
                    wizard.link_status.set_text("Copy this link to the other PC. It expires in five minutes.");
                    let weak = Rc::downgrade(&wizard);
                    glib::timeout_add_local_once(Duration::from_secs(300), move || {
                        if let Some(wizard) = weak.upgrade() {
                            if wizard.link_generation.get() == generation {
                                wizard.link.set_text("");
                                wizard.copy_link.set_sensitive(false);
                                wizard.link_status.set_text("This link expired. Create a new one when needed.");
                            }
                        }
                    });
                }
                _ => wizard.link_status.set_text("Could not start the bridge or create a link. Check the connection on this PC and try again."),
            }
        });
    }

    fn poll_host_requests(self: &Rc<Self>) {
        if !self.is_open()
            || self.page.get() != Page::Share
            || self.request_busy.get()
            || self.poll_busy.replace(true)
        {
            return;
        }
        let generation = self.link_generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(crate::bridge::pending_requests).await;
            let Some(wizard) = weak.upgrade() else { return; };
            wizard.poll_busy.set(false);
            if !wizard.is_open() || wizard.page.get() != Page::Share
                || wizard.link_generation.get() != generation { return; }
            let request = result.ok().and_then(|items| items.into_iter().next());
            let Some(request) = request else {
                if wizard.request_id.borrow().is_some() {
                    wizard.request_id.borrow_mut().take();
                    wizard.request_box.set_visible(false);
                }
                return;
            };
            let Some(id) = request["requestId"].as_str() else { return; };
            if wizard.request_id.borrow().as_deref() == Some(id) { return; }
            *wizard.request_id.borrow_mut() = Some(id.to_string());
            let device = request["deviceName"].as_str().unwrap_or("Another PC");
            wizard.request_name.set_text(&format!("{device} wants to view this PC's harnesses. Compare the code below with the code on that PC."));
            wizard.request_code.set_text(request["code"].as_str().unwrap_or(""));
            wizard.reset_request_controls();
            wizard.request_box.set_visible(true);
            wizard.link_status.set_text("Connection request received. Approve only if both codes match.");
        });
    }

    fn decide(self: &Rc<Self>, approve: bool) {
        if approve && !self.code_matches.is_active() {
            return;
        }
        if self.request_busy.replace(true) {
            return;
        }
        let Some(id) = self.request_id.borrow().clone() else {
            self.request_busy.set(false);
            return;
        };
        self.approve.set_sensitive(false);
        self.deny.set_sensitive(false);
        self.code_matches.set_sensitive(false);
        let generation = self.link_generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result =
                gtk4::gio::spawn_blocking(move || crate::bridge::decide_request(&id, approve))
                    .await;
            let Some(wizard) = weak.upgrade() else {
                return;
            };
            wizard.request_busy.set(false);
            if wizard.link_generation.get() != generation
                || !wizard.is_open()
                || wizard.page.get() != Page::Share
            {
                return;
            }
            wizard.deny.set_sensitive(true);
            wizard.code_matches.set_sensitive(true);
            wizard
                .approve
                .set_sensitive(wizard.code_matches.is_active());
            match result {
                Ok(Ok(())) => {
                    wizard
                        .link_generation
                        .set(wizard.link_generation.get().wrapping_add(1));
                    wizard.request_id.borrow_mut().take();
                    wizard.request_box.set_visible(false);
                    wizard.link_status.set_text(if approve {
                        "Approved. This PC is now available from the other PC's selector."
                    } else {
                        "Request denied. Create a new link to try again."
                    });
                    wizard.link.set_text("");
                    wizard.copy_link.set_sensitive(false);
                }
                _ => wizard.link_status.set_text(
                    "Could not decide this request. Check whether it expired, then try again.",
                ),
            }
        });
    }

    fn reset_request_controls(&self) {
        self.code_matches.set_sensitive(true);
        self.code_matches.set_active(false);
        self.approve.set_sensitive(false);
        self.deny.set_sensitive(true);
    }
}

fn page_box() -> gtk4::Box {
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    body.add_css_class("launcher-body");
    body.add_css_class("pc-wizard-page");
    body
}

fn scroll_page(body: &gtk4::Box) -> gtk4::ScrolledWindow {
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("launcher-scroll");
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_child(Some(body));
    scroll
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

fn status_label() -> gtk4::Label {
    let label = gtk4::Label::new(None);
    label.add_css_class("pc-wizard-status");
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_selectable(true);
    label
}

fn field(panel: &gtk4::Box, label: &str, placeholder: &str) -> gtk4::Entry {
    panel.append(&headline(label));
    let entry = gtk4::Entry::new();
    entry.add_css_class("ws-entry");
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    panel.append(&entry);
    entry
}

fn primary_button(text: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(text);
    button.add_css_class("hud-button");
    button.add_css_class("hud-action-primary");
    button.set_halign(gtk4::Align::Start);
    button
}

fn choice_button(icon: &str, title: &str, description: &str) -> gtk4::Button {
    let button = gtk4::Button::new();
    button.add_css_class("settings-entry");
    button.update_property(&[gtk4::accessible::Property::Label(title)]);
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    let icon = gtk4::Label::new(Some(icon));
    icon.add_css_class("settings-entry-icon");
    row.append(&icon);
    let words = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    words.set_hexpand(true);
    words.append(&headline(title));
    words.append(&help(description));
    row.append(&words);
    let arrow = gtk4::Label::new(Some("›"));
    arrow.add_css_class("settings-entry-arrow");
    row.append(&arrow);
    button.set_child(Some(&row));
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Driven by tests/peer_pairing_smoke.py with a disposable real TLS bridge.
    #[test]
    fn wire_inner() {
        let Ok(file) = std::env::var("SUPER_DESKTOP_PAIRING_TEST_INPUT") else {
            return;
        };
        gtk4::init().unwrap();
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        let result_path = std::env::var("SUPER_DESKTOP_PAIRING_TEST_RESULT").unwrap();
        let cancel_path = result_path.clone();
        let saved = Rc::new(Cell::new(false));
        let finished = saved.clone();
        let app = gtk4::Application::new(
            Some("com.superdesktop.PairingWizardTest"),
            gtk4::gio::ApplicationFlags::NON_UNIQUE,
        );
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        let wizard = PairingWizard::new(
            &window,
            Rc::new(move |peer| {
                std::fs::write(&result_path, serde_json::to_vec(&peer).unwrap()).unwrap();
                finished.set(true);
            }),
        );
        window.set_child(Some(&wizard.widget));
        wizard.open();
        assert_eq!(wizard.page.get(), Page::Choose);
        wizard.show(Page::Connect);
        wizard.invitation.set_text(&data["invitation"].to_string());
        wizard.host.set_text("127.0.0.1");
        wizard.port.set_text(&data["port"].to_string());
        wizard.connect();
        assert!(wizard.invitation.text().is_empty());
        assert_eq!(wizard.page.get(), Page::Verify);
        if data["unmap"] == true {
            wizard.widget.emit_by_name::<()>("unmap", &[]);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut printed_code = false;
        let mut canceled_at = None;
        loop {
            while glib::MainContext::default().iteration(false) {}
            if wizard.verify_status.text().starts_with("Compare code") && !printed_code {
                eprintln!("{}", wizard.verify_status.text());
                printed_code = true;
                if data["cancel"] == true {
                    wizard.back();
                    std::fs::write(&cancel_path, "cancelled").unwrap();
                    canceled_at = Some(std::time::Instant::now());
                }
            }
            if wizard.connect_status.text().contains("host denied") {
                assert_eq!(data["approve"], false);
                assert!(crate::peer_store::PeerStore::default_store()
                    .unwrap()
                    .peers()
                    .unwrap()
                    .is_empty());
                eprintln!("pairing_denied");
                return;
            }
            if canceled_at
                .is_some_and(|at: std::time::Instant| at.elapsed() > Duration::from_secs(3))
            {
                assert!(!saved.get());
                assert!(crate::peer_store::PeerStore::default_store()
                    .unwrap()
                    .peers()
                    .unwrap()
                    .is_empty());
                return;
            }
            if saved.get() {
                assert_ne!(data["cancel"], true);
                assert_eq!(data["approve"], true);
                assert!(printed_code);
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pairing wizard never completed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Driven by the same smoke test with the host's private control socket.
    #[test]
    fn share_approval_inner() {
        let Ok(result_path) = std::env::var("SUPER_DESKTOP_SHARE_TEST_RESULT") else {
            return;
        };
        gtk4::init().unwrap();
        let app = gtk4::Application::new(
            Some("com.superdesktop.ShareWizardTest"),
            gtk4::gio::ApplicationFlags::NON_UNIQUE,
        );
        app.register(None::<&gtk4::gio::Cancellable>).unwrap();
        let window = gtk4::ApplicationWindow::new(&app);
        let wizard = PairingWizard::new(&window, Rc::new(|_| {}));
        window.set_child(Some(&wizard.widget));
        wizard.open();
        wizard.show(Page::Share);
        // The isolated bridge's invitation was created through its control
        // socket. The wizard must find pending requests even after reopening,
        // when its local copy of the link has been cleared.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut code = None;
        loop {
            while glib::MainContext::default().iteration(false) {}
            if code.is_none() && wizard.request_id.borrow().is_some() {
                let visible_code = wizard.request_code.text().to_string();
                assert_eq!(visible_code.len(), 6);
                code = Some(visible_code);
                wizard.code_matches.set_active(true);
                assert!(wizard.approve.is_sensitive());
                wizard.decide(true);
            }
            if wizard.link_status.text().starts_with("Approved.") {
                let code = code.expect("approved without a code");
                std::fs::write(result_path, code).unwrap();
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "host wizard did not approve request"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
