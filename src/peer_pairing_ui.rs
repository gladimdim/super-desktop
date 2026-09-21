//! Pairing form reuses the selector's themed popover.
use crate::{
    peer_client::{PeerError, PeerSummary},
    peer_pairing::{self, Event, Session},
};
use gtk4::{gdk, glib, prelude::*};
use std::{cell::RefCell, rc::Rc, time::Duration};

pub fn build(on_back: Rc<dyn Fn()>, on_saved: Rc<dyn Fn(PeerSummary)>) -> gtk4::Box {
    let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    panel.add_css_class("ws-pop-box");
    panel.set_size_request(380, -1);
    let title = gtk4::Label::new(Some("Connect PCs"));
    title.add_css_class("settings-entry-title");
    title.set_xalign(0.0);
    panel.append(&title);
    let share_title = gtk4::Label::new(Some("Share this PC with another PC"));
    share_title.add_css_class("settings-entry-title");
    share_title.set_xalign(0.0);
    panel.append(&share_title);
    let instructions = gtk4::Label::new(Some(
        "Create a one-time connection link here, then paste it into SUPER DESKTOP on the other PC.",
    ));
    instructions.set_wrap(true);
    instructions.set_max_width_chars(46);
    instructions.set_xalign(0.0);
    instructions.add_css_class("ws-row-path");
    panel.append(&instructions);
    let share_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let create_link = gtk4::Button::with_label("Create connection link");
    create_link.add_css_class("hud-button");
    create_link.add_css_class("hud-action-primary");
    share_actions.append(&create_link);
    let copy_link = gtk4::Button::with_label("Copy link");
    copy_link.add_css_class("hud-button");
    copy_link.set_sensitive(false);
    share_actions.append(&copy_link);
    panel.append(&share_actions);
    let generated_link = gtk4::Entry::new();
    generated_link.add_css_class("ws-entry");
    generated_link.set_editable(false);
    generated_link.set_placeholder_text(Some("One-time link appears here"));
    generated_link.set_tooltip_text(Some(
        "Single-use link. Copy it to the other PC; it expires after five minutes.",
    ));
    panel.append(&generated_link);
    let share_status = gtk4::Label::new(None);
    share_status.add_css_class("ws-row-path");
    share_status.set_xalign(0.0);
    panel.append(&share_status);
    let link_for_copy = generated_link.clone();
    let share_status_copy = share_status.clone();
    copy_link.connect_clicked(move |_| {
        let link = link_for_copy.text();
        if !link.is_empty() {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&link);
                share_status_copy.set_text("Connection link copied. Paste it into the other PC.");
            }
        }
    });
    let generated_link_for_worker = generated_link.clone();
    let share_status_worker = share_status.clone();
    let copy_link_worker = copy_link.clone();
    let create_link_button = create_link.clone();
    create_link.connect_clicked(move |_| {
        create_link_button.set_sensitive(false);
        share_status_worker.set_text("Creating a secure one-time connection link…");
        let generated_link = generated_link_for_worker.clone();
        let status = share_status_worker.clone();
        let copy = copy_link_worker.clone();
        let create = create_link_button.clone();
        glib::MainContext::default().spawn_local(async move {
            match gtk4::gio::spawn_blocking(crate::bridge::pairing_invitation).await {
                Ok(Ok(payload)) => {
                    let encoded = crate::ws::base64(payload.as_bytes()).trim_end_matches('=')
                        .replace('+', "-").replace('/', "_");
                    generated_link.set_text(&format!("superdesktop://pair?data={encoded}"));
                    generated_link.set_position(0);
                    copy.set_sensitive(true);
                    status.set_text("Copy this link into SUPER DESKTOP on the other PC. It expires in five minutes.");
                    let generated_link_expiry = generated_link.clone();
                    let copy_expiry = copy.clone();
                    let status_expiry = status.clone();
                    glib::timeout_add_local_once(Duration::from_secs(300), move || {
                        generated_link_expiry.set_text("");
                        copy_expiry.set_sensitive(false);
                        status_expiry.set_text("That connection link expired. Create a new one when needed.");
                    });
                }
                _ => status.set_text("Start the secure bridge in Settings → Android, then create the link again."),
            }
            create.set_sensitive(true);
        });
    });
    let divider = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    panel.append(&divider);
    let receive_title = gtk4::Label::new(Some("Connect this PC to another PC"));
    receive_title.add_css_class("settings-entry-title");
    receive_title.set_xalign(0.0);
    panel.append(&receive_title);
    let receive_help = gtk4::Label::new(Some(
        "On the other PC, choose Add a PC, create a connection link, and paste it below. Then compare and approve the code on that PC.",
    ));
    receive_help.set_wrap(true);
    receive_help.set_max_width_chars(46);
    receive_help.set_xalign(0.0);
    receive_help.add_css_class("ws-row-path");
    panel.append(&receive_help);
    let invitation = field(
        &panel,
        "Connection link",
        "Paste the other PC's connection link",
    );
    invitation.set_visibility(false);
    invitation.set_max_length(crate::peer_client::MAX_INVITATION as i32);
    let name = field(&panel, "PC name (optional)", "Use the host's name");
    name.set_max_length(64);
    let advanced = gtk4::Expander::new(Some("Connection address (optional)"));
    let address = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    let host = field(&address, "Host address", "Use the invitation's address");
    host.set_max_length(253);
    let port = field(&address, "Port", "Use the invitation's port");
    port.set_max_length(5);
    advanced.set_child(Some(&address));
    panel.append(&advanced);
    let status = gtk4::Label::new(None);
    status.set_wrap(true);
    status.set_max_width_chars(46);
    status.set_xalign(0.0);
    status.add_css_class("ws-row-name");
    status.set_selectable(true);
    panel.append(&status);
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let back = gtk4::Button::with_label("Back");
    back.add_css_class("hud-button");
    actions.append(&back);
    let connect = gtk4::Button::with_label("Connect");
    connect.add_css_class("hud-button");
    connect.add_css_class("hud-action-primary");
    actions.append(&connect);
    panel.append(&actions);
    let session: Rc<RefCell<Option<Session>>> = Rc::new(RefCell::new(None));
    let secret = invitation.clone();
    panel.connect_unmap(move |_| {
        // A MenuButton popover may transiently unmap while Wayland negotiates
        // focus. The authenticated request is already pending on the host, so
        // do not silently throw away its session here. Explicit Back/Cancel
        // below remains the user's cancellation control.
        secret.set_text("");
    });
    let canceled = session.clone();
    back.connect_clicked(move |_| {
        canceled.borrow_mut().take();
        on_back();
    });
    let weak_panel = panel.downgrade();
    let status_start = status.clone();
    connect.connect_clicked(move |connect| {
        if session.borrow().is_some() { return; }
        let result = peer_pairing::begin(invitation.text().to_string(), host.text().to_string(), port.text().to_string(), name.text().to_string());
        match result {
            Err(error) => status_start.set_text(peer_pairing::message(error)),
            Ok(job) => {
                // Clear the link once submitted; local validation errors allow editing.
                invitation.set_text("");
                *session.borrow_mut() = Some(job);
                status_start.set_text("Connecting securely…");
                connect.set_sensitive(false);
                back.set_label("Cancel");
                invitation.set_sensitive(false); name.set_sensitive(false); advanced.set_sensitive(false);
                let session = session.clone(); let weak_panel = weak_panel.clone();
                let status = status_start.clone(); let connect = connect.clone();
                let invitation = invitation.clone(); let name = name.clone(); let advanced = advanced.clone();
                let back = back.clone(); let on_saved = on_saved.clone();
                glib::timeout_add_local(Duration::from_millis(100), move || {
                    let visible = weak_panel.upgrade().is_some_and(|panel| panel.is_mapped());
                    let event = {
                        let current = session.borrow();
                        let Some(job) = current.as_ref() else { return glib::ControlFlow::Break; };
                        job.events.try_recv()
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                        Err(_) => Event::Failed(PeerError("pairing_worker_unavailable")),
                    };
                    match event {
                        Event::Code(code) => {
                            status.set_text(&format!("Compare code {code} on the host, then approve it there.\nUse Cancel to stop this request."));
                            return glib::ControlFlow::Continue;
                        }
                        Event::Saved(peer) => {
                            session.borrow_mut().take();
                            on_saved(peer);
                            return glib::ControlFlow::Break;
                        }
                        Event::Failed(error) => {
                            status.set_text(peer_pairing::message(error));
                        }
                    }
                    session.borrow_mut().take();
                    if visible || !connect.is_sensitive() {
                        connect.set_sensitive(true); back.set_label("Back");
                        invitation.set_sensitive(true); name.set_sensitive(true); advanced.set_sensitive(true);
                    }
                    glib::ControlFlow::Break
                });
            }
        }
    });
    panel
}
fn field(panel: &gtk4::Box, text: &str, placeholder: &str) -> gtk4::Entry {
    let label = gtk4::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("ws-row-path");
    panel.append(&label);
    let entry = gtk4::Entry::new();
    entry.add_css_class("ws-entry");
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    panel.append(&entry);
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn descendants(widget: &gtk4::Widget, output: &mut Vec<gtk4::Widget>) {
        if let Some(expander) = widget.downcast_ref::<gtk4::Expander>() {
            expander.set_expanded(true);
        }
        output.push(widget.clone());
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            descendants(&widget, output);
        }
    }
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
        let panel = build(
            Rc::new(|| {}),
            Rc::new(move |peer| {
                std::fs::write(&result_path, serde_json::to_vec(&peer).unwrap()).unwrap();
                finished.set(true);
            }),
        );
        let mut widgets = Vec::new();
        descendants(panel.upcast_ref(), &mut widgets);
        let entries: Vec<_> = widgets
            .iter()
            .filter_map(|w| w.clone().downcast::<gtk4::Entry>().ok())
            .collect();
        assert_eq!(entries.len(), 5);
        assert!(entries[0].text().is_empty()); // Generated links start blank.
        entries[1].set_text(&data["invitation"].to_string());
        entries[3].set_text("127.0.0.1");
        entries[4].set_text(&data["port"].to_string());
        let connect = widgets
            .iter()
            .filter_map(|w| w.clone().downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        connect.emit_clicked();
        assert!(entries[1].text().is_empty());
        assert!(!connect.is_sensitive());
        if data["unmap"] == true {
            panel.emit_by_name::<()>("unmap", &[]);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut printed_code = false;
        let mut canceled_at = None;
        loop {
            while glib::MainContext::default().iteration(false) {}
            for label in widgets
                .iter()
                .filter_map(|w| w.clone().downcast::<gtk4::Label>().ok())
            {
                if label.text().starts_with("Compare code") && !printed_code {
                    eprintln!("{}", label.text());
                    printed_code = true;
                    if data["cancel"] == true {
                        let cancel = widgets
                            .iter()
                            .filter_map(|w| w.clone().downcast::<gtk4::Button>().ok())
                            .find(|b| b.label().as_deref() == Some("Cancel"))
                            .unwrap();
                        cancel.emit_clicked();
                        std::fs::write(&cancel_path, "cancelled").unwrap();
                        canceled_at = Some(std::time::Instant::now());
                    }
                }
                if label.text().contains("host denied") {
                    assert_eq!(data["approve"], false);
                    assert!(connect.is_sensitive());
                    assert!(crate::peer_store::PeerStore::default_store()
                        .unwrap()
                        .peers()
                        .unwrap()
                        .is_empty());
                    eprintln!("pairing_denied");
                    return;
                }
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
                "pairing form never completed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
