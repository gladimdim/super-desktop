//! What the viewer tells the user after one command sent to another PC.
//!
//! Every remote control ends in one typed command, and every command ends in
//! exactly one of: applied, refused as a conflict (with the host's own
//! geometry), refused for a stable reason, or failed without an answer. This
//! module turns that result into what the chrome shows — a brief, non-blocking
//! notice on the card (or under the top bar for workspace commands) — and into
//! what happens to the card's geometry. It is pure, so the decision is tested
//! without GTK or a network.
//!
//! Nothing here retries. An answer whose outcome is unknown (a timeout after
//! the request left, a host whose owner never answered, a broken reply) is
//! reported as unknown and the view refreshes from the host instead, exactly as
//! the protocol requires: a command is never replayed on the user's behalf.
use crate::desktop_protocol::{CommandReply, CommandResult, WorkspaceCommand};
use std::time::Duration;

/// Transport code for a command whose request may have reached the host but
/// whose answer did not come back (see `peer_client::command`).
pub const OUTCOME_UNKNOWN: &str = "command_outcome_unknown";

/// Which card command a result belongs to. The wording differs for a close,
/// because "not applied" reads wrong for a card that is still on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardCommand {
    /// Move, resize or iconify (`setLayout`).
    Layout,
    /// Maximize/restore (`setExpanded`).
    Expand,
    /// `closeTerminal`.
    Close,
}

impl CardCommand {
    pub fn of(command: &WorkspaceCommand) -> Option<Self> {
        match command {
            WorkspaceCommand::SetLayout { .. } => Some(Self::Layout),
            WorkspaceCommand::SetExpanded { .. } => Some(Self::Expand),
            WorkspaceCommand::CloseTerminal { .. } => Some(Self::Close),
            _ => None,
        }
    }
}

/// Workspace-level commands, reported under the top bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceAction {
    /// A harness button (`createTerminal`).
    Create,
    /// The folder picker (`setWorkspace`).
    Folder,
}

/// How a notice is coloured. Each tone maps to a theme-driven CSS class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// The host decided differently and the view now shows its state.
    Info,
    /// Nothing is wrong with the connection, but the change did not happen or
    /// its result is not known.
    Warning,
    /// The host could not be reached or refused the command outright.
    Error,
}

impl Tone {
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Info => "term-notice-info",
            Self::Warning => "term-notice-warning",
            Self::Error => "term-notice-error",
        }
    }
}

pub const TONE_CLASSES: [&str; 3] = ["term-notice-info", "term-notice-warning", "term-notice-error"];

/// A brief line in the chrome that dismisses itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: &'static str,
    pub tone: Tone,
    pub duration: Duration,
}

const INFO: Duration = Duration::from_secs(4);
const FAILURE: Duration = Duration::from_secs(6);
/// An unknown outcome asks the user to check before acting again, so it stays
/// long enough to be read after the refresh lands.
const UNKNOWN: Duration = Duration::from_secs(8);

fn notice(text: &'static str, tone: Tone) -> Notice {
    let duration = match tone {
        Tone::Info => INFO,
        Tone::Warning | Tone::Error => FAILURE,
    };
    Notice { text, tone, duration }
}

/// What happens to the card after the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Geometry {
    /// Take the host's published state (an applied command).
    Adopt,
    /// Take the host's published state and animate the card to it: the host
    /// refused the edit, so the card visibly returns to where the host has it.
    SnapToHost,
    /// No host state came back: drop the viewer's optimistic drop and redraw
    /// the last snapshot, so nothing on screen claims a change that may not
    /// exist.
    Revert,
}

/// The whole decision for one card command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feedback {
    pub notice: Option<Notice>,
    pub geometry: Geometry,
    /// Ask the host for a fresh snapshot now rather than waiting out the poll.
    pub refresh: bool,
}

/// One command's result, without the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    Applied,
    Conflict,
    /// The host answered with one of the stable refusal codes.
    Rejected(&'a str),
    /// No typed answer: a transport or validation code from `peer_client`.
    Failed(&'a str),
}

impl<'a> Outcome<'a> {
    pub fn of_reply(reply: &'a CommandReply) -> Self {
        match &reply.result {
            CommandResult::Applied { .. } => Self::Applied,
            CommandResult::Conflict { .. } => Self::Conflict,
            CommandResult::Rejected { error } => Self::Rejected(error),
        }
    }

    /// A command's full result, including a worker that never returned (its
    /// request may already have left, so that is an unknown outcome too).
    pub fn of<E>(
        result: &'a Result<Result<CommandReply, crate::peer_client::PeerError>, E>,
    ) -> Self {
        match result {
            Ok(Ok(reply)) => Self::of_reply(reply),
            Ok(Err(error)) => Self::Failed(error.0),
            Err(_) => Self::Failed(OUTCOME_UNKNOWN),
        }
    }
}

/// Why a command did not take effect, grouped by what the user can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    /// The request may have been applied: check before retrying.
    Unknown,
    /// The host never received it.
    Unreachable,
    Restarted,
    Gone,
    Expanded,
    DesktopDown,
    Outdated,
    Pairing,
    Identity,
    FolderGone,
    Other,
}

fn reason(code: &str) -> Reason {
    match code {
        // `unknown_outcome` is the host saying the same request id never got an
        // answer from its owner; `desktop_timeout` is that owner timing out
        // while the request was in its hands. A broken or mismatched reply may
        // also follow an applied change.
        OUTCOME_UNKNOWN
        | "unknown_outcome"
        | "desktop_timeout"
        | "peer_response_incomplete"
        | "invalid_desktop_response"
        | "invalid_peer_response" => Reason::Unknown,
        "connection_failed_or_pin_mismatch" | "peer_endpoint_unavailable" => Reason::Unreachable,
        "epoch_changed" => Reason::Restarted,
        "unknown_card" => Reason::Gone,
        "terminal_expanded" => Reason::Expanded,
        "desktop_unavailable" | "desktop_not_ready" | "remote_desktop_unavailable" => {
            Reason::DesktopDown
        }
        "unsupported_command" | "invalid_command" | "invalid_layout" => Reason::Outdated,
        "peer_revoked_or_expired" | "peer_not_found" => Reason::Pairing,
        "wrong_machine" | "peer_identity_changed" => Reason::Identity,
        "invalid_workspace" => Reason::FolderGone,
        _ => Reason::Other,
    }
}

/// The notice for a change that did not happen (or may have), shared by card
/// layout/expand commands and the folder picker.
fn change_failed(code: &str) -> Notice {
    match reason(code) {
        Reason::Unknown => Notice {
            text: "Result unknown · check before retrying",
            tone: Tone::Warning,
            duration: UNKNOWN,
        },
        Reason::Unreachable => notice("Cannot reach that PC · change not applied", Tone::Error),
        Reason::Restarted => notice("That PC restarted · change not applied", Tone::Warning),
        Reason::Gone => notice("Already closed on that PC", Tone::Info),
        Reason::Expanded => notice("Expanded on that PC · restore it first", Tone::Warning),
        Reason::DesktopDown => {
            notice("That PC's desktop is not running · change not applied", Tone::Error)
        }
        Reason::Outdated => notice("Update SUPER DESKTOP on that PC", Tone::Error),
        Reason::Pairing => notice("Pairing required · change not applied", Tone::Error),
        Reason::Identity => notice("That PC's identity changed · change not applied", Tone::Error),
        Reason::FolderGone => notice("That PC no longer offers that folder", Tone::Warning),
        Reason::Other => notice("That PC refused the change", Tone::Error),
    }
}

/// Feedback for a card's own command.
pub fn for_card(command: CardCommand, outcome: Outcome<'_>) -> Feedback {
    match outcome {
        Outcome::Applied => Feedback {
            notice: None,
            geometry: Geometry::Adopt,
            refresh: true,
        },
        // The conflict carries the host's geometry; the card animates to it.
        Outcome::Conflict => Feedback {
            notice: Some(notice(
                match command {
                    CardCommand::Close => "Changed on that PC · not closed",
                    CardCommand::Layout | CardCommand::Expand => {
                        "Changed on that PC · showing its layout"
                    }
                },
                Tone::Info,
            )),
            geometry: Geometry::SnapToHost,
            refresh: true,
        },
        Outcome::Rejected(code) | Outcome::Failed(code) => {
            let answered = matches!(outcome, Outcome::Rejected(_));
            let reason = reason(code);
            let mut shown = change_failed(code);
            if command == CardCommand::Close {
                shown.text = match reason {
                    Reason::Unknown => shown.text,
                    Reason::Gone => shown.text,
                    Reason::Unreachable => "Cannot reach that PC · not closed",
                    Reason::Restarted => "That PC restarted · not closed",
                    Reason::DesktopDown => "That PC's desktop is not running · not closed",
                    Reason::Pairing => "Pairing required · not closed",
                    Reason::Identity => "That PC's identity changed · not closed",
                    Reason::Outdated => shown.text,
                    Reason::Expanded | Reason::FolderGone | Reason::Other => {
                        "That PC refused to close it"
                    }
                };
            }
            Feedback {
                notice: Some(shown),
                geometry: Geometry::Revert,
                // Any typed refusal means this view is behind the host, and an
                // unknown outcome is resolved by looking, never by retrying. A
                // host that cannot be reached is left to the regular poll.
                refresh: answered || reason == Reason::Unknown,
            }
        }
    }
}

/// Feedback for a workspace command, shown under the top bar. `None` means
/// nothing needs saying: the new card, or the new folder, is the answer.
pub fn for_workspace(action: WorkspaceAction, outcome: Outcome<'_>) -> Option<Notice> {
    match (action, outcome) {
        (_, Outcome::Applied) => None,
        (WorkspaceAction::Create, Outcome::Conflict) => {
            Some(notice("That PC changed · try launching again", Tone::Info))
        }
        (WorkspaceAction::Folder, Outcome::Conflict) => {
            Some(notice("Folder changed on that PC · showing its folder", Tone::Info))
        }
        (WorkspaceAction::Create, Outcome::Rejected(code) | Outcome::Failed(code)) => {
            let text = crate::harness_bar::launch_error(code);
            Some(match reason(code) {
                Reason::Unknown => Notice {
                    text,
                    tone: Tone::Warning,
                    duration: UNKNOWN,
                },
                Reason::Restarted | Reason::FolderGone => notice(text, Tone::Warning),
                _ => notice(text, Tone::Error),
            })
        }
        (WorkspaceAction::Folder, Outcome::Rejected(code) | Outcome::Failed(code)) => {
            Some(change_failed(code))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(feedback: Feedback) -> &'static str {
        feedback.notice.expect("a notice").text
    }

    #[test]
    fn an_applied_command_says_nothing_and_adopts_the_host_state() {
        for command in [CardCommand::Layout, CardCommand::Expand, CardCommand::Close] {
            let feedback = for_card(command, Outcome::Applied);
            assert_eq!(feedback.notice, None);
            assert_eq!(feedback.geometry, Geometry::Adopt);
        }
        assert_eq!(for_workspace(WorkspaceAction::Create, Outcome::Applied), None);
        assert_eq!(for_workspace(WorkspaceAction::Folder, Outcome::Applied), None);
    }

    #[test]
    fn a_conflict_snaps_to_the_host_and_says_so() {
        let feedback = for_card(CardCommand::Layout, Outcome::Conflict);
        assert_eq!(feedback.geometry, Geometry::SnapToHost);
        assert_eq!(text(feedback), "Changed on that PC · showing its layout");
        assert_eq!(feedback.notice.unwrap().tone, Tone::Info);
        assert_eq!(
            text(for_card(CardCommand::Close, Outcome::Conflict)),
            "Changed on that PC · not closed"
        );
        let folder = for_workspace(WorkspaceAction::Folder, Outcome::Conflict).unwrap();
        assert_eq!(folder.text, "Folder changed on that PC · showing its folder");
        let launch = for_workspace(WorkspaceAction::Create, Outcome::Conflict).unwrap();
        assert_eq!(launch.text, "That PC changed · try launching again");
    }

    #[test]
    fn an_unknown_outcome_refreshes_and_never_claims_a_result() {
        for code in [
            OUTCOME_UNKNOWN,
            "unknown_outcome",
            "desktop_timeout",
            "peer_response_incomplete",
            "invalid_desktop_response",
        ] {
            for outcome in [Outcome::Rejected(code), Outcome::Failed(code)] {
                let feedback = for_card(CardCommand::Layout, outcome);
                assert_eq!(text(feedback), "Result unknown · check before retrying", "{code}");
                assert_eq!(feedback.geometry, Geometry::Revert);
                assert!(feedback.refresh, "{code}");
                assert_eq!(feedback.notice.unwrap().duration, UNKNOWN);
                // A close whose outcome is unknown is not reported as closed or
                // as refused.
                assert_eq!(
                    text(for_card(CardCommand::Close, outcome)),
                    "Result unknown · check before retrying"
                );
            }
        }
        let launch = for_workspace(WorkspaceAction::Create, Outcome::Failed(OUTCOME_UNKNOWN)).unwrap();
        assert_eq!(launch.text, "Launch status unknown · check that PC and refresh");
        assert_eq!(launch.duration, UNKNOWN);
        let folder = for_workspace(WorkspaceAction::Folder, Outcome::Failed("desktop_timeout")).unwrap();
        assert_eq!(folder.text, "Result unknown · check before retrying");
    }

    #[test]
    fn an_unreachable_host_reverts_without_a_refresh() {
        let feedback = for_card(
            CardCommand::Layout,
            Outcome::Failed("connection_failed_or_pin_mismatch"),
        );
        assert_eq!(text(feedback), "Cannot reach that PC · change not applied");
        assert_eq!(feedback.notice.unwrap().tone, Tone::Error);
        assert_eq!(feedback.geometry, Geometry::Revert);
        // The regular poll reports the lost connection; nothing to ask for.
        assert!(!feedback.refresh);
        assert_eq!(
            text(for_card(
                CardCommand::Close,
                Outcome::Failed("connection_failed_or_pin_mismatch")
            )),
            "Cannot reach that PC · not closed"
        );
    }

    #[test]
    fn typed_refusals_are_explained_and_refresh_the_view() {
        let cases = [
            ("epoch_changed", "That PC restarted · change not applied"),
            ("unknown_card", "Already closed on that PC"),
            ("terminal_expanded", "Expanded on that PC · restore it first"),
            ("desktop_unavailable", "That PC's desktop is not running · change not applied"),
            ("unsupported_command", "Update SUPER DESKTOP on that PC"),
            ("wrong_machine", "That PC's identity changed · change not applied"),
            ("something_new", "That PC refused the change"),
        ];
        for (code, expected) in cases {
            let feedback = for_card(CardCommand::Expand, Outcome::Rejected(code));
            assert_eq!(text(feedback), expected, "{code}");
            assert_eq!(feedback.geometry, Geometry::Revert);
            assert!(feedback.refresh, "{code}");
        }
        assert_eq!(
            text(for_card(CardCommand::Close, Outcome::Rejected("epoch_changed"))),
            "That PC restarted · not closed"
        );
        assert_eq!(
            for_workspace(WorkspaceAction::Folder, Outcome::Rejected("invalid_workspace"))
                .unwrap()
                .text,
            "That PC no longer offers that folder"
        );
        assert_eq!(
            for_workspace(WorkspaceAction::Create, Outcome::Rejected("unsupported_harness"))
                .unwrap()
                .text,
            "That PC does not offer this harness"
        );
    }

    #[test]
    fn every_notice_dismisses_itself_and_has_a_theme_class() {
        let codes = crate::desktop_protocol::COMMAND_ERRORS
            .iter()
            .copied()
            .chain([OUTCOME_UNKNOWN, "connection_failed_or_pin_mismatch", "peer_revoked_or_expired"]);
        for code in codes {
            for command in [CardCommand::Layout, CardCommand::Expand, CardCommand::Close] {
                let shown = for_card(command, Outcome::Rejected(code)).notice.unwrap();
                assert!(shown.duration > Duration::ZERO && shown.duration <= UNKNOWN);
                assert!(TONE_CLASSES.contains(&shown.tone.css_class()));
                assert!(!shown.text.is_empty());
            }
        }
    }

    #[test]
    fn a_worker_that_never_returned_is_an_unknown_outcome() {
        let lost: Result<Result<CommandReply, crate::peer_client::PeerError>, ()> = Err(());
        assert_eq!(Outcome::of(&lost), Outcome::Failed(OUTCOME_UNKNOWN));
        let refused: Result<_, ()> = Ok(Err(crate::peer_client::PeerError("epoch_changed")));
        assert_eq!(Outcome::of(&refused), Outcome::Failed("epoch_changed"));
    }

    #[test]
    fn only_card_commands_have_card_feedback() {
        use crate::desktop_protocol::WorkspaceCommand;
        let close = WorkspaceCommand::CloseTerminal {
            card_id: "card-one".into(),
            expected_revision: 1,
        };
        assert_eq!(CardCommand::of(&close), Some(CardCommand::Close));
        let folder = WorkspaceCommand::SetWorkspace {
            workspace: "/project".into(),
            expected_revision: 1,
        };
        assert_eq!(CardCommand::of(&folder), None);
    }
}
