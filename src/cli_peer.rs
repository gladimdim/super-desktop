use crate::cli::{render_reply, Output};
use crate::cli_extended::{json_requested, send, Options};
use crate::control::{Command, Reply};
pub(crate) fn run(args: &[String]) -> Option<Output> {
    if args.first()?.as_str() != "peer" {
        return None;
    }
    let build = || -> Result<Output, &'static str> {
        let action = args
            .get(1)
            .ok_or("Choose peer list/inspect/add/pairing/workspace/command/forget.")?;
        let o = Options::parse(
            args,
            if action == "add" {
                &["--file", "--host", "--port", "--name"][..]
            } else if action == "command" {
                &["--file"][..]
            } else {
                &[]
            },
            if action == "add" {
                &["--stdin"][..]
            } else if action == "command" {
                &["--stdin", "--allow-peer-mutation"][..]
            } else {
                &[]
            },
        )?;
        let (command, method) = match o
            .words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["peer", "add"] => (
                Command::PeerAdd {
                    invitation: o.text(8192)?,
                    host: o.values.get("--host").cloned(),
                    port: o
                        .values
                        .get("--port")
                        .map(|p| {
                            p.parse::<u16>()
                                .ok()
                                .filter(|p| *p > 0)
                                .ok_or("Use a port from 1 to 65535.")
                        })
                        .transpose()?,
                    name: o.values.get("--name").cloned(),
                },
                "peer.add",
            ),
            ["peer", "pairing", id] => (Command::PeerPairing { id: (*id).into() }, "peer.pairing"),
            ["peer", "list"] => (
                Command::PeerRead {
                    id: None,
                    query: "list".into(),
                },
                "peer.read",
            ),
            ["peer", query @ ("inspect" | "workspace"), id] => (
                Command::PeerRead {
                    id: Some((*id).into()),
                    query: (*query).into(),
                },
                "peer.read",
            ),
            ["peer", "forget", id] => (Command::PeerForget { id: (*id).into() }, "peer.forget"),
            ["peer", "command", id] => (
                Command::PeerCommand {
                    id: (*id).into(),
                    document: serde_json::from_str(&o.text(8192)?)
                        .map_err(|_| "Input must be a typed JSON peer CommandRequest envelope.")?,
                    allow_mutation: o.flags.contains("--allow-peer-mutation"),
                },
                "peer.command",
            ),
            _ => return Err("Unknown peer command or extra positional arguments."),
        };
        Ok(render_reply(send(command, &o, method), o.json))
    };
    Some(build().unwrap_or_else(|m| {
        render_reply(
            Reply::failure("", "invalid_arguments", m),
            json_requested(args),
        )
    }))
}
