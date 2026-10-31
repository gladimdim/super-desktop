//! What a phone needs to wake this PC later: the hardware address and IPv4
//! broadcast address of each physical network card, sent in the harness list
//! while the PC is online. The phone remembers them and sends the Wake-on-LAN
//! magic packet itself when the PC no longer answers.
//!
//! Only the logged-in harness list carries this; the open ping does not, so
//! the hardware addresses never reach an unpaired device.
use super::now_epoch;
use crate::connection_panel::{link_kind, LinkKind};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// Network cards change rarely; the harness list is built every second.
const CACHE_SECONDS: f64 = 60.0;
static WAKE_CACHE: Mutex<Option<(f64, serde_json::Value)>> = Mutex::new(None);

/// Whether the card is set to wake the PC on a magic packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WakeOnLan {
    Enabled,
    Disabled,
    Unknown,
}

impl WakeOnLan {
    fn as_str(self) -> &'static str {
        match self {
            WakeOnLan::Enabled => "enabled",
            WakeOnLan::Disabled => "disabled",
            WakeOnLan::Unknown => "unknown",
        }
    }
}

/// `{"interfaces":[…]}` for the harness list, refreshed at most once a minute.
pub(super) fn wake_document() -> serde_json::Value {
    let mut cache = WAKE_CACHE.lock().unwrap();
    if let Some((time, document)) = cache.as_ref() {
        if now_epoch() - time < CACHE_SECONDS {
            return document.clone();
        }
    }
    let interfaces = Command::new("ip")
        .args(["-j", "address", "show", "up"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .map(|value| wake_interfaces(&value, link_kind, wake_on_lan))
        .unwrap_or_default();
    let document = serde_json::json!({ "interfaces": interfaces });
    *cache = Some((now_epoch(), document.clone()));
    document
}

/// Physical cards with a hardware address and a global IPv4 address, Ethernet
/// first because Wi-Fi cards rarely wake a PC. Docker and libvirt bridges,
/// veth pairs, VPN tunnels, Tailscale and loopback have no card to wake.
pub(super) fn wake_interfaces(
    document: &serde_json::Value,
    kind_of: impl Fn(&str) -> Option<LinkKind>,
    wake_of: impl Fn(&str) -> WakeOnLan,
) -> Vec<serde_json::Value> {
    let mut found = Vec::new();
    for interface in document.as_array().into_iter().flatten() {
        let name = interface["ifname"].as_str().unwrap_or("");
        if name.is_empty() || interface["link_type"] != "ether" || name.starts_with("tailscale") {
            continue;
        }
        let Some(mac) = interface["address"].as_str().and_then(normalized_mac) else { continue };
        let Some(kind) = kind_of(name) else { continue };
        for info in interface["addr_info"].as_array().into_iter().flatten() {
            if info["family"] != "inet" || info["scope"] != "global" {
                continue;
            }
            let Some(address) = info["local"].as_str().and_then(|a| a.parse::<Ipv4Addr>().ok()) else { continue };
            let prefix = info["prefixlen"].as_u64().unwrap_or(32).min(32) as u8;
            let broadcast = info["broadcast"]
                .as_str()
                .and_then(|b| b.parse::<Ipv4Addr>().ok())
                .unwrap_or_else(|| broadcast_of(address, prefix));
            found.push(serde_json::json!({
                "interface": name,
                "kind": match kind {
                    LinkKind::Ethernet => "ethernet",
                    LinkKind::Wifi => "wifi",
                    LinkKind::Other => "other",
                },
                "mac": mac,
                "address": address.to_string(),
                "broadcast": broadcast.to_string(),
                "wakeOnLan": wake_of(name).as_str(),
            }));
            // One entry per card: the packet is addressed to the card, not an IP.
            break;
        }
    }
    found.sort_by_key(|v| (v["kind"] != "ethernet", v["interface"].as_str().unwrap_or("").to_string()));
    found
}

/// Lowercase `aa:bb:cc:dd:ee:ff`, or `None` for anything that cannot be woken.
fn normalized_mac(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.split(':').collect();
    let valid = parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()));
    let mac = text.to_ascii_lowercase();
    (valid && mac != "00:00:00:00:00:00" && mac != "ff:ff:ff:ff:ff:ff").then_some(mac)
}

fn broadcast_of(address: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let host_bits = if prefix >= 32 { 0 } else { u32::MAX >> prefix };
    Ipv4Addr::from(u32::from(address) | host_bits)
}

/// `ethtool`'s `Wake-on:` line when it can be read; otherwise the kernel's
/// device wakeup flag, which only tells for sure when waking is off.
fn wake_on_lan(interface: &str) -> WakeOnLan {
    let ethtool = Command::new("ethtool")
        .arg(interface)
        .output()
        .ok()
        .and_then(|o| parse_ethtool_wake_on(&String::from_utf8_lossy(&o.stdout)));
    if let Some(state) = ethtool {
        return state;
    }
    let path = Path::new("/sys/class/net").join(interface).join("device/power/wakeup");
    match std::fs::read_to_string(path).map(|s| s.trim().to_string()).as_deref() {
        Ok("disabled") => WakeOnLan::Disabled,
        _ => WakeOnLan::Unknown,
    }
}

/// `Wake-on: g` (magic packet) is enabled; any other setting, such as `d`, is not.
fn parse_ethtool_wake_on(output: &str) -> Option<WakeOnLan> {
    let modes = output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Wake-on:"))?
        .trim();
    Some(if modes.contains('g') { WakeOnLan::Enabled } else { WakeOnLan::Disabled })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip_document() -> serde_json::Value {
        serde_json::json!([
            {"ifname":"lo","link_type":"loopback","address":"00:00:00:00:00:00",
             "addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host"}]},
            {"ifname":"wlo1","link_type":"ether","address":"E8:8D:A6:E0:89:93",
             "addr_info":[{"family":"inet","local":"192.168.50.40","prefixlen":24,"broadcast":"192.168.50.255","scope":"global"},
                          {"family":"inet6","local":"fe80::1","prefixlen":64,"scope":"link"}]},
            {"ifname":"enp5s0","link_type":"ether","address":"aa:bb:cc:dd:ee:01",
             "addr_info":[{"family":"inet6","local":"fe80::2","prefixlen":64,"scope":"link"},
                          {"family":"inet","local":"10.0.3.7","prefixlen":22,"scope":"global"},
                          {"family":"inet","local":"10.0.3.8","prefixlen":22,"scope":"global"}]},
            {"ifname":"docker0","link_type":"ether","address":"02:42:ac:11:00:01",
             "addr_info":[{"family":"inet","local":"172.17.0.1","prefixlen":16,"broadcast":"172.17.255.255","scope":"global"}]},
            {"ifname":"tailscale0","link_type":"none",
             "addr_info":[{"family":"inet","local":"100.78.110.32","prefixlen":32,"scope":"global"}]},
            {"ifname":"enp6s0","link_type":"ether","address":"aa:bb:cc:dd:ee:02",
             "addr_info":[{"family":"inet6","local":"fe80::3","prefixlen":64,"scope":"link"}]}
        ])
    }

    fn kind_of(name: &str) -> Option<LinkKind> {
        match name {
            "wlo1" => Some(LinkKind::Wifi),
            "enp5s0" | "enp6s0" => Some(LinkKind::Ethernet),
            _ => None,
        }
    }

    #[test]
    fn wake_lists_physical_cards_with_mac_and_broadcast_ethernet_first() {
        let found = wake_interfaces(&ip_document(), kind_of, |name| {
            if name == "enp5s0" { WakeOnLan::Enabled } else { WakeOnLan::Disabled }
        });
        assert_eq!(found, vec![
            serde_json::json!({"interface":"enp5s0","kind":"ethernet","mac":"aa:bb:cc:dd:ee:01",
                "address":"10.0.3.7","broadcast":"10.0.3.255","wakeOnLan":"enabled"}),
            serde_json::json!({"interface":"wlo1","kind":"wifi","mac":"e8:8d:a6:e0:89:93",
                "address":"192.168.50.40","broadcast":"192.168.50.255","wakeOnLan":"disabled"}),
        ]);
    }

    #[test]
    fn wake_skips_cards_without_a_usable_hardware_address() {
        let document = serde_json::json!([
            {"ifname":"enp5s0","link_type":"ether","address":"00:00:00:00:00:00",
             "addr_info":[{"family":"inet","local":"10.0.0.2","prefixlen":24,"scope":"global"}]},
            {"ifname":"enp6s0","link_type":"ether","address":"not-a-mac",
             "addr_info":[{"family":"inet","local":"10.0.1.2","prefixlen":24,"scope":"global"}]},
            {"ifname":"enp7s0","link_type":"ether",
             "addr_info":[{"family":"inet","local":"10.0.2.2","prefixlen":24,"scope":"global"}]}
        ]);
        assert!(wake_interfaces(&document, |_| Some(LinkKind::Ethernet), |_| WakeOnLan::Unknown).is_empty());
        assert!(wake_interfaces(&serde_json::json!({}), |_| Some(LinkKind::Ethernet), |_| WakeOnLan::Unknown).is_empty());
    }

    #[test]
    fn wake_broadcast_is_computed_when_ip_omits_it() {
        assert_eq!(broadcast_of("192.168.1.20".parse().unwrap(), 24), "192.168.1.255".parse::<Ipv4Addr>().unwrap());
        assert_eq!(broadcast_of("10.0.3.7".parse().unwrap(), 22), "10.0.3.255".parse::<Ipv4Addr>().unwrap());
        assert_eq!(broadcast_of("10.1.2.3".parse().unwrap(), 32), "10.1.2.3".parse::<Ipv4Addr>().unwrap());
        assert_eq!(broadcast_of("10.1.2.3".parse().unwrap(), 0), Ipv4Addr::BROADCAST);
    }

    #[test]
    fn wake_reads_the_ethtool_wake_on_setting_not_the_supported_modes() {
        let enabled = "Settings for enp5s0:\n\tSupports Wake-on: pumbg\n\tWake-on: g\n\tLink detected: yes\n";
        let disabled = "Settings for enp5s0:\n\tSupports Wake-on: pumbg\n\tWake-on: d\n";
        assert_eq!(parse_ethtool_wake_on(enabled), Some(WakeOnLan::Enabled));
        assert_eq!(parse_ethtool_wake_on(disabled), Some(WakeOnLan::Disabled));
        assert_eq!(parse_ethtool_wake_on("Cannot get wake-on-lan settings: Operation not permitted\n"), None);
    }
}
