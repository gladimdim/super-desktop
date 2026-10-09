//! Settings page for local MCP access and client connection instructions.
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation, Switch};
use std::cell::Cell;
use std::rc::Rc;
use super_desktop::mcp_settings::{self, Config};

type Load = Rc<dyn Fn() -> std::io::Result<Config>>;
type Save = Rc<dyn Fn(Config) -> std::io::Result<()>>;

fn text(body: &Box, value: &str) -> Label {
    let label = Label::new(Some(value));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_max_width_chars(60);
    body.append(&label);
    label
}

pub fn build(root: &Box) -> Rc<dyn Fn()> {
    build_with_store(
        root,
        Rc::new(mcp_settings::load),
        Rc::new(mcp_settings::save),
    )
}

fn build_with_store(root: &Box, load: Load, save: Save) -> Rc<dyn Fn()> {
    let (_, body) = crate::launcher_settings::section_card(root, "", "Local MCP access");
    text(&body, "Let agents on this computer discover SUPER DESKTOP tools. Your agent's client starts the server when it connects.");
    let status = text(&body, "");
    status.add_css_class("launcher-note");
    let mut switches = Vec::new();
    for (name, title, description) in [
        ("mcp-enabled", "Enable MCP", "Allow local agents to inspect desktop and harness metadata."),
        ("mcp-output", "Read terminal output", "Allow screen and scrollback capture. Output may contain private data."),
        ("mcp-launch", "Launch harnesses", "Allow agents to create terminal cards. Unsafe launch and download opt-ins still apply."),
        ("mcp-prompts", "Submit prompts", "Allow guarded text prompts to existing harnesses. Commands run with your user account's authority."),
        ("mcp-controls", "Control terminals", "Allow literal text, named keys and Ctrl-C interruption. Input can execute commands."),
        ("mcp-close", "Close terminals", "Allow agents to stop one exact session and remove its card with explicit confirmation."),
    ] {
        let row = Box::new(Orientation::Horizontal, 12);
        let words = Box::new(Orientation::Vertical, 4);
        words.set_hexpand(true);
        text(&words, title).add_css_class("settings-entry-title");
        text(&words, description).add_css_class("launcher-note");
        let toggle = Switch::new();
        toggle.set_widget_name(name);
        toggle.set_valign(Align::Center);
        toggle.update_property(&[gtk4::accessible::Property::Label(title)]);
        row.append(&words);
        row.append(&toggle);
        body.append(&row);
        switches.push(toggle);
    }
    let painting = Rc::new(Cell::new(false));
    let paint: Rc<dyn Fn(Config)> = Rc::new({
        let switches = switches.clone();
        let painting = painting.clone();
        move |config| {
            painting.set(true);
            for (index, value) in [
                config.enabled,
                config.read_output,
                config.launch,
                config.prompts,
                config.controls,
                config.close,
            ]
            .into_iter()
            .enumerate()
            {
                switches[index].set_active(value);
                if index > 0 {
                    switches[index].set_sensitive(config.enabled);
                }
            }
            painting.set(false);
        }
    });
    let refresh: Rc<dyn Fn()> = Rc::new({
        let load = load.clone();
        let paint = paint.clone();
        let status = status.clone();
        move || match load() {
            Ok(config) => {
                paint(config);
                status.set_text(if config.enabled { "MCP is on. Changes apply to the next tool call. Refresh or reconnect your client to discover newly enabled tools." } else { "MCP is off. Existing connections cannot call desktop tools. Calls already in progress may finish." });
            }
            Err(_) => {
                paint(Config::disabled());
                status.set_text("MCP settings could not be read. Access is blocked. Turn on Enable MCP to save fresh settings.");
            }
        }
    });
    for (index, toggle) in switches.iter().enumerate() {
        let load = load.clone();
        let save = save.clone();
        let paint = paint.clone();
        let painting = painting.clone();
        let status = status.clone();
        let refresh = refresh.clone();
        toggle.connect_active_notify(move |toggle| {
            if painting.get() {
                return;
            }
            let mut config = load().unwrap_or_else(|_| Config::disabled());
            match index {
                0 => config.enabled = toggle.is_active(),
                1 => config.read_output = toggle.is_active(),
                2 => config.launch = toggle.is_active(),
                3 => config.prompts = toggle.is_active(),
                4 => config.controls = toggle.is_active(),
                _ => config.close = toggle.is_active(),
            }
            match save(config) {
                Ok(()) => refresh(),
                Err(_) => {
                    paint(load().unwrap_or_else(|_| Config::disabled()));
                    status.set_text("Could not save MCP settings. Your change was not applied.");
                }
            }
        });
    }
    refresh();

    let (_, body) = crate::launcher_settings::section_card(root, "", "Connect an agent");
    text(&body, "Copy the configuration into your client's MCP settings, or give the setup instructions to an agent that can configure its client. Configuration formats vary by client.");
    text(&body, "You do not need to run the server in a separate terminal. This connection works only on this computer. SUPER DESKTOP must be running for desktop tools.").add_css_class("launcher-note");
    match mcp_settings::executable()
        .and_then(|executable| mcp_settings::config_root().map(|root| (executable, root)))
    {
        Ok((executable, config_root)) => {
            let runtime = super_desktop::control::runtime_dir();
            let config = mcp_settings::connection_config(&executable, &config_root, &runtime);
            let label = text(&body, &config);
            label.add_css_class("monospace");
            label.set_selectable(true);
            label.set_wrap_mode(gtk4::pango::WrapMode::Char);
            for (title, value) in [
                ("Copy MCP configuration", config),
                (
                    "Copy agent setup instructions",
                    mcp_settings::connection_instructions(&executable, &config_root, &runtime),
                ),
            ] {
                let button = Button::with_label(title);
                button.add_css_class("launcher-btn");
                let status = status.clone();
                button.connect_clicked(move |button| {
                    button.clipboard().set_text(&value);
                    status.set_text("Copied to clipboard.");
                });
                body.append(&button);
            }
        }
        Err(_) => {
            text(&body, "Could not locate the executable. Configure your client with the absolute path to super-desktop and arguments [\"mcp\", \"serve\"].");
        }
    }
    refresh
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_settings_page_persists_toggles_and_reverts_failed_saves() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("harness_settings::mcp_settings_page::tests::mcp_settings_page_persists_toggles_and_reverts_failed_saves");
            return;
        }
        gtk4::init().expect("private GTK display");
        let stored = Rc::new(Cell::new(Config::default()));
        let fail = Rc::new(Cell::new(false));
        let root = Box::new(Orientation::Vertical, 10);
        let refresh = build_with_store(
            &root,
            Rc::new({
                let stored = stored.clone();
                move || Ok(stored.get())
            }),
            Rc::new({
                let stored = stored.clone();
                let fail = fail.clone();
                move |config| {
                    if fail.get() {
                        Err(std::io::Error::other("test failure"))
                    } else {
                        stored.set(config);
                        Ok(())
                    }
                }
            }),
        );
        let find = |name: &str| -> Option<Switch> {
            crate::gtk_test::find_where(root.upcast_ref(), |w| w.widget_name() == name).and_then(|w| w.downcast().ok())
        };
        let enabled = find("mcp-enabled").unwrap();
        let output = find("mcp-output").unwrap();
        let launch = find("mcp-launch").unwrap();
        let prompts = find("mcp-prompts").unwrap();
        let controls = find("mcp-controls").unwrap();
        let close = find("mcp-close").unwrap();
        assert!(!controls.is_active() && !close.is_active());
        controls.set_active(true);
        close.set_active(true);
        assert!(stored.get().controls && stored.get().close);
        assert!(enabled.is_active());
        assert!(!launch.is_active());
        output.set_active(true);
        launch.set_active(true);
        prompts.set_active(true);
        assert!(stored.get().read_output && stored.get().launch && stored.get().prompts);
        enabled.set_active(false);
        assert!(!stored.get().enabled);
        assert!(!launch.is_sensitive());
        assert!(!controls.is_sensitive() && !close.is_sensitive());
        enabled.set_active(true);
        assert!(launch.is_sensitive());
        assert!(launch.is_active());
        fail.set(true);
        launch.set_active(false);
        assert!(launch.is_active());
        assert!(stored.get().launch);
        refresh();
        assert!(prompts.is_active());
        assert!(controls.is_active() && close.is_active());
    }
}
