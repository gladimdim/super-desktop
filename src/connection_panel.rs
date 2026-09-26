//! What the remote workspace shows while a paired PC is connecting or cannot
//! be reached: the likeliest cause, what to do about it, and the state of each
//! link between the two machines, in the order a connection crosses them. The
//! links are this PC's network, Tailscale, the route to that PC, and its port.
//!
//! The checks run off the GTK thread, only while a failure is on screen, and at
//! most once per `RECHECK` for the same PC; Retry runs them at once. They read
//! this PC's interfaces and Tailscale state, and open one plain TCP connection
//! to the paired address, which sends nothing and is closed at once.
use crate::peer_client::Endpoint;
use gtk4::{glib, prelude::*};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long one set of checks describes the PC on screen; a failure after
/// that runs them again.
const RECHECK: Duration = Duration::from_secs(10);
/// How long the port check waits for that PC to answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// More interfaces than this is a VM host or a lab, not a network to read.
const MAX_INTERFACES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Wifi,
    Ethernet,
    /// A bridge carrying a physical port, USB tethering and the like.
    Other,
}

/// One of this PC's own network connections, with its IPv4 address.
#[derive(Clone, Debug, PartialEq)]
pub struct Interface {
    pub name: String,
    pub kind: LinkKind,
    pub address: Ipv4Addr,
    pub prefix: u8,
    /// The Wi-Fi network's name, when it could be read.
    pub ssid: Option<String>,
}

impl Interface {
    fn contains(&self, ip: Ipv4Addr) -> bool {
        network(ip, self.prefix) == network(self.address, self.prefix)
    }

    fn subnet(&self) -> String {
        format!("{}/{}", network(self.address, self.prefix), self.prefix)
    }

    /// "Wi-Fi" or "Ethernet", for a sentence.
    fn short(&self) -> &str {
        match self.kind {
            LinkKind::Wifi => "Wi-Fi",
            LinkKind::Ethernet => "Ethernet",
            LinkKind::Other => &self.name,
        }
    }

    fn describe(&self) -> String {
        let name = match (self.kind, &self.ssid) {
            (LinkKind::Wifi, Some(ssid)) => format!("Wi-Fi “{ssid}”"),
            (LinkKind::Wifi, None) => format!("Wi-Fi {}", self.name),
            (LinkKind::Ethernet, _) => format!("Ethernet {}", self.name),
            (LinkKind::Other, _) => self.name.clone(),
        };
        format!("{name} · {}/{}", self.address, self.prefix)
    }
}

fn network(ip: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix.min(32))).unwrap_or(0);
    Ipv4Addr::from(u32::from(ip) & mask)
}

/// This PC's Tailscale, as `tailscale status` reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum Tailscale {
    NotInstalled,
    /// Installed, but its daemon is not running.
    NotRunning,
    Stopped,
    NeedsLogin,
    Starting,
    Running {
        ip: Option<String>,
        tailnet: Option<String>,
    },
}

impl Tailscale {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// That PC as this PC's tailnet knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TailnetDevice {
    pub name: String,
    pub ip: Option<String>,
    pub online: bool,
    /// Seconds since Tailscale last saw it, when it is offline.
    pub last_seen: Option<i64>,
}

impl TailnetDevice {
    fn state(&self) -> String {
        match (self.online, self.last_seen, &self.ip) {
            (true, _, Some(ip)) => format!("is online on the tailnet · {ip}"),
            (true, _, None) => "is online on the tailnet".into(),
            (false, Some(seconds), _) => format!("is offline on the tailnet · last seen {}", ago(seconds)),
            (false, None, _) => "is offline on the tailnet".into(),
        }
    }
}

/// What the paired address said to a plain TCP connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Answered,
    Refused,
    TimedOut,
    NoRoute,
    Unresolved,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressKind {
    ThisPc,
    Tailscale,
    Local,
    Internet,
}

pub fn address_kind(ip: IpAddr) -> AddressKind {
    match ip {
        ip if ip.is_loopback() => AddressKind::ThisPc,
        // Tailscale hands out 100.64.0.0/10 and fd7a:115c:a1e0::/48.
        IpAddr::V4(v4) if v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64 => {
            AddressKind::Tailscale
        }
        IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() => AddressKind::Local,
        IpAddr::V6(v6) if v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0] => AddressKind::Tailscale,
        IpAddr::V6(v6) if v6.is_unique_local() || v6.is_unicast_link_local() => AddressKind::Local,
        _ => AddressKind::Internet,
    }
}

/// The paired address and what this PC found out about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub endpoint: Endpoint,
    /// What `endpoint.host` resolved to.
    pub address: Option<IpAddr>,
    /// The address this PC would send from, if it has a route there.
    pub source: Option<IpAddr>,
    pub reach: Reach,
}

impl Remote {
    fn kind(&self) -> Option<AddressKind> {
        // A MagicDNS name does not resolve while Tailscale is off.
        self.address
            .map(address_kind)
            .or_else(|| self.endpoint.host.ends_with(".ts.net").then_some(AddressKind::Tailscale))
    }

    fn authority(&self) -> String {
        match self.endpoint.host.contains(':') {
            true => format!("[{}]:{}", self.endpoint.host, self.endpoint.port),
            false => format!("{}:{}", self.endpoint.host, self.endpoint.port),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnosis {
    /// `None` when this PC's interfaces could not be read at all.
    pub interfaces: Option<Vec<Interface>>,
    pub tailscale: Tailscale,
    /// That PC in the tailnet: by its address, or else by its name.
    pub device: Option<TailnetDevice>,
    /// `None` when this PC has no saved address for it.
    pub remote: Option<Remote>,
}

impl Diagnosis {
    fn offline(&self) -> bool {
        self.interfaces.as_ref().is_some_and(Vec::is_empty) && !self.tailscale.is_running()
    }
}

// ---------- the checks (blocking: never on the GTK thread) ----------

/// Every check for the saved PC `id`.
fn diagnose_saved(id: &str, label: &str) -> Diagnosis {
    let endpoint = crate::peer_store::PeerStore::default_store()
        .and_then(|store| store.get(id))
        .ok()
        .map(|peer| peer.endpoint);
    diagnose(label, endpoint.as_ref())
}

fn diagnose(label: &str, endpoint: Option<&Endpoint>) -> Diagnosis {
    let remote = endpoint.map(probe);
    let address = remote.as_ref().and_then(|remote| remote.address);
    let (tailscale, device) = match tailscale_status() {
        Ok(status) => parse_tailscale(&status, label, address, chrono::Utc::now()),
        Err(state) => (state, None),
    };
    Diagnosis {
        interfaces: interfaces(),
        tailscale,
        device,
        remote,
    }
}

fn interfaces() -> Option<Vec<Interface>> {
    let output = Command::new("ip")
        .args(["-j", "-4", "address", "show", "up"])
        .output()
        .ok()?;
    let document: Value = serde_json::from_slice(&output.stdout).ok()?;
    let mut found = parse_interfaces(&document, link_kind);
    for interface in found.iter_mut().filter(|i| i.kind == LinkKind::Wifi) {
        interface.ssid = ssid(&interface.name);
    }
    Some(found)
}

/// Physical links only: Docker and libvirt bridges, veth pairs and VPN
/// tunnels have no device behind them (Tailscale is reported on its own).
fn link_kind(name: &str) -> Option<LinkKind> {
    let base = Path::new("/sys/class/net").join(name);
    if base.join("wireless").exists() || base.join("phy80211").exists() {
        return Some(LinkKind::Wifi);
    }
    if base.join("device").exists() {
        let ethernet = std::fs::read_to_string(base.join("type")).is_ok_and(|t| t.trim() == "1");
        return Some(if ethernet { LinkKind::Ethernet } else { LinkKind::Other });
    }
    // A bridge that carries this PC's own network card holds its address.
    let physical_port = std::fs::read_dir(base.join("brif")).is_ok_and(|ports| {
        ports.flatten().any(|port| {
            Path::new("/sys/class/net").join(port.file_name()).join("device").exists()
        })
    });
    physical_port.then_some(LinkKind::Other)
}

fn parse_interfaces(document: &Value, kind_of: impl Fn(&str) -> Option<LinkKind>) -> Vec<Interface> {
    let mut found = Vec::new();
    for interface in document.as_array().into_iter().flatten() {
        let name = interface["ifname"].as_str().unwrap_or("");
        if name.is_empty() || interface["link_type"] == "loopback" || name.starts_with("tailscale") {
            continue;
        }
        let Some(kind) = kind_of(name) else {
            continue;
        };
        for info in interface["addr_info"].as_array().into_iter().flatten() {
            if info["family"] != "inet" || info["scope"] != "global" {
                continue;
            }
            let address = info["local"].as_str().and_then(|a| a.parse().ok());
            let (Some(address), Some(prefix)) = (address, info["prefixlen"].as_u64()) else {
                continue;
            };
            found.push(Interface {
                name: name.to_string(),
                kind,
                address,
                prefix: prefix.min(32) as u8,
                ssid: None,
            });
        }
    }
    found.truncate(MAX_INTERFACES);
    found
}

/// The Wi-Fi network's name, from `iw`, or `iwctl` where iwd has no `iw`.
fn ssid(interface: &str) -> Option<String> {
    let run = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
    };
    run("iw", &["dev", interface, "link"])
        .and_then(|text| parse_ssid(&text, "SSID:"))
        .or_else(|| {
            run("iwctl", &["station", interface, "show"])
                .and_then(|text| parse_ssid(&text, "Connected network"))
        })
}

fn parse_ssid(text: &str, key: &str) -> Option<String> {
    strip_ansi(text)
        .lines()
        .find_map(|line| line.trim().strip_prefix(key))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// `iwctl` colours its tables even when nothing is reading them.
fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

fn tailscale_status() -> Result<Value, Tailscale> {
    let output = match Command::new("tailscale").args(["status", "--json"]).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Tailscale::NotInstalled)
        }
        Err(_) => return Err(Tailscale::NotRunning),
    };
    // Without its daemon the command prints an error instead of a document.
    serde_json::from_slice(&output.stdout).map_err(|_| Tailscale::NotRunning)
}

fn parse_tailscale(
    status: &Value,
    label: &str,
    address: Option<IpAddr>,
    now: chrono::DateTime<chrono::Utc>,
) -> (Tailscale, Option<TailnetDevice>) {
    let ipv4 = |ips: &Value| {
        ips.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|ip| ip.contains('.'))
            .map(str::to_string)
    };
    let state = match status["BackendState"].as_str() {
        Some("Running") => Tailscale::Running {
            ip: ipv4(&status["Self"]["TailscaleIPs"]).or_else(|| ipv4(&status["TailscaleIPs"])),
            tailnet: status["CurrentTailnet"]["Name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        },
        Some("Stopped") => Tailscale::Stopped,
        Some("NeedsLogin" | "NeedsMachineAuth") => Tailscale::NeedsLogin,
        Some("Starting") => Tailscale::Starting,
        _ => Tailscale::NotRunning,
    };
    let peers: Vec<&Value> = status["Peer"]
        .as_object()
        .map(|peers| peers.values().collect())
        .unwrap_or_default();
    let by_address = address.map(|ip| ip.to_string()).and_then(|ip| {
        peers.iter().copied().find(|peer| {
            peer["TailscaleIPs"].as_array().into_iter().flatten().any(|v| v == ip.as_str())
        })
    });
    let by_name = || {
        peers.iter().copied().find(|peer| {
            let dns = peer["DNSName"].as_str().unwrap_or("").split('.').next().unwrap_or("");
            let host = peer["HostName"].as_str().unwrap_or("");
            !label.is_empty() && (host.eq_ignore_ascii_case(label) || dns.eq_ignore_ascii_case(label))
        })
    };
    let device = by_address.or_else(by_name).map(|peer| {
        let online = peer["Online"].as_bool().unwrap_or(false);
        // Tailscale writes year 1 for "never" and for devices online now.
        let last_seen = peer["LastSeen"]
            .as_str()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .filter(|at| !online && chrono::Datelike::year(at) > 2000)
            .map(|at| (now - at.with_timezone(&chrono::Utc)).num_seconds().max(0));
        TailnetDevice {
            name: peer["HostName"].as_str().unwrap_or(label).to_string(),
            ip: ipv4(&peer["TailscaleIPs"]),
            online,
            last_seen,
        }
    });
    (state, device)
}

/// Resolve the paired address, find the route there and knock on its port.
fn probe(endpoint: &Endpoint) -> Remote {
    let resolved: Vec<SocketAddr> = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map(Iterator::collect)
        .unwrap_or_default();
    // The bridge listens on IPv4.
    let Some(address) = resolved.iter().find(|a| a.is_ipv4()).or(resolved.first()).copied() else {
        return Remote {
            endpoint: endpoint.clone(),
            address: None,
            source: None,
            reach: Reach::Unresolved,
        };
    };
    let reach = match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
        Ok(_) => Reach::Answered,
        Err(error) => reach_of(&error),
    };
    Remote {
        endpoint: endpoint.clone(),
        address: Some(address.ip()),
        source: source_for(address),
        reach,
    }
}

/// The address this PC would send from: connecting a UDP socket asks the
/// kernel for a route, Tailscale's policy routing included, and sends nothing.
fn source_for(address: SocketAddr) -> Option<IpAddr> {
    let any: IpAddr = match address {
        SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let socket = UdpSocket::bind((any, 0)).ok()?;
    socket.connect(address).ok()?;
    socket.local_addr().ok().map(|local| local.ip())
}

fn reach_of(error: &std::io::Error) -> Reach {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionRefused => Reach::Refused,
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => Reach::NoRoute,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => Reach::TimedOut,
        _ => Reach::Failed,
    }
}

fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 48 * 3600 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

// ---------- what the panel says (pure) ----------

/// A check's result, drawn as the invitation checklist's marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Working,
    Ok,
    /// Not in use, and not needed.
    Off,
    Warn,
    Fail,
}

#[derive(Debug, PartialEq)]
pub struct Advice {
    pub mark: Mark,
    pub title: String,
    pub hint: String,
    pub pair_again: bool,
}

impl Advice {
    fn new(mark: Mark, title: String, hint: String) -> Self {
        Self {
            mark,
            title,
            hint,
            pair_again: false,
        }
    }

    fn pair_again(mut self) -> Self {
        self.pair_again = true;
        self
    }
}

/// Errors that mean the secure connection never came up.
fn network_error(error: &str) -> bool {
    matches!(error, "connection_failed_or_pin_mismatch" | "connection_failed")
}

/// What the panel leads with: the likeliest cause, and what to do about it.
/// `error` is `None` while connecting.
pub fn advice(label: &str, error: Option<&str>, diagnosis: Option<&Diagnosis>) -> Advice {
    let Some(error) = error else {
        return Advice::new(
            Mark::Working,
            format!("Connecting to {label}…"),
            "Opening a secure connection to the address it was paired at.".into(),
        );
    };
    match error {
        "peer_revoked_or_expired" | "peer_not_found" => Advice::new(
            Mark::Warn,
            format!("{label} needs pairing again"),
            "That PC removed this one, or the pairing expired. Pair the two PCs again to reconnect."
                .into(),
        )
        .pair_again(),
        "update_remote_super_desktop" | "peer_endpoint_unavailable" => Advice::new(
            Mark::Warn,
            format!("{label} needs an update"),
            "It runs an older SUPER DESKTOP that cannot share its workspace. Update SUPER DESKTOP \
             on that PC (git pull && ./rebuild.sh), then retry."
                .into(),
        ),
        "peer_identity_changed" => Advice::new(
            Mark::Fail,
            format!("Cannot verify {label}"),
            "Another SUPER DESKTOP answered at its address. If you reinstalled it there, pair \
             again. Otherwise that address now belongs to a different PC."
                .into(),
        )
        .pair_again(),
        error if network_error(error) => unreachable(label, diagnosis),
        "invalid_peer_store" | "unsafe_peer_store_permissions" => stored(label),
        error if error.starts_with("peer_store") => stored(label),
        _ => Advice::new(
            Mark::Warn,
            format!("{label} is not ready"),
            "Its SUPER DESKTOP answered, but cannot share its workspace right now. Retrying \
             automatically."
                .into(),
        ),
    }
}

fn stored(label: &str) -> Advice {
    Advice::new(
        Mark::Fail,
        format!("Cannot open {label}"),
        "This PC could not read its list of paired PCs. Run super-desktop peer-list to see why."
            .into(),
    )
}

fn unreachable(label: &str, diagnosis: Option<&Diagnosis>) -> Advice {
    let fail = |title: String, hint: String| Advice::new(Mark::Fail, title, hint);
    let cannot_reach = format!("Cannot reach {label}");
    let Some(diagnosis) = diagnosis else {
        return fail(
            cannot_reach,
            "Checking this PC's network and the route to that PC…".into(),
        );
    };
    if diagnosis.offline() {
        return fail(
            "This PC is offline".into(),
            "Connect this PC to Wi-Fi, Ethernet or Tailscale, then retry.".into(),
        );
    }
    let Some(remote) = &diagnosis.remote else {
        return fail(
            cannot_reach,
            "Check that SUPER DESKTOP is running on that PC and that port 8759/tcp is reachable."
                .into(),
        );
    };
    let (host, port) = (&remote.endpoint.host, remote.endpoint.port);
    let kind = remote.kind();
    let device = diagnosis.device.as_ref();
    let last_seen = |device: &TailnetDevice| match device.last_seen {
        Some(seconds) => format!("Tailscale last saw it {}.", ago(seconds)),
        None => "Tailscale reports it offline.".into(),
    };
    if kind == Some(AddressKind::Tailscale) {
        if !diagnosis.tailscale.is_running() {
            return fail(
                "Tailscale is off on this PC".into(),
                format!(
                    "{label} is paired at its Tailscale address {host}. Turn Tailscale on here \
                     (tailscale up), then retry."
                ),
            );
        }
        match device {
            None => {
                return fail(
                    format!("{label} is not on your tailnet"),
                    format!(
                        "Your tailnet has no device at {host}. Sign both PCs in to the same \
                         tailnet, or pair again at the address it has now."
                    ),
                )
                .pair_again()
            }
            Some(device) if !device.online => {
                return fail(
                    format!("{label} is offline"),
                    format!("{} Wake it or turn it on, then retry.", last_seen(device)),
                )
            }
            Some(_) => {}
        }
    }
    let off_network = match remote.address {
        Some(IpAddr::V4(ip)) if kind == Some(AddressKind::Local) => diagnosis
            .interfaces
            .as_ref()
            .filter(|interfaces| !interfaces.is_empty())
            .filter(|interfaces| !interfaces.iter().any(|i| i.contains(ip)))
            .map(|interfaces| {
                interfaces.iter().map(Interface::subnet).collect::<Vec<_>>().join(", ")
            }),
        _ => None,
    };
    match remote.reach {
        Reach::Refused => fail(
            format!("SUPER DESKTOP is not running on {label}"),
            format!(
                "{host} is up but refused port {port}. Start SUPER DESKTOP on that PC; its \
                 bridge starts with it."
            ),
        ),
        Reach::Answered => fail(
            format!("Cannot verify {label}"),
            format!(
                "Something answers at {} but could not prove it is the PC you paired. If \
                 SUPER DESKTOP was reinstalled there, or the address now belongs to another \
                 device, pair again.",
                remote.authority()
            ),
        )
        .pair_again(),
        Reach::Unresolved => fail(
            format!("Cannot find {label}"),
            format!("{host} does not resolve from this PC. Check its name, or pair again at its address."),
        ),
        Reach::NoRoute | Reach::TimedOut | Reach::Failed => {
            if let Some(subnets) = off_network {
                return Advice::new(
                    Mark::Warn,
                    format!("{label} is on another network"),
                    format!(
                        "{host} is not on this PC's network ({subnets}). Join the network that \
                         PC is on, or connect both PCs to Tailscale."
                    ),
                );
            }
            if remote.reach == Reach::NoRoute {
                return fail(
                    format!("No route to {label}"),
                    format!("This PC has no route to {host}. Check this PC's network connection."),
                );
            }
            let mut hint = format!(
                "No answer from {}. Check that it is awake and connected, and that its firewall \
                 allows port {port}.",
                remote.authority()
            );
            match device {
                Some(device) if !device.online => {
                    hint = format!("{hint} {} It is probably asleep or off.", last_seen(device))
                }
                Some(TailnetDevice { online: true, ip: Some(ip), .. })
                    if kind != Some(AddressKind::Tailscale) =>
                {
                    hint = format!(
                        "{hint} It is online on Tailscale at {ip}: pair it at that address to \
                         reach it from any network."
                    )
                }
                _ => {}
            }
            fail(format!("{label} is not answering"), hint)
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Check {
    pub mark: Mark,
    pub title: String,
    pub detail: String,
}

/// The four links between the two PCs, in the order a connection crosses
/// them: this PC's network, Tailscale, the route, and that PC's port. `None`
/// for a link nothing is known about (no saved or resolvable address).
pub fn checks(label: &str, error: &str, diagnosis: &Diagnosis) -> [Option<Check>; 4] {
    let check = |mark, title: &str, detail: String| Check {
        mark,
        title: title.to_string(),
        detail,
    };
    let remote = diagnosis.remote.as_ref();
    let kind = remote.and_then(Remote::kind);

    let network = match &diagnosis.interfaces {
        None => check(Mark::Off, "This PC's network", "Could not read this PC's interfaces".into()),
        Some(interfaces) if interfaces.is_empty() => check(
            if diagnosis.tailscale.is_running() { Mark::Warn } else { Mark::Fail },
            "This PC's network",
            "No Wi-Fi or Ethernet connection".into(),
        ),
        Some(interfaces) => check(
            Mark::Ok,
            "This PC's network",
            interfaces.iter().map(Interface::describe).collect::<Vec<_>>().join("\n"),
        ),
    };

    let needed = kind == Some(AddressKind::Tailscale);
    let (mut mark, mut detail) = match &diagnosis.tailscale {
        Tailscale::Running { ip, tailnet } => {
            let mut parts = vec!["On".to_string()];
            parts.extend(ip.clone());
            parts.extend(tailnet.as_ref().map(|name| format!("tailnet {name}")));
            (Mark::Ok, parts.join(" · "))
        }
        state => (
            if needed { Mark::Fail } else { Mark::Off },
            match state {
                Tailscale::NotInstalled => "Not installed",
                Tailscale::NotRunning => "Off · tailscaled is not running",
                Tailscale::NeedsLogin => "Signed out · run tailscale login",
                Tailscale::Starting => "Starting…",
                _ => "Off",
            }
            .to_string(),
        ),
    };
    match &diagnosis.device {
        Some(device) => detail = format!("{detail}\n{} {}", device.name, device.state()),
        None if needed && diagnosis.tailscale.is_running() => {
            mark = Mark::Warn;
            if let Some(remote) = remote {
                detail = format!("{detail}\nNo device at {} on this tailnet", remote.endpoint.host);
            }
        }
        None => {}
    }
    let tailscale = check(mark, "Tailscale", detail);

    let route = remote.and_then(|remote| Some((remote, remote.address?))).map(|(remote, address)| {
        let interfaces = diagnosis.interfaces.as_deref().unwrap_or_default();
        let (mark, detail) = match remote.source {
            _ if address_kind(address) == AddressKind::ThisPc => (Mark::Ok, "This PC itself".into()),
            None => (Mark::Fail, format!("This PC has no route to {address}")),
            Some(source) if address_kind(source) == AddressKind::Tailscale => {
                (Mark::Ok, format!("Through Tailscale, from {source}"))
            }
            Some(_) if needed => (
                Mark::Fail,
                format!("{address} is a Tailscale address, but Tailscale does not carry it from this PC"),
            ),
            Some(source) => match interfaces.iter().find(|i| IpAddr::V4(i.address) == source) {
                Some(i) if matches!(address, IpAddr::V4(ip) if i.contains(ip)) => (
                    Mark::Ok,
                    format!("Same local network, over {} ({})", i.short(), i.subnet()),
                ),
                Some(i) if kind == Some(AddressKind::Local) => (
                    Mark::Warn,
                    format!("{address} is not on this PC's network ({}), so it goes through the router", i.subnet()),
                ),
                Some(i) => (Mark::Ok, format!("Over {}, from {source}", i.short())),
                None => (Mark::Ok, format!("From {source}")),
            },
        };
        check(mark, &format!("Route to {label}"), detail)
    });

    let port = remote.map(|remote| {
        let port = remote.endpoint.port;
        let (mark, detail) = match remote.reach {
            Reach::Answered if network_error(error) => (
                Mark::Warn,
                format!("Port {port} answers, but the secure connection failed"),
            ),
            Reach::Answered => (Mark::Ok, format!("Port {port} answers")),
            Reach::Refused => (
                Mark::Fail,
                format!("Port {port} refused · SUPER DESKTOP is not accepting connections there"),
            ),
            Reach::TimedOut => (
                Mark::Fail,
                format!("No answer on port {port} within {} s", PROBE_TIMEOUT.as_secs()),
            ),
            Reach::NoRoute => (Mark::Fail, "Unreachable from this PC".into()),
            Reach::Unresolved => (
                Mark::Fail,
                format!("{} does not resolve from this PC", remote.endpoint.host),
            ),
            Reach::Failed => (Mark::Fail, format!("Could not connect to port {port}")),
        };
        check(mark, &format!("{label} · {}", remote.authority()), detail)
    });

    [Some(network), Some(tailscale), route, port]
}

// ---------- the panel ----------

pub(crate) type Diagnose = Arc<dyn Fn(&str, &str) -> Diagnosis + Send + Sync>;

/// One link in the checklist: mark, what it is, what was found.
struct Row {
    widget: gtk4::Box,
    mark: gtk4::Label,
    title: gtk4::Label,
    detail: gtk4::Label,
}

impl Row {
    fn new(parent: &gtk4::Box) -> Self {
        let widget = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        widget.add_css_class("invite-step");
        let mark = gtk4::Label::new(None);
        mark.add_css_class("invite-step-mark");
        mark.set_valign(gtk4::Align::Start);
        widget.append(&mark);
        let words = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
        words.set_hexpand(true);
        let title = wrapping_label("invite-step-title");
        title.set_xalign(0.0);
        words.append(&title);
        let detail = wrapping_label("invite-step-detail");
        detail.set_xalign(0.0);
        detail.set_max_width_chars(60);
        // Addresses are what the user types elsewhere.
        detail.set_selectable(true);
        words.append(&detail);
        widget.append(&words);
        parent.append(&widget);
        Self { widget, mark, title, detail }
    }

    fn show(&self, check: Option<&Check>) {
        self.widget.set_visible(check.is_some());
        if let Some(check) = check {
            paint_mark(&self.mark, check.mark);
            set_text(&self.title, &check.title);
            set_text(&self.detail, &check.detail);
        }
    }
}

fn wrapping_label(class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(None);
    label.add_css_class(class);
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label
}

fn paint_mark(label: &gtk4::Label, mark: Mark) {
    let (text, class) = match mark {
        Mark::Working => ("…", "invite-working"),
        Mark::Ok => ("✓", "invite-ok"),
        Mark::Off => ("–", "invite-working"),
        Mark::Warn => ("!", "invite-warn"),
        Mark::Fail => ("✕", "invite-fail"),
    };
    set_text(label, text);
    for other in ["invite-working", "invite-ok", "invite-warn", "invite-fail"] {
        if other != class {
            label.remove_css_class(other);
        }
    }
    label.add_css_class(class);
}

/// Skip a label that already says this: a failure repeats every poll.
fn set_text(label: &gtk4::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
    }
}

/// The PC on screen, and what is known about it.
struct Shown {
    id: String,
    label: String,
    /// `None` while connecting.
    error: Option<String>,
    attempt: Option<chrono::DateTime<chrono::Local>>,
    diagnosis: Option<(Instant, Diagnosis)>,
}

pub struct ConnectionPanel {
    pub widget: gtk4::ScrolledWindow,
    mark: gtk4::Label,
    title: gtk4::Label,
    hint: gtk4::Label,
    checks: gtk4::Box,
    rows: [Row; 4],
    retry: gtk4::Button,
    pair: gtk4::Button,
    footer: gtk4::Label,
    shown: RefCell<Option<Shown>>,
    /// Bumped whenever another PC is shown, so late checks are dropped.
    serial: Cell<u64>,
    /// The serial whose checks are running now.
    checking: Cell<Option<u64>>,
    diagnose: RefCell<Diagnose>,
}

impl ConnectionPanel {
    pub fn new() -> Rc<Self> {
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        content.add_css_class("remote-status");
        content.set_halign(gtk4::Align::Center);
        content.set_valign(gtk4::Align::Center);
        let mark = gtk4::Label::new(None);
        mark.add_css_class("invite-step-mark");
        mark.add_css_class("remote-status-mark");
        mark.set_halign(gtk4::Align::Center);
        content.append(&mark);
        let title = wrapping_label("remote-status-title");
        title.set_justify(gtk4::Justification::Center);
        title.set_max_width_chars(40);
        content.append(&title);
        let hint = wrapping_label("remote-status-hint");
        hint.set_justify(gtk4::Justification::Center);
        hint.set_max_width_chars(64);
        content.append(&hint);
        let checks = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        checks.add_css_class("launcher-section");
        checks.add_css_class("invite-checklist");
        checks.add_css_class("remote-status-checks");
        let rows = [(); 4].map(|_| Row::new(&checks));
        content.append(&checks);
        let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        actions.set_halign(gtk4::Align::Center);
        let retry = gtk4::Button::with_label("↻ Retry now");
        retry.add_css_class("launcher-btn");
        actions.append(&retry);
        let pair = gtk4::Button::with_label("Pair again");
        pair.add_css_class("launcher-btn");
        pair.set_tooltip_text(Some("Open Add a PC to pair with a new invitation"));
        actions.append(&pair);
        content.append(&actions);
        let footer = wrapping_label("remote-status-footer");
        footer.set_justify(gtk4::Justification::Center);
        content.append(&footer);
        // Scrolls on a short screen instead of asking the overlay for height;
        // the labels wrap instead of asking it for width.
        let widget = gtk4::ScrolledWindow::new();
        widget.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        widget.set_propagate_natural_width(false);
        widget.set_propagate_natural_height(false);
        widget.set_min_content_height(0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_child(Some(&content));
        let panel = Rc::new(Self {
            widget,
            mark,
            title,
            hint,
            checks,
            rows,
            retry,
            pair,
            footer,
            shown: RefCell::new(None),
            serial: Cell::new(0),
            checking: Cell::new(None),
            diagnose: RefCell::new(Arc::new(diagnose_saved)),
        });
        let weak = Rc::downgrade(&panel);
        panel.retry.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.check();
            }
        });
        panel
    }

    /// `retry` runs on Retry, after the panel starts its own checks again.
    pub fn connect_retry(&self, retry: impl Fn() + 'static) {
        self.retry.connect_clicked(move |_| retry());
    }

    pub fn connect_pair(&self, pair: impl Fn() + 'static) {
        self.pair.connect_clicked(move |_| pair());
    }

    #[cfg(test)]
    pub(crate) fn set_diagnose(&self, diagnose: Diagnose) {
        *self.diagnose.borrow_mut() = diagnose;
    }

    #[cfg(test)]
    pub(crate) fn title(&self) -> glib::GString {
        self.title.text()
    }

    /// Retry and Pair again.
    #[cfg(test)]
    pub(crate) fn buttons(&self) -> (gtk4::Button, gtk4::Button) {
        (self.retry.clone(), self.pair.clone())
    }

    /// Connecting to `label` for the first time since it was selected.
    pub fn connecting(&self, id: &str, label: &str) {
        self.serial.set(self.serial.get() + 1);
        *self.shown.borrow_mut() = Some(Shown {
            id: id.to_string(),
            label: label.to_string(),
            error: None,
            attempt: None,
            diagnosis: None,
        });
        self.render();
    }

    /// The last attempt to reach `id` failed with `error`. Checks the links
    /// again when the last check is older than `RECHECK`.
    pub fn failed(self: &Rc<Self>, id: &str, label: &str, error: &str) {
        let stale = {
            let mut shown = self.shown.borrow_mut();
            if shown.as_ref().is_none_or(|shown| shown.id != id) {
                self.serial.set(self.serial.get() + 1);
                *shown = Some(Shown {
                    id: id.to_string(),
                    label: label.to_string(),
                    error: None,
                    attempt: None,
                    diagnosis: None,
                });
            }
            let shown = shown.as_mut().expect("set above");
            shown.label = label.to_string();
            shown.error = Some(error.to_string());
            shown.attempt = Some(chrono::Local::now());
            shown
                .diagnosis
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() >= RECHECK)
        };
        self.render();
        if stale {
            self.check();
        }
    }

    /// Check every link now, unless a check for this PC is already running.
    fn check(self: &Rc<Self>) {
        let serial = self.serial.get();
        let Some((id, label)) = self
            .shown
            .borrow()
            .as_ref()
            .filter(|shown| shown.error.is_some())
            .map(|shown| (shown.id.clone(), shown.label.clone()))
        else {
            return;
        };
        if self.checking.get() == Some(serial) {
            return;
        }
        self.checking.set(Some(serial));
        self.render();
        let diagnose = Arc::clone(&self.diagnose.borrow());
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk4::gio::spawn_blocking(move || diagnose(&id, &label)).await;
            let Some(panel) = weak.upgrade() else {
                return;
            };
            if panel.checking.get() == Some(serial) {
                panel.checking.set(None);
            }
            if panel.serial.get() != serial {
                return;
            }
            if let (Ok(diagnosis), Some(shown)) = (result, panel.shown.borrow_mut().as_mut()) {
                shown.diagnosis = Some((Instant::now(), diagnosis));
            }
            panel.render();
        });
    }

    fn render(&self) {
        let shown = self.shown.borrow();
        let Some(shown) = shown.as_ref() else {
            return;
        };
        let diagnosis = shown.diagnosis.as_ref().map(|(_, diagnosis)| diagnosis);
        let advice = advice(&shown.label, shown.error.as_deref(), diagnosis);
        paint_mark(&self.mark, advice.mark);
        set_text(&self.title, &advice.title);
        set_text(&self.hint, &advice.hint);
        let checking = self.checking.get() == Some(self.serial.get());
        match (shown.error.as_deref(), diagnosis) {
            (Some(error), Some(diagnosis)) => {
                for (row, check) in self.rows.iter().zip(checks(&shown.label, error, diagnosis)) {
                    row.show(check.as_ref());
                }
            }
            _ => {
                self.rows[0].show(Some(&Check {
                    mark: Mark::Working,
                    title: "Checking the connection".into(),
                    detail: "This PC's network, Tailscale, and whether that PC answers".into(),
                }));
                for row in &self.rows[1..] {
                    row.show(None);
                }
            }
        }
        let failed = shown.error.is_some();
        self.checks.set_visible(failed);
        self.retry.set_visible(failed);
        self.retry.set_sensitive(!checking);
        self.retry
            .set_label(if checking { "Checking…" } else { "↻ Retry now" });
        self.pair.set_visible(advice.pair_again);
        // When a new pairing is the fix, it is the action to take.
        let (primary, secondary) = match advice.pair_again {
            true => (&self.pair, &self.retry),
            false => (&self.retry, &self.pair),
        };
        primary.add_css_class("launcher-btn-primary");
        secondary.remove_css_class("launcher-btn-primary");
        self.footer.set_visible(shown.attempt.is_some());
        if let Some(attempt) = shown.attempt {
            set_text(
                &self.footer,
                &format!(
                    "Last attempt {}{}",
                    attempt.format("%H:%M:%S"),
                    if advice.pair_again { "" } else { " · retrying automatically" }
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(host: &str) -> Endpoint {
        Endpoint::new(host, 8759).unwrap()
    }

    fn wifi(address: &str) -> Interface {
        Interface {
            name: "wlo1".into(),
            kind: LinkKind::Wifi,
            address: address.parse().unwrap(),
            prefix: 24,
            ssid: Some("Home".into()),
        }
    }

    fn running() -> Tailscale {
        Tailscale::Running {
            ip: Some("100.112.186.79".into()),
            tailnet: Some("home.example".into()),
        }
    }

    /// This PC on Wi-Fi 192.168.50.0/24 with Tailscale on, and that PC at
    /// `host`, reached from `source` with `reach`.
    fn diagnosis(host: &str, source: Option<&str>, reach: Reach) -> Diagnosis {
        let address = host.parse().ok();
        Diagnosis {
            interfaces: Some(vec![wifi("192.168.50.177")]),
            tailscale: running(),
            device: None,
            remote: Some(Remote {
                endpoint: endpoint(host),
                address,
                source: source.map(|s| s.parse().unwrap()),
                reach,
            }),
        }
    }

    const FAILED: &str = "connection_failed_or_pin_mismatch";

    #[test]
    fn addresses_are_told_apart() {
        let kind = |ip: &str| address_kind(ip.parse().unwrap());
        assert_eq!(kind("100.64.0.1"), AddressKind::Tailscale);
        assert_eq!(kind("100.127.255.73"), AddressKind::Tailscale);
        assert_eq!(kind("100.128.0.1"), AddressKind::Internet);
        assert_eq!(kind("100.63.0.1"), AddressKind::Internet);
        assert_eq!(kind("fd7a:115c:a1e0::e635:ba51"), AddressKind::Tailscale);
        assert_eq!(kind("192.168.50.219"), AddressKind::Local);
        assert_eq!(kind("10.1.2.3"), AddressKind::Local);
        assert_eq!(kind("172.20.0.4"), AddressKind::Local);
        assert_eq!(kind("fd00::1"), AddressKind::Local);
        assert_eq!(kind("127.0.0.1"), AddressKind::ThisPc);
        assert_eq!(kind("8.8.8.8"), AddressKind::Internet);
        let interface = wifi("192.168.50.177");
        assert!(interface.contains("192.168.50.219".parse().unwrap()));
        assert!(!interface.contains("192.168.51.2".parse().unwrap()));
        assert_eq!(interface.subnet(), "192.168.50.0/24");
        assert_eq!(network("10.9.8.7".parse().unwrap(), 0), Ipv4Addr::UNSPECIFIED);
        assert_eq!(network("10.9.8.7".parse().unwrap(), 32), "10.9.8.7".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn interfaces_keep_physical_ipv4_links_only() {
        let document: Value = serde_json::from_str(
            r#"[
            {"ifname":"lo","link_type":"loopback","addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host"}]},
            {"ifname":"enp3s0","link_type":"ether","addr_info":[]},
            {"ifname":"wlo1","link_type":"ether","addr_info":[
                {"family":"inet","local":"192.168.50.177","prefixlen":24,"scope":"global"},
                {"family":"inet","local":"169.254.3.4","prefixlen":16,"scope":"link"}]},
            {"ifname":"tailscale0","link_type":"none","addr_info":[{"family":"inet","local":"100.112.186.79","prefixlen":32,"scope":"global"}]},
            {"ifname":"docker0","link_type":"ether","addr_info":[{"family":"inet","local":"172.17.0.1","prefixlen":16,"scope":"global"}]}
        ]"#,
        )
        .unwrap();
        let found = parse_interfaces(&document, |name| match name {
            "wlo1" => Some(LinkKind::Wifi),
            "enp3s0" => Some(LinkKind::Ethernet),
            _ => None,
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "wlo1");
        assert_eq!(found[0].address, "192.168.50.177".parse::<Ipv4Addr>().unwrap());
        assert_eq!(found[0].prefix, 24);
        let mut named = found[0].clone();
        named.ssid = Some("glad-rog".into());
        assert_eq!(named.describe(), "Wi-Fi “glad-rog” · 192.168.50.177/24");
    }

    #[test]
    fn wifi_names_come_from_iw_or_iwctl() {
        let iw = "Connected to a0:36:bc:b4:30:98 (on wlo1)\n\tSSID: glad-rog-gt6_5G2\n\tfreq: 5640.0\n";
        assert_eq!(parse_ssid(iw, "SSID:").as_deref(), Some("glad-rog-gt6_5G2"));
        assert_eq!(parse_ssid("Not connected.\n", "SSID:"), None);
        let iwctl = "\u{1b}[1;90m      Station: wlan0\u{1b}[0m\n  Settable  Property   Value\n            \
                     State                 connected\n            Connected network     Home Net   \n";
        assert_eq!(parse_ssid(iwctl, "Connected network").as_deref(), Some("Home Net"));
    }

    #[test]
    fn tailscale_status_names_the_state_and_that_pc() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let status: Value = serde_json::from_str(
            r#"{"BackendState":"Running","TailscaleIPs":["100.112.186.79","fd7a:115c:a1e0::1"],
            "Self":{"TailscaleIPs":["100.112.186.79"]},"CurrentTailnet":{"Name":"home.example"},
            "Peer":{
              "a":{"HostName":"gladimdim-b9","DNSName":"gladimdim-b9.tail.ts.net.","TailscaleIPs":["100.67.193.30","fd7a:115c:a1e0::2"],"Online":false,"LastSeen":"2026-09-25T19:00:00Z"},
              "b":{"HostName":"phone","DNSName":"phone.tail.ts.net.","TailscaleIPs":["100.103.159.83"],"Online":true,"LastSeen":"0001-01-01T00:00:00Z"}}}"#,
        )
        .unwrap();
        // By address first.
        let (state, device) =
            parse_tailscale(&status, "Anything", Some("100.103.159.83".parse().unwrap()), now);
        assert_eq!(state, running());
        let device = device.unwrap();
        assert_eq!((device.name.as_str(), device.online, device.last_seen), ("phone", true, None));
        // A LAN pairing is matched by its label, and an offline device says when.
        let (_, device) = parse_tailscale(&status, "GLADIMDIM-B9", Some("192.168.50.219".parse().unwrap()), now);
        let device = device.unwrap();
        assert_eq!(device.ip.as_deref(), Some("100.67.193.30"));
        assert_eq!(device.last_seen, Some(17 * 3600));
        assert_eq!(device.state(), "is offline on the tailnet · last seen 17 h ago");
        assert_eq!(parse_tailscale(&status, "desk", None, now).1, None);
        let stopped: Value = serde_json::from_str(r#"{"BackendState":"Stopped","Peer":null}"#).unwrap();
        assert_eq!(parse_tailscale(&stopped, "desk", None, now), (Tailscale::Stopped, None));
        let login: Value = serde_json::from_str(r#"{"BackendState":"NeedsLogin"}"#).unwrap();
        assert_eq!(parse_tailscale(&login, "desk", None, now).0, Tailscale::NeedsLogin);
    }

    #[test]
    fn a_closed_port_and_an_open_one_are_told_apart() {
        // A port nothing listens on: bind one, then close it.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let remote = probe(&Endpoint::new("127.0.0.1", port).unwrap());
        assert_eq!(remote.reach, Reach::Refused);
        assert_eq!(remote.source, Some("127.0.0.1".parse().unwrap()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(probe(&Endpoint::new("127.0.0.1", port).unwrap()).reach, Reach::Answered);
    }

    #[test]
    fn advice_names_the_likeliest_cause() {
        let title = |d: &Diagnosis| advice("Desk", Some(FAILED), Some(d)).title;
        // No answer on this PC's own network.
        let mut d = diagnosis("192.168.50.219", Some("192.168.50.177"), Reach::TimedOut);
        let quiet = advice("Desk", Some(FAILED), Some(&d));
        assert_eq!((quiet.mark, quiet.title.as_str()), (Mark::Fail, "Desk is not answering"));
        assert!(quiet.hint.contains("192.168.50.219:8759"));
        assert!(!quiet.pair_again);
        // Tailscale saw it go: it is asleep.
        d.device = Some(TailnetDevice { name: "desk".into(), ip: Some("100.67.193.30".into()), online: false, last_seen: Some(7200) });
        assert!(advice("Desk", Some(FAILED), Some(&d)).hint.contains("last saw it 2 h ago"));
        // Online on the tailnet: offer that address.
        d.device.as_mut().unwrap().online = true;
        assert!(advice("Desk", Some(FAILED), Some(&d)).hint.contains("online on Tailscale at 100.67.193.30"));
        // Refused: up, but no SUPER DESKTOP.
        let d = diagnosis("192.168.50.219", Some("192.168.50.177"), Reach::Refused);
        assert_eq!(title(&d), "SUPER DESKTOP is not running on Desk");
        // Answered but the TLS pin failed: someone else, or reinstalled.
        let d = diagnosis("192.168.50.219", Some("192.168.50.177"), Reach::Answered);
        let pin = advice("Desk", Some(FAILED), Some(&d));
        assert_eq!(pin.title, "Cannot verify Desk");
        assert!(pin.pair_again);
        // Another LAN.
        let d = diagnosis("10.0.0.5", Some("192.168.50.177"), Reach::TimedOut);
        let other = advice("Desk", Some(FAILED), Some(&d));
        assert_eq!((other.mark, other.title.as_str()), (Mark::Warn, "Desk is on another network"));
        assert!(other.hint.contains("192.168.50.0/24"));
        // A Tailscale pairing with Tailscale off here.
        let mut d = diagnosis("100.67.193.30", Some("192.168.50.177"), Reach::TimedOut);
        d.tailscale = Tailscale::Stopped;
        assert_eq!(title(&d), "Tailscale is off on this PC");
        // A MagicDNS name does not resolve with Tailscale off.
        let mut d = diagnosis("desk.tail.ts.net", None, Reach::Unresolved);
        d.tailscale = Tailscale::NotRunning;
        assert_eq!(title(&d), "Tailscale is off on this PC");
        // Tailscale on, but that address is not in this tailnet.
        let d = diagnosis("100.67.193.30", Some("100.112.186.79"), Reach::TimedOut);
        assert_eq!(title(&d), "Desk is not on your tailnet");
        // In the tailnet, offline.
        let mut d = diagnosis("100.67.193.30", Some("100.112.186.79"), Reach::TimedOut);
        d.device = Some(TailnetDevice { name: "desk".into(), ip: Some("100.67.193.30".into()), online: false, last_seen: None });
        assert_eq!(title(&d), "Desk is offline");
        // This PC has no network at all.
        let mut d = diagnosis("192.168.50.219", None, Reach::NoRoute);
        d.interfaces = Some(Vec::new());
        d.tailscale = Tailscale::Stopped;
        assert_eq!(title(&d), "This PC is offline");
        // Interfaces that could not be read are not "offline".
        d.interfaces = None;
        assert_eq!(title(&d), "No route to Desk");
        let d = diagnosis("desk.example", None, Reach::Unresolved);
        assert_eq!(title(&d), "Cannot find Desk");
        // Before the checks finish.
        assert_eq!(advice("Desk", Some(FAILED), None).title, "Cannot reach Desk");
    }

    #[test]
    fn advice_for_errors_the_network_cannot_explain() {
        let d = diagnosis("192.168.50.219", Some("192.168.50.177"), Reach::Answered);
        let say = |error: &str| advice("Desk", Some(error), Some(&d));
        assert_eq!(advice("Desk", None, None).title, "Connecting to Desk…");
        assert_eq!(advice("Desk", None, None).mark, Mark::Working);
        let pairing = say("peer_revoked_or_expired");
        assert_eq!((pairing.title.as_str(), pairing.pair_again), ("Desk needs pairing again", true));
        assert!(say("peer_not_found").pair_again);
        assert_eq!(say("update_remote_super_desktop").title, "Desk needs an update");
        assert_eq!(say("peer_endpoint_unavailable").title, "Desk needs an update");
        let identity = say("peer_identity_changed");
        assert_eq!((identity.title.as_str(), identity.pair_again), ("Cannot verify Desk", true));
        assert_eq!(say("peer_store_io_error").title, "Cannot open Desk");
        assert_eq!(say("unsafe_peer_store_permissions").title, "Cannot open Desk");
        let busy = say("remote_desktop_unavailable");
        assert_eq!((busy.mark, busy.title.as_str()), (Mark::Warn, "Desk is not ready"));
    }

    #[test]
    fn checks_walk_the_route_link_by_link() {
        let mut d = diagnosis("192.168.50.219", Some("192.168.50.177"), Reach::TimedOut);
        d.interfaces.as_mut().unwrap()[0].ssid = Some("Home".into());
        let [network, tailscale, route, port] = checks("Desk", FAILED, &d);
        let network = network.unwrap();
        assert_eq!((network.mark, network.detail.as_str()), (Mark::Ok, "Wi-Fi “Home” · 192.168.50.177/24"));
        let tailscale = tailscale.unwrap();
        assert_eq!(tailscale.mark, Mark::Ok);
        assert_eq!(tailscale.detail, "On · 100.112.186.79 · tailnet home.example");
        let route = route.unwrap();
        assert_eq!((route.mark, route.title.as_str()), (Mark::Ok, "Route to Desk"));
        assert_eq!(route.detail, "Same local network, over Wi-Fi (192.168.50.0/24)");
        let port = port.unwrap();
        assert_eq!((port.mark, port.title.as_str()), (Mark::Fail, "Desk · 192.168.50.219:8759"));
        assert_eq!(port.detail, "No answer on port 8759 within 3 s");

        // Tailscale off does not matter for a LAN pairing...
        d.tailscale = Tailscale::Stopped;
        assert_eq!(checks("Desk", FAILED, &d)[1].as_ref().unwrap().mark, Mark::Off);
        // ...and fails a Tailscale one, whose route then leaves over Wi-Fi.
        let mut d = diagnosis("100.67.193.30", Some("192.168.50.177"), Reach::TimedOut);
        d.tailscale = Tailscale::NotRunning;
        let [_, tailscale, route, _] = checks("Desk", FAILED, &d);
        let tailscale = tailscale.unwrap();
        assert_eq!((tailscale.mark, tailscale.detail.as_str()), (Mark::Fail, "Off · tailscaled is not running"));
        assert_eq!(route.unwrap().mark, Mark::Fail);
        // Through Tailscale, with that PC named in the tailnet.
        let mut d = diagnosis("100.67.193.30", Some("100.112.186.79"), Reach::Answered);
        d.device = Some(TailnetDevice { name: "desk".into(), ip: Some("100.67.193.30".into()), online: true, last_seen: None });
        let [_, tailscale, route, port] = checks("Desk", FAILED, &d);
        assert!(tailscale.unwrap().detail.ends_with("\ndesk is online on the tailnet · 100.67.193.30"));
        assert_eq!(route.unwrap().detail, "Through Tailscale, from 100.112.186.79");
        // It answers, but the secure connection failed; for another error it is fine.
        assert_eq!(port.unwrap().mark, Mark::Warn);
        assert_eq!(checks("Desk", "update_remote_super_desktop", &d)[3].as_ref().unwrap().mark, Mark::Ok);
        // Another LAN goes through the router.
        let d = diagnosis("10.0.0.5", Some("192.168.50.177"), Reach::TimedOut);
        assert_eq!(checks("Desk", FAILED, &d)[2].as_ref().unwrap().mark, Mark::Warn);
        // Nothing to say about a route to a name that does not resolve.
        let d = diagnosis("desk.example", None, Reach::Unresolved);
        let [_, _, route, port] = checks("Desk", FAILED, &d);
        assert!(route.is_none());
        assert_eq!(port.unwrap().detail, "desk.example does not resolve from this PC");
        // No saved address: only this PC's side.
        let mut d = diagnosis("192.168.50.219", None, Reach::TimedOut);
        d.remote = None;
        d.interfaces = Some(Vec::new());
        let [network, _, route, port] = checks("Desk", "peer_not_found", &d);
        assert_eq!(network.unwrap().mark, Mark::Warn);
        assert!(route.is_none() && port.is_none());
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(125), "2 min ago");
        assert_eq!(ago(3 * 3600 + 5), "3 h ago");
        assert_eq!(ago(5 * 86400), "5 days ago");
    }

    #[test]
    fn connection_panel_explains_a_failure() {
        crate::gtk_test::run_in_child_process("connection_panel::tests::panel_inner");
    }

    #[test]
    fn panel_inner() {
        if !crate::gtk_test::is_child() {
            return;
        }
        gtk4::init().unwrap();
        crate::styles::apply_styles();
        let panel = ConnectionPanel::new();
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        panel.set_diagnose(Arc::new({
            let runs = Arc::clone(&runs);
            move |_: &str, _: &str| {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                diagnosis("192.168.50.219", Some("192.168.50.177"), Reach::Refused)
            }
        }));
        let pump = |done: &dyn Fn() -> bool| {
            let until = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < until, "timed out");
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let visible_rows = |panel: &ConnectionPanel| panel.rows.iter().filter(|row| row.widget.is_visible()).count();
        let id = "a".repeat(32);
        panel.connecting(&id, "Desk");
        assert_eq!(panel.title.text(), "Connecting to Desk…");
        assert!(!panel.checks.is_visible() && !panel.retry.is_visible() && !panel.footer.is_visible());

        panel.failed(&id, "Desk", FAILED);
        // At once: the failure, and the checks it started.
        assert_eq!(panel.title.text(), "Cannot reach Desk");
        assert!(panel.checks.is_visible() && panel.retry.is_visible() && !panel.retry.is_sensitive());
        assert!(panel.footer.text().starts_with("Last attempt "));
        pump(&|| panel.retry.is_sensitive());
        assert_eq!(panel.title.text(), "SUPER DESKTOP is not running on Desk");
        assert_eq!(visible_rows(&panel), 4);
        assert_eq!(panel.rows[3].title.text(), "Desk · 192.168.50.219:8759");
        assert!(panel.rows[3].mark.has_css_class("invite-fail"));
        assert!(panel.rows[0].mark.has_css_class("invite-ok") && !panel.rows[0].mark.has_css_class("invite-working"));
        assert!(!panel.pair.is_visible());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);

        // The poll fails again soon after: the checks are fresh, not rerun.
        panel.failed(&id, "Desk", FAILED);
        pump(&|| !glib::MainContext::default().pending());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Retry reruns them, and tells the owner.
        let retried = Rc::new(Cell::new(false));
        panel.connect_retry({
            let retried = Rc::clone(&retried);
            move || retried.set(true)
        });
        panel.retry.emit_clicked();
        assert!(retried.get() && !panel.retry.is_sensitive());
        pump(&|| panel.retry.is_sensitive());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A pairing problem offers to pair again.
        let paired = Rc::new(Cell::new(false));
        panel.connect_pair({
            let paired = Rc::clone(&paired);
            move || paired.set(true)
        });
        panel.failed(&id, "Desk", "peer_revoked_or_expired");
        assert_eq!(panel.title.text(), "Desk needs pairing again");
        assert!(panel.pair.is_visible());
        assert!(!panel.footer.text().contains("retrying"));
        panel.pair.emit_clicked();
        assert!(paired.get());

        // Checks for a PC that is no longer on screen are dropped.
        panel.failed(&id, "Desk", FAILED);
        panel.retry.emit_clicked();
        panel.connecting(&"b".repeat(32), "Other");
        pump(&|| panel.checking.get().is_none());
        assert_eq!(panel.title.text(), "Connecting to Other…");
        assert!(!panel.checks.is_visible());

        // It never asks the overlay for room: narrow, short screens scroll.
        panel.failed(&id, "A very long paired computer name that goes on and on", FAILED);
        pump(&|| panel.retry.is_sensitive());
        assert!(panel.widget.measure(gtk4::Orientation::Horizontal, -1).0 <= 300);
        assert!(panel.widget.measure(gtk4::Orientation::Vertical, 300).0 <= 100);
    }
}
