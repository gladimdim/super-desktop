use super::*;

/// The blocking OpenClaw steps behind the plugin line under OpenClaw's row.
/// Plain function pointers, so they can run on a worker thread and tests can
/// swap in fakes (a test must never touch the real gateway or its config).
#[derive(Clone, Copy)]
pub(in crate::harness_settings) struct OpenClawActions {
    pub(in crate::harness_settings) detect: fn() -> crate::harness_metadata::OpenClawPlugin,
    pub(in crate::harness_settings) connect: fn() -> Result<(), String>,
    pub(in crate::harness_settings) restart: fn() -> Result<(), String>,
}

impl OpenClawActions {
    pub(in crate::harness_settings) const SYSTEM: Self = Self {
        detect: crate::harness_metadata::openclaw_plugin,
        connect: crate::harness_metadata::connect_openclaw,
        restart: crate::harness_metadata::restart_openclaw_gateway,
    };
}

/// What the OpenClaw plugin setup is doing right now.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness_settings) enum PluginStep {
    Idle,
    Connecting,
    /// Registered from Settings: the running gateway still has to load it.
    RestartNeeded,
    Restarting,
    Restarted,
}

/// The setup state every OpenClaw plugin line shares. Settings rebuilds its
/// harness list on each open, and a registration that is still running, or a
/// gateway restart still owed, must be neither forgotten nor started twice.
/// Main thread only.
#[derive(Default)]
pub(in crate::harness_settings) struct PluginSetup {
    pub(in crate::harness_settings) step: Cell<Option<PluginStep>>,
    pub(in crate::harness_settings) error: RefCell<Option<String>>,
    /// Repaint callbacks of the lines on screen (dead once a line is dropped).
    pub(in crate::harness_settings) lines: RefCell<Vec<std::rc::Weak<dyn Fn()>>>,
}

impl PluginSetup {
    pub(in crate::harness_settings) fn step(&self) -> PluginStep {
        self.step.get().unwrap_or(PluginStep::Idle)
    }

    pub(in crate::harness_settings) fn set(&self, step: PluginStep, error: Option<String>) {
        self.step.set(Some(step));
        self.error.replace(error);
        let lines: Vec<_> = self.lines.borrow().iter().filter_map(std::rc::Weak::upgrade).collect();
        for paint in lines {
            paint();
        }
    }
}

thread_local! {
    pub(in crate::harness_settings) static PLUGIN_SETUP: Rc<PluginSetup> = Rc::default();
}

/// The line under an installed OpenClaw's row. OpenClaw cards get status,
/// prompts and native titles only through the SUPER DESKTOP gateway plugin,
/// so say whether it is connected and offer the one-click setup
/// (`install_openclaw`), then a gateway restart. Both run off the GTK thread.
pub(in crate::harness_settings) fn openclaw_plugin_notice(actions: OpenClawActions) -> Box {
    use crate::harness_metadata::OpenClawPlugin;
    let setup = PLUGIN_SETUP.with(Rc::clone);
    // A finished restart or an old error was already reported.
    match setup.step() {
        PluginStep::Restarted => setup.set(PluginStep::Idle, None),
        step => { setup.error.replace(None); setup.step.set(Some(step)); }
    }
    let line = Box::new(Orientation::Horizontal, 8);
    line.add_css_class("harness-plugin-notice");
    let text = Label::new(None);
    text.add_css_class("harness-plugin-text");
    text.set_xalign(0.0);
    text.set_hexpand(true);
    text.set_wrap(true);
    text.set_valign(Align::Center);
    line.append(&text);
    let connected = Label::new(Some("Connected"));
    connected.add_css_class("harness-plugin-connected");
    connected.set_valign(Align::Center);
    connected.set_tooltip_text(Some("OpenClaw loads the SUPER DESKTOP gateway plugin: cards report status, prompts and titles"));
    line.append(&connected);
    let connect = Button::with_label("Connect");
    connect.add_css_class("launcher-btn");
    connect.add_css_class("launcher-btn-primary");
    connect.set_valign(Align::Center);
    connect.set_tooltip_text(Some("Register the SUPER DESKTOP plugin with OpenClaw (openclaw plugins install)"));
    line.append(&connect);
    let restart = Button::with_label("Restart gateway");
    restart.add_css_class("launcher-btn");
    restart.set_valign(Align::Center);
    restart.set_tooltip_text(Some("openclaw gateway restart"));
    line.append(&restart);

    let paint: Rc<dyn Fn()> = {
        let (text, connected, connect, restart) = (text.clone(), connected.clone(), connect.clone(), restart.clone());
        let setup = Rc::clone(&setup);
        Rc::new(move || {
            let plugin = (actions.detect)();
            let step = setup.step();
            let (message, tooltip) = match (step, plugin) {
                (PluginStep::Connecting, _) => ("Connecting… registering the plugin with OpenClaw", None),
                (PluginStep::Restarting, _) => ("Restarting the OpenClaw gateway…", None),
                (PluginStep::RestartNeeded, _) => (
                    "Plugin registered. Restart the OpenClaw gateway to load it; open cards update from their next turn.",
                    None,
                ),
                (PluginStep::Restarted, _) => ("Gateway restarted. OpenClaw cards report status from their next turn.", None),
                (PluginStep::Idle, OpenClawPlugin::Connected) => ("Status & titles come from the SUPER DESKTOP plugin", None),
                (PluginStep::Idle, OpenClawPlugin::Missing(why)) => ("Status & titles need the SUPER DESKTOP plugin", Some(why)),
                (PluginStep::Idle, OpenClawPlugin::Unreadable) => (
                    "Status & titles need the SUPER DESKTOP plugin (OpenClaw's config could not be read)",
                    None,
                ),
            };
            let failed = setup.error.borrow().clone();
            text.set_text(failed.as_deref().unwrap_or(message));
            text.set_tooltip_text(tooltip);
            if failed.is_some() {
                text.add_css_class("launcher-note-error");
            } else {
                text.remove_css_class("launcher-note-error");
            }
            connected.set_visible(plugin == OpenClawPlugin::Connected && step != PluginStep::Connecting);
            connect.set_visible(plugin != OpenClawPlugin::Connected || step == PluginStep::Connecting);
            connect.set_sensitive(step != PluginStep::Connecting);
            restart.set_visible(matches!(step, PluginStep::RestartNeeded | PluginStep::Restarting));
            restart.set_sensitive(step != PluginStep::Restarting);
        })
    };
    setup.lines.borrow_mut().retain(|line| line.strong_count() > 0);
    setup.lines.borrow_mut().push(Rc::downgrade(&paint));
    // One blocking OpenClaw step on a worker; `done` or `fallback` is next.
    let run = {
        let setup = Rc::clone(&setup);
        move |busy: PluginStep, work: fn() -> Result<(), String>, done: PluginStep, fallback: PluginStep| {
            if matches!(setup.step(), PluginStep::Connecting | PluginStep::Restarting) {
                return;
            }
            setup.set(busy, None);
            let setup = Rc::clone(&setup);
            glib::MainContext::default().spawn_local(async move {
                match gtk4::gio::spawn_blocking(work).await {
                    Ok(Ok(())) => setup.set(done, None),
                    Ok(Err(message)) => setup.set(fallback, Some(message)),
                    Err(_) => setup.set(fallback, Some("The OpenClaw step stopped unexpectedly".into())),
                }
            });
        }
    };
    // The buttons own this line's repaint; the shared state only holds it
    // weakly, so a line dropped from the rebuilt list stops being painted.
    connect.connect_clicked({
        let (run, paint) = (run.clone(), Rc::clone(&paint));
        move |_| {
            let _owned = &paint;
            run(PluginStep::Connecting, actions.connect, PluginStep::RestartNeeded, PluginStep::Idle)
        }
    });
    restart.connect_clicked({
        let paint = Rc::clone(&paint);
        move |_| {
            let _owned = &paint;
            run(PluginStep::Restarting, actions.restart, PluginStep::Restarted, PluginStep::RestartNeeded)
        }
    });
    paint();
    line
}

