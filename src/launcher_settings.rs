//! 📱 Launcher connection page of the ⚙ settings card in the overlay HUD.
//!
//! The overlay is a single layer-shell surface (never a separate Hyprland
//! window), and the ⚙ card is a two-page panel: this file builds its second
//! page, with everything needed to connect the OmarchyAILauncher Android app:
//! bridge status (start/stop), firewall unlock, LAN + Tailscale IPs, port, the
//! pending phone requests and explicit approval. The card chrome, the header and
//! the settings ⇄ launcher navigation live in `harness_settings`.
//!
//! Layout: connection, pairing, and devices; network/firewall diagnostics stay
//! collapsed until needed. Every colour/radius/
//! padding lives in `styles.rs` (see the "Launcher Connection Page" block) —
//! this file only builds the widgets.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation, PolicyType, ScrolledWindow, Separator};
use std::cell::Cell;
use std::rc::Rc;

use crate::bridge;

/// Page widget + its refresh handle. `harness_settings` embeds `widget` as the
/// ⚙ card's second page, shows it on navigation, and calls `refresh` so every
/// value is live.
pub struct LauncherPage {
    pub widget: gtk4::Widget,
    pub refresh: Rc<dyn Fn()>,
}

struct LauncherSnapshot {
    online: bool,
    harnesses: usize,
    firewall: (String, bool),
    host: String,
    lan: String,
    tailscale: Option<String>,
    mdns: String,
    pending: Vec<serde_json::Value>,
    devices: Vec<serde_json::Value>,
}

impl LauncherSnapshot {
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
            pending: if online { bridge::pending_requests() } else { vec![] },
            devices: bridge::paired_devices(),
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

/// Builds the Android-connection page: a scrolling stack of compact section
/// cards. Card chrome and the header belong to the ⚙ settings card.
pub fn build_launcher_page() -> LauncherPage {
    let root = Box::new(Orientation::Vertical, 10);
    root.add_css_class("launcher-body");
    root.add_css_class("android-page");
    let network = Box::new(Orientation::Vertical, 10);

    // ---- 1 · bridge status + controls ----
    let (head, body) = section_card(&root, "", "Connection");
    let v_bridge_state = chip("…");
    v_bridge_state.add_css_class("launcher-offline");
    head.append(&v_bridge_state);

    let status = Label::new(None);
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
    btn_stop.set_tooltip_text(Some("Stop the local harness bridge"));
    btn_stop.add_css_class("launcher-btn");
    btn_stop.add_css_class("launcher-btn-danger");
    controls.append(&btn_start);
    controls.append(&btn_stop);
    body.append(&controls);
    let start_weak = btn_start.downgrade();
    let stop_weak = btn_stop.downgrade();

    let port_hint = Label::new(Some("Encrypted end to end · Wi-Fi or Tailscale"));
    port_hint.add_css_class("launcher-hint");
    port_hint.set_xalign(0.0);
    port_hint.set_wrap(true);
    body.append(&port_hint);

    // Outcome of the last start/stop. Failures used to go to the stderr of a
    // daemon nobody reads, which reads as "the button does nothing".
    let bridge_note = Label::new(None);
    bridge_note.add_css_class("launcher-note");
    bridge_note.set_xalign(0.0);
    bridge_note.set_wrap(true);
    bridge_note.set_visible(false);
    body.append(&bridge_note);

    // ---- 2 · firewall (prerequisite) ----
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
    body.append(&v_fw_note);

    // ---- 3 · connect to ----
    let (_, body) = section_card(&network, "", "Network addresses");
    let v_host = kv(&body, "Laptop");
    body.append(&row_sep());
    let v_lan = kv(&body, "LAN IP");
    body.append(&row_sep());
    let v_tail = kv(&body, "Tailscale IP");
    body.append(&row_sep());
    let v_port = kv(&body, "Port");
    body.append(&row_sep());
    let v_mdns = kv(&body, "mDNS");

    // ---- 4 · pairing ----
    let (_, body) = section_card(&root, "", "Pair a phone");
    let explanation = Label::new(Some("Scan the QR on your phone, then approve the matching code here."));
    explanation.add_css_class("launcher-hint");
    explanation.set_wrap(true);
    explanation.set_xalign(0.0);
    body.append(&explanation);
    let invite_button = Button::with_label("＋ Pair Android device");
    invite_button.add_css_class("launcher-btn");
    invite_button.add_css_class("launcher-btn-primary");
    invite_button.set_halign(Align::Start);
    body.append(&invite_button);
    let qr_box = Box::new(Orientation::Vertical, 8);
    body.append(&qr_box);
    invite_button.connect_clicked(move |button| {
        button.set_sensitive(false);
        let button = button.clone();
        let qr_box = qr_box.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(bridge::pairing_invitation).await;
            while let Some(child) = qr_box.first_child() { qr_box.remove(&child); }
            if let Ok(Ok(payload)) = result {
                let encoded = crate::ws::base64(payload.as_bytes()).trim_end_matches('=').replace('+', "-").replace('/', "_");
                let link = format!("superdesktop://pair?data={encoded}");
                if let Ok(code) = qrcode::QrCode::new(link.as_bytes()) {
                    let area = gtk4::DrawingArea::new();
                    area.set_content_width(320);
                    area.set_content_height(320);
                    area.set_halign(gtk4::Align::Start);
                    area.set_valign(gtk4::Align::Start);
                    area.set_hexpand(false);
                    area.set_vexpand(false);
                    area.set_draw_func(move |_, cr, width, height| {
                        let size = code.width();
                        let unit = (width.min(height) as f64 / (size + 8) as f64).floor().max(1.0);
                        let offset_x = (width as f64 - (size+8) as f64 * unit) / 2.0;
                        let offset_y = (height as f64 - (size+8) as f64 * unit) / 2.0;
                        cr.set_source_rgb(1.0,1.0,1.0); let _ = cr.paint();
                        cr.set_source_rgb(0.0,0.0,0.0);
                        for y in 0..size { for x in 0..size {
                            if code[(x,y)] == qrcode::Color::Dark { cr.rectangle(offset_x+(x+4) as f64*unit,offset_y+(y+4) as f64*unit,unit,unit); }
                        } }
                        let _ = cr.fill();
                    });
                    qr_box.append(&area);
                }
                let text = Label::new(Some(&link));
                text.set_selectable(true); text.set_wrap(true); text.set_max_width_chars(60);
                let help = Label::new(Some("Scan and tap Open in SUPER DESKTOP.\nIf your camera does not offer Open, use Bridges → Scan pairing QR.\nSingle-use invitation · expires in 3 minutes."));
                help.set_xalign(0.0); help.set_wrap(true);
                qr_box.append(&help);
                let details = gtk4::Expander::new(Some("Copy pairing link"));
                details.set_child(Some(&text));
                qr_box.append(&details);
                let expiry_box = qr_box.clone();
                glib::timeout_add_local_once(std::time::Duration::from_secs(180), move || {
                    // Only clear the invitation this timer belongs to.
                    if text.parent().is_some() { while let Some(child) = expiry_box.first_child() { expiry_box.remove(&child); } }
                });
            } else { qr_box.append(&Label::new(Some("Start the secure bridge first."))); }
            button.set_sensitive(true);
        });
    });
    let pending_rows = Box::new(Orientation::Vertical, 8);
    body.append(&pending_rows);
    let previous_requests = std::cell::RefCell::new(None);
    let (device_head, body) = section_card(&root, "", "Android devices");
    let device_count = chip("0/0");
    device_count.set_tooltip_text(Some("Active / registered devices. Active means connected or seen in the last 60 seconds."));
    device_head.append(&device_count);
    let device_rows = Box::new(Orientation::Vertical, 8);
    body.append(&device_rows);
    let previous_devices = std::cell::RefCell::new(None);

    let advanced = gtk4::Expander::new(Some("Network & troubleshooting"));
    advanced.add_css_class("android-advanced");
    advanced.set_child(Some(&network));
    root.append(&advanced);

    let footer = Label::new(Some("Only approved devices can access your terminals."));
    footer.add_css_class("launcher-footer");
    footer.set_xalign(0.5);
    footer.set_wrap(true);
    root.append(&footer);

    let scroll = ScrolledWindow::new();
    scroll.add_css_class("launcher-scroll");
    scroll.add_css_class("harness-page");
    // Horizontal scrolling would only ever clip the panels; labels wrap.
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);

    // System commands, sockets and disk reads never run on the GTK thread.
    let firewall_busy = Rc::new(Cell::new(false));
    let firewall_refresh_busy = Rc::clone(&firewall_busy);
    let page_weak = scroll.downgrade();
    let refresh = background_refresh(LauncherSnapshot::collect, move |snapshot| {
        if page_weak.upgrade().is_none() {
            return;
        }
        set_state_class(
            &v_bridge_state,
            snapshot.online,
            "launcher-online",
            "launcher-offline",
        );
        if let Some(start) = start_weak.upgrade() { start.set_visible(!snapshot.online); }
        if let Some(stop) = stop_weak.upgrade() { stop.set_visible(snapshot.online); }
        if snapshot.online {
            let n = snapshot.harnesses;
            set_text(&v_bridge_state, "● ONLINE");
            set_text(
                &status,
                &format!(
                    "{} · {n} terminal{} available",
                    snapshot.host, if n == 1 { "" } else { "s" }
                ),
            );
        } else {
            set_text(&v_bridge_state, "○ OFFLINE");
            set_text(&status, "Start the bridge, then pair the phone below.");
        }
        let (fw_text, fw_can_unlock) = snapshot.firewall;
        set_text(&v_fw, &fw_text);
        if let Some(b) = btn_fw_weak.upgrade() {
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
        let active = snapshot.devices.iter().filter(|d| d["active"] == true).count();
        set_text(&device_count, &format!("{active}/{}", snapshot.devices.len()));
        if previous_requests.borrow().as_ref() != Some(&snapshot.pending) {
            while let Some(child) = pending_rows.first_child() { pending_rows.remove(&child); }
            pending_rows.set_visible(!snapshot.pending.is_empty());
            for request in &snapshot.pending {
                let row = Box::new(Orientation::Vertical, 4);
                row.add_css_class("android-request");
                let label = Label::new(Some(&format!("{} · {}\nVerification code: {}",
                    request["deviceName"].as_str().unwrap_or("Phone"),
                    request["address"].as_str().unwrap_or(""),
                    request["code"].as_str().unwrap_or(""))));
                label.set_xalign(0.0);
                label.set_wrap(true);
                row.append(&label);
                let buttons = Box::new(Orientation::Horizontal, 8);
                for (title, approve) in [("Approve", true), ("Deny", false)] {
                    let button = Button::with_label(title);
                    button.add_css_class("launcher-btn");
                    let id = request["requestId"].as_str().unwrap_or("").to_string();
                    let buttons_for_click = buttons.clone();
                    let label = label.clone();
                    button.connect_clicked(move |_| {
                        buttons_for_click.set_sensitive(false);
                        let id = id.clone();
                        let label = label.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let result = gtk4::gio::spawn_blocking(move || bridge::decide_request(&id, approve)).await;
                            label.set_text(match result {
                                Ok(Ok(())) => if approve {"Phone approved."} else {"Request denied."},
                                _ => "Decision failed or request expired. Request pairing again on the phone.",
                            });
                        });
                    });
                    buttons.append(&button);
                }
                row.append(&buttons);
                pending_rows.append(&row);
            }
            *previous_requests.borrow_mut() = Some(snapshot.pending);
        }
        if previous_devices.borrow().as_ref() != Some(&snapshot.devices) {
            while let Some(child) = device_rows.first_child() { device_rows.remove(&child); }
            if snapshot.devices.is_empty() {
                let empty = Label::new(Some("No devices yet\nPair your first phone using the QR above."));
                empty.add_css_class("android-empty"); empty.set_xalign(0.0);
                device_rows.append(&empty);
            }
            for device in &snapshot.devices {
                let row = Box::new(Orientation::Horizontal, 8);
                row.add_css_class("android-device-row");
                let name = Label::new(Some(device["name"].as_str().unwrap_or("Phone")));
                name.set_wrap(true); name.set_hexpand(true); name.set_xalign(0.0);
                row.append(&name);
                let active = device["active"] == true;
                let state = chip(if active { "● Active" } else { "Offline" });
                state.add_css_class(if active { "launcher-online" } else { "launcher-offline" });
                row.append(&state);
                let revoke = Button::with_label("Revoke access");
                revoke.add_css_class("launcher-btn");
                revoke.add_css_class("launcher-btn-danger");
                let id = device["id"].as_str().unwrap_or("").to_string();
                revoke.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    let button = button.clone(); let id = id.clone();
                    glib::MainContext::default().spawn_local(async move {
                        let ok = matches!(gtk4::gio::spawn_blocking(move || bridge::revoke_device(&id)).await, Ok(Ok(())));
                        button.set_label(if ok { "Revoked" } else { "Retry revoke" });
                        button.set_sensitive(!ok);
                    });
                });
                row.append(&revoke); device_rows.append(&row);
            }
            *previous_devices.borrow_mut() = Some(snapshot.devices);
        }
    });

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
                        note.set_text(&format!("● Bridge answering on :{}", bridge::BRIDGE_PORT));
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
    // session), `tailscale` (~100ms) and `hostname`, and this page is built on
    // the window-build path. The content is filled when the ⚙ card navigates
    // here (`refresh`) and then every 2s while it stays on screen.

    // Live refresh while the page is on screen. `is_mapped` is false both when
    // the card navigated away from this page and when the card (or the whole
    // overlay) is hidden, so nothing probes tmux / `tailscale` / `hostname`
    // every 2s for a page nobody is looking at.
    let weak = scroll.downgrade();
    let tick = Rc::clone(&refresh);
    gtk4::glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        let Some(s) = weak.upgrade() else {
            return gtk4::glib::ControlFlow::Break;
        };
        if s.is_mapped() {
            tick();
        }
        gtk4::glib::ControlFlow::Continue
    });

    LauncherPage {
        widget: scroll.upcast(),
        refresh,
    }
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
    if !num.is_empty() { head.append(&n); }
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
fn kv(parent: &Box, key: &str) -> Label {
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
    fn android_page_visual_preview() {
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let page = build_launcher_page();
        let window = gtk4::Window::new();
        window.set_title(Some("SUPER DESKTOP · Android settings preview"));
        window.set_default_size(600, 700);
        window.add_css_class("mini-terminal");
        window.set_child(Some(&page.widget));
        window.present();
        (page.refresh)();
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < until {
            while context.pending() { context.iteration(false); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let snapshot = gtk4::Snapshot::new();
        let paintable = gtk4::WidgetPaintable::new(Some(&page.widget));
        paintable.snapshot(&snapshot, page.widget.width() as f64, page.widget.height() as f64);
        let node = snapshot.to_node().unwrap();
        window.renderer().unwrap().render_texture(&node, None).save_to_png("/tmp/sd-android-settings-preview.png").unwrap();
        window.close();
    }

    /// Count widgets carrying `class` in the subtree rooted at `w`.
    fn count_class(w: &gtk4::Widget, class: &str) -> usize {
        let mut n = usize::from(w.has_css_class(class));
        let mut child = w.first_child();
        while let Some(c) = child {
            n += count_class(&c, class);
            child = c.next_sibling();
        }
        n
    }

    #[test]
    fn test_page_is_built_from_numbered_section_cards() {
        // GTK may only be used from one thread per process, so the widget
        // assertions run in a child process (see `crate::gtk_test`).
        crate::gtk_test::run_in_child_process("launcher_settings::tests::page_gtk_structure");
    }

    /// Only meaningful when re-run as the single test of a fresh process.
    #[test]
    fn page_gtk_structure() {
        if !crate::gtk_test::is_child() {
            return;
        }
        let _ = gtk4::init();
        check_background_refresh();
        let page = build_launcher_page();

        // The ⚙ settings card owns the chrome (`mini-terminal`/`harness-panel`);
        // this page is only the scrolling body it embeds.
        assert!(page.widget.has_css_class("launcher-scroll"));
        assert!(page.widget.downcast_ref::<ScrolledWindow>().is_some());
        assert!(!page.widget.has_css_class("mini-terminal"));

        // Only connection, pairing and devices are shown initially.
        assert_eq!(count_class(&page.widget, "launcher-section"), 3);
        assert_eq!(count_class(&page.widget, "launcher-section-num"), 0);
        assert_eq!(count_class(&page.widget, "launcher-section-title"), 3);

        // No legacy PIN panel; five address rows and four phone steps.
        assert_eq!(count_class(&page.widget, "launcher-pin-box"), 0);
        assert_eq!(count_class(&page.widget, "launcher-value"), 0);
        assert_eq!(count_class(&page.widget, "launcher-step"), 0);

        // Start / Stop / Unlock / pairing QR; approvals appear for requests.
        assert_eq!(count_class(&page.widget, "launcher-btn"), 3);

        // Navigation calls this on every entry: it must not panic and must
        // leave the bridge chip in one of its two styled states.
        (page.refresh)();
        assert_eq!(count_class(&page.widget, "term-status-badge"), 2);
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
