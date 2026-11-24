//! Harness bridge hosted by super-desktop (Rust-only backend).
//!
//! Exposes this machine's harnesses to the Android launcher and its local
//! workspace to paired SUPER DESKTOP PCs over LAN / Tailscale. The owning
//! daemon remains authoritative for card layout and lifecycle.
//!
//! Endpoints (protocol version `PROTOCOL_VERSION`):
//!   GET  /api/v1/ping
//!   GET  /api/v1/harnesses            (Bearer token)
//!   GET  /api/v1/theme                 (Bearer token)
//!   DELETE /api/v1/harnesses/<id>     (Bearer token)
//!   POST /api/v1/pair                 (open; requests desktop approval)
//!   POST /api/v1/pair/poll            (unguessable request capability)
//!   GET  /api/v1/pair/state           (owner-only Unix control socket)
//!   POST /api/v1/pair/approve, /deny   (owner-only; deny also blocks the device)
//!   GET  /api/v1/pair/rejected, POST /api/v1/pair/rejected/remove   (owner-only)

use serde::{Deserialize, Serialize};
#[path = "bridge_pairing.rs"]
mod pairing;
pub use pairing::{
    pending_requests, decide_request, paired_devices, revoke_device, pairing_invitation,
    rejected_devices, forget_rejected, pairing_requests,
    open_code_pairing, close_code_pairing, confirm_pairing_code,
};
pub(crate) use pairing::local_request as owner_pairing_request;
#[path = "bridge_security.rs"]
mod security;
#[path = "bridge_lifecycle.rs"]
mod lifecycle;
pub use lifecycle::supervise as supervise_bridge;
pub use lifecycle::supervise_while as supervise_bridge_while;
use security::Connection;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::state::load_state;
use crate::tmux::{
    capture_pane_text, extract_composer_draft, get_agent_config,
    get_composer_draft, get_opencode_user_text_by_id, inspect_status,
    inspect_status_with_screen, resolve_own_opencode_id, resolve_workspace_dir,
    strip_terminal_escapes, SessionStatus,
};

pub const BRIDGE_PORT: u16 = 8759;
const SERVICE_NAME: &str = "Omarchy Harness Bridge";
const PROTOCOL_VERSION: u32 = 3;
#[path = "desktop_bridge.rs"]
mod desktop;
#[path = "desktop_events.rs"]
mod desktop_events;
#[path = "bridge_harness_list.rs"]
mod harness_list;
#[path = "bridge_wake.rs"]
mod wake;
#[path = "bridge_terminal_stream.rs"]
mod terminal_stream;
#[path = "bridge_completions.rs"]
mod completions;
#[path = "bridge_previews.rs"]
mod previews;

/// Phone input reached `session`: wake that session's terminal streams only.
fn wake_terminal_streams(session: &str) {
    terminal_stream::wake(session);
}

fn utc_now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The active Omarchy palette, kept explicit so the wire format remains stable
/// if the desktop-side theme struct gains implementation-only fields.
fn theme_document() -> serde_json::Value {
    let t = crate::theme::current_theme_fresh();
    serde_json::json!({
        "name": t.name,
        "mode": t.mode,
        "accent": t.accent,
        "selection": t.selection,
        "muted": t.muted,
        "background": t.background,
        "darkBackground": t.dark_background,
        "darkerBackground": t.darker_background,
        "lighterBackground": t.lighter_background,
        "foreground": t.foreground,
        "darkForeground": t.dark_foreground,
        "lightForeground": t.light_foreground,
        "brightForeground": t.bright_foreground,
        "red": t.red,
        "yellow": t.yellow,
        "orange": t.orange,
        "green": t.green,
        "cyan": t.cyan,
        "blue": t.blue,
        "magenta": t.magenta,
        "brown": t.brown,
        "brightRed": t.bright_red,
        "brightYellow": t.bright_yellow,
        "brightGreen": t.bright_green,
        "brightCyan": t.bright_cyan,
        "brightBlue": t.bright_blue,
        "brightMagenta": t.bright_magenta,
        "fontFamily": t.font_family,
        "fontSize": t.font_size,
    })
}


mod state;
mod http;
mod handlers;
mod discovery;
mod process;
#[cfg(test)]
mod tests;

pub use state::{collect_harnesses, own_bridge_id};
pub(crate) use state::{last_user_text, session_title};
pub(in crate::bridge) use state::{
    pair_state, random_hex, state_path, tag_color, BridgeConfig, PairState, PairedDevice,
    RejectedDevice,
};

pub(in crate::bridge) use http::{
    authorize, bearer, read_request, reply_binary, reply_body, reply_error, reply_json,
    require_live_session, require_pairing, ws_client_alive, ws_client_message, ws_upgrade,
    Envelope, Request, STREAM_MAX_SECS,
};
#[cfg(test)]
pub(in crate::bridge) use http::reason_phrase;

pub use handlers::serve;
pub(in crate::bridge) use handlers::{serve_connection, session_meta};
#[cfg(test)]
pub(in crate::bridge) use handlers::{creation_command, stream_keys, stream_keys_as};

pub use discovery::{hostname, lan_ip, mdns_summary, print_once, publish_mdns, MDNS_SERVICE_TYPE};
pub(in crate::bridge) use discovery::{bridge_addresses, now_epoch, TAILSCALE_CACHE};
#[cfg(test)]
pub(in crate::bridge) use discovery::{
    any_output_line, browse_line_matches, interface_addresses, mdns_state_path, BROWSE_TIMEOUT,
};

pub use process::{bridge_running, firewall_summary, start_bridge, stop_bridge, tailscale_ip, unlock_firewall};
pub(in crate::bridge) use process::{
    bridge_exe, bridge_log_path, health_body, start_bridge_inner, HEALTH_QUERY,
};
#[cfg(test)]
pub(in crate::bridge) use process::port_taken;

/// A connected loopback pair for tests: (client, accepted server socket).
/// The client gives up reading after 10 s instead of hanging a failed test.
#[cfg(test)]
fn loopback_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let (server, _) = listener.accept().unwrap();
    (client, server)
}

/// A loopback client whose server end `serve` handles on its own thread.
#[cfg(test)]
fn loopback_server<T: Send + 'static>(
    serve: impl FnOnce(Connection) -> T + Send + 'static,
) -> (TcpStream, std::thread::JoinHandle<T>) {
    let (client, server) = loopback_pair();
    (client, std::thread::spawn(move || serve(Connection::plain(server))))
}

