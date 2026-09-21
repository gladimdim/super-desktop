use crate::{
    peer_client::{self, Endpoint, Invitation, Pairing, PeerError, Result},
    peer_store::PeerStore,
};
use std::io::{BufRead, IsTerminal, Read};

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
        _ => Err(PeerError("usage: peer-add [--host ADDRESS] [--port PORT] [--name LABEL] | peer-list | peer-workspace ID | peer-forget ID")),
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
    eprintln!("Compare code {} on the host. Approve in SUPER DESKTOP Settings → Android only if it matches.", pairing.code);
    loop {
        if let Some(peer) = pairing.poll()? {
            let summary = peer.summary();
            PeerStore::default_store()?.upsert(peer)?;
            return output(&summary);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
