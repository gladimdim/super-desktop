use crate::cli::{render_reply, Output};
use crate::cli_extended::{json_requested, send, Options};
use crate::control::{Command, Reply};

pub(crate) fn run(args: &[String]) -> Option<Output> {
    if args.first()?.as_str() != "connection" {
        return None;
    }
    let parse = || -> Result<Output, &'static str> {
        let action = args.get(1).map(String::as_str).unwrap_or("");
        let o = Options::parse(
            args,
            match action {
                "invite" => &["--output"][..],
                "approve" | "reject" => &["--code"][..],
                _ => &[],
            },
            if action == "approve" {
                &["--allow-access"]
            } else {
                &[]
            },
        )?;
        let words: Vec<_> = o.words.iter().map(String::as_str).collect();
        let (command, method) = match words.as_slice() {
            ["connection", "list" | "pending"] => (
                Command::ConnectionRead {
                    pending: action == "pending",
                },
                "connection.read",
            ),
            ["connection", "invite"] => (
                Command::ConnectionInvite {
                    output: o.required("--output")?,
                },
                "connection.invite",
            ),
            ["connection", "approve" | "reject", id] => (
                Command::ConnectionDecide {
                    id: (*id).into(),
                    code: o.required("--code")?,
                    approve: action == "approve",
                    allow_access: o.flags.contains("--allow-access"),
                },
                "connection.decide",
            ),
            ["connection", "revoke", id] => (
                Command::ConnectionRevoke { id: (*id).into() },
                "connection.revoke",
            ),
            _ => return Err(
                "Use connection list/invite/pending/approve/reject/revoke; see help connection.",
            ),
        };
        Ok(render_reply(send(command, &o, method), o.json))
    };
    Some(parse().unwrap_or_else(|m| {
        render_reply(
            Reply::failure("", "invalid_arguments", m),
            json_requested(args),
        )
    }))
}
