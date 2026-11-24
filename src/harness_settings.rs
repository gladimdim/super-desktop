//! ⚙ Settings card for the overlay HUD.
//!
//! A floating card that lives INSIDE the super-desktop layer-shell overlay
//! (centered above notes and terminals) — never a separate Hyprland window. It
//! is a settings hub with dedicated pages for the keyboard shortcut, harness
//! launchers, top bar, and Android connection. The header (badge, title, ←
//! back, ✕) is shared by every page.
//!
//! Same visual language as notes/terminals: `mini-terminal` + `term-header`
//! card chrome, with compact navigation on the hub and a focused page for each
//! setting. Colours, radii and spacing live in `styles.rs` — this file builds
//! widgets, reads detection, drives the recorder and swaps pages.
//!
//! The selection itself is owned by the window: this panel reads it, reports
//! changes through `on_change` (harnesses) and `on_shortcut_change` (shortcut)
//! and re-detects every time it is opened.

#[path = "mcp_settings_page.rs"]
mod mcp_settings_page;
#[path = "telegram_settings_page.rs"]
pub(in crate::harness_settings) mod telegram_settings_page;

pub(in crate::harness_settings) use gtk4::gdk;
pub(in crate::harness_settings) use gtk4::glib;
pub(in crate::harness_settings) use gtk4::prelude::*;
pub(in crate::harness_settings) use gtk4::{
    pango, Align, Box, Button, EventControllerKey, Image, Label, Orientation,
    PropagationPhase,
};
pub(in crate::harness_settings) use std::cell::{Cell, RefCell};
pub(in crate::harness_settings) use std::path::Path;
pub(in crate::harness_settings) use std::rc::Rc;
pub(in crate::harness_settings) use std::time::Duration;

pub(in crate::harness_settings) use crate::launcher_settings::{chip, hint, page_scroll, section_card, ConnectionHooks, ConnectionPage, ConnectionPages};
pub(in crate::harness_settings) use crate::shortcut::{Capture, CaptureGuard};
pub(in crate::harness_settings) use crate::state::{AppState, TopBarSize};
pub(in crate::harness_settings) use crate::tmux::{detect_harnesses, HarnessInfo};


mod custom_args;
mod harnesses_page;
mod home_page;
mod openclaw;
mod shortcut_page;
mod system_pages;
mod updates_page;
#[cfg(test)]
mod tests;

pub(in crate::harness_settings) use custom_args::{
    args_page, custom_harness_row, custom_page, wire_args_page, wire_custom_page, CustomForm,
};
#[allow(unused_imports)]
pub use harnesses_page::{detected_keys, resolve_visible, toggle, visible_keys};
pub(crate) use harnesses_page::paint_toggle;
pub(in crate::harness_settings) use harnesses_page::{
    harnesses_page, paint_size_button, wire_harness_list, HarnessList, HarnessListHooks,
};
pub(crate) use home_page::settings_entry_with_summary;
pub(in crate::harness_settings) use home_page::{home_page, run_refresh, settings_entry, settings_page_root};
pub(in crate::harness_settings) use openclaw::{openclaw_plugin_notice, OpenClawActions};
pub(in crate::harness_settings) use shortcut_page::{shortcut_page, wire_shortcut_recorder};
pub(in crate::harness_settings) use system_pages::{
    editor_page, sleep_lock_page, system_launch_page, top_bar_page,
};
pub(in crate::harness_settings) use updates_page::build_updates_page;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness_settings) enum SettingsPage {
    Home,
    Shortcut,
    Harnesses,
    CustomHarness,
    HarnessArgs,
    TopBar,
    Editor,
    Mcp,
    Telegram,
    Plugins,
    /// One plugin's page, titled with its name.
    PluginDetail,
    SleepLock,
    SystemLaunch,
    Updates,
    Connections(ConnectionPage),
}

impl SettingsPage {
    /// Badge, title and subtitle for the panel header. The pages that edit a
    /// harness are titled by it in `nav`.
    pub(in crate::harness_settings) fn header(self, connections: &ConnectionPages) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Home => ("⚙", "Settings", "Choose a section"),
            Self::Plugins => ("🧩", "Plugins", "Widgets, docks and panels made by AI agents"),
            Self::PluginDetail => ("🧩", "Plugin", "Its settings, actions and log"),
            Self::Shortcut => ("⌨", "Keyboard shortcut", "Show or hide SUPER DESKTOP"),
            Self::Harnesses => ("⌘", "Harness launchers", "Choose what appears in the top bar"),
            Self::CustomHarness => ("＋", "Add a harness", "Icon · name · executable · arguments"),
            Self::HarnessArgs => ("⌘", "Harness parameters", "What new cards start with"),
            Self::TopBar => ("▤", "Top bar", "Choose the desktop dock size"),
            Self::Editor => ("✎", "SD Editor", "The built-in text editor"),
            Self::Mcp => ("⚙", "MCP", "Local agent access and connection setup"),
            Self::Telegram => ("✈", "Telegram", "Your private Telegram bot and Forum Topics"),
            Self::SleepLock => ("☀", "Sleep lock", "Keep harnesses available on charger power"),
            Self::SystemLaunch => ("⚡", "System launch", "Start the daemon when this PC starts"),
            Self::Updates => ("⟳", "Updates", "Newer versions from GitHub"),
            Self::Connections(page) => connections.header(page),
        }
    }
}


/// The settings card's size the first time it opens, before the user has
/// dragged its edges. A saved size replaces it (see `state`).
pub const SETTINGS_PANEL_DEFAULT_SIZE: (i32, i32) = (1024, 768);

/// The smallest the card may be dragged to. Every page fits inside it, so a
/// page with more to show scrolls rather than pushing the card wider.
pub const SETTINGS_PANEL_MIN_SIZE: (i32, i32) = (660, 620);

/// On a screen narrower than 1200, the card takes at most this share of its
/// width.
pub const SETTINGS_PANEL_SMALL_SCREEN_SHARE: f64 = 0.8;

/// Floating card + its refresh handle. The overlay adds `widget` centered and
/// toggles visibility; `refresh` re-detects the harnesses on this machine and
/// re-reads the stored selection.
pub struct HarnessSettingsPanel {
    pub widget: gtk4::Widget,
    pub refresh: Rc<dyn Fn()>,
}


/// Build the panel.
///
/// `on_change` receives the new full selection (harness keys, in detection
/// order) whenever the user flips a row or a bulk button;
/// `on_shortcut_change` receives a freshly recorded key combination once it has
/// been written into Hyprland's config; `on_top_bar_size_change` applies and
/// persists a newly selected dock scale.
/// The overlay only needs a placeholder until Settings is actually opened.
/// Keep the constructed panel afterwards so navigation and controls survive.
pub fn build_lazy_harness_settings_panel(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn(Vec<String>)>,
    on_detected: Rc<dyn Fn(Vec<String>)>,
    on_shortcut_change: Rc<dyn Fn(String)>,
    on_top_bar_size_change: Rc<dyn Fn(TopBarSize)>,
    connections: ConnectionHooks,
) -> HarnessSettingsPanel {
    let host = Box::new(Orientation::Vertical, 0);
    host.set_visible(false);
    let panel: Rc<RefCell<Option<HarnessSettingsPanel>>> = Rc::new(RefCell::new(None));
    let weak_host = host.downgrade();
    let refresh = Rc::new(move || {
        let Some(host) = weak_host.upgrade() else { return };
        if let Some(panel) = panel.borrow().as_ref() {
            panel.widget.set_visible(host.is_visible());
            (panel.refresh)();
            return;
        }
        if !host.is_visible() { return; }
        let built = build_harness_settings_panel(
            Rc::clone(&state), Rc::clone(&on_change),
            Rc::clone(&on_detected),
            Rc::clone(&on_shortcut_change), Rc::clone(&on_top_bar_size_change),
            connections.clone(),
        );
        // The panel's close button hides its root; mirror that on the host so
        // the next gear click opens it instead of requiring two clicks.
        let weak_host = host.downgrade();
        built.widget.connect_visible_notify(move |widget| {
            if !widget.is_visible() {
                if let Some(host) = weak_host.upgrade() { host.set_visible(false); }
            }
        });
        let weak_panel = built.widget.downgrade();
        host.connect_visible_notify(move |host| {
            if let Some(widget) = weak_panel.upgrade() { widget.set_visible(host.is_visible()); }
        });
        host.append(&built.widget);
        *panel.borrow_mut() = Some(built);
    });
    HarnessSettingsPanel { widget: host.upcast(), refresh }
}


/// Holds the harness list's refresh for the controls built before it. They
/// keep the slot weakly; the panel's own refresh keeps it alive.
pub(in crate::harness_settings) type RefreshSlot = Rc<RefCell<Option<Rc<dyn Fn()>>>>;


/// The card and its header: badge, title + subtitle, ← back, close.
pub(in crate::harness_settings) struct PanelShell {
    pub(in crate::harness_settings) outer: Box,
    pub(in crate::harness_settings) badge: Label,
    pub(in crate::harness_settings) title: Label,
    pub(in crate::harness_settings) subtitle: Label,
    pub(in crate::harness_settings) btn_back: Button,
}

pub(in crate::harness_settings) fn panel_shell() -> PanelShell {
    let outer = Box::new(Orientation::Vertical, 0);
    outer.add_css_class("mini-terminal");
    outer.add_css_class("harness-panel");
    outer.set_size_request(SETTINGS_PANEL_MIN_SIZE.0, SETTINGS_PANEL_MIN_SIZE.1);

    let badge = Label::new(Some("⚙"));
    let header = crate::floating_panel::PanelHeader::new(&badge, "Settings", "Connections · shortcuts · top bar", true);
    let (title, subtitle) = (header.title.clone(), header.subtitle.clone());
    // Shown on every page after the settings hub (see `nav`).
    let btn_back = header.button("←", Some("term-btn"), Some("Back to settings"));
    btn_back.set_visible(false);
    let btn_close = header.close_button("Close panel");
    outer.append(&header.widget);

    let weak_outer = outer.downgrade();
    btn_close.connect_clicked(move |_| {
        if let Some(o) = weak_outer.upgrade() {
            o.set_visible(false);
        }
    });
    PanelShell { outer, badge, title, subtitle, btn_back }
}

pub fn build_harness_settings_panel(
    state: Rc<RefCell<AppState>>,
    on_change: Rc<dyn Fn(Vec<String>)>,
    on_detected: Rc<dyn Fn(Vec<String>)>,
    on_shortcut_change: Rc<dyn Fn(String)>,
    on_top_bar_size_change: Rc<dyn Fn(TopBarSize)>,
    connections: ConnectionHooks,
) -> HarnessSettingsPanel {
    let PanelShell { outer, badge, title, subtitle, btn_back } = panel_shell();
    let home = home_page();
    let shortcut = shortcut_page();
    let harnesses = harnesses_page();
    let custom = custom_page(&subtitle);
    let args = args_page(&subtitle);
    let (top_bar_root, paint_size) = top_bar_page(&state, &on_top_bar_size_change);
    let editor_root = editor_page(&state);
    let sleep_root = sleep_lock_page(&state);
    let system_launch_root = system_launch_page(&state);

    let mcp_root = settings_page_root();
    let refresh_mcp = mcp_settings_page::build(&mcp_root);

    let telegram_root = settings_page_root();
    let refresh_telegram = telegram_settings_page::build(&telegram_root);

    let plugins_root = settings_page_root();
    let plugin_detail_root = settings_page_root();
    let plugin_title: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    let updates_root = settings_page_root();
    let check_updates = build_updates_page(&updates_root, &home.updates_chip);
    // Opening Settings checks at most hourly, so the entry can name a newer
    // version; opening the page always checks.
    home.btn_updates.connect_map({
        let check_updates = Rc::clone(&check_updates);
        move |_| check_updates(false)
    });

    // Connections is a family of destinations with its own overview. They
    // own their live bridge controls and refresh only while shown. `nav` is
    // built below, after every page exists.
    // Weak: the pages must not keep `nav`, which keeps them, alive.
    let nav_slot: Rc<RefCell<std::rc::Weak<dyn Fn(SettingsPage)>>> =
        Rc::new(RefCell::new(std::rc::Weak::<fn(SettingsPage)>::new()));
    let connection_pages = crate::launcher_settings::build_connection_pages(
        connections,
        Rc::new({
            let nav_slot = Rc::clone(&nav_slot);
            move |page| {
                let nav = nav_slot.borrow().upgrade();
                if let Some(nav) = nav {
                    nav(SettingsPage::Connections(page));
                }
            }
        }),
    );
    let connection_pages = Rc::new(connection_pages);
    // Refreshes itself whenever it is shown and when plugins change; a row
    // opens the plugin's own page.
    let _ = crate::plugin_ui::settings_page::build(&plugins_root, &plugin_detail_root, Rc::new({
        let (nav_slot, plugin_title) = (Rc::clone(&nav_slot), Rc::clone(&plugin_title));
        move |name: &str| {
            *plugin_title.borrow_mut() = name.to_owned();
            let nav = nav_slot.borrow().upgrade();
            if let Some(nav) = nav {
                nav(SettingsPage::PluginDetail);
            }
        }
    }));

    // Every page, in the order the panel holds them; the hub shows first.
    let mut views: Vec<(SettingsPage, gtk4::Widget)> = vec![
        (SettingsPage::Home, page_scroll(&home.root).upcast()),
        (SettingsPage::Shortcut, page_scroll(&shortcut.root).upcast()),
        (SettingsPage::Harnesses, page_scroll(&harnesses.root).upcast()),
        (SettingsPage::CustomHarness, page_scroll(&custom.root).upcast()),
        (SettingsPage::TopBar, page_scroll(&top_bar_root).upcast()),
        (SettingsPage::Editor, page_scroll(&editor_root).upcast()),
        (SettingsPage::Mcp, page_scroll(&mcp_root).upcast()),
        (SettingsPage::Plugins, page_scroll(&plugins_root).upcast()),
    ];
    views.extend(connection_pages.widgets().map(|(page, view)| (SettingsPage::Connections(page), view.clone())));
    views.extend([
        (SettingsPage::SleepLock, page_scroll(&sleep_root).upcast()),
        (SettingsPage::SystemLaunch, page_scroll(&system_launch_root).upcast()),
        (SettingsPage::Updates, page_scroll(&updates_root).upcast()),
        (SettingsPage::Telegram, page_scroll(&telegram_root).upcast()),
        (SettingsPage::HarnessArgs, page_scroll(&args.root).upcast()),
        (SettingsPage::PluginDetail, page_scroll(&plugin_detail_root).upcast()),
    ]);
    let pages = Box::new(Orientation::Vertical, 0);
    pages.add_css_class("harness-pages");
    pages.set_vexpand(true);
    for (page, view) in &views {
        pages.append(view);
        view.set_visible(*page == SettingsPage::Home);
    }
    outer.append(&pages);

    let current_page = Rc::new(Cell::new(SettingsPage::Home));
    let nav: Rc<dyn Fn(SettingsPage)> = {
        let refresh_mcp = refresh_mcp.clone();
        let refresh_telegram = refresh_telegram.clone();
        let connection_pages = Rc::clone(&connection_pages);
        let check_updates = Rc::clone(&check_updates);
        let btn_back = btn_back.clone();
        let badge = badge.clone();
        let title = title.clone();
        let subtitle = subtitle.clone();
        let editing_custom = Rc::clone(&custom.form.editing);
        let editing_args = Rc::clone(&args.editing);
        let plugin_title = Rc::clone(&plugin_title);
        let current_page = Rc::clone(&current_page);
        Rc::new(move |page| {
            current_page.set(page);
            subtitle.remove_css_class("launcher-note-error");
            for (shown, view) in &views {
                view.set_visible(page == *shown);
            }
            // An invitation lives only while its page is shown.
            if page != SettingsPage::Connections(ConnectionPage::Invite) {
                connection_pages.invite.stop();
            }
            btn_back.set_visible(page != SettingsPage::Home);
            btn_back.set_tooltip_text(Some(match page {
                SettingsPage::CustomHarness | SettingsPage::HarnessArgs => "Back to harness launchers",
                SettingsPage::PluginDetail => "Back to plugins",
                SettingsPage::Connections(ConnectionPage::Invite) => "Back to Add a device",
                SettingsPage::Connections(ConnectionPage::Overview) => "Back to settings",
                SettingsPage::Connections(_) => "Back to Connections",
                _ => "Back to settings",
            }));

            let (icon, heading, summary) = page.header(&connection_pages);
            badge.set_label(icon);
            match page {
                SettingsPage::CustomHarness if editing_custom.borrow().is_some() => title.set_label("Edit harness"),
                SettingsPage::HarnessArgs => {
                    let name = editing_args.get().map_or("Harness", |key| crate::tmux::get_agent_config(key).name);
                    title.set_label(&format!("{name} parameters"));
                }
                SettingsPage::PluginDetail => title.set_label(&plugin_title.borrow()),
                _ => title.set_label(heading),
            }
            subtitle.set_label(summary);
            match page {
                SettingsPage::Mcp => refresh_mcp(),
                SettingsPage::Telegram => refresh_telegram(),
                SettingsPage::Updates => check_updates(true),
                SettingsPage::Connections(_) => (connection_pages.refresh)(),
                _ => {}
            }
        })
    };

    for (button, page) in [
        (&home.btn_shortcut, SettingsPage::Shortcut),
        (&home.btn_harnesses, SettingsPage::Harnesses),
        (&home.btn_top_bar, SettingsPage::TopBar),
        (&home.btn_editor, SettingsPage::Editor),
        (&home.btn_mcp, SettingsPage::Mcp),
        (&home.btn_telegram, SettingsPage::Telegram),
        (&home.btn_plugins, SettingsPage::Plugins),
        (&home.btn_connections, SettingsPage::Connections(ConnectionPage::Overview)),
        (&home.btn_sleep_lock, SettingsPage::SleepLock),
        (&home.btn_system_launch, SettingsPage::SystemLaunch),
        (&home.btn_updates, SettingsPage::Updates),
    ] {
        let nav = Rc::clone(&nav);
        button.connect_clicked(move |_| nav(page));
    }
    *nav_slot.borrow_mut() = Rc::downgrade(&nav);
    home.btn_review_firewall.connect_clicked({
        let nav = Rc::clone(&nav);
        move |_| nav(SettingsPage::Connections(ConnectionPage::Network))
    });
    // Reopening always lands on the hub. A theme refresh while the card stays
    // open deliberately leaves the current page alone.
    outer.connect_visible_notify({
        let nav = Rc::clone(&nav);
        move |o| {
            if o.is_visible() {
                nav(SettingsPage::Home);
            }
        }
    });

    let firewall_notice_refresh = Rc::clone(&home.firewall_refresh);
    btn_back.connect_clicked({
        let nav = Rc::clone(&nav);
        let refresh = Rc::clone(&firewall_notice_refresh);
        let current_page = Rc::clone(&current_page);
        move |_| match current_page.get() {
            SettingsPage::CustomHarness | SettingsPage::HarnessArgs => nav(SettingsPage::Harnesses),
            SettingsPage::PluginDetail => nav(SettingsPage::Plugins),
            SettingsPage::Connections(page) if page.parent().is_some() => {
                nav(SettingsPage::Connections(page.parent().unwrap_or(ConnectionPage::Overview)))
            }
            _ => {
                nav(SettingsPage::Home);
                refresh();
            }
        }
    });

    harnesses.btn_add_custom.connect_clicked({
        let form = custom.form.clone();
        let nav = Rc::clone(&nav);
        move |_| form.open(None, &*nav)
    });
    let refresh_slot: RefreshSlot = Rc::new(RefCell::new(None));
    wire_custom_page(&custom, &state, &subtitle, &nav, &on_change, &refresh_slot);
    let open_args = wire_args_page(&args, &state, &subtitle, &nav, &refresh_slot);
    let (stop_recording, paint_recorder) = wire_shortcut_recorder(&outer, &shortcut, &state, &on_shortcut_change);

    let refresh = wire_harness_list(&harnesses, HarnessListHooks {
        state: Rc::clone(&state),
        on_change: Rc::clone(&on_change),
        on_detected: Rc::clone(&on_detected),
        open_args,
        custom: custom.form.clone(),
        nav: Rc::clone(&nav),
        refresh: Rc::downgrade(&refresh_slot),
        // Reopening (or restyling on a theme switch) must never leave a
        // recording armed with the keyboard held.
        reset: Rc::new(move || {
            stop_recording(None);
            paint_recorder();
            paint_size();
            firewall_notice_refresh();
        }),
    });
    *refresh_slot.borrow_mut() = Some(Rc::clone(&refresh));
    refresh();

    let refresh: Rc<dyn Fn()> = Rc::new({
        let slot = Rc::clone(&refresh_slot);
        let run = Rc::clone(&refresh);
        move || { let _keep_alive = &slot; run(); }
    });
    HarnessSettingsPanel {
        widget: outer.upcast(),
        refresh: Rc::clone(&refresh),
    }
}

/// The settings panel with callbacks that do nothing.
#[cfg(test)]
pub(crate) fn inert_panel(state: Rc<RefCell<AppState>>) -> HarnessSettingsPanel {
    build_harness_settings_panel(
        state,
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        ConnectionHooks::inert(),
    )
}

