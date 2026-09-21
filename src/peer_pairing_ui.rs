//! Pairing form reuses the selector's themed popover.
use crate::{
    peer_client::{PeerError, PeerSummary},
    peer_pairing::{self, Event, Session},
};
use gtk4::{glib, prelude::*};
use std::{cell::RefCell, rc::Rc, time::Duration};

pub fn build(on_back: Rc<dyn Fn()>, on_saved: Rc<dyn Fn(PeerSummary)>) -> gtk4::Box {
    let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    panel.add_css_class("ws-pop-box");
    panel.set_size_request(380, -1);
    let title = gtk4::Label::new(Some("Add a PC"));
    title.add_css_class("settings-entry-title");
    title.set_xalign(0.0);
    panel.append(&title);
    let instructions = gtk4::Label::new(Some(
        "On the other PC, open Settings → Android, enable its bridge and copy a new pairing link.",
    ));
    instructions.set_wrap(true);
    instructions.set_max_width_chars(46);
    instructions.set_xalign(0.0);
    instructions.add_css_class("ws-row-path");
    panel.append(&instructions);
    let invitation = field(&panel, "Pairing link", "Paste the host's pairing link");
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
    let canceled = session.clone();
    let secret = invitation.clone();
    panel.connect_unmap(move |_| {
        canceled.borrow_mut().take();
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
                    if weak_panel.upgrade().is_none() { session.borrow_mut().take(); return glib::ControlFlow::Break; }
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
                            status.set_text(&format!("Compare code {code} on the host, then approve it there.\nClosing this form stops pairing; deny any pending request on the host."));
                            return glib::ControlFlow::Continue;
                        }
                        Event::Saved(peer) => {
                            session.borrow_mut().take();
                            on_saved(peer);
                            return glib::ControlFlow::Break;
                        }
                        Event::Failed(error) => status.set_text(peer_pairing::message(error)),
                    }
                    session.borrow_mut().take();
                    connect.set_sensitive(true); back.set_label("Back");
                    invitation.set_sensitive(true); name.set_sensitive(true); advanced.set_sensitive(true);
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
        assert_eq!(entries.len(), 4);
        assert!(!gtk4::prelude::EntryExt::is_visible(&entries[0])); // Entry text visibility, not widget mapping.
        entries[0].set_text(&data["invitation"].to_string());
        entries[2].set_text("127.0.0.1");
        entries[3].set_text(&data["port"].to_string());
        let connect = widgets
            .iter()
            .filter_map(|w| w.clone().downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        connect.emit_clicked();
        assert!(entries[0].text().is_empty());
        assert!(!connect.is_sensitive());
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
