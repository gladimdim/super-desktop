//! 📱 Launcher connection card for the overlay HUD.
//!
//! A floating hover card that lives INSIDE the super-desktop layer-shell
//! overlay (centered above notes and terminals) — never a separate Hyprland
//! window. Shows everything needed to connect the OmarchyAILauncher Android
//! app: bridge status (start/stop), firewall unlock, LAN + Tailscale IPs,
//! port, the pairing PIN, and the 120s pairing window.

use gtk4::prelude::*;
use gtk4::{Box, Button, Label, Orientation, ScrolledWindow};
use std::rc::Rc;

use crate::bridge;

/// Floating card + its refresh handle. The overlay adds `widget` centered
/// and toggles visibility; `refresh` re-reads everything live.
pub struct LauncherPanel {
    pub widget: gtk4::Widget,
    pub refresh: Rc<dyn Fn()>,
}

pub fn build_launcher_panel() -> LauncherPanel {
    let outer = Box::new(Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.set_size_request(620, 700);

    // ---- header ----
    let header = Box::new(Orientation::Horizontal, 8);
    header.add_css_class("term-header");
    let title = Label::new(Some("📱 Launcher connection"));
    title.add_css_class("term-title");
    title.set_halign(gtk4::Align::Start);
    title.set_hexpand(true);
    header.append(&title);
    let btn_close = Button::with_label("✕");
    btn_close.set_tooltip_text(Some("Close panel"));
    btn_close.add_css_class("term-btn");
    header.append(&btn_close);
    outer.append(&header);

    let weak_outer = outer.downgrade();
    btn_close.connect_clicked(move |_| {
        if let Some(o) = weak_outer.upgrade() {
            o.set_visible(false);
        }
    });

    let root = Box::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(14);
    root.set_margin_end(14);

    // ---- bridge status + controls ----
    let status = Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    root.append(&status);

    let controls = Box::new(Orientation::Horizontal, 8);
    let btn_start = Button::with_label("▶ Start bridge");
    btn_start.set_tooltip_text(Some("Launch super-desktop harness-bridge on :8759"));
    btn_start.add_css_class("hud-button");
    let btn_stop = Button::with_label("■ Stop");
    btn_stop.set_tooltip_text(Some("Stop the local harness bridge"));
    btn_stop.add_css_class("hud-button");
    controls.append(&btn_start);
    controls.append(&btn_stop);
    root.append(&controls);

    // ---- firewall (prerequisite) ----
    root.append(&section("1 · Firewall"));
    let fw_row = Box::new(Orientation::Horizontal, 8);
    let v_fw = Label::new(None);
    v_fw.set_xalign(0.0);
    v_fw.set_hexpand(true);
    v_fw.set_wrap(true);
    fw_row.append(&v_fw);
    let btn_fw = Button::with_label("🔓 Unlock");
    btn_fw.set_tooltip_text(Some("Allow 8759/tcp via a password prompt"));
    btn_fw.add_css_class("hud-button");
    fw_row.append(&btn_fw);
    let btn_fw_weak = btn_fw.downgrade();
    root.append(&fw_row);
    let v_fw_note = Label::new(None);
    v_fw_note.set_xalign(0.0);
    v_fw_note.set_wrap(true);
    root.append(&v_fw_note);

    // ---- connect to ----
    root.append(&section("2 · Connect to"));
    let rows = Box::new(Orientation::Vertical, 2);
    root.append(&rows);
    let v_host = kv(&rows, "Laptop");
    let v_lan = kv(&rows, "LAN IP");
    let v_tail = kv(&rows, "Tailscale IP");
    let v_port = kv(&rows, "Port");
    let v_mdns = kv(&rows, "mDNS");

    // ---- pairing ----
    root.append(&section("3 · Pair"));
    let pin_row = Box::new(Orientation::Horizontal, 8);
    let pin_title = Label::new(Some("PIN:"));
    pin_row.append(&pin_title);
    let v_pin = Label::new(None);
    pin_row.append(&v_pin);
    root.append(&pin_row);

    let pair_row = Box::new(Orientation::Horizontal, 8);
    let btn_pin = Button::with_label("🎲 New PIN");
    btn_pin.set_tooltip_text(Some("Generate a fresh pairing PIN (applies instantly)"));
    btn_pin.add_css_class("hud-button");
    let btn_window = Button::with_label("🔓 Open 120s window");
    btn_window.set_tooltip_text(Some("Let the phone pair with no PIN for 2 minutes"));
    btn_window.add_css_class("hud-button");
    pair_row.append(&btn_pin);
    pair_row.append(&btn_window);
    root.append(&pair_row);

    let v_window = Label::new(None);
    v_window.set_xalign(0.0);
    root.append(&v_window);

    // ---- phone steps ----
    root.append(&section("On the phone"));
    let steps = Label::new(Some(
        "1. Unlock the firewall above (unless it is off).\n\
         2. In OmarchyAILauncher open ⋮⋮⋮ → ⚙ → Bridge connection.\n\
         3. Enter the LAN IP (same Wi-Fi) or the Tailscale IP, then Test.\n\
         4. Tap Pair — empty PIN if you opened the window here, \
         or type the PIN shown above.",
    ));
    steps.set_xalign(0.0);
    steps.set_wrap(true);
    root.append(&steps);

    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&root));
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    outer.append(&scroll);

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
        let (fw_text, fw_can_unlock) = bridge::firewall_summary();
        v_fw.set_text(&fw_text);
        if let Some(b) = btn_fw_weak.upgrade() {
            b.set_sensitive(fw_can_unlock);
        }
        v_host.set_text(&bridge::hostname());
        v_lan.set_text(&bridge::lan_ip());
        v_tail.set_text(
            bridge::tailscale_ip()
                .as_deref()
                .unwrap_or("— (Tailscale off)"),
        );
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
    btn_fw.connect_clicked({
        let refresh = Rc::clone(&refresh);
        let note = v_fw_note.clone();
        move |btn| {
            btn.set_sensitive(false);
            note.set_text("Waiting for the password prompt…");
            // Only the result string crosses threads; widgets stay on the
            // GTK thread and poll the channel from an idle callback.
            // Fresh clones per click: the outer closure is Fn.
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            std::thread::spawn(move || {
                let msg = match bridge::unlock_firewall() {
                    Ok(m) => m,
                    Err(e) => format!("Unlock failed: {e}"),
                };
                let _ = tx.send(msg);
            });
            let note2 = note.clone();
            let refresh2 = Rc::clone(&refresh);
            gtk4::glib::idle_add_local(move || {
                match rx.try_recv() {
                    Ok(msg) => {
                        note2.set_text(&msg);
                        refresh2();
                        gtk4::glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        note2.set_text("Unlock failed");
                        refresh2();
                        gtk4::glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        gtk4::glib::ControlFlow::Continue
                    }
                }
            });
        }
    });

    refresh();

    // Live refresh while the overlay lives; only re-reads when visible.
    let weak = outer.downgrade();
    let tick = Rc::clone(&refresh);
    gtk4::glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        let Some(o) = weak.upgrade() else {
            return gtk4::glib::ControlFlow::Break;
        };
        if o.is_visible() {
            tick();
        }
        gtk4::glib::ControlFlow::Continue
    });

    LauncherPanel {
        widget: outer.upcast(),
        refresh,
    }
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
