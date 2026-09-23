use crate::{
    peer_client::{self, Endpoint, Invitation, Pairing, PeerError, Result},
    peer_store::PeerStore,
    peer_terminal::{Event, TerminalStream},
};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::time::{Duration, Instant};

pub fn run(action: &str, args: &[String]) -> Result<()> {
    match action {
        "peer-add" => add(args),
        "peer-list" if args.is_empty() => {
            let peers = PeerStore::default_store()?.peers()?;
            output(&peers.iter().map(|p| p.summary()).collect::<Vec<_>>())
        }
        "peer-workspace" if args.len() == 1 => {
            let peer = PeerStore::default_store()?.get(&args[0])?;
            output(&peer_client::workspace(&peer)?)
        }
        "peer-forget" if args.len() == 1 => {
            PeerStore::default_store()?.forget(&args[0])?;
            output(&serde_json::json!({"forgotten":args[0]}))
        }
        "peer-attach" if !args.is_empty() => attach(args),
        _ => Err(PeerError("usage: peer-add [--host ADDRESS] [--port PORT] [--name LABEL] | peer-list | peer-workspace ID | peer-attach ID CARD [--seconds N] | peer-forget ID")),
    }
}

/// Stream one host console to stdout, and type stdin back when it is a pipe.
///
/// A debugging and verification tool: it exercises the same pinned WSS attach
/// path the remote panel uses, so a two-PC problem can be narrowed to the
/// transport or to the viewer. Terminal output is written unchanged, which
/// means a real terminal shows exactly the host's colors. An interactive
/// terminal keeps stdin for itself; a pipe (the smoke test, a here-string)
/// is written to the host session as raw terminal bytes.
fn attach(args: &[String]) -> Result<()> {
    let mut seconds = None;
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--seconds" if seconds.is_none() => {
                let value = args.get(index + 1).ok_or(PeerError("invalid_peer_attach_option"))?;
                seconds = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| PeerError("invalid_peer_attach_option"))?,
                );
                index += 2;
            }
            _ => {
                positionals.push(args[index].as_str());
                index += 1;
            }
        }
    }
    let [machine_id, card_id] = positionals[..] else {
        return Err(PeerError("peer_attach_requires_machine_and_card"));
    };
    let peer = PeerStore::default_store()?.get(machine_id)?;
    let (stream, mut events) = TerminalStream::open(peer, card_id);
    // Piped stdin types into the host session: the same binary viewer→host
    // frames the graphical remote cards use. An interactive terminal keeps
    // stdin, so this command does not steal the keyboard.
    let typing = !std::io::stdin().is_terminal();
    if typing {
        let sender = stream.sender();
        std::thread::Builder::new()
            .name("peer-attach-stdin".into())
            .spawn(move || {
                let mut input = std::io::stdin().lock();
                let mut chunk = [0u8; 4096];
                loop {
                    match input.read(&mut chunk) {
                        Ok(0) => return,
                        Ok(n) => {
                            sender.send_input(&chunk[..n]);
                            if sender.is_stopped() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            })
            .ok();
    }
    let deadline = seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
    let mut stdout = std::io::stdout().lock();
    loop {
        match events.try_recv() {
            Ok(Event::Bytes(bytes)) => stdout
                .write_all(&bytes)
                .and_then(|()| stdout.flush())
                .map_err(|_| PeerError("output_failed"))?,
            Ok(Event::Attached { columns, rows }) => {
                eprintln!(
                    "attached {columns}x{rows} · {}",
                    if typing {
                        "typing stdin into the host session"
                    } else {
                        "output only"
                    }
                )
            }
            Ok(Event::Grid { columns, rows }) => eprintln!("host grid {columns}x{rows}"),
            // Normal endings are not failures; anything else keeps its code so
            // a caller can tell trust problems from an exited session.
            Ok(Event::Closed("closed" | "terminal_exited" | "reconnect")) => {
                drop(stream);
                eprintln!("detached");
                return Ok(());
            }
            Ok(Event::Closed(reason)) => return Err(PeerError(reason)),
            Err(futures_channel::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(futures_channel::mpsc::TryRecvError::Closed) => return Ok(()),
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(());
        }
    }
}

fn output(value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|_| PeerError("output_failed"))?;
    writeln!(stdout).map_err(|_| PeerError("output_failed"))
}
struct EchoGuard(Option<libc::termios>);
impl Drop for EchoGuard {
    fn drop(&mut self) {
        if let Some(state) = self.0 {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &state);
            }
        }
    }
}
fn add(args: &[String]) -> Result<()> {
    let (mut host, mut port, mut name) = (None, None, None);
    if args.len() % 2 != 0 {
        return Err(PeerError("peer_add_requires_option_value_pairs"));
    }
    for pair in args.chunks_exact(2) {
        match pair[0].as_str() {
            "--host" if host.is_none() => host = Some(pair[1].as_str()),
            "--port" if port.is_none() => {
                port = Some(
                    pair[1]
                        .parse::<u16>()
                        .map_err(|_| PeerError("invalid_peer_port"))?,
                )
            }
            "--name" if name.is_none() => name = Some(pair[1].as_str()),
            _ => return Err(PeerError("invalid_peer_add_option")),
        }
    }
    // Validate storage before consuming the host's invitation, without holding
    // the registry lock while waiting for approval.
    PeerStore::default_store()?.peers()?;
    let input = std::io::stdin();
    let mut guard = EchoGuard(None);
    if input.is_terminal() {
        eprintln!("Paste the host's pairing invitation, then press Enter (input hidden):");
        unsafe {
            let mut state: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut state) != 0 {
                return Err(PeerError("cannot_hide_invitation_input"));
            }
            let mut hidden = state;
            hidden.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(0, libc::TCSANOW, &hidden) != 0 {
                return Err(PeerError("cannot_hide_invitation_input"));
            }
            guard.0 = Some(state);
        }
    }
    let mut line = String::new();
    std::io::BufReader::new(input.lock().take((peer_client::MAX_INVITATION + 1) as u64))
        .read_line(&mut line)
        .map_err(|_| PeerError("invitation_input_failed"))?;
    drop(guard);
    let mut invitation = Invitation::parse(&line)?;
    invitation.endpoint = Endpoint::new(
        host.unwrap_or(&invitation.endpoint.host),
        port.unwrap_or(invitation.endpoint.port),
    )?;
    let local_id = crate::bridge::own_bridge_id();
    let pairing = Pairing::begin(
        invitation,
        local_id.as_deref(),
        &crate::bridge::hostname(),
        name,
    )?;
    eprintln!("Compare code {} on the host. Approve in SUPER DESKTOP Settings → Connections only if it matches.", pairing.code);
    loop {
        if let Some(peer) = pairing.poll()? {
            let summary = peer.summary();
            PeerStore::default_store()?.upsert(peer)?;
            return output(&summary);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
