//! Interface discovery from `/sys/class/net`.
//!
//! Kind, link state, MAC and MTU come straight from sysfs (cheap, no fork). IPv4
//! addresses and wireless detail need `ip -j` and `iw`, and both are *optional*:
//! on a host without them the interface list is still complete, just without
//! addresses.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::networking::runner::Runner;

pub const SYS_CLASS_NET: &str = "/sys/class/net";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    Ethernet,
    Wifi,
    Bridge,
    Vlan,
    Loopback,
    /// Anything without a physical device behind it: `docker0`, `tun0`, `veth…`.
    Virtual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddressInfo {
    pub cidr: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct WifiInfo {
    pub ssid: Option<String>,
    /// `managed`, `AP`, `monitor`, …
    pub mode: Option<String>,
    pub channel: Option<u32>,
    pub frequency_mhz: Option<u32>,
    /// Any radio reporting AP support in `iw list`.
    pub ap_capable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub kind: InterfaceKind,
    pub operstate: String,
    pub carrier: Option<bool>,
    pub speed_mbps: Option<u32>,
    pub mac: Option<String>,
    pub mtu: Option<u32>,
    pub addresses: Vec<AddressInfo>,
    pub wifi: Option<WifiInfo>,
}

/// Enumerate every interface the kernel knows about, sorted by name.
pub fn discover(runner: &dyn Runner) -> Vec<InterfaceInfo> {
    discover_from(Path::new(SYS_CLASS_NET), runner)
}

/// Testable core: `sys_dir` is normally `/sys/class/net`.
pub fn discover_from(sys_dir: &Path, runner: &dyn Runner) -> Vec<InterfaceInfo> {
    let Ok(entries) = std::fs::read_dir(sys_dir) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();

    let addresses = addresses_by_interface(runner);
    let ap_capable = ap_support(runner);
    // Kind detection needs the full name list (to spot `eth1.10` on `eth1`),
    // so keep a copy while the originals are consumed by the map.
    let known = names.clone();

    names
        .into_iter()
        .map(|name| {
            let dir = sys_dir.join(&name);
            let kind = detect_kind(&name, &dir, &known);
            let wifi = if kind == InterfaceKind::Wifi {
                Some(wifi_info(runner, &name, ap_capable))
            } else {
                None
            };

            InterfaceInfo {
                addresses: addresses.get(&name).cloned().unwrap_or_default(),
                operstate: read_trim(&dir.join("operstate"))
                    .unwrap_or_else(|| "unknown".to_string()),
                carrier: read_trim(&dir.join("carrier")).map(|value| value.trim() == "1"),
                speed_mbps: read_trim(&dir.join("speed"))
                    .and_then(|value| value.trim().parse::<u32>().ok())
                    // Down interfaces report 4294967295 (u32 -1).
                    .filter(|speed| *speed > 0 && *speed < 1_000_000),
                mac: read_trim(&dir.join("address")),
                mtu: read_trim(&dir.join("mtu")).and_then(|value| value.trim().parse::<u32>().ok()),
                name,
                kind,
                wifi,
            }
        })
        .collect()
}

fn detect_kind(name: &str, dir: &Path, all: &[String]) -> InterfaceKind {
    if name == "lo" {
        return InterfaceKind::Loopback;
    }
    if dir.join("bridge").is_dir() {
        return InterfaceKind::Bridge;
    }
    if dir.join("wireless").is_dir() || dir.join("phy80211").is_dir() {
        return InterfaceKind::Wifi;
    }
    // `eth1.10` is a VLAN on `eth1`, but `docker0` and `tun0` are not.
    if let Some((parent, _)) = name.rsplit_once('.')
        && !parent.is_empty()
        && all.iter().any(|candidate| candidate == parent)
    {
        return InterfaceKind::Vlan;
    }
    if dir.join("device").exists() {
        return InterfaceKind::Ethernet;
    }
    if dir.join("tun_flags").exists() || name.starts_with("tun") || name.starts_with("tap") {
        return InterfaceKind::Virtual;
    }
    InterfaceKind::Virtual
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// Addresses via `ip -j addr`
// ---------------------------------------------------------------------------

fn addresses_by_interface(runner: &dyn Runner) -> HashMap<String, Vec<AddressInfo>> {
    let Ok(output) = runner.call("ip", &["-j", "-4", "addr", "show"]) else {
        return HashMap::new();
    };
    if !output.success() {
        return HashMap::new();
    }
    parse_ip_addr_json(&output.stdout)
}

/// Parse the JSON array emitted by `ip -j -4 addr show`.
pub fn parse_ip_addr_json(json: &str) -> HashMap<String, Vec<AddressInfo>> {
    let mut map: HashMap<String, Vec<AddressInfo>> = HashMap::new();
    let Ok(entries) = serde_json::from_str::<serde_json::Value>(json) else {
        return map;
    };
    let Some(interfaces) = entries.as_array() else {
        return map;
    };

    for interface in interfaces {
        let Some(name) = interface.get("ifname").and_then(|value| value.as_str()) else {
            continue;
        };
        let mut addresses = Vec::new();
        if let Some(addr_info) = interface
            .get("addr_info")
            .and_then(|value| value.as_array())
        {
            for info in addr_info {
                if info.get("family").and_then(|value| value.as_str()) != Some("inet") {
                    continue;
                }
                let Some(local) = info.get("local").and_then(|value| value.as_str()) else {
                    continue;
                };
                let prefix = info
                    .get("prefixlen")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(32);
                let scope = info
                    .get("scope")
                    .and_then(|value| value.as_str())
                    .unwrap_or("global");
                addresses.push(AddressInfo {
                    cidr: format!("{local}/{prefix}"),
                    scope: scope.to_string(),
                });
            }
        }
        map.insert(name.to_string(), addresses);
    }

    map
}

// ---------------------------------------------------------------------------
// Wireless via `iw`
// ---------------------------------------------------------------------------

fn wifi_info(runner: &dyn Runner, interface: &str, ap_capable: bool) -> WifiInfo {
    let mut info = WifiInfo {
        ap_capable,
        ..WifiInfo::default()
    };

    if let Ok(output) = runner.call("iw", &["dev", interface, "info"])
        && output.success()
    {
        let parsed = parse_iw_info(&output.stdout);
        info.ssid = parsed.ssid;
        info.mode = parsed.mode;
        info.channel = parsed.channel;
        info.frequency_mhz = parsed.frequency_mhz;
    }

    info
}

/// `iw dev <iface> info` text -> the fields the UI shows.
pub fn parse_iw_info(text: &str) -> WifiInfo {
    let mut info = WifiInfo::default();

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ssid ") {
            let ssid = printable(rest.trim());
            if !ssid.is_empty() {
                info.ssid = Some(ssid);
            }
        } else if let Some(rest) = trimmed.strip_prefix("type ") {
            let mode = rest.trim();
            if !mode.is_empty() {
                info.mode = Some(mode.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("channel ") {
            // `channel 36 (5180 MHz), width: 80 MHz, ...`
            let mut parts = rest.split_whitespace();
            info.channel = parts.next().and_then(|value| value.parse::<u32>().ok());
            if let Some(freq) = rest.split('(').nth(1)
                && let Some(value) = freq.split_whitespace().next()
            {
                info.frequency_mhz = value.parse::<u32>().ok();
            }
        }
    }

    info
}

/// One `iw list` call answers "can this box host an AP at all?".
fn ap_support(runner: &dyn Runner) -> bool {
    match runner.call("iw", &["list"]) {
        Ok(output) if output.success() => parse_iw_list_ap(&output.stdout),
        _ => false,
    }
}

/// True when any "Supported interface modes" section lists `* AP`.
pub fn parse_iw_list_ap(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == "* AP" || trimmed.starts_with("* AP ")
    })
}

/// SSIDs are arbitrary bytes; keep what a terminal can show, bounded.
fn printable(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::networking::runner::fake::FakeRunner;

    fn fixture_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-sysfs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write_interface(root: &Path, name: &str, files: &[(&str, &str)], dirs: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for (file, content) in files {
            std::fs::write(dir.join(file), content).expect("write");
        }
        for sub in dirs {
            std::fs::create_dir_all(dir.join(sub)).expect("mkdir");
        }
    }

    #[test]
    fn classifies_interfaces_from_sysfs() {
        let root = fixture_dir("kinds");
        write_interface(
            &root,
            "lo",
            &[("operstate", "unknown"), ("address", "00:00:00:00:00:00")],
            &[],
        );
        write_interface(
            &root,
            "eth0",
            &[("operstate", "up"), ("speed", "1000"), ("carrier", "1")],
            &["device"],
        );
        write_interface(&root, "eth1", &[("operstate", "down")], &["device"]);
        write_interface(&root, "br0", &[("operstate", "up")], &["bridge"]);
        write_interface(&root, "eth1.10", &[("operstate", "up")], &[]);
        write_interface(&root, "wlan0", &[("operstate", "dormant")], &["wireless"]);
        write_interface(&root, "docker0", &[("operstate", "up")], &[]);

        let runner = FakeRunner::new();
        let interfaces = discover_from(&root, &runner);
        let by_name: HashMap<&str, &InterfaceInfo> = interfaces
            .iter()
            .map(|iface| (iface.name.as_str(), iface))
            .collect();

        assert_eq!(by_name["lo"].kind, InterfaceKind::Loopback);
        assert_eq!(by_name["eth0"].kind, InterfaceKind::Ethernet);
        assert_eq!(by_name["br0"].kind, InterfaceKind::Bridge);
        assert_eq!(by_name["eth1.10"].kind, InterfaceKind::Vlan);
        assert_eq!(by_name["wlan0"].kind, InterfaceKind::Wifi);
        assert_eq!(by_name["docker0"].kind, InterfaceKind::Virtual);

        assert_eq!(by_name["eth0"].speed_mbps, Some(1000));
        assert_eq!(by_name["eth0"].carrier, Some(true));
        assert_eq!(by_name["eth1"].carrier, None);
        assert_eq!(by_name["eth0"].operstate, "up");
    }

    #[test]
    fn absurd_speed_value_on_a_down_link_is_dropped() {
        let root = fixture_dir("speed");
        write_interface(
            &root,
            "eth0",
            &[("operstate", "down"), ("speed", "4294967295")],
            &["device"],
        );
        let interfaces = discover_from(&root, &FakeRunner::new());
        assert_eq!(interfaces[0].speed_mbps, None);
    }

    #[test]
    fn missing_sysfs_yields_no_interfaces() {
        let interfaces = discover_from(Path::new("/nonexistent/sys/class/net"), &FakeRunner::new());
        assert!(interfaces.is_empty());
    }

    #[test]
    fn parses_addresses_from_ip_json() {
        let json = r#"[
            {"ifname":"lo","addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host"}]},
            {"ifname":"br0","addr_info":[
                {"family":"inet","local":"192.168.1.1","prefixlen":24,"scope":"global"},
                {"family":"inet6","local":"fe80::1","prefixlen":64}
            ]}
        ]"#;
        let map = parse_ip_addr_json(json);
        assert_eq!(
            map["lo"],
            vec![AddressInfo {
                cidr: "127.0.0.1/8".to_string(),
                scope: "host".to_string()
            }]
        );
        // IPv6 is ignored: the router model is IPv4.
        assert_eq!(map["br0"].len(), 1);
        assert_eq!(map["br0"][0].cidr, "192.168.1.1/24");
    }

    #[test]
    fn malformed_ip_json_is_not_fatal() {
        assert!(parse_ip_addr_json("not json").is_empty());
        assert!(parse_ip_addr_json("{}").is_empty());
    }

    #[test]
    fn discovers_addresses_when_ip_is_available() {
        let root = fixture_dir("with-ip");
        write_interface(&root, "br0", &[("operstate", "up")], &["bridge"]);
        let runner = FakeRunner::new().respond(
            "ip",
            Some(0),
            r#"[{"ifname":"br0","addr_info":[{"family":"inet","local":"192.168.1.1","prefixlen":24,"scope":"global"}]}]"#,
            "",
        );
        let interfaces = discover_from(&root, &runner);
        assert_eq!(interfaces[0].addresses[0].cidr, "192.168.1.1/24");
    }

    #[test]
    fn parses_iw_dev_info() {
        let text = "Interface wlan0\n\tifindex 3\n\taddr aa:bb:cc:dd:ee:ff\n\tssid My Home Net\n\ttype AP\n\tchannel 36 (5180 MHz), width: 80 MHz, center1: 5210 MHz\n";
        let info = parse_iw_info(text);
        assert_eq!(info.ssid.as_deref(), Some("My Home Net"));
        assert_eq!(info.mode.as_deref(), Some("AP"));
        assert_eq!(info.channel, Some(36));
        assert_eq!(info.frequency_mhz, Some(5180));
    }

    #[test]
    fn parses_iw_dev_info_without_ssid() {
        let text = "Interface wlan1\n\ttype managed\n\tchannel 6 (2437 MHz), width: 20 MHz\n";
        let info = parse_iw_info(text);
        assert!(info.ssid.is_none());
        assert_eq!(info.channel, Some(6));
    }

    #[test]
    fn detects_ap_support_from_iw_list() {
        let text = "\tSupported interface modes:\n\t\t * IBSS\n\t\t * managed\n\t\t * AP\n\t\t * monitor\n";
        assert!(parse_iw_list_ap(text));
        let no_ap = "\tSupported interface modes:\n\t\t * managed\n\t\t * monitor\n";
        assert!(!parse_iw_list_ap(no_ap));
    }

    #[test]
    fn wifi_interface_gets_ssid_from_iw() {
        let root = fixture_dir("wifi");
        write_interface(&root, "wlan0", &[("operstate", "up")], &["wireless"]);
        let runner = FakeRunner::new()
            .respond("iw", Some(0), "Supported interface modes:\n\t\t * AP\n", "")
            .respond_when(
                "iw",
                "dev wlan0 info",
                Some(0),
                "Interface wlan0\n\tssid Home\n\ttype AP\n\tchannel 6 (2437 MHz)\n",
                "",
            );
        let interfaces = discover_from(&root, &runner);
        let wifi = interfaces[0].wifi.as_ref().expect("wifi info");
        assert_eq!(wifi.ssid.as_deref(), Some("Home"));
        assert_eq!(wifi.mode.as_deref(), Some("AP"));
    }
}
