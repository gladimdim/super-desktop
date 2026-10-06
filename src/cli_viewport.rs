use crate::cli::{render_reply, Output};
use crate::cli_extended::{json_requested, seconds, send, valid_id, Options};
use crate::control::{Command, Reply, ViewportAction as Action};
pub(crate) fn run(args: &[String]) -> Option<Output> {
    if args.first().map(String::as_str) != Some("terminal")
        || args.get(1).map(String::as_str) != Some("viewport")
    {
        return None;
    }
    let build = || -> Result<(Command, Options), &'static str> {
        let action = args
            .get(2)
            .map(String::as_str)
            .ok_or("Choose viewport list/acquire/set/release.")?;
        let sizing = matches!(action, "acquire" | "set");
        let options = Options::parse(
            args,
            if sizing {
                &[
                    "--columns",
                    "--rows",
                    "--ttl",
                    "--expect-epoch",
                    "--expect-revision",
                    "--expect-pane-identity",
                ]
            } else {
                &[]
            },
            &[],
        )?;
        let words: Vec<_> = options.words.iter().map(String::as_str).collect();
        let id = words
            .get(3)
            .filter(|s| valid_id(s, 128))
            .ok_or("Use an exact card ID.")?
            .to_string();
        if words.as_slice() == ["terminal", "viewport", "list", id.as_str()] {
            return Ok((Command::Viewports { id }, options));
        }
        let (epoch, revision, pane) = if sizing {
            let (e, r, p) = options.guard()?;
            (Some(e), Some(r), Some(p))
        } else {
            (None, None, None)
        };
        let action = if sizing {
            let columns = options
                .required("--columns")?
                .parse::<u16>()
                .ok()
                .filter(|n| (20..=500).contains(n))
                .ok_or("Use 20..500 columns.")?;
            let rows = options
                .required("--rows")?
                .parse::<u16>()
                .ok()
                .filter(|n| (5..=300).contains(n))
                .ok_or("Use 5..300 rows.")?;
            let ttl = options
                .values
                .get("--ttl")
                .map_or(Ok(std::time::Duration::from_secs(60)), |v| seconds(v))?
                .as_secs();
            if ttl > 300 {
                return Err("TTL must be 1..300 seconds.");
            }
            match words.as_slice() {
                ["terminal", "viewport", "acquire", _] => Action::Acquire {
                    columns,
                    rows,
                    ttl: ttl as u16,
                },
                ["terminal", "viewport", "set", _, lease] if valid_id(lease, 80) => Action::Set {
                    lease: (*lease).into(),
                    columns,
                    rows,
                    ttl: ttl as u16,
                },
                _ => return Err("Acquire takes a card ID; set also requires an exact lease ID."),
            }
        } else {
            match words.as_slice() {
                ["terminal", "viewport", "release", _, lease] if valid_id(lease, 80) => {
                    Action::Release {
                        lease: (*lease).into(),
                    }
                }
                _ => return Err("Release requires card ID and exact lease ID."),
            }
        };
        Ok((
            Command::Viewport {
                id,
                action,
                expect_epoch: epoch,
                expect_revision: revision,
                expect_pane_identity: pane,
            },
            options,
        ))
    };
    Some(match build() {
        Ok((command, options)) => {
            let method = if command.is_mutation() {
                "terminal.viewport"
            } else {
                "terminal.viewport.list"
            };
            render_reply(send(command, &options, method), options.json)
        }
        Err(message) => render_reply(
            Reply::failure("", "invalid_arguments", message),
            json_requested(args),
        ),
    })
}
