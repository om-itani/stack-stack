//! "What address is this box reachable on?" for building UI links.
//!
//! Primary method is the classic routing trick: `connect` a UDP socket to a
//! public address. No packet is ever sent, but the kernel still resolves the
//! route and stamps the socket with the source address of the default-route
//! interface. That is exactly the address a LAN client should hit.
//!
//! Fallback is `/proc/net/fib_trie`, which lists every local address. On a host
//! with no default route (isolated LAN) the first non-loopback local address
//! wins. If neither works we return `0.0.0.0`, which the UI treats as
//! "unknown".

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use crate::containers::model::HostIp;

/// Public DNS resolvers used purely to make the kernel pick a route.
const PROBE_ADDRS: [&str; 2] = ["1.1.1.1:80", "8.8.8.8:80"];

pub fn detect() -> HostIp {
    HostIp {
        ip: default_route_ipv4()
            .or_else(first_non_loopback_ipv4)
            .unwrap_or_else(|| "0.0.0.0".to_string()),
    }
}

fn default_route_ipv4() -> Option<String> {
    // A v4 socket bound to the unspecified address can still be `connect`ed;
    // the OS resolves the route without transmitting anything.
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    for probe in PROBE_ADDRS {
        if socket.connect(probe).is_err() {
            continue;
        }
        if let Ok(SocketAddr::V4(addr)) = socket.local_addr()
            && is_usable(*addr.ip())
        {
            return Some(addr.ip().to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn first_non_loopback_ipv4() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/net/fib_trie").ok()?;
    parse_fib_trie(&raw)
}

#[cfg(not(target_os = "linux"))]
fn first_non_loopback_ipv4() -> Option<String> {
    None
}

fn is_usable(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified() && !ip.is_broadcast()
}

/// `fib_trie` pairs each local address with a `|-- <ip>` line followed by a
/// `/32 host LOCAL` classification line. Only those entries are addresses of
/// this host (remote routes show up as UNICAST/BROADCAST/LOCAL prefixes too,
/// but never as a `/32 host LOCAL` leaf for a foreign address).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_fib_trie(raw: &str) -> Option<String> {
    let mut pending: Option<Ipv4Addr> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("|-- ") {
            pending = rest.trim().parse::<Ipv4Addr>().ok();
            continue;
        }
        if trimmed.starts_with("/32 host LOCAL")
            && let Some(ip) = pending
            && is_usable(ip)
        {
            return Some(ip.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_a_parseable_address() {
        let host = detect();
        assert!(
            host.ip.parse::<Ipv4Addr>().is_ok(),
            "detect must always yield a valid IPv4, got {}",
            host.ip
        );
    }

    #[test]
    fn parses_first_non_loopback_local_address() {
        // Trimmed real output: 127.0.0.1 first, then the LAN address.
        let raw = "\
Main:
  +-- 192.168.1.0/24 2 0 2
     |-- 192.168.1.0
        /24 link UNICAST
     |-- 192.168.1.1
        /32 host LOCAL
     |-- 192.168.1.42
        /32 host LOCAL
Local:
  +-- 127.0.0.0/8 2 0 2
     |-- 127.0.0.1
        /32 host LOCAL
";
        assert_eq!(parse_fib_trie(raw), Some("192.168.1.1".to_string()));
    }

    #[test]
    fn ignores_loopback_only_tables() {
        let raw = "\
Local:
  +-- 127.0.0.0/8 2 0 2
     |-- 127.0.0.1
        /32 host LOCAL
";
        assert_eq!(parse_fib_trie(raw), None);
    }
}
