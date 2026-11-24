use super::*;

pub(in crate::harness_settings) fn settings_entry(icon: &str, title: &str, summary: &str, class: &str) -> (Button, Box) {
    let (button, _, trailing) = settings_entry_with_summary(icon, title, summary, class);
    (button, trailing)
}

/// A settings-hub entry: icon, title, summary and › in one button. Also
/// returns the summary, for entries that describe live state, and the
/// trailing box, for a count chip.
pub(crate) fn settings_entry_with_summary(
    icon: &str,
    title: &str,
    summary: &str,
    class: &str,
) -> (Button, Label, Box) {
    let button = Button::new();
    button.add_css_class("settings-entry");
    button.add_css_class(class);
    button.set_hexpand(true);

    let row = Box::new(Orientation::Horizontal, 12);
    let icon = Label::new(Some(icon));
    icon.add_css_class("settings-entry-icon");
    icon.set_valign(Align::Center);
    row.append(&icon);

    let words = Box::new(Orientation::Vertical, 3);
    words.set_hexpand(true);
    let heading = Label::new(Some(title));
    heading.add_css_class("settings-entry-title");
    heading.set_xalign(0.0);
    words.append(&heading);
    let description = Label::new(Some(summary));
    description.add_css_class("settings-entry-summary");
    description.set_xalign(0.0);
    description.set_wrap(true);
    words.append(&description);
    row.append(&words);

    let arrow = Label::new(Some("›"));
    arrow.add_css_class("settings-entry-arrow");
    arrow.set_valign(Align::Center);
    let trailing = Box::new(Orientation::Horizontal, 6);
    trailing.set_valign(Align::Center);
    trailing.append(&arrow);
    row.append(&trailing);
    button.set_child(Some(&row));

    (button, description, trailing)
}


/// A settings destination's content, before `page_scroll` wraps it.
pub(in crate::harness_settings) fn settings_page_root() -> Box {
    let root = Box::new(Orientation::Vertical, 10);
    root.add_css_class("launcher-body");
    root
}


pub(in crate::harness_settings) fn run_refresh(slot: &std::rc::Weak<RefCell<Option<Rc<dyn Fn()>>>>) {
    if let Some(refresh) = slot.upgrade().and_then(|slot| slot.borrow().clone()) { refresh(); }
}

/// The settings hub. The landing page keeps the choices short: each
/// substantial setting gets its own scrollable destination, so a user never
/// has to hunt through an ever-growing stack of unrelated controls.
pub(in crate::harness_settings) struct HomePage {
    pub(in crate::harness_settings) root: Box,
    /// Shows or hides the firewall warning from a fresh check.
    pub(in crate::harness_settings) firewall_refresh: Rc<dyn Fn()>,
    pub(in crate::harness_settings) btn_review_firewall: Button,
    pub(in crate::harness_settings) btn_shortcut: Button,
    pub(in crate::harness_settings) btn_harnesses: Button,
    pub(in crate::harness_settings) btn_top_bar: Button,
    pub(in crate::harness_settings) btn_editor: Button,
    pub(in crate::harness_settings) btn_mcp: Button,
    pub(in crate::harness_settings) btn_telegram: Button,
    pub(in crate::harness_settings) btn_plugins: Button,
    pub(in crate::harness_settings) btn_sleep_lock: Button,
    pub(in crate::harness_settings) btn_system_launch: Button,
    pub(in crate::harness_settings) btn_connections: Button,
    pub(in crate::harness_settings) btn_updates: Button,
    pub(in crate::harness_settings) updates_chip: Label,
}

pub(in crate::harness_settings) fn home_page() -> HomePage {
    let root = settings_page_root();
    root.add_css_class("settings-home");

    let firewall_notice = Box::new(Orientation::Horizontal, 10);
    firewall_notice.add_css_class("settings-firewall-warning");
    firewall_notice.set_visible(false);
    let firewall_words = Box::new(Orientation::Vertical, 2);
    firewall_words.set_hexpand(true);
    let firewall_title = Label::new(Some("Connections need attention"));
    firewall_title.add_css_class("settings-firewall-warning-title");
    firewall_title.set_xalign(0.0);
    firewall_words.append(&firewall_title);
    let firewall_text = Label::new(None);
    firewall_text.add_css_class("settings-firewall-warning-text");
    firewall_text.set_xalign(0.0);
    firewall_text.set_wrap(true);
    firewall_words.append(&firewall_text);
    firewall_notice.append(&firewall_words);
    let btn_review_firewall = Button::with_label("Review");
    btn_review_firewall.add_css_class("launcher-btn");
    btn_review_firewall.add_css_class("launcher-btn-primary");
    btn_review_firewall.set_valign(Align::Center);
    firewall_notice.append(&btn_review_firewall);
    root.append(&firewall_notice);

    // First, where a new user looks for help.
    let (btn_tour, _) = settings_entry(
        "✨",
        "Getting started guide",
        "A short animated tour: launch agents, hide them, use your phone and other PCs.",
        "settings-tour-entry",
    );
    btn_tour.set_tooltip_text(Some("Open the getting started guide"));
    btn_tour.connect_clicked(|_| crate::welcome_tour::replay());
    root.append(&btn_tour);

    let (btn_shortcut, _) = settings_entry(
        "⌨",
        "Keyboard shortcut",
        "Record the shortcut that shows or hides SUPER DESKTOP.",
        "settings-shortcut-entry",
    );
    btn_shortcut.set_tooltip_text(Some("Change the overlay shortcut"));
    root.append(&btn_shortcut);

    let (btn_harnesses, _) = settings_entry(
        "⌘",
        "Harness launchers",
        "Choose which installed coding agents appear in the top bar.",
        "settings-harnesses-entry",
    );
    btn_harnesses.set_tooltip_text(Some("Manage harness launch buttons"));
    root.append(&btn_harnesses);

    let (btn_top_bar, _) = settings_entry(
        "▤",
        "Top bar",
        "Choose the size of the desktop dock.",
        "settings-top-bar-entry",
    );
    btn_top_bar.set_tooltip_text(Some("Change the top-bar size"));
    root.append(&btn_top_bar);

    let (btn_editor, _) = settings_entry(
        "✎",
        "SD Editor",
        "The built-in text editor: show or hide its button in the top bar.",
        "settings-editor-entry",
    );
    root.append(&btn_editor);

    let (btn_mcp, _) = settings_entry("⚙", "MCP", "Control local agent access and copy connection instructions.", "settings-mcp-entry");
    root.append(&btn_mcp);
    let (btn_telegram, telegram_trailing) = settings_entry(
        "✈",
        "Telegram",
        "Control harnesses from your own Telegram bot and Forum Topics.",
        "settings-telegram-entry",
    );
    let telegram_chip = chip("Off");
    telegram_chip.add_css_class("telegram-status-chip");
    telegram_trailing.prepend(&telegram_chip);
    root.append(&btn_telegram);
    let telegram_refresh = crate::launcher_settings::background_refresh(
        crate::harness_settings::telegram_settings_page::home_status_summary,
        move |label| {
            if telegram_chip.text().as_str() != label {
                telegram_chip.set_text(label);
            }
        },
    );
    btn_telegram.connect_map({
        let refresh = Rc::clone(&telegram_refresh);
        move |_| refresh()
    });
    crate::launcher_settings::tick_while_mapped(
        &[btn_telegram.clone().upcast()],
        Duration::from_secs(2),
        move || telegram_refresh(),
    );
    let (btn_plugins, _) = settings_entry("🧩", "Plugins", "Create widgets, docks, panels and games with AI; turn plugins on or off.", "settings-plugins-entry");
    root.append(&btn_plugins);

    let (btn_sleep_lock, _) = settings_entry(
        "☀", "Sleep lock", "Keep the bridge and AI harnesses awake on external power.",
        "settings-sleep-lock-entry",
    );
    root.append(&btn_sleep_lock);

    let (btn_system_launch, _) = settings_entry(
        "⚡", "System launch", "Start the daemon at login on this PC, headless included.",
        "settings-system-launch-entry",
    );
    root.append(&btn_system_launch);

    let (btn_connections, connections_trailing) = settings_entry(
        "▣",
        "Connections",
        "Add devices, review requests and manage PCs, phones and rejected devices.",
        "android-settings-entry",
    );
    btn_connections.set_tooltip_text(Some("Manage PCs, mobile devices and secure pairing"));
    let counts = chip("…/…");
    counts.add_css_class("android-connection-count");
    counts.set_tooltip_text(Some(
        "Active / registered devices with access to this computer. Active means connected or seen in the last 60 seconds.",
    ));
    connections_trailing.prepend(&counts);
    root.append(&btn_connections);
    let count_refresh = crate::launcher_settings::background_refresh(
        crate::bridge::paired_devices,
        move |devices| {
            let active = devices.iter().filter(|d| d["active"] == true).count();
            let text = format!("{active}/{}", devices.len());
            if counts.text().as_str() != text {
                counts.set_text(&text);
            }
        },
    );
    btn_connections.connect_map({
        let refresh = Rc::clone(&count_refresh);
        move |_| refresh()
    });
    crate::launcher_settings::tick_while_mapped(
        &[btn_connections.clone().upcast()],
        Duration::from_secs(2),
        move || count_refresh(),
    );

    let (btn_updates, updates_trailing) = settings_entry(
        "⟳",
        "Updates",
        "Check GitHub for a newer SUPER DESKTOP and update this computer.",
        "settings-updates-entry",
    );
    btn_updates.set_tooltip_text(Some("Check for a newer version"));
    let updates_chip = chip(&crate::updates::running().to_string());
    updates_chip.add_css_class("update-version");
    updates_trailing.prepend(&updates_chip);
    root.append(&btn_updates);

    let home_footer = Label::new(Some("Settings are saved as you change them."));
    home_footer.add_css_class("launcher-footer");
    home_footer.set_xalign(0.5);
    root.append(&home_footer);

    let firewall_refresh = crate::launcher_settings::background_refresh(
        crate::bridge::firewall_summary,
        move |(summary, can_unlock)| {
            firewall_notice.set_visible(can_unlock);
            if can_unlock {
                firewall_text.set_text(&format!(
                    "{summary}. Allow {}/tcp so Android devices can reach this computer.",
                    crate::bridge::BRIDGE_PORT,
                ));
            }
        },
    );
    HomePage {
        root,
        firewall_refresh,
        btn_review_firewall,
        btn_shortcut,
        btn_harnesses,
        btn_top_bar,
        btn_editor,
        btn_mcp,
        btn_telegram,
        btn_plugins,
        btn_sleep_lock,
        btn_system_launch,
        btn_connections,
        btn_updates,
        updates_chip,
    }
}

