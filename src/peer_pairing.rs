//! Cancelable outgoing pairing worker. No GTK objects or secrets in events.
use crate::{
    peer_client::{Endpoint, Invitation, Pairing, PeerError, PeerSummary},
    peer_store::PeerStore,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

static RUNNING: AtomicBool = AtomicBool::new(false);
pub enum Event {
    Code(String),
    Saved(PeerSummary),
    Failed(PeerError),
}
pub struct Session {
    active: Arc<AtomicBool>,
    pub events: mpsc::Receiver<Event>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
    }
}
struct RunningGuard;
impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

pub fn begin(text: String, host: String, port: String, name: String) -> Result<Session, PeerError> {
    let mut invitation = Invitation::parse(&text)?;
    let host = if host.trim().is_empty() {
        invitation.endpoint.host.as_str()
    } else {
        host.trim()
    };
    let port = if port.trim().is_empty() {
        invitation.endpoint.port
    } else {
        port.trim()
            .parse()
            .map_err(|_| PeerError("invalid_peer_port"))?
    };
    invitation.endpoint = Endpoint::new(host, port)?;
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(PeerError("pairing_worker_busy"));
    }
    let active = Arc::new(AtomicBool::new(true));
    let (send, events) = mpsc::channel();
    let worker_active = active.clone();
    let spawned = std::thread::Builder::new()
        .name("pc-pairing".into())
        .spawn(move || {
            let _guard = RunningGuard;
            let result = (|| {
                PeerStore::default_store()?.peers()?;
                if !worker_active.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let local = crate::bridge::own_bridge_id();
                let name = name.trim();
                let pairing = Pairing::begin(
                    invitation,
                    local.as_deref(),
                    &crate::bridge::hostname(),
                    if name.is_empty() { None } else { Some(name) },
                )?;
                if !worker_active.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let _ = send.send(Event::Code(pairing.code.clone()));
                while worker_active.load(Ordering::SeqCst) {
                    if let Some(peer) = pairing.poll()? {
                        // Claim completion before writing. Closing before this point
                        // cancels persistence; a save already begun is allowed to finish.
                        if !claim_save(&worker_active) {
                            return Ok(());
                        }
                        let summary = peer.summary();
                        PeerStore::default_store()?.upsert(peer)?;
                        let _ = send.send(Event::Saved(summary));
                        return Ok(());
                    }
                    for _ in 0..20 {
                        if !worker_active.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Ok::<_, PeerError>(())
            })();
            if let Err(error) = result {
                let _ = send.send(Event::Failed(error));
            }
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(PeerError("pairing_worker_unavailable"));
    }
    Ok(Session { active, events })
}
fn claim_save(active: &AtomicBool) -> bool {
    active
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}
pub fn message(error: PeerError) -> &'static str {
    match error.0 {
        "pairing_denied" => "The host denied pairing. Ask it for a new invitation to try again.",
        "pairing_request_expired" | "peer_endpoint_unavailable" => "The pairing request expired or the host needs an update. Generate a new invitation.",
        "invalid_pairing_invitation" | "invalid_certificate_pin" => "Paste the complete pairing link from the host's settings.",
        "invalid_peer_address" | "invalid_peer_port" => "Check the host address and port.",
        "cannot_pair_this_pc_with_itself" => "This invitation belongs to this PC. Use the other PC's invitation.",
        "invitation_rejected" => "The invitation expired or was already used. Generate a new one on the host.",
        "connection_failed_or_pin_mismatch" | "peer_identity_changed" => "Cannot verify or reach the host. Check its address, bridge and invitation.",
        "pairing_worker_busy" => "The previous connection is still finishing. Try again in a few seconds.",
        "pairing_rate_limited" => "The host is limiting requests. Deny any old request and wait two minutes before retrying.",
        "unsafe_peer_store_permissions" => "The saved-PC directory has unsafe permissions. Check it before pairing.",
        "pairing_outcome_uncertain_check_host" => "The connection ended during pairing. Check the host for a pending request before trying a new invitation.",
        _ => "Could not complete or save pairing. Check the host's paired devices before trying again.",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_prevents_a_late_approval_from_saving() {
        let active = Arc::new(AtomicBool::new(true));
        let (_, events) = mpsc::channel();
        let session = Session {
            active: active.clone(),
            events,
        };
        drop(session);
        assert!(!claim_save(&active));
    }
}
