use crate::cli::{render_reply, Output};
use crate::cli_extended::{respond, send, Options};
use crate::control::{Command, TelegramAction as Action};

pub(crate) fn run(args: &[String]) -> Option<Output> {
    if args.first()?.as_str() != "telegram" {
        return None;
    }
    respond(args, || {
        let set = args.get(1).is_some_and(|s| s == "set");
        let options = Options::parse(
            args,
            if set { &["--file"] } else { &[] },
            if set {
                &["--stdin"]
            } else if args.get(1).is_some_and(|a| a == "setup-mini-app") {
                &["--allow-serve"]
            } else {
                &[]
            },
        )?;
        let words: Vec<_> = options.words.iter().map(String::as_str).collect();
        let command = match words.as_slice() {
            ["telegram", action @ ("status" | "invitation")] => Command::TelegramRead {
                invitation: *action == "invitation",
            },
            _ => Command::TelegramEdit {
                action: match words.as_slice() {
                    ["telegram", "setup-mini-app"] if options.flags.contains("--allow-serve") => {
                        Action::SetupMiniApp { confirm: true }
                    }
                    ["telegram", "invite"] => Action::Invite,
                    ["telegram", "approve", id, code] => Action::Approve {
                        id: (*id).into(),
                        code: (*code).into(),
                    },
                    ["telegram", "reject", id, code] => Action::Reject {
                        id: (*id).into(),
                        code: (*code).into(),
                    },
                    ["telegram", "revoke"] => Action::Revoke,
                    ["telegram", "clear-rejected"] => Action::ClearRejected,
                    ["telegram", "unlink-forum"] => Action::UnlinkForum,
                    ["telegram", "sync-topics"] => Action::SyncTopics,
                    ["telegram", "set", key] => Action::Settings {
                        key: (*key).into(),
                        value: serde_json::from_str(&options.text(4096)?)
                            .map_err(|_| "Supply one JSON setting value.")?,
                    },
                    _ => return Err("Unknown Telegram command or incorrect arguments."),
                },
            },
        };
        Ok(render_reply(send(command, &options), options.json))
    })
}
