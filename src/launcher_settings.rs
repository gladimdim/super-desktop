//! 📱 Launcher connection page of the ⚙ settings card in the overlay HUD.
//!
//! The overlay is a single layer-shell surface (never a separate Hyprland
//! window), and the ⚙ card is a two-page panel: this file builds its second
//! page, with everything needed to connect the OmarchyAILauncher Android app:
//! bridge status (start/stop), firewall unlock, LAN + Tailscale IPs, port, the
//! pairing PIN, and the 120s pairing window. The card chrome, the header and
//! the settings ⇄ launcher navigation live in `harness_settings`.
//!
//! Layout: a stack of bordered, numbered section panels (`.launcher-section`):
//! bridge, firewall, addresses, pairing, phone steps. Every colour/radius/
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
    pin: String,
    pairing_left: u64,
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
            pin: bridge::read_pin(),
            pairing_left: bridge::pairing_seconds_left(),
        }
    }
}

/// At most one probe runs at a time. Requests arriving during it trigger a
/// follow-up, so a slow old probe cannot leave a completed action stale.
fn background_refresh<T: Send + 'static>(
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

/// Builds the launcher-connection page: a scrolling stack of numbered section
/// cards. Card chrome and the header belong to the ⚙ settings card.
pub fn build_launcher_page() -> LauncherPage {
    let root = Box::new(Orientation::Vertical, 10);
    root.add_css_class("launcher-body");

    // ---- 1 · bridge status + controls ----
    let (head, body) = section_card(&root, "1", "Bridge");
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
    let btn_start = Button::with_label("▶ Start bridge");
    btn_start.set_tooltip_text(Some("Launch super-desktop harness-bridge on :8759"));
    btn_start.add_css_class("launcher-btn");
    btn_start.add_css_class("launcher-btn-primary");
    let btn_stop = Button::with_label("■ Stop");
    btn_stop.set_tooltip_text(Some("Stop the local harness bridge"));
    btn_stop.add_css_class("launcher-btn");
    btn_stop.add_css_class("launcher-btn-danger");
    controls.append(&btn_start);
    controls.append(&btn_stop);
    body.append(&controls);

    let port_hint = Label::new(Some(&format!(
        "Listens on 0.0.0.0:{} · your LAN / Tailscale only, nothing leaves the network",
        bridge::BRIDGE_PORT
    )));
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
    let (_, body) = section_card(&root, "2", "Firewall");
    let fw_row = Box::new(Orientation::Horizontal, 10);
    fw_row.add_css_class("launcher-row");
    let v_fw = Label::new(None);
    v_fw.add_css_class("launcher-status-text");
    v_fw.set_xalign(0.0);
    v_fw.set_hexpand(true);
    v_fw.set_wrap(true);
    fw_row.append(&v_fw);
    let btn_fw = Button::with_label("🔓 Unlock");
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
    let (_, body) = section_card(&root, "3", "Connect to");
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
    let (_, body) = section_card(&root, "4", "Pair");
    let pin_box = Box::new(Orientation::Horizontal, 12);
    pin_box.add_css_class("launcher-pin-box");
    let pin_label = Label::new(Some("PIN"));
    pin_label.add_css_class("launcher-pin-label");
    pin_label.set_valign(Align::Center);
    pin_box.append(&pin_label);
    let v_pin = Label::new(None);
    v_pin.add_css_class("launcher-pin-value");
    v_pin.set_xalign(0.0);
    v_pin.set_hexpand(true);
    pin_box.append(&v_pin);
    body.append(&pin_box);

    let pair_row = Box::new(Orientation::Horizontal, 8);
    pair_row.add_css_class("launcher-actions");
    let btn_pin = Button::with_label("🎲 New PIN");
    btn_pin.set_tooltip_text(Some("Generate a fresh pairing PIN (applies instantly)"));
    btn_pin.add_css_class("launcher-btn");
    let btn_window = Button::with_label("🔓 Open 120s window");
    btn_window.set_tooltip_text(Some("Let the phone pair with no PIN for 2 minutes"));
    btn_window.add_css_class("launcher-btn");
    btn_window.add_css_class("launcher-btn-primary");
    pair_row.append(&btn_pin);
    pair_row.append(&btn_window);
    body.append(&pair_row);

    let v_window = Label::new(None);
    v_window.add_css_class("launcher-window-state");
    v_window.set_xalign(0.0);
    v_window.set_wrap(true);
    body.append(&v_window);

    // ---- 5 · phone steps ----
    let (_, body) = section_card(&root, "5", "On the phone");
    for (i, step) in [
        "Unlock the firewall above (unless it is off).",
        "In OmarchyAILauncher open ⋮⋮⋮ → ⚙ → Bridge connection.",
        "Enter the LAN IP (same Wi-Fi) or the Tailscale IP, then Test.",
        "Tap Pair — empty PIN if you opened the window here, or type the PIN shown above.",
    ]
    .iter()
    .enumerate()
    {
        let row = Box::new(Orientation::Horizontal, 8);
        row.add_css_class("launcher-step");
        let num = Label::new(Some(&(i + 1).to_string()));
        num.add_css_class("launcher-step-num");
        num.set_valign(Align::Start);
        let text = Label::new(Some(step));
        text.add_css_class("launcher-step-text");
        text.set_xalign(0.0);
        text.set_hexpand(true);
        text.set_wrap(true);
        row.append(&num);
        row.append(&text);
        body.append(&row);
    }

    let footer = Label::new(Some(&format!(
        "super-desktop harness-bridge · port {} · serves the sd_term_* tmux sessions",
        bridge::BRIDGE_PORT
    )));
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
        if snapshot.online {
            let n = snapshot.harnesses;
            set_text(&v_bridge_state, "● ONLINE");
            set_text(
                &status,
                &format!(
                    "Serving {n} live harness{} over LAN / Tailscale.",
                    if n == 1 { "" } else { "es" }
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
        set_text(&v_pin, &snapshot.pin);
        let left = snapshot.pairing_left;
        set_state_class(
            &v_window,
            left > 0,
            "launcher-window-open",
            "launcher-window-closed",
        );
        if left > 0 {
            set_text(&v_window, &format!("🔓 Pairing window OPEN — {left}s left"));
        } else {
            set_text(
                &v_window,
                "🔒 Pairing window closed — the phone needs the PIN above.",
            );
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
    head.append(&n);
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

        // Bridge, Firewall, Connect to, Pair, On the phone.
        assert_eq!(count_class(&page.widget, "launcher-section"), 5);
        assert_eq!(count_class(&page.widget, "launcher-section-num"), 5);
        assert_eq!(count_class(&page.widget, "launcher-section-title"), 5);

        // One PIN panel, five address rows, four numbered phone steps.
        assert_eq!(count_class(&page.widget, "launcher-pin-box"), 1);
        assert_eq!(count_class(&page.widget, "launcher-value"), 5);
        assert_eq!(count_class(&page.widget, "launcher-step"), 4);

        // Start / Stop / Unlock / New PIN / Open window.
        assert_eq!(count_class(&page.widget, "launcher-btn"), 5);

        // Navigation calls this on every entry: it must not panic and must
        // leave the bridge chip in one of its two styled states.
        (page.refresh)();
        assert_eq!(count_class(&page.widget, "term-status-badge"), 1);
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
