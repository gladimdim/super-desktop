//! ⚙ Launcher settings panel for the overlay HUD.
//!
//! Shows everything needed to connect the OmarchyAILauncher Android app:
//! bridge status (start/stop), LAN + Tailscale IPs, port, mDNS name, the
//! pairing PIN, and the 120s pairing window — plus the phone-side steps.

use gtk4::prelude::*;
use gtk4::{Box, Button, Label, Orientation, ScrolledWindow, Window};
use std::rc::Rc;

use crate::bridge;

/// Open the Launcher connection panel. Safe to call repeatedly.
pub fn show_launcher_settings() {
    let win = Window::new();
    win.set_title(Some("📱 Launcher connection"));
    win.set_default_size(580, 700);

    let root = Box::new(Orientation::Vertical, 8);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    // ---- bridge status + controls ----
    let status = Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    root.append(&status);

    let controls = Box::new(Orientation::Horizontal, 8);
    let btn_start = Button::with_label("▶ Start bridge");
    btn_start.set_tooltip_text(Some("Launch super-desktop harness-bridge on :8759"));
    let btn_stop = Button::with_label("■ Stop");
    btn_stop.set_tooltip_text(Some("Stop the local harness bridge"));
    let btn_refresh = Button::with_label("↻ Refresh");
    controls.append(&btn_start);
    controls.append(&btn_stop);
    controls.append(&btn_refresh);
    root.append(&controls);

    root.append(&section("Connect to"));
    let rows = Box::new(Orientation::Vertical, 2);
    root.append(&rows);
    let v_host = kv(&rows, "Laptop");
    let v_lan = kv(&rows, "LAN IP");
    let v_tail = kv(&rows, "Tailscale IP");
    let v_port = kv(&rows, "Port");
    let v_mdns = kv(&rows, "mDNS");

    // ---- pairing ----
    root.append(&section("Pairing"));
    let pin_row = Box::new(Orientation::Horizontal, 8);
    let pin_title = Label::new(Some("PIN:  "));
    pin_row.append(&pin_title);
    let v_pin = Label::new(None);
    pin_row.append(&v_pin);
    root.append(&pin_row);

    let pair_row = Box::new(Orientation::Horizontal, 8);
    let btn_pin = Button::with_label("🎲 New PIN");
    btn_pin.set_tooltip_text(Some("Generate a fresh pairing PIN (applies instantly)"));
    let btn_window = Button::with_label("🔓 Open 120s window");
    btn_window.set_tooltip_text(Some("Let the phone pair with no PIN for 2 minutes"));
    pair_row.append(&btn_pin);
    pair_row.append(&btn_window);
    root.append(&pair_row);

    let v_window = Label::new(None);
    v_window.set_xalign(0.0);
    root.append(&v_window);

    // ---- phone steps ----
    root.append(&section("On the phone"));
    let steps = Label::new(Some(
        "1. Open OmarchyAILauncher → ⋮⋮⋮ → ⚙ → Bridge connection.\n\
         2. Enter the LAN IP (same Wi-Fi) or the Tailscale IP above, then Test.\n\
         3. Tap Pair — with an empty PIN if you opened the window here, \
         or type the PIN shown above.\n\
         4. Start the bridge first if the status says OFFLINE.",
    ));
    steps.set_xalign(0.0);
    steps.set_wrap(true);
    root.append(&steps);

    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);
    win.set_child(Some(&scroll));
    win.present();

    // Shared refresh: re-read everything live.
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        if bridge::bridge_running(bridge::BRIDGE_PORT) {
            let n = bridge::collect_harnesses().len();
            status.set_markup(&format!(
                "<b>● Bridge ONLINE</b> — {n} live harness{}",
                if n == 1 { "" } else { "es" }
            ));
        } else {
            status.set_markup("<b>○ Bridge OFFLINE</b> — start it, then pair the phone");
        }
        v_host.set_text(&bridge::hostname());
        v_lan.set_text(&bridge::lan_ip());
        v_tail.set_text(bridge::tailscale_ip().as_deref().unwrap_or("— (Tailscale off)"));
        v_port.set_text(&bridge::BRIDGE_PORT.to_string());
        v_mdns.set_text("_omarchy-harness._tcp");
        v_pin.set_markup(&format!(
            "<span size='32000' weight='bold'>{}</span>",
            bridge::read_pin()
        ));
        let left = bridge::pairing_seconds_left();
        if left > 0 {
            v_window.set_markup(&format!("<b>🔓 Window open — {left}s left</b>"));
        } else {
            v_window.set_text("Window closed — the phone needs the PIN unless you open it.");
        }
    });

    btn_start.connect_clicked({
        let refresh = Rc::clone(&refresh);
        move |_| {
            if let Err(e) = bridge::start_bridge() {
                eprintln!("SUPER DESKTOP: start bridge: {e}");
            }
            refresh();
        }
    });
    btn_stop.connect_clicked({
        let refresh = Rc::clone(&refresh);
        move |_| {
            if let Err(e) = bridge::stop_bridge() {
                eprintln!("SUPER DESKTOP: stop bridge: {e}");
            }
            refresh();
        }
    });
    btn_refresh.connect_clicked({
        let refresh = Rc::clone(&refresh);
        move |_| refresh()
    });
    btn_pin.connect_clicked({
        let refresh = Rc::clone(&refresh);
        move |_| {
            bridge::rotate_pin();
            refresh();
        }
    });
    btn_window.connect_clicked({
        let refresh = Rc::clone(&refresh);
        move |_| {
            if let Err(e) = bridge::open_pairing_window() {
                eprintln!("SUPER DESKTOP: open pairing window: {e}");
            }
            refresh();
        }
    });

    refresh();

    // Live refresh while open; stops itself when the window closes.
    let weak = win.downgrade();
    let tick = Rc::clone(&refresh);
    gtk4::glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        if weak.upgrade().is_none() {
            return gtk4::glib::ControlFlow::Break;
        }
        tick();
        gtk4::glib::ControlFlow::Continue
    });
}

fn section(title: &str) -> Label {
    let l = Label::new(None);
    l.set_markup(&format!("<b>{title}</b>"));
    l.set_xalign(0.0);
    l.set_margin_top(8);
    l
}

/// Selectable key: value row (values are copy-pasteable). Returns the value label.
fn kv(parent: &Box, key: &str) -> Label {
    let row = Box::new(Orientation::Horizontal, 8);
    let k = Label::new(Some(&format!("{key}:")));
    k.set_xalign(0.0);
    k.set_size_request(110, -1);
    let v = Label::new(None);
    v.set_xalign(0.0);
    v.set_selectable(true);
    v.set_hexpand(true);
    row.append(&k);
    row.append(&v);
    parent.append(&row);
    v
}
