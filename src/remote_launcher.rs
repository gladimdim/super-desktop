//! The native harness bar, driven by a remote host's own snapshot.
//!
//! The list is the host's: order, labels, logos and tooltips come from the same
//! helpers the local HUD uses, and the only harnesses offered are the ones the
//! host says it can run. A click sends one typed `createTerminal` command, and
//! the host owns the harness inventory, the sandbox flags and the working
//! folder, so this bar never names a command, a flag or a path of its own.
use crate::desktop_protocol::{CommandResult, WorkspaceCommand, WorkspaceSnapshot};
use crate::peer_client::{self, Peer};
use gtk4::{glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Display name and icon of one harness. One source of truth so the local HUD
/// and a remote host's bar cannot label the same harness differently.
pub fn harness_label(key: &str) -> (&'static str, &'static str) {
    match key {
        "antigravity" => ("Antigravity", "🌌"),
        "claude" => ("Claude", "⚡"),
        "codex" => ("Codex", "🤖"),
        "opencode" => ("OpenCode", "🔮"),
        "grok" => ("Grok", "🚀"),
        "reasonix" => ("Reasonix", "🧭"),
        "aider" => ("Aider", "🧠"),
        _ => ("Shell", "💻"),
    }
}

/// What launching this harness does. The flags are applied by the machine that
/// runs the harness, so the same wording is true on either side.
pub fn harness_tooltip(key: &str) -> &'static str {
    match key {
        "antigravity" => "Launch Antigravity CLI (--dangerously-skip-permissions)",
        "claude" => "Launch Claude Code (--dangerously-skip-permissions)",
        "codex" => "Launch OpenAI Codex (--dangerously-bypass-approvals-and-sandbox)",
        "opencode" => "Launch OpenCode (--auto)",
        "grok" => "Launch Grok CLI (--dangerously-skip-permissions)",
        "reasonix" => "Launch Reasonix (reasonix code, else npx -y reasonix code)",
        "aider" => "Launch Aider (--yes-always)",
        _ => "Launch Terminal Shell",
    }
}

/// Everything a launch needs from the last snapshot: the peer, the epoch the
/// command must name, and the folder that host publishes for new cards.
#[derive(Clone)]
struct Target {
    peer: Peer,
    epoch: String,
    workspace: String,
}

/// The harness buttons for the selected remote PC, in the host's own order.
pub struct RemoteLauncher {
    /// The centered launch group, styled and sized like the local one.
    pub widget: gtk4::Box,
    /// What the bar is doing: a launch in progress, why one was refused, or
    /// what this host offers. Never overwritten by a poll mid-launch.
    pub note: gtk4::Label,
    buttons: Vec<(String, gtk4::Button)>,
    /// Logo images, so a theme switch can swap them like the local bar does.
    images: Rc<RefCell<Vec<(gtk4::Image, String)>>>,
    target: RefCell<Option<Target>>,
    /// The host accepts commands, as its last capability answer said.
    writable: Cell<bool>,
    in_flight: Cell<bool>,
    /// A refusal is explained until the user tries again, not for the two
    /// seconds a poll would otherwise leave it on screen.
    failed: Cell<bool>,
    on_launched: Rc<dyn Fn()>,
}

impl RemoteLauncher {
    pub fn new(on_launched: Rc<dyn Fn()>) -> Rc<Self> {
        let widget = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        widget.add_css_class("hud-launchers");
        widget.set_halign(gtk4::Align::Center);
        widget.set_valign(gtk4::Align::Center);
        let note = gtk4::Label::new(None);
        note.add_css_class("term-preview-text");
        note.set_halign(gtk4::Align::Center);
        note.set_visible(false);

        let light_theme = crate::theme::current_theme().mode == "light";
        let mut buttons = Vec::new();
        let images = Rc::new(RefCell::new(Vec::new()));
        for key in crate::tmux::HARNESS_KEYS.iter().copied() {
            let (name, emoji) = harness_label(key);
            let button = gtk4::Button::new();
            button.add_css_class("hud-button");
            if let Some(logo) = crate::brand::logo_path(key, light_theme) {
                let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
                let image = gtk4::Image::from_file(&logo);
                image.set_pixel_size(crate::brand::BRAND_ICON_SIZE);
                row.append(&image);
                row.append(&gtk4::Label::new(Some(name)));
                button.set_child(Some(&row));
                images.borrow_mut().push((image, key.to_string()));
            } else {
                button.set_label(&format!("{emoji} {name}"));
            }
            // No usage card here: those numbers come from this PC's own
            // provider state and would describe the wrong machine.
            button.set_tooltip_text(Some(harness_tooltip(key)));
            button.set_visible(false);
            button.set_sensitive(false);
            buttons.push((key.to_string(), button.clone()));
            widget.append(&button);
        }

        let view = Rc::new(Self {
            widget,
            note,
            buttons,
            images,
            target: RefCell::new(None),
            writable: Cell::new(false),
            in_flight: Cell::new(false),
            failed: Cell::new(false),
            on_launched,
        });
        for (key, button) in &view.buttons {
            let key = key.clone();
            let weak = Rc::downgrade(&view);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.launch(&key);
                }
            });
        }
        view
    }

    /// Logo images for the theme-swap pass, like the local toolbar's.
    pub fn brand_images(&self) -> Vec<(gtk4::Image, String)> {
        self.images.borrow().clone()
    }

    /// Point the bar at what the selected PC just reported.
    ///
    /// `writable` is the host's own `workspace-layout-v1` answer: a host that
    /// does not accept commands is never offered as a launch target.
    pub fn apply(&self, peer: &Peer, snapshot: &WorkspaceSnapshot, writable: bool) {
        let local = &snapshot.local;
        *self.target.borrow_mut() = Some(Target {
            peer: peer.clone(),
            epoch: local.epoch.clone(),
            workspace: local.workspace.clone(),
        });
        self.writable.set(writable);
        let mut offered = 0;
        for (key, button) in &self.buttons {
            let listed = local
                .visible_harnesses
                .iter()
                .any(|visible| visible == key);
            offered += usize::from(listed);
            button.set_visible(listed);
        }
        self.paint_sensitivity();
        // A launch in flight, or one that was refused, owns this line until it
        // is resolved; otherwise it describes what the host offers.
        if self.in_flight.get() || self.failed.get() {
            return;
        }
        self.set_note(match (writable, offered) {
            (false, _) => "Update SUPER DESKTOP on that PC to launch harnesses there",
            (true, 0) => "That PC offers no harnesses to launch",
            _ => "",
        });
    }

    /// Forget the host: no snapshot, another PC, or a failed connection.
    pub fn clear(&self) {
        *self.target.borrow_mut() = None;
        self.writable.set(false);
        self.in_flight.set(false);
        self.failed.set(false);
        for (_, button) in &self.buttons {
            button.set_visible(false);
        }
        self.paint_sensitivity();
        self.set_note("");
    }

    fn set_note(&self, text: &str) {
        self.note.set_text(text);
        self.note.set_visible(!text.is_empty());
    }

    /// Explain a refused launch, and keep explaining it until the user tries
    /// again or leaves this PC.
    fn fail(&self, message: &str) {
        self.failed.set(true);
        self.set_note(message);
    }

    /// A launch is one command at a time: a click that is still in flight is
    /// not repeated, so a slow answer cannot make two cards for one intent.
    fn paint_sensitivity(&self) {
        let ready = self.writable.get() && !self.in_flight.get();
        for (_, button) in &self.buttons {
            button.set_sensitive(ready && button.is_visible());
        }
    }

    /// Whether this host currently offers `key`.
    #[cfg(test)]
    pub fn offered(&self, key: &str) -> bool {
        self.buttons
            .iter()
            .find(|(button_key, _)| button_key == key)
            .is_some_and(|(_, button)| button.is_visible())
    }

    #[cfg(test)]
    pub fn sensitive(&self, key: &str) -> bool {
        self.buttons
            .iter()
            .find(|(button_key, _)| button_key == key)
            .is_some_and(|(_, button)| button.is_sensitive())
    }

    /// Launch one harness on the selected PC.
    ///
    /// One command per click, never retried: the host deduplicates on the
    /// request id, so a repeat of the same click cannot make two cards, and an
    /// answer whose outcome is unknown is reported instead of guessed at.
    fn launch(self: &Rc<Self>, agent_key: &str) {
        let Some(target) = self.target.borrow().clone() else {
            return;
        };
        if self.in_flight.replace(true) {
            return;
        }
        let (name, _) = harness_label(agent_key);
        self.failed.set(false);
        self.paint_sensitivity();
        self.set_note(&format!("Launching {name} on that PC…"));
        let request = crate::desktop_protocol::CommandRequest {
            request_id: peer_client::next_request_id(),
            machine_id: target.peer.machine_id.clone(),
            expected_epoch: target.epoch.clone(),
            command: WorkspaceCommand::CreateTerminal {
                agent_type: agent_key.to_string(),
                workspace: target.workspace.clone(),
            },
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reply = gtk4::gio::spawn_blocking(move || {
                peer_client::command(&target.peer, &request)
            })
            .await;
            let Some(launcher) = weak.upgrade() else {
                return;
            };
            launcher.in_flight.set(false);
            launcher.paint_sensitivity();
            match reply {
                Ok(Ok(reply)) => {
                    match reply.result {
                        // The new card belongs to the host and is already in its
                        // next snapshot. The card appearing is the success the
                        // user sees, so this line goes back to describing that
                        // host.
                        CommandResult::Applied { .. } => launcher.set_note(""),
                        CommandResult::Conflict { .. } => {
                            launcher.fail("That PC changed · try launching again")
                        }
                        CommandResult::Rejected { error } => {
                            launcher.fail(launch_error(&error))
                        }
                    }
                    // A refusal usually means this view is behind that host
                    // (its folder or epoch moved on), and "try again" only works
                    // once the bar shows what it reports now. On success this is
                    // how the new card shows up at once.
                    (launcher.on_launched)();
                }
                Ok(Err(failure)) => launcher.fail(launch_error(failure.0)),
                Err(_) => launcher.fail("That PC did not answer · try again"),
            }
        });
    }
}

/// One place for the viewer-visible wording of a refused launch. Codes are the
/// protocol's own, so no host text is ever shown here.
fn launch_error(error: &str) -> &'static str {
    match error {
        "unsupported_harness" => "That PC does not offer this harness",
        "invalid_workspace" => "That PC's folder changed · try launching again",
        "unsupported_command" | "invalid_command" | "invalid_layout" => {
            "Update SUPER DESKTOP on that PC to launch harnesses there"
        }
        "epoch_changed" | "peer_identity_changed" => "That PC restarted · try again",
        "desktop_unavailable" | "desktop_not_ready" | "remote_desktop_unavailable" => {
            "That PC's desktop is not running"
        }
        "unknown_outcome" | "desktop_timeout" => {
            "Launch status unknown · check that PC and refresh"
        }
        "terminal_unavailable" => "That PC could not start it · check its workspace",
        _ => "Could not launch on that PC",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_harness_the_protocol_can_carry_has_a_label_and_a_tooltip() {
        assert_eq!(harness_label("claude"), ("Claude", "⚡"));
        for key in crate::tmux::HARNESS_KEYS {
            let (name, icon) = harness_label(key);
            assert!(!name.is_empty() && !icon.is_empty(), "{key}");
            assert!(harness_tooltip(key).starts_with("Launch"), "{key}");
        }
        // The bar shows what the host offers, so an unknown key is still
        // labelled rather than left blank.
        assert_eq!(harness_label("shell"), ("Shell", "💻"));
    }

    #[test]
    fn a_remote_bar_offers_exactly_what_the_host_offers() {
        crate::gtk_test::run_in_child_process("remote_launcher::tests::bar_inner");
    }

    #[test]
    fn bar_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        let launcher = RemoteLauncher::new(Rc::new(|| {}));
        // Nothing is offered before the host has said what it can run.
        assert!(!launcher.offered("shell") && !launcher.sensitive("shell"));

        let peer = peer_client::test_peer('a');
        let mut snapshot = crate::remote_workspace::fixture();
        snapshot.local.visible_harnesses = vec!["shell".into()];
        launcher.apply(&peer, &snapshot, true);
        assert!(launcher.offered("shell"));
        assert!(launcher.sensitive("shell"));
        // A harness the host does not list is not offered at all, and no other
        // host's list leaks into this bar.
        assert!(!launcher.offered("claude"));
        assert!(!launcher.offered("aider"));

        // A host that does not accept commands keeps the bar inert: the drop
        // and the launch are the same capability.
        launcher.apply(&peer, &snapshot, false);
        assert!(launcher.offered("shell") && !launcher.sensitive("shell"));

        // A refusal stays visible until the user tries again, instead of being
        // wiped by the next poll.
        launcher.fail("That PC does not offer this harness");
        assert!(launcher.note.text().contains("does not offer"));
        launcher.apply(&peer, &snapshot, true);
        assert!(launcher.note.text().contains("does not offer"));

        // Leaving the PC clears it, so the next host cannot be launched into
        // with the previous host's state.
        launcher.clear();
        assert!(!launcher.offered("shell") && !launcher.sensitive("shell"));
        assert!(launcher.note.text().is_empty() && !launcher.note.is_visible());
    }

    #[test]
    fn refusals_are_explained_in_the_viewer_s_own_words() {
        assert!(launch_error("unsupported_harness").contains("does not offer"));
        assert!(launch_error("invalid_workspace").contains("folder changed"));
        assert!(launch_error("unknown_outcome").contains("unknown"));
        // A code from nowhere still gets a sentence, never the raw code.
        assert_eq!(launch_error("shadowy_code"), "Could not launch on that PC");
        assert!(!launch_error("desktop_unavailable").contains("desktop_unavailable"));
    }
}
