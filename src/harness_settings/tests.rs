use super::*;
use crate::gtk_test::{count_class, find_all};

/// The website's "connect your phone" screenshots of Settings →
/// Connections → Add a device → Pair a phone, drawn by the real Settings
/// card on the private display. Opt-in, and only against a disposable
/// bridge: run `harness-bridge` with its own SUPER_DESKTOP_BRIDGE_STATE_DIR
/// first (opening Pair a phone starts the bridge, which must find that one
/// running), then `SD_PAIRING_SHOTS=<dir> cargo test pairing_screenshots`.
#[test]
fn pairing_screenshots() {
    if std::env::var_os("SD_PAIRING_SHOTS").is_none() {
        return;
    }
    crate::gtk_test::run_in_child_process("harness_settings::tests::pairing_screenshots_inner");
}

#[test]
fn pairing_screenshots_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    let Some(out) = std::env::var_os("SD_PAIRING_SHOTS").map(std::path::PathBuf::from) else { return };
    assert!(std::env::var_os("SUPER_DESKTOP_BRIDGE_STATE_DIR").is_some(), "only against a disposable bridge state dir");
    assert!(crate::bridge::bridge_running(), "start the disposable bridge first");
    gtk4::init().unwrap();
    gtk4::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
    crate::styles::apply_styles();
    let panel = inert_panel(Rc::new(RefCell::new(AppState::default())));
    let window = gtk4::Window::new();
    window.add_css_class("super-desktop");
    window.set_default_size(720, 760);
    window.set_child(Some(&panel.widget));
    window.present();
    crate::gtk_test::pump(800);

    fn labelled(w: &gtk4::Widget, text: &str) -> Option<Button> {
        if let Some(button) = w.downcast_ref::<Button>() {
            let mut stack = vec![w.clone()];
            while let Some(node) = stack.pop() {
                if node.downcast_ref::<Label>().is_some_and(|l| l.text() == text) || button.label().as_deref() == Some(text) {
                    return Some(button.clone());
                }
                let mut child = node.first_child();
                while let Some(c) = child {
                    stack.push(c.clone());
                    child = c.next_sibling();
                }
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if c.is_visible() && c.is_child_visible() {
                if let Some(found) = labelled(&c, text) {
                    return Some(found);
                }
            }
            child = c.next_sibling();
        }
        None
    }
    let root: gtk4::Widget = panel.widget.clone().upcast();
    crate::gtk_test::save_png(&panel.widget, &out.join("desktop-settings.png"));
    for (entry, file, wait) in [
        ("Connections", "desktop-connections.png", 1500),
        ("Add a device", "desktop-add-device.png", 600),
        ("Pair a phone", "desktop-pair-phone.png", 3000),
    ] {
        labelled(&root, entry).unwrap_or_else(|| panic!("no `{entry}` button")).emit_clicked();
        crate::gtk_test::pump(wait);
        crate::gtk_test::save_png(&panel.widget, &out.join(file));
    }
}

#[test]
fn lazy_panel_builds_only_on_open_and_reopens_after_close() {
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("harness_settings::tests::lazy_panel_builds_only_on_open_and_reopens_after_close");
        return;
    }
    if gtk4::init().is_err() { return; }
    let panel = build_lazy_harness_settings_panel(
        Rc::new(RefCell::new(AppState::default())),
        Rc::new(|_| {}), Rc::new(|_| {}), Rc::new(|_| {}), Rc::new(|_| {}),
        ConnectionHooks::inert(),
    );
    assert!(panel.widget.first_child().is_none());
    (panel.refresh)(); // Theme changes while unopened must remain cheap.
    assert!(panel.widget.first_child().is_none());
    panel.widget.set_visible(true);
    (panel.refresh)();
    let child = panel.widget.first_child().expect("built on first open");
    child.set_visible(false); // Inner close button.
    assert!(!panel.widget.is_visible());
    panel.widget.set_visible(true);
    (panel.refresh)();
    assert_eq!(panel.widget.first_child(), Some(child.clone()));
    assert!(child.is_visible());
    panel.widget.set_visible(false); // Overlay hide.
    assert!(!child.is_visible());
    (panel.refresh)();
    assert!(!panel.widget.is_visible());
    assert!(!child.is_visible());
}

fn info(key: &'static str) -> HarnessInfo {
    HarnessInfo {
        key,
        name: key,
        icon: "💻",
        command: format!("/usr/bin/{key}"),
    }
}

#[test]
fn test_resolve_visible_defaults_to_everything_detected() {
    let detected = vec![info("claude"), info("shell")];
    assert_eq!(
        resolve_visible(None, &detected),
        vec!["claude".to_string(), "shell".to_string()]
    );
}

#[test]
fn saved_custom_harness_respects_visibility_and_availability() {
    let mut state = AppState::default();
    let item = crate::custom_harness::CustomHarness::create("Echo", "💻", "/bin/echo", "hello").unwrap();
    let id = item.id.clone();
    state.custom_harnesses.push(item);
    let detected = vec![info("shell")];
    assert_eq!(visible_keys(&state, &detected), vec!["shell".to_string(), id.clone()]);
    state.visible_harnesses = Some(vec![id.clone()]);
    assert_eq!(visible_keys(&state, &detected), vec![id]);
}

#[test]
fn rescan_discovers_a_later_install_and_updates_launchers_without_saving_visibility() {
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("harness_settings::tests::rescan_discovers_a_later_install_and_updates_launchers_without_saving_visibility");
        return;
    }
    gtk4::init().unwrap();
    let home = std::env::temp_dir().join(format!("sd-rescan-ui-{}", std::process::id()));
    let bin = home.join(".local/bin");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&bin).unwrap();
    std::env::set_var("HOME", &home);
    if crate::tmux::detect_harness_command("kiro").is_some() {
        std::fs::remove_dir_all(&home).unwrap();
        return;
    }
    let state = Rc::new(RefCell::new(AppState::default()));
    let detected = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = Rc::clone(&detected);
    let panel = build_harness_settings_panel(
        Rc::clone(&state),
        Rc::new(|_| panic!("rescan must not persist the visibility preference")),
        Rc::new(move |keys| *captured.borrow_mut() = keys),
        Rc::new(|_| {}), Rc::new(|_| {}),
        ConnectionHooks::inert(),
    );
    assert!(!detected.borrow().contains(&"kiro".to_string()));
    let kiro = bin.join("kiro-cli");
    std::fs::write(&kiro, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&kiro, std::fs::Permissions::from_mode(0o755)).unwrap();
    find_all::<Button>(&panel.widget, "launcher-btn").into_iter()
        .find(|button| button.label().as_deref() == Some("⟳ Rescan"))
        .unwrap().emit_clicked();
    assert!(detected.borrow().contains(&"kiro".to_string()));
    assert_eq!(state.borrow().visible_harnesses, None);
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn saved_t3code_selection_is_ignored_without_an_empty_button() {
    // T3 Code was removed: an older state.json may still list it (and a
    // T3 card). Loading must not fail and no launcher may appear for it.
    let saved = r#"{"notes":[],"terminals":[{"id":"t","session_name":"sd_term_old_t3","agent_type":"t3code",
        "command":"/usr/bin/t3 serve","x":0,"y":0,"created_at":0}],
        "visible_harnesses":["t3code","claude"]}"#;
    let state: AppState = serde_json::from_str(saved).expect("an old t3code state still loads");
    assert!(!crate::tmux::HARNESS_KEYS.contains(&"t3code"));
    let detected = vec![info("claude"), info("shell")];
    assert_eq!(visible_keys(&state, &detected), vec!["claude".to_string()]);
    assert_eq!(resolve_visible(Some(&["t3code".to_string()]), &detected), Vec::<String>::new());
    // Toggling in Settings saves only known keys, dropping the stale one.
    let mut selection = visible_keys(&state, &detected);
    toggle(&mut selection, "shell", &detected_keys(&detected));
    assert_eq!(selection, vec!["claude".to_string(), "shell".to_string()]);
    // The saved card restores as a generic terminal running its own command.
    let card = &state.terminals[0];
    assert_eq!(crate::tmux::get_agent_config(&card.agent_type).name, "Terminal");
    assert_eq!(
        crate::tmux::resolve_resume_command_in(&card.agent_type, Some(card.command.as_str()), None, None),
        "/usr/bin/t3 serve"
    );
    assert!(crate::brand::logo_path("t3code", false).is_none());
    assert!(crate::brand::logo_filename("t3code", true).is_none());
}

/// Fake OpenClaw for the plugin line: 0 missing, 1 connected, 2 unreadable.
static FAKE_PLUGIN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static FAKE_FAILS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FAKE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn fake_detect() -> crate::harness_metadata::OpenClawPlugin {
    use crate::harness_metadata::OpenClawPlugin;
    match FAKE_PLUGIN.load(std::sync::atomic::Ordering::SeqCst) {
        1 => OpenClawPlugin::Connected,
        2 => OpenClawPlugin::Unreadable,
        _ => OpenClawPlugin::Missing("the plugin is not registered"),
    }
}

fn fake_connect() -> Result<(), String> {
    use std::sync::atomic::Ordering::SeqCst;
    FAKE_RUNS.fetch_add(1, SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    if FAKE_FAILS.load(SeqCst) {
        return Err("OpenClaw plugin registration failed: boom".into());
    }
    FAKE_PLUGIN.store(1, SeqCst);
    Ok(())
}

fn fake_restart() -> Result<(), String> {
    use std::sync::atomic::Ordering::SeqCst;
    FAKE_RUNS.fetch_add(1, SeqCst);
    if FAKE_FAILS.load(SeqCst) {
        return Err("Gateway restart failed: no service".into());
    }
    Ok(())
}

fn pump_until(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Text, chip, Connect and Restart of one plugin line.
fn plugin_line(line: &gtk4::Widget) -> (String, bool, Button, Button) {
    let text = find_all::<gtk4::Widget>(line, "harness-plugin-text")[0].clone().downcast::<Label>().unwrap();
    let chip = find_all::<gtk4::Widget>(line, "harness-plugin-connected")[0].clone();
    let button = |label: &str| find_all::<Button>(line, "launcher-btn").into_iter()
        .find(|b| b.label().as_deref() == Some(label)).unwrap();
    (text.text().to_string(), shown(&chip), button("Connect"), button("Restart gateway"))
}

#[test]
fn openclaw_plugin_line_connects_then_offers_a_gateway_restart() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("harness_settings::tests::openclaw_plugin_line_connects_then_offers_a_gateway_restart");
        return;
    }
    gtk4::init().unwrap();
    let actions = OpenClawActions { detect: fake_detect, connect: fake_connect, restart: fake_restart };
    let line = openclaw_plugin_notice(actions).upcast::<gtk4::Widget>();
    let (text, chip, connect, restart) = plugin_line(&line);
    assert_eq!(text, "Status & titles need the SUPER DESKTOP plugin");
    assert!(!chip && shown(&connect) && !shown(&restart));
    let text_label = find_all::<gtk4::Widget>(&line, "harness-plugin-text")[0].clone();
    assert_eq!(text_label.tooltip_text().as_deref(), Some("the plugin is not registered"));

    // A failed registration explains itself and can be retried.
    FAKE_FAILS.store(true, SeqCst);
    connect.emit_clicked();
    let (text, _, connect, _) = plugin_line(&line);
    assert!(text.starts_with("Connecting"), "{text}");
    assert!(!connect.is_sensitive(), "no second registration while one runs");
    connect.emit_clicked();
    // Settings rebuilds its list on every open: a rebuilt line knows too.
    let during = openclaw_plugin_notice(actions).upcast::<gtk4::Widget>();
    assert!(!plugin_line(&during).2.is_sensitive());
    pump_until("the failed registration", || plugin_line(&line).2.is_sensitive());
    assert_eq!(FAKE_RUNS.load(SeqCst), 1);
    let (text, chip, connect, restart) = plugin_line(&line);
    assert_eq!(text, "OpenClaw plugin registration failed: boom");
    assert!(text_label.has_css_class("launcher-note-error"));
    assert!(!chip && shown(&connect) && !shown(&restart));

    // Success: Connected, and the running gateway still has to load it.
    FAKE_FAILS.store(false, SeqCst);
    connect.emit_clicked();
    pump_until("the registration", || shown(&plugin_line(&line).3));
    let (text, chip, connect, restart) = plugin_line(&line);
    assert!(text.starts_with("Plugin registered. Restart the OpenClaw gateway"), "{text}");
    assert!(!text_label.has_css_class("launcher-note-error"));
    assert!(chip && !shown(&connect) && restart.is_sensitive());
    assert!(shown(&plugin_line(&during).3), "every open line is repainted");
    // Reopening Settings keeps the restart the gateway still needs.
    let reopened = openclaw_plugin_notice(actions).upcast::<gtk4::Widget>();
    let (text, chip, _, again) = plugin_line(&reopened);
    assert!(text.starts_with("Plugin registered."), "{text}");
    assert!(chip && shown(&again));

    FAKE_FAILS.store(true, SeqCst);
    restart.emit_clicked();
    pump_until("the failed restart", || plugin_line(&line).3.is_sensitive());
    let (text, _, _, restart) = plugin_line(&line);
    assert!(text.contains("no service"), "{text}");
    assert!(shown(&restart), "a failed restart can be retried");
    FAKE_FAILS.store(false, SeqCst);
    restart.emit_clicked();
    pump_until("the restart", || !shown(&plugin_line(&line).3));
    let (text, chip, connect, _) = plugin_line(&line);
    assert_eq!(text, "Gateway restarted. OpenClaw cards report status from their next turn.");
    assert!(chip && !shown(&connect));
    assert!(!shown(&plugin_line(&reopened).3));
    assert_eq!(FAKE_RUNS.load(SeqCst), 4);

    // Already connected: just say so. An unreadable config still offers setup.
    let (text, chip, connect, restart) = plugin_line(&openclaw_plugin_notice(actions).upcast());
    assert_eq!(text, "Status & titles come from the SUPER DESKTOP plugin");
    assert!(chip && !shown(&connect) && !shown(&restart));
    FAKE_PLUGIN.store(2, SeqCst);
    let (text, chip, connect, _) = plugin_line(&openclaw_plugin_notice(actions).upcast());
    assert!(text.contains("could not be read"), "{text}");
    assert!(!chip && shown(&connect));
}

#[test]
fn openclaw_row_shows_the_plugin_state_from_openclaws_config() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("harness_settings::tests::openclaw_row_shows_the_plugin_state_from_openclaws_config");
        return;
    }
    gtk4::init().unwrap();
    // An isolated home and OpenClaw config: never the user's.
    let home = std::env::temp_dir().join(format!("sd-openclaw-row-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::env::set_var("HOME", &home);
    for key in ["OPENCLAW_HOME", "OPENCLAW_STATE_DIR"] {
        std::env::remove_var(key);
    }
    let config = home.join("claw/openclaw.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::env::set_var("OPENCLAW_CONFIG_PATH", &config);
    if crate::tmux::harness_binary("openclaw").is_none() {
        let fake = home.join(".local/bin/openclaw");
        std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(&config, r#"{"gateway":{"mode":"local"}}"#).unwrap();
    let panel = inert_panel(Rc::new(RefCell::new(AppState::default())));
    let line = || {
        let row = harness_row_named(&panel.widget, "OpenClaw");
        let notice = row.next_sibling().expect("OpenClaw plugin notice");
        assert!(notice.has_css_class("harness-plugin-notice"));
        let lines = find_all::<gtk4::Widget>(&panel.widget, "harness-plugin-notice");
        assert!(lines.iter().any(|w| w == &notice));
        notice
    };
    let (text, chip, connect, _) = plugin_line(&line());
    assert_eq!(text, "Status & titles need the SUPER DESKTOP plugin");
    assert!(!chip && shown(&connect));

    let plugin = home.join(".local/state/super-desktop/harness/openclaw");
    std::fs::write(&config, serde_json::json!({"plugins": {
        "entries": {"super-desktop-metadata": {"enabled": true, "hooks": {"allowConversationAccess": true}}},
        "load": {"paths": [plugin]}}}).to_string()).unwrap();
    (panel.refresh)();
    let (text, chip, connect, _) = plugin_line(&line());
    assert_eq!(text, "Status & titles come from the SUPER DESKTOP plugin");
    assert!(chip && !shown(&connect));

    std::fs::write(&config, "{ not json").unwrap();
    (panel.refresh)();
    let (text, chip, connect, _) = plugin_line(&line());
    assert!(text.contains("could not be read"), "{text}");
    assert!(!chip && shown(&connect));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn custom_launcher_form_saves_and_removes() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("harness_settings::tests::custom_launcher_form_saves_and_removes");
        return;
    }
    gtk4::init().unwrap();
    let home = std::env::temp_dir().join(format!("sd-custom-ui-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    let state = Rc::new(RefCell::new(AppState::default()));
    let saved = Rc::clone(&state);
    let panel = build_harness_settings_panel(
        Rc::clone(&state),
        Rc::new(move |keys| {
            saved.borrow_mut().visible_harnesses = Some(keys);
            crate::state::save_state_async(saved.borrow().clone());
        }),
        Rc::new(|_| {}), Rc::new(|_| {}), Rc::new(|_| {}),
        ConnectionHooks::inert(),
    );
    find_all::<Button>(&panel.widget, "settings-harnesses-entry")[0].emit_clicked();
    let pages = find_all::<gtk4::Widget>(&panel.widget, "harness-page");
    let add = find_all::<Button>(&panel.widget, "settings-add-harness-entry").remove(0);
    add.emit_clicked();
    assert!(shown(&pages[3]));
    assert_eq!(title_text(&panel.widget), "Add a harness");
    let buttons = find_all::<Button>(&panel.widget, "launcher-btn");
    buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
    assert!(shown(&pages[3]), "invalid details keep the form open");
    let error = find_all::<gtk4::Widget>(&panel.widget, "custom-harness-error")[0].clone().downcast::<Label>().unwrap();
    assert!(error.is_visible());
    assert_eq!(error.text().as_str(), "Enter a name of up to 48 characters");
    buttons.iter().find(|button| button.label().as_deref() == Some("🧭")).unwrap().emit_clicked();
    let entries = find_all::<gtk4::Widget>(&panel.widget, "ws-entry");
    for (placeholder, value) in [
        ("Harness name", "My Echo"),
        ("/absolute/path/to/executable", "/bin/echo"),
        ("Optional arguments, e.g. --model 'my model'", "hello 'two words'"),
    ] {
        entries.iter().filter_map(|widget| widget.clone().downcast::<gtk4::Entry>().ok())
            .find(|entry| entry.placeholder_text().as_deref() == Some(placeholder)).unwrap().set_text(value);
    }
    buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
    assert!(shown(&pages[2]));
    assert_eq!(title_text(&panel.widget), "Harness launchers");
    let item = state.borrow().custom_harnesses[0].clone();
    assert_eq!(item.name, "My Echo");
    assert_eq!(item.icon, "🧭");
    assert_eq!(item.arguments, ["hello", "two words"]);
    assert_eq!(crate::state::load_state().custom_harnesses, vec![item.clone()]);
    assert_eq!(std::fs::metadata(crate::state::get_state_path()).unwrap().permissions().mode() & 0o777, 0o600);
    let edit = find_all::<Button>(&panel.widget, "launcher-btn").into_iter()
        .find(|button| button.label().as_deref() == Some("Edit")).unwrap();
    edit.emit_clicked();
    assert!(shown(&pages[3]));
    assert_eq!(title_text(&panel.widget), "Edit harness");
    entries.iter().filter_map(|widget| widget.clone().downcast::<gtk4::Entry>().ok())
        .find(|entry| entry.placeholder_text().as_deref() == Some("Harness name")).unwrap().set_text("Echo again");
    buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
    assert!(shown(&pages[2]));
    assert_eq!(state.borrow().custom_harnesses.len(), 1);
    assert_eq!(state.borrow().custom_harnesses[0].name, "Echo again");
    assert_eq!(state.borrow().custom_harnesses[0].id, item.id);
    add.emit_clicked();
    assert!(shown(&pages[3]));
    buttons.iter().find(|button| button.label().as_deref() == Some("Cancel")).unwrap().emit_clicked();
    assert!(shown(&pages[2]), "cancel returns to the launcher list");
    assert_eq!(state.borrow().custom_harnesses.len(), 1);
    let remove = find_all::<Button>(&panel.widget, "launcher-btn").into_iter()
        .find(|button| button.label().as_deref() == Some("Remove")).unwrap();
    remove.emit_clicked();
    crate::state::flush_state_saves();
    assert!(state.borrow().custom_harnesses.is_empty());
    assert!(crate::state::load_state().custom_harnesses.is_empty());

    // A path alone is enough: derive the name from its filename. If the
    // executable is missing, the error stays visible until it is fixed.
    add.emit_clicked();
    let path = entries.iter().filter_map(|widget| widget.clone().downcast::<gtk4::Entry>().ok())
        .find(|entry| entry.placeholder_text().as_deref() == Some("/absolute/path/to/executable")).unwrap();
    path.set_text("/missing/harness");
    buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
    assert!(shown(&pages[3]));
    assert!(error.is_visible());
    assert_eq!(error.text().as_str(), "The path is not an executable file");
    assert!(path.has_css_class("ws-entry-invalid"));
    path.set_text("/bin/echo");
    assert!(!error.is_visible());
    assert!(!path.has_css_class("ws-entry-invalid"));
    buttons.iter().find(|button| button.label().as_deref() == Some("Save harness")).unwrap().emit_clicked();
    assert!(shown(&pages[2]));
    assert_eq!(state.borrow().custom_harnesses[0].name, "echo");
    find_all::<Button>(&panel.widget, "launcher-btn").into_iter()
        .find(|button| button.label().as_deref() == Some("Remove")).unwrap().emit_clicked();
    crate::state::flush_state_saves();
    assert!(state.borrow().custom_harnesses.is_empty());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn clone_button_duplicates_a_launcher_as_a_custom_copy() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process(
            "harness_settings::tests::clone_button_duplicates_a_launcher_as_a_custom_copy",
        );
        return;
    }
    gtk4::init().unwrap();
    let home = std::env::temp_dir().join(format!("sd-clone-ui-{}", std::process::id()));
    let bin = home.join(".local/bin");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&bin).unwrap();
    std::env::set_var("HOME", &home);
    let stub = bin.join("aider");
    std::fs::write(&stub, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let state = Rc::new(RefCell::new(AppState::default()));
    let saved = Rc::clone(&state);
    let panel = build_harness_settings_panel(
        Rc::clone(&state),
        Rc::new(move |keys| {
            saved.borrow_mut().visible_harnesses = Some(keys);
            crate::state::save_state_async(saved.borrow().clone());
        }),
        Rc::new(|_| {}), Rc::new(|_| {}), Rc::new(|_| {}),
        ConnectionHooks::inert(),
    );
    find_all::<Button>(&panel.widget, "settings-harnesses-entry")[0].emit_clicked();
    let pages = find_all::<gtk4::Widget>(&panel.widget, "harness-page");
    let (harnesses, form_page) = (pages[2].clone(), pages[3].clone());
    let clone_button = |name: &str| find_all::<Button>(
        &harness_row_named(&panel.widget, name), "launcher-btn",
    ).into_iter().find(|button| button.label().as_deref() == Some("Clone")).unwrap();
    let entry = |placeholder: &str| find_all::<gtk4::Widget>(&panel.widget, "ws-entry").iter()
        .filter_map(|widget| widget.clone().downcast::<gtk4::Entry>().ok())
        .find(|widget| widget.placeholder_text().as_deref() == Some(placeholder)).unwrap();
    let button = |text: &str| find_all::<Button>(&panel.widget, "launcher-btn").into_iter()
        .find(|b| b.label().as_deref() == Some(text)).unwrap();

    // Cloning a built-in adds a custom copy with its executable and
    // current arguments, then opens the edit form pre-filled on it.
    clone_button("Aider").emit_clicked();
    assert!(shown(&form_page));
    assert_eq!(title_text(&panel.widget), "Edit harness");
    assert_eq!(state.borrow().custom_harnesses.len(), 1);
    crate::state::flush_state_saves();
    let copy = state.borrow().custom_harnesses[0].clone();
    assert_eq!(copy.name, "Aider");
    assert_eq!(copy.executable, crate::tmux::harness_binary("aider").unwrap());
    assert_eq!(copy.arguments, ["--yes-always"]);
    assert_eq!(crate::state::load_state().custom_harnesses, vec![copy]);
    assert_eq!(entry("Harness name").text(), "Aider");
    assert_eq!(entry("Optional arguments, e.g. --model 'my model'").text(), "--yes-always");
    entry("Harness name").set_text("Aider two");
    entry("Optional arguments, e.g. --model 'my model'").set_text("--model 'gpt 4'");
    button("Save harness").emit_clicked();
    assert!(shown(&harnesses));
    assert_eq!(state.borrow().custom_harnesses[0].name, "Aider two");
    assert_eq!(state.borrow().custom_harnesses[0].arguments, ["--model", "gpt 4"]);

    // Cloning a custom launcher copies its fields under a fresh id and
    // opens the form on the copy; Cancel still keeps the copy.
    clone_button("Aider two").emit_clicked();
    assert!(shown(&form_page));
    assert_eq!(state.borrow().custom_harnesses.len(), 2);
    let (first, second) =
        (state.borrow().custom_harnesses[0].id.clone(), state.borrow().custom_harnesses[1].id.clone());
    assert_ne!(first, second);
    assert_eq!(state.borrow().custom_harnesses[1].name, "Aider two");
    assert_eq!(state.borrow().custom_harnesses[1].arguments, ["--model", "gpt 4"]);
    assert_eq!(crate::state::load_state().custom_harnesses.len(), 2);
    button("Cancel").emit_clicked();
    assert!(shown(&harnesses), "cancel returns to the launcher list, copy stays");
    assert_eq!(state.borrow().custom_harnesses.len(), 2);

    // Removing one copy leaves the other with its own id and fields.
    let rows: Vec<gtk4::Widget> = find_all::<gtk4::Widget>(&panel.widget, "harness-row").into_iter()
        .filter(|row| find_all::<gtk4::Widget>(row, "harness-name").iter()
            .any(|label| label.downcast_ref::<Label>().is_some_and(|l| l.text() == "Aider two")))
        .collect();
    rows[1].add_css_class("clone-test-second-row");
    let second_row = find_all::<gtk4::Widget>(&panel.widget, "clone-test-second-row")[0].clone();
    find_all::<Button>(&second_row, "launcher-btn").into_iter()
        .find(|b| b.label().as_deref() == Some("Remove")).unwrap().emit_clicked();
    crate::state::flush_state_saves();
    assert_eq!(state.borrow().custom_harnesses.len(), 1);
    assert_eq!(state.borrow().custom_harnesses[0].id, first);
    assert_eq!(state.borrow().custom_harnesses[0].name, "Aider two");
    assert_eq!(crate::state::load_state().custom_harnesses[0].arguments, ["--model", "gpt 4"]);
    assert!(find_all::<gtk4::Widget>(&panel.widget, "harness-row").into_iter()
        .any(|row| find_all::<gtk4::Widget>(&row, "harness-name").iter()
            .any(|label| label.downcast_ref::<Label>().is_some_and(|l| l.text() == "Aider"))),
        "the built-in launcher survives its own cloning");
    std::fs::remove_dir_all(home).unwrap();
}

/// The Parameters button of the installed built-in harness called `name`.
fn params_button(panel: &gtk4::Widget, name: &str) -> Button {
    let row = harness_row_named(panel, name);
    find_all::<Button>(&row, "harness-params").remove(0)
}

fn harness_row_named(panel: &gtk4::Widget, name: &str) -> gtk4::Widget {
    find_all::<gtk4::Widget>(panel, "harness-row")
        .into_iter()
        .find(|row| {
            find_all::<gtk4::Widget>(row, "harness-name")
                .iter()
                .any(|label| label.downcast_ref::<Label>().is_some_and(|l| l.text() == name))
        })
        .unwrap_or_else(|| panic!("no launcher row for {name}"))
}

#[test]
fn built_in_parameters_replace_defaults_and_reach_new_launches() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process(
            "harness_settings::tests::built_in_parameters_replace_defaults_and_reach_new_launches",
        );
        return;
    }
    gtk4::init().unwrap();
    let home = std::env::temp_dir().join(format!("sd-params-ui-{}", std::process::id()));
    let bin = home.join(".local/bin");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&bin).unwrap();
    std::env::set_var("HOME", &home);
    // Aider stands in for any harness with built-in defaults; the stub gets
    // it detected whether or not the real CLI is installed.
    let stub = bin.join("aider");
    std::fs::write(&stub, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let aider = crate::tmux::detect_harness_command("aider").unwrap()
        .strip_suffix(" --yes-always").unwrap().to_string();
    let state = Rc::new(RefCell::new(AppState::default()));
    let panel = inert_panel(Rc::clone(&state));
    find_all::<Button>(&panel.widget, "settings-harnesses-entry")[0].emit_clicked();
    let pages = find_all::<gtk4::Widget>(&panel.widget, "harness-page");
    // The parameters page comes just before the plugin page, which is last.
    let (harnesses, args_page) = (pages[2].clone(), pages[pages.len() - 2].clone());
    let entry = find_all::<gtk4::Widget>(&panel.widget, "harness-args-entry")[0].clone()
        .downcast::<gtk4::Entry>().unwrap();
    let label = |class: &str| find_all::<gtk4::Widget>(&panel.widget, class)[0].clone().downcast::<Label>().unwrap();
    let (preview, default, error) =
        (label("harness-args-preview"), label("harness-args-default"), label("harness-args-error"));
    let button = |text: &str| find_all::<Button>(&args_page, "launcher-btn").into_iter()
        .find(|button| button.label().as_deref() == Some(text)).unwrap();
    let row_command = || find_all::<gtk4::Widget>(&harness_row_named(&panel.widget, "Aider"), "harness-cmd")[0]
        .clone().downcast::<Label>().unwrap().text().to_string();

    params_button(&panel.widget, "Aider").emit_clicked();
    assert!(shown(&args_page));
    assert_eq!(title_text(&panel.widget), "Aider parameters");
    assert_eq!(default.text(), "Built-in default: --yes-always");
    assert_eq!(entry.text(), "--yes-always");
    entry.set_text("--model \"gpt 5\" --no-auto-commits");
    assert_eq!(preview.text(), format!("Starts: {aider} --model 'gpt 5' --no-auto-commits"));
    entry.set_text("--model 'gpt");
    button("Save parameters").emit_clicked();
    assert!(shown(&args_page), "invalid parameters keep the page open");
    assert!(error.is_visible());
    assert_eq!(error.text(), "Parameters have an unmatched quote");
    assert!(entry.has_css_class("ws-entry-invalid"));
    assert!(state.borrow().harness_args.is_empty());

    // An empty field replaces the defaults: no parameters at all, and a
    // restored card does not get the dropped flag back.
    entry.set_text("");
    assert!(!error.is_visible());
    assert!(!entry.has_css_class("ws-entry-invalid"));
    button("Save parameters").emit_clicked();
    assert!(shown(&harnesses));
    assert_eq!(state.borrow().harness_args["aider"], Vec::<String>::new());
    assert_eq!(crate::tmux::resolve_command("aider", None), aider);
    assert_eq!(crate::tmux::resolve_command("aider", Some("/opt/aider")), "/opt/aider");
    assert_eq!(row_command(), aider);
    crate::state::flush_state_saves();
    assert_eq!(crate::state::load_state().harness_args["aider"], Vec::<String>::new());

    // Enter saves. New launches use the saved words; a restored card keeps
    // its own value and gains only what it lacks.
    params_button(&panel.widget, "Aider").emit_clicked();
    assert_eq!(entry.text(), "");
    entry.set_text("--model 'gpt 5'");
    entry.emit_activate();
    assert!(shown(&harnesses));
    assert_eq!(crate::tmux::resolve_command("aider", None), format!("{aider} --model 'gpt 5'"));
    assert_eq!(crate::tmux::resolve_command("aider", Some("/opt/aider --model x")), "/opt/aider --model x");
    assert_eq!(crate::tmux::resolve_command("aider", Some("/opt/aider")), "/opt/aider --model 'gpt 5'");
    assert_eq!(row_command(), format!("{aider} --model 'gpt 5'"));

    // Back to the built-in default: nothing stays pinned in state.
    params_button(&panel.widget, "Aider").emit_clicked();
    button("Use built-in default").emit_clicked();
    assert_eq!(entry.text(), "--yes-always");
    button("Save parameters").emit_clicked();
    assert!(!state.borrow().harness_args.contains_key("aider"));
    assert_eq!(crate::tmux::resolve_command("aider", None), format!("{aider} --yes-always"));
    crate::state::flush_state_saves();
    assert!(crate::state::load_state().harness_args.is_empty());

    params_button(&panel.widget, "Aider").emit_clicked();
    entry.set_text("--changed");
    button("Cancel").emit_clicked();
    assert!(shown(&harnesses), "cancel returns to the launcher list");
    assert!(state.borrow().harness_args.is_empty());
    assert_eq!(params_button(&panel.widget, "Terminal").label().as_deref(), Some("Parameters"));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn test_resolve_visible_filters_to_detection_in_detection_order() {
    let detected = vec![info("claude"), info("codex"), info("shell")];
    // Unknown (uninstalled) keys are dropped, and the stored order does
    // not decide the top-bar order — detection does.
    let stored = vec![
        "shell".to_string(),
        "grok".to_string(),
        "claude".to_string(),
    ];
    assert_eq!(
        resolve_visible(Some(&stored), &detected),
        vec!["claude".to_string(), "shell".to_string()]
    );
    // "Hide all" must survive a reopen as "nothing shown".
    assert!(resolve_visible(Some(&[]), &detected).is_empty());
}

#[test]
fn test_toggle_keeps_detection_order() {
    let order = vec![
        "claude".to_string(),
        "codex".to_string(),
        "grok".to_string(),
    ];
    // Turning a harness back on puts it where detection says it belongs.
    let mut selection = vec!["codex".to_string(), "grok".to_string()];
    toggle(&mut selection, "claude", &order);
    assert_eq!(selection, vec!["claude", "codex", "grok"]);
    // …and turning it off removes exactly that one.
    toggle(&mut selection, "codex", &order);
    assert_eq!(selection, vec!["claude", "grok"]);
    // Harnesses that are not detected here can never be selected.
    toggle(&mut selection, "aider", &order);
    assert_eq!(selection, vec!["claude", "grok"]);
}

#[test]
fn settings_editor_button_switch() {
    crate::gtk_test::run_in_child_process("harness_settings::tests::settings_editor_button_switch_inner");
}

/// Settings → SD Editor turns the top bar's ✎ off and on, and saves it.
#[test]
fn settings_editor_button_switch_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    // Saving writes state.json under HOME: a private one.
    let root = std::env::temp_dir().join(format!("sd-settings-editor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("HOME", &root);
    let _ = gtk4::init();
    let app_state = Rc::new(RefCell::new(AppState::default()));
    let button = crate::editor::toolbar_button(Rc::new(|| std::path::PathBuf::from("/tmp")), !app_state.borrow().editor_button_hidden);
    assert!(button.get_visible(), "shown by default");
    let panel = build_harness_settings_panel(
        Rc::clone(&app_state),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        ConnectionHooks::inert(),
    );
    (panel.refresh)();
    find_all::<Button>(&panel.widget, "settings-editor-entry").pop().expect("an SD Editor entry").emit_clicked();
    assert_eq!(title_text(&panel.widget), "SD Editor");
    let toggle = find_all::<gtk4::Switch>(&panel.widget, "editor-button-toggle").pop().expect("the switch");
    assert!(toggle.is_active());
    toggle.set_active(false);
    assert!(!button.get_visible());
    assert!(app_state.borrow().editor_button_hidden);
    let saved = || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(crate::state::get_state_path()).unwrap_or_default();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(hidden) = value["editor_button_hidden"].as_bool() {
                    return hidden;
                }
            }
            assert!(std::time::Instant::now() < deadline, "state.json was not written");
            crate::gtk_test::pump(20);
        }
    };
    assert!(saved());
    toggle.set_active(true);
    assert!(button.get_visible());
    assert!(!app_state.borrow().editor_button_hidden);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while saved() && std::time::Instant::now() < deadline {
        crate::gtk_test::pump(20);
    }
    assert!(!saved());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn settings_plugin_row_opens_its_page() {
    crate::gtk_test::run_in_child_process("harness_settings::tests::settings_plugin_row_opens_its_page_inner");
}

/// Settings → Plugins: a row opens the plugin's own page in the same
/// panel, titled with its name, and ← goes back to the list.
#[test]
fn settings_plugin_row_opens_its_page_inner() {
    if !crate::gtk_test::is_child() {
        return;
    }
    let root = std::env::temp_dir().join(format!("sd-settings-plugin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::env::set_var("SD_PLUGINS_DIR", root.join("plugins"));
    std::env::set_var("SD_PLUGINS_STATE_DIR", root.join("state"));
    let dir = root.join("plugins/clock");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.json"), serde_json::json!({
        "id": "clock", "name": "Big Clock", "description": "A clock", "api": 1, "version": "2.0.1", "enabled": false,
        "surfaces": [{"id": "w", "slot": "toolbar.item", "qml": "W.qml"}]
    }).to_string()).unwrap();
    std::fs::write(dir.join("W.qml"), "import QtQuick\nItem {}\n").unwrap();
    let _ = gtk4::init();
    let panel = build_harness_settings_panel(
        Rc::new(RefCell::new(AppState::default())),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        ConnectionHooks::inert(),
    );
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&panel.widget));
    let slots = crate::plugin_ui::slots::Slots::install(&overlay, 46);
    let window = gtk4::Window::new();
    window.set_default_size(700, 700);
    window.set_child(Some(&overlay));
    window.present();
    (panel.refresh)();
    find_all::<Button>(&panel.widget, "settings-plugins-entry").pop().expect("a Plugins entry").emit_clicked();
    assert_eq!(title_text(&panel.widget), "Plugins");
    let row = || find_all::<Button>(&panel.widget, "plugin-row-open").into_iter().find(|b| b.is_mapped());
    crate::gtk_test::pump_until("its row", || row().is_some());
    row().unwrap().emit_clicked();
    crate::gtk_test::pump(50);
    assert_eq!(title_text(&panel.widget), "Big Clock");
    let names = find_all::<Label>(&panel.widget, "plugin-detail-name");
    assert!(names.iter().any(|l| l.is_mapped() && l.text() == "Big Clock"));
    let back = find_all::<Button>(&panel.widget, "term-btn").into_iter()
        .find(|b| b.label().as_deref() == Some("←")).unwrap();
    assert!(back.is_mapped());
    back.emit_clicked();
    crate::gtk_test::pump(50);
    assert_eq!(title_text(&panel.widget), "Plugins");
    assert!(row().is_some(), "back on the list");
    drop(slots);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn test_panel_gtk_structure() {
    // GTK may only be used from one thread per process, so the widget
    // assertions run in a child process (see `crate::gtk_test`).
    crate::gtk_test::run_in_child_process(
        "harness_settings::tests::panel_gtk_structure_child",
    );
}

/// Only meaningful when re-run as the single test of a fresh process.
#[test]
fn panel_gtk_structure_child() {
    if !crate::gtk_test::is_child() {
        return;
    }
    let _ = gtk4::init();

    let detected = detect_harnesses();
    let app_state = Rc::new(RefCell::new(AppState::default()));
    let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_cb = Rc::clone(&seen);
    let shortcut_changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let shortcut_cb = Rc::clone(&shortcut_changes);
    let size_changes: Rc<RefCell<Vec<TopBarSize>>> = Rc::new(RefCell::new(Vec::new()));
    let size_cb = Rc::clone(&size_changes);
    let panel = build_harness_settings_panel(
        Rc::clone(&app_state),
        Rc::new(move |keys: Vec<String>| seen_cb.borrow_mut().push(keys)),
        Rc::new(|_| {}),
        Rc::new(move |combo: String| shortcut_cb.borrow_mut().push(combo)),
        Rc::new(move |size| size_cb.borrow_mut().push(size)),
        ConnectionHooks::inert(),
    );
    (panel.refresh)();

    // The hub is deliberately short. Each substantial setting has its own
    // page; Connections is an overview with a sub-page per kind of
    // connection, so none of them is a long scroll.
    let pages = find_all::<gtk4::Widget>(&panel.widget, "harness-page");
    assert_eq!(
        pages.len(),
        21,
        "hub + five destinations + SD Editor + plugins + custom harness page + seven Connections pages + system launch + updates + harness parameters page + a plugin's page + Telegram"
    );
    let sections: Vec<usize> = pages
        .iter()
        .map(|p| count_class(p, "launcher-section"))
        .collect();
    // hub, shortcut, harnesses, custom, top bar, SD Editor, MCP, plugins,
    // overview, add, invitation, PCs, phones, rejected, network, sleep,
    // system launch, updates, Telegram, harness parameters, a plugin's page
    assert_eq!(sections, vec![0, 1, 2, 1, 1, 1, 2, 0, 1, 0, 2, 2, 3, 1, 2, 1, 1, 1, 5, 1, 0]);
    assert_eq!(count_class(&panel.widget, "launcher-section-num"), 0);
    assert_eq!(count_class(&panel.widget, "launcher-section-title"), 27);
    assert_eq!(count_class(&panel.widget, "settings-firewall-warning"), 1);

    // The card opens on the hub, and ← appears on every destination page.
    let btn_back = find_all::<Button>(&panel.widget, "term-btn")
        .into_iter()
        .find(|b| b.label().as_deref() == Some("←"))
        .expect("the header must offer a back button");
    assert!(shown(&pages[0]), "settings hub is the landing page");
    assert!(pages[1..].iter().all(|page| !shown(page)));
    assert!(!shown(&btn_back));
    assert_eq!(title_text(&panel.widget), "Settings");

    for (class, page_index, page_title) in [
        ("settings-shortcut-entry", 1, "Keyboard shortcut"),
        ("settings-harnesses-entry", 2, "Harness launchers"),
        ("settings-top-bar-entry", 4, "Top bar"),
        ("settings-editor-entry", 5, "SD Editor"),
        ("settings-mcp-entry", 6, "MCP"),
        ("settings-plugins-entry", 7, "Plugins"),
        ("android-settings-entry", 8, "Connections"),
        ("settings-sleep-lock-entry", 15, "Sleep lock"),
        ("settings-system-launch-entry", 16, "System launch"),
        ("settings-updates-entry", 17, "Updates"),
        ("settings-telegram-entry", 18, "Telegram"),
    ] {
        let button = find_all::<Button>(&panel.widget, class)
            .into_iter()
            .next()
            .expect("each settings destination needs a navigation button");
        button.emit_clicked();
        assert!(shown(&pages[page_index]));
        assert!(pages
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != page_index)
            .all(|(_, page)| !shown(page)));
        assert!(shown(&btn_back));
        assert_eq!(title_text(&panel.widget), page_title);
        btn_back.emit_clicked();
        assert!(shown(&pages[0]));
        assert!(!shown(&btn_back));
        assert_eq!(title_text(&panel.widget), "Settings");
    }

    // The hub entry names this build; the Updates page checks on opening.
    // A test binary is not a clone's release build, so the check says it
    // cannot update rather than asking git anything.
    let version = crate::updates::running().to_string();
    assert!(find_all::<Label>(&panel.widget, "update-version").iter().any(|chip| chip.text() == version));
    find_all::<Button>(&panel.widget, "settings-updates-entry")[0].emit_clicked();
    let status = find_all::<Label>(&panel.widget, "update-status").into_iter().next().expect("update status");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while status.text() == "Checking GitHub…" || status.text() == "Not checked yet." {
        assert!(std::time::Instant::now() < deadline, "the check never answered");
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(status.text().contains("cannot update itself"), "{}", status.text());
    assert!(status.has_css_class("launcher-note-error"));
    let update = find_all::<Button>(&panel.widget, "launcher-btn-primary")
        .into_iter()
        .find(|button| button.label().is_some_and(|label| label.starts_with("⬇ Update")))
        .expect("the update button exists");
    assert!(!update.is_visible(), "nothing to update from here");
    btn_back.emit_clicked();

    find_all::<Button>(&panel.widget, "settings-harnesses-entry")[0].emit_clicked();
    find_all::<Button>(&panel.widget, "settings-add-harness-entry")[0].emit_clicked();
    assert!(shown(&pages[3]));
    assert_eq!(title_text(&panel.widget), "Add a harness");
    btn_back.emit_clicked();
    assert!(shown(&pages[2]), "back from the form returns to harness launchers");
    assert_eq!(title_text(&panel.widget), "Harness launchers");
    btn_back.emit_clicked();
    assert!(shown(&pages[0]));

    // Each Connections entry opens its own page; ← climbs back to the
    // overview, then to the hub.
    find_all::<Button>(&panel.widget, "android-settings-entry")[0].emit_clicked();
    for (class, page_index, page_title) in [
        ("connections-add-entry", 9, "Add a device"),
        ("connections-pcs-entry", 11, "PCs"),
        ("connections-phones-entry", 12, "Phones & other devices"),
        ("connections-rejected-entry", 13, "Rejected devices"),
        ("connections-network-entry", 14, "Network & firewall"),
    ] {
        find_all::<Button>(&panel.widget, class)[0].emit_clicked();
        assert!(shown(&pages[page_index]), "{class}");
        assert_eq!(pages.iter().filter(|page| shown(*page)).count(), 1);
        assert_eq!(title_text(&panel.widget), page_title);
        btn_back.emit_clicked();
        assert!(shown(&pages[8]), "back from {page_title} returns to Connections");
        assert_eq!(title_text(&panel.widget), "Connections");
    }
    btn_back.emit_clicked();
    assert!(shown(&pages[0]));

    // The firewall warning's Review goes straight to Network & firewall.
    find_all::<Button>(&panel.widget, "launcher-btn")
        .into_iter()
        .find(|b| b.label().as_deref() == Some("Review") && b.ancestor(gtk4::Box::static_type()).is_some_and(|a| a.has_css_class("settings-firewall-warning")))
        .expect("the hub's firewall warning offers Review")
        .emit_clicked();
    assert!(shown(&pages[14]));
    btn_back.emit_clicked();
    btn_back.emit_clicked();
    assert!(shown(&pages[0]));

    // Reopening returns to the hub even when Connections was the last page.
    let btn_launcher = find_all::<Button>(&panel.widget, "android-settings-entry")
        .into_iter()
        .next()
        .expect("the Android destination must offer a navigation button");
    panel.widget.set_visible(false);
    btn_launcher.emit_clicked();
    assert!(shown(&pages[8]));
    panel.widget.set_visible(true);
    assert!(shown(&pages[0]), "reopening resets to the settings hub");
    assert!(pages[1..].iter().all(|page| !shown(page)));
    assert!(!shown(&btn_back));
    assert_eq!(title_text(&panel.widget), "Settings");

    // The recorder shows the shipped shortcut until one is recorded, and
    // switches to the stored one as soon as state.json has it. Note what
    // this test must NOT do: click Record. That would arm the real keymap
    // guard and park the shortcuts of the machine running the tests (see
    // `shortcut::begin_capture`).
    let record = find_all::<Button>(&panel.widget, "launcher-btn")
        .into_iter()
        .find(|b| b.label().as_deref() == Some("⏺ Record"))
        .expect("the shortcut section must offer a Record button");
    assert_eq!(combo_text(&panel.widget), crate::shortcut::DEFAULT_COMBO);
    assert!(shortcut_changes.borrow().is_empty(), "nothing recorded yet");

    app_state.borrow_mut().toggle_shortcut = Some("SUPER + SHIFT + K".to_string());
    (panel.refresh)();
    assert_eq!(combo_text(&panel.widget), "SUPER + SHIFT + K");
    assert_eq!(record.label().as_deref(), Some("⏺ Record"));

    // Large preserves the original toolbar scale. Choosing another size
    // updates state, selection styling and the live-dock callback.
    let size_buttons = find_all::<Button>(&panel.widget, "top-bar-size");
    assert_eq!(size_buttons.len(), 3);
    assert!(size_buttons[2].has_css_class("top-bar-size-active"));
    size_buttons[0].emit_clicked();
    assert_eq!(app_state.borrow().top_bar_size, TopBarSize::Small);
    assert_eq!(*size_changes.borrow(), [TopBarSize::Small]);
    assert!(size_buttons[0].has_css_class("top-bar-size-active"));
    assert!(!size_buttons[2].has_css_class("top-bar-size-active"));

    // One toggle row per detected harness, all ON by default.
    let toggles = find_all::<Button>(&panel.widget, "harness-toggle");
    assert_eq!(
        toggles.len(),
        detected.len(),
        "expected one toggle per detected harness"
    );
    assert!(
        toggles.iter().all(|b| b.label().as_deref() == Some("ON")),
        "every detected harness must start enabled"
    );

    // Flipping the first row reports exactly that harness as hidden, and
    // reopening the panel keeps it off.
    toggles[0].emit_clicked();
    let hidden = detected[0].key.to_string();
    let expected: Vec<String> = detected_keys(&detected)
        .into_iter()
        .filter(|k| *k != hidden)
        .collect();
    assert_eq!(seen.borrow().len(), 1, "one change per click");
    assert_eq!(seen.borrow()[0], expected);

    app_state.borrow_mut().visible_harnesses = Some(expected.clone());
    (panel.refresh)();
    let toggles = find_all::<Button>(&panel.widget, "harness-toggle");
    assert_eq!(
        toggles[0].label().as_deref(),
        Some("OFF"),
        "stored selection must stick"
    );
    assert!(toggles[1..]
        .iter()
        .all(|b| b.label().as_deref() == Some("ON")));
}

/// The text of the recorder's combo label.
fn combo_text(w: &gtk4::Widget) -> String {
    let found = find_all::<gtk4::Widget>(w, "shortcut-combo");
    assert_eq!(found.len(), 1, "exactly one combo label");
    found[0]
        .downcast_ref::<Label>()
        .expect("combo label is a Label")
        .label()
        .to_string()
}

/// The widget's own `visible` property. NOT `is_visible()`: GTK4's walks
/// up to the root, so every page of an unshown card would read as hidden
/// and the navigation assertions below could not tell them apart.
fn shown<W: IsA<gtk4::Widget>>(w: &W) -> bool {
    w.property::<bool>("visible")
}

/// The card header's title, which follows the current page.
fn title_text(w: &gtk4::Widget) -> String {
    let found = find_all::<gtk4::Widget>(w, "term-title");
    assert_eq!(found.len(), 1, "exactly one card title");
    found[0]
        .downcast_ref::<Label>()
        .expect("card title is a Label")
        .label()
        .to_string()
}

#[test]
fn settings_opens_the_getting_started_guide() {
    if !crate::gtk_test::is_child() {
        crate::gtk_test::run_in_child_process("harness_settings::tests::settings_opens_the_getting_started_guide");
        return;
    }
    gtk4::init().unwrap();
    let opened = Rc::new(Cell::new(0));
    crate::welcome_tour::set_replay(Rc::new({
        let opened = Rc::clone(&opened);
        move || opened.set(opened.get() + 1)
    }));
    let panel = inert_panel(Rc::new(RefCell::new(AppState::default())));
    let entries = find_all::<Button>(&panel.widget, "settings-entry");
    let guide = find_all::<Button>(&panel.widget, "settings-tour-entry");
    assert_eq!(guide.len(), 1, "one Getting started guide entry");
    assert_eq!(entries.first(), guide.first(), "it is the first entry on the Settings home");
    let titles: Vec<String> = find_all::<Label>(guide[0].upcast_ref(), "settings-entry-title")
        .iter().map(|label| label.text().to_string()).collect();
    assert_eq!(titles, ["Getting started guide"]);
    guide[0].emit_clicked();
    assert_eq!(opened.get(), 1, "the entry opens the guide");
}
