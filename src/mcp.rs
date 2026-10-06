//! Local read-only MCP adapter; all desktop access uses the owner control socket.
use crate::control::{self, Command};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const MAX_LINE: usize = 64 * 1024;
const TOOLS: &[(&str, &str, &str)] = &[
    ("app_status", "Inspect daemon readiness, visibility and card counts.", ""),
    ("capabilities", "Inspect supported local control operations and limits.", ""),
    ("list_terminals", "List saved terminal cards and launch directories. Does not observe process liveness or read output.", ""),
    ("inspect_terminal", "Inspect a saved card by exact ID. Geometry is saved logical pixels; runtime liveness is not observed.", "id"),
    ("list_harnesses", "List configured launcher types. Optional all includes unavailable launchers. Detection does not run or install programs.", "all"),
    ("inspect_harness", "Inspect a launcher by exact ID. Availability does not prove authentication or a verified security policy.", "id"),
];

fn tools() -> Value {
    Value::Array(TOOLS.iter().map(|(name, description, arg)| {
        let (properties, required) = match *arg {
            "id" => (json!({"id":{"type":"string","minLength":1}}), json!(["id"])),
            "all" => (json!({"all":{"type":"boolean","default":false}}), json!([])),
            _ => (json!({}), json!([])),
        };
        json!({"name":name,"description":description,
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}})
    }).collect())
}

fn command(params: &Value) -> Result<Command, &'static str> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("Tool name required")?;
    let (_, _, arg) = TOOLS
        .iter()
        .find(|(n, _, _)| *n == name)
        .ok_or("Unknown tool")?;
    let empty = json!({});
    let args = params
        .get("arguments")
        .unwrap_or(&empty)
        .as_object()
        .ok_or("Arguments must be an object")?;
    if args.keys().any(|key| key != arg) {
        return Err("Unknown tool argument");
    }
    let id = || {
        args.get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or("Nonempty id required")
    };
    Ok(match name {
        "app_status" => Command::Status {},
        "capabilities" => Command::Capabilities {},
        "list_terminals" => Command::Terminals {},
        "inspect_terminal" => Command::Terminal { id: id()? },
        "inspect_harness" => Command::Harness { id: id()? },
        "list_harnesses" => Command::Harnesses {
            all: match args.get("all") {
                None => false,
                Some(v) => v.as_bool().ok_or("all must be boolean")?,
            },
        },
        _ => unreachable!(),
    })
}

#[derive(Default)]
struct Session {
    initialized: bool,
    ready: bool,
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
impl Session {
    fn handle(&mut self, msg: Value, call: &mut impl FnMut(Command) -> Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str);
        if !msg.is_object()
            || msg["jsonrpc"] != "2.0"
            || method.is_none()
            || id
                .as_ref()
                .is_some_and(|v| !(v.is_string() || v.is_i64() || v.is_u64()))
        {
            return Some(error(Value::Null, -32600, "Invalid request"));
        }
        if id.is_none() {
            if method == Some("notifications/initialized") && self.initialized {
                self.ready = true;
            }
            return None;
        }
        let id = id.unwrap();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result = match method.unwrap() {
            "ping" => json!({}),
            "initialize" => {
                if self.initialized {
                    return Some(error(id, -32600, "Already initialized"));
                }
                if !params["protocolVersion"].is_string()
                    || !params["capabilities"].is_object()
                    || !params["clientInfo"]["name"].is_string()
                    || !params["clientInfo"]["version"].is_string()
                {
                    return Some(error(id, -32602, "Invalid initialization parameters"));
                }
                self.initialized = true;
                let version = if params["protocolVersion"] == "2025-03-26" {
                    "2025-03-26"
                } else {
                    "2025-11-25"
                };
                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"super-desktop","version":env!("CARGO_PKG_VERSION")},"instructions":"Read-only local desktop tools. Saved cards do not prove running sessions. Launch paths are private user data. Tools never start the daemon."})
            }
            _ if !self.ready => {
                return Some(error(
                    id,
                    -32600,
                    "Initialize and send notifications/initialized first",
                ))
            }
            "tools/list" => {
                if !params.is_object() || params.get("cursor").is_some() {
                    return Some(error(id, -32602, "Invalid list parameters"));
                }
                json!({"tools":tools()})
            }
            "tools/call" => match command(&params) {
                Err(message) => return Some(error(id, -32602, message)),
                Ok(command) => {
                    let reply = call(command);
                    json!({"content":[{"type":"text","text":reply.to_string()}],"structuredContent":reply,"isError":reply["ok"] != true})
                }
            },
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

fn run(
    mut input: impl BufRead,
    mut output: impl Write,
    mut call: impl FnMut(Command) -> Value,
) -> io::Result<()> {
    let mut session = Session::default();
    loop {
        let mut line = Vec::new();
        let n =
            std::io::Read::take(&mut input, (MAX_LINE + 1) as u64).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        if n > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message too large",
            ));
        }
        let reply = match serde_json::from_slice(&line) {
            Ok(msg) => session.handle(msg, &mut call),
            Err(_) => Some(error(Value::Null, -32700, "Parse error")),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

pub fn serve() -> i32 {
    let result = run(io::stdin().lock(), io::stdout().lock(), |command| {
        let reply = match control::new_request(command) {
            Ok(request) => control::request_at(&control::runtime_dir(), &request),
            Err(_) => {
                control::Reply::failure("mcp", "unavailable", "Cannot create local control request")
            }
        };
        serde_json::to_value(reply).expect("control reply serializes")
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("SUPER DESKTOP MCP: {e}");
            8
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn initialized() -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
    }
    #[test]
    fn mcp_handshake_discovery_and_call() {
        let messages = [
            initialized(),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"inspect_terminal","arguments":{"id":"card"}}}),
        ];
        let input = messages
            .iter()
            .map(|m| format!("{m}\n"))
            .collect::<String>();
        let mut output = Vec::new();
        let mut calls = 0;
        run(input.as_bytes(), &mut output, |c| {
            assert!(matches!(c, Command::Terminal { id } if id == "card"));
            calls += 1;
            json!({"ok":true,"data":{"id":"card"}})
        })
        .unwrap();
        let replies: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(replies.len(), 3);
        assert_eq!(calls, 1);
        assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 6);
        assert_eq!(replies[2]["result"]["isError"], false);
    }
    #[test]
    fn mcp_rejects_mutations_and_bad_arguments() {
        for params in [
            json!({"name":"harness_launch"}),
            json!({"name":"app_status","arguments":{"evil":true}}),
            json!({"name":"inspect_terminal","arguments":{}}),
            json!({"name":"list_harnesses","arguments":{"all":"yes"}}),
        ] {
            assert!(command(&params).is_err());
        }
        let mut s = Session::default();
        assert_eq!(
            s.handle(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                &mut |_| panic!()
            )
            .unwrap()["error"]["code"],
            -32600
        );
    }
    #[test]
    fn mcp_parse_errors_limits_and_unavailable() {
        let mut output = Vec::new();
        run(&b"broken\n"[..], &mut output, |_| panic!()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["error"]["code"],
            -32700
        );
        assert!(run(
            vec![b'x'; MAX_LINE + 1].as_slice(),
            Vec::new(),
            |_| panic!()
        )
        .is_err());
        let mut s = Session {
            initialized: true,
            ready: true,
        };
        let r = s.handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"app_status"}}), &mut |_| json!({"ok":false,"error":{"code":"unavailable"}})).unwrap();
        assert_eq!(r["result"]["isError"], true);
    }
}
