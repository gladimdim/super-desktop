use crate::cli::{render_reply, Output};
use crate::cli_extended::{respond, send, Options};
use crate::control::Command;

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
        let command = match words.as_slice() {
            ["connection", "list" | "pending"] => Command::ConnectionRead {
                pending: action == "pending",
            },
            ["connection", "invite"] => Command::ConnectionInvite {
                output: o.required("--output")?,
            },
            ["connection", "approve" | "reject", id] => Command::ConnectionDecide {
                id: (*id).into(),
                code: o.required("--code")?,
                approve: action == "approve",
                allow_access: o.flags.contains("--allow-access"),
            },
            ["connection", "revoke", id] => Command::ConnectionRevoke { id: (*id).into() },
            _ => return Err(
                "Use connection list/invite/pending/approve/reject/revoke; see help connection.",
            ),
        };
        Ok(render_reply(send(command, &o), o.json))
    };
    respond(args, parse)
}
