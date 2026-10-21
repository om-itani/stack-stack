//! IPv4/CIDR arithmetic, MAC and port parsing.
//!
//! Every router decision in this module — "is this DHCP range inside its
//! subnet", "do these two LANs overlap", "what is the broadcast address" — is
//! integer arithmetic over `u32`. Doing it here, once, keeps the generators
//! free of ad-hoc string maths and makes the edge cases testable.

use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

/// A CIDR network. The address keeps whatever host bits the caller supplied;
/// use [`Ipv4Net::network`] for the masked form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Net {
    addr: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Net {
    pub fn new(addr: Ipv4Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 32 {
            return Err(format!("prefix /{prefix} is out of range (0-32)"));
        }
        Ok(Self { addr, prefix })
    }

    /// Parse `"a.b.c.d"` (a /32) or `"a.b.c.d/N"`.
    pub fn parse(input: &str) -> Result<Self, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("empty network".to_string());
        }
        let (addr_part, prefix_part) = match trimmed.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (trimmed, None),
        };

        let addr = parse_ipv4(addr_part)?;
        let prefix = match prefix_part {
            Some(prefix) => prefix
                .trim()
                .parse::<u8>()
                .map_err(|_| format!("invalid prefix length: {prefix:?}"))?,
            None => 32,
        };
        Self::new(addr, prefix)
    }

    pub fn addr(&self) -> Ipv4Addr {
        self.addr
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// Network mask as an address, e.g. /24 -> 255.255.255.0.
    pub fn netmask(&self) -> Ipv4Addr {
        Ipv4Addr::from(mask_u32(self.prefix))
    }

    /// Wildcard mask (bitwise complement of the netmask).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn wildcard(&self) -> Ipv4Addr {
        Ipv4Addr::from(!mask_u32(self.prefix))
    }

    /// Address with host bits cleared.
    pub fn network(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.addr_u32() & mask_u32(self.prefix))
    }

    /// Last address in the block (network | ~mask). For /32 this is the address
    /// itself; for /31 it is the second of the two RFC 3021 addresses.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.network_u32() | !mask_u32(self.prefix))
    }

    /// First host address. Equal to [`Ipv4Net::network`] for /31 and /32, where
    /// there is no reserved network address.
    pub fn first_host(&self) -> Ipv4Addr {
        if self.prefix >= 31 {
            Ipv4Addr::from(self.network_u32())
        } else {
            Ipv4Addr::from(self.network_u32() + 1)
        }
    }

    /// Last host address. Equal to [`Ipv4Net::broadcast`] for /31 and /32.
    pub fn last_host(&self) -> Ipv4Addr {
        if self.prefix >= 31 {
            Ipv4Addr::from(self.broadcast_u32())
        } else {
            Ipv4Addr::from(self.broadcast_u32() - 1)
        }
    }

    /// Number of addresses that can be assigned to hosts.
    ///
    /// /31 carries two (RFC 3021 point-to-point), /32 carries one, and every
    /// other prefix loses the network and broadcast addresses.
    pub fn usable_host_count(&self) -> u64 {
        match self.prefix {
            32 => 1,
            31 => 2,
            _ => u64::from(self.broadcast_u32() - self.network_u32()) - 1,
        }
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let ip = u32::from(ip);
        ip & mask_u32(self.prefix) == self.network_u32()
    }

    /// True when the two blocks share at least one address. Two CIDR blocks
    /// overlap iff one's network address falls inside the other.
    pub fn overlaps(&self, other: &Ipv4Net) -> bool {
        self.contains(Ipv4Addr::from(other.network_u32()))
            || other.contains(Ipv4Addr::from(self.network_u32()))
    }

    /// `network/prefix`, the canonical form used in generated config.
    pub fn network_cidr(&self) -> String {
        format!("{}/{}", self.network(), self.prefix)
    }

    /// `addr/prefix` exactly as supplied.
    pub fn addr_cidr(&self) -> String {
        format!("{}/{}", self.addr, self.prefix)
    }

    fn addr_u32(&self) -> u32 {
        u32::from(self.addr)
    }

    fn network_u32(&self) -> u32 {
        self.addr_u32() & mask_u32(self.prefix)
    }

    fn broadcast_u32(&self) -> u32 {
        self.network_u32() | !mask_u32(self.prefix)
    }
}

impl fmt::Display for Ipv4Net {
    /// Renders the address exactly as given, with its prefix — never silently
    /// masks, so what the operator typed is what they see echoed back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr(), self.prefix())
    }
}

impl FromStr for Ipv4Net {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

fn mask_u32(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

/// Strict dotted-quad parse. `Ipv4Addr::from_str` alone accepts a few forms we
/// do not want in generated config (e.g. `010.0.0.1`), so reject anything that
/// is not exactly four 1-3 digit decimal components.
pub fn parse_ipv4(input: &str) -> Result<Ipv4Addr, String> {
    let trimmed = input.trim();
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() != 4 {
        return Err(format!("invalid IPv4 address: {input:?}"));
    }

    let mut octets = [0u8; 4];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("invalid IPv4 address: {input:?}"));
        }
        // Leading zeros are ambiguous (some tools read them as octal) and must
        // never end up verbatim in a generated config.
        if part.len() > 1 && part.starts_with('0') {
            return Err(format!("invalid IPv4 address: {input:?}"));
        }
        let value = part
            .parse::<u16>()
            .map_err(|_| format!("invalid IPv4 address: {input:?}"))?;
        if value > 255 {
            return Err(format!("invalid IPv4 address: {input:?}"));
        }
        octets[index] = value as u8;
    }
    Ok(Ipv4Addr::from(octets))
}

/// A MAC address, normalised to lower-case colon-separated pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddr([u8; 6]);

impl MacAddr {
    pub fn parse(input: &str) -> Result<Self, String> {
        let trimmed = input.trim();
        let compact: String = trimmed
            .chars()
            .filter(|c| *c != ':' && *c != '-')
            .collect::<String>();
        if compact.len() != 12 || !compact.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("invalid MAC address: {input:?}"));
        }

        let mut octets = [0u8; 6];
        for (index, octet) in octets.iter_mut().enumerate() {
            let slice = &compact[index * 2..index * 2 + 2];
            *octet = u8::from_str_radix(slice, 16)
                .map_err(|_| format!("invalid MAC address: {input:?}"))?;
        }
        Ok(Self(octets))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

impl FromStr for MacAddr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A TCP/UDP port number, rejecting 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Port(u16);

impl Port {
    pub fn new(value: u16) -> Result<Self, String> {
        if value == 0 {
            return Err("port 0 is not a valid service port".to_string());
        }
        Ok(Self(value))
    }

    /// Parse a decimal port number, rejecting 0 and anything out of `u16`.
    pub fn parse(input: &str) -> Result<Self, String> {
        let value = input
            .trim()
            .parse::<u16>()
            .map_err(|_| format!("invalid port: {input:?}"))?;
        Self::new(value)
    }

    /// The numeric port.
    pub fn value(&self) -> u16 {
        self.0
    }
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Port {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Interface names: Linux caps `IFNAMSIZ` at 16 bytes including the NUL, so 15
/// usable characters. Kernel names also never contain whitespace or `/`.
pub fn validate_ifname(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("interface name must not be empty".to_string());
    }
    if name.len() > 15 {
        return Err(format!(
            "interface name {name:?} is longer than 15 characters"
        ));
    }
    if name.contains('/') || name.contains(char::is_whitespace) {
        return Err(format!(
            "interface name {name:?} contains illegal characters"
        ));
    }
    if name == "." || name == ".." {
        return Err(format!("interface name {name:?} is not usable"));
    }
    // nftables/iproute2 token hygiene: refuse shell metacharacters outright.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | ':'))
    {
        return Err(format!(
            "interface name {name:?} contains illegal characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cidr_with_and_without_prefix() {
        let net = Ipv4Net::parse("192.168.1.1/24").expect("parse");
        assert_eq!(net.addr(), "192.168.1.1".parse::<Ipv4Addr>().expect("addr"));
        assert_eq!(net.prefix(), 24);
        assert_eq!(Ipv4Net::parse("10.0.0.1").expect("parse").prefix(), 32);
    }

    #[test]
    fn rejects_malformed_addresses() {
        for bad in [
            "",
            "192.168.1",
            "192.168.1.1.1",
            "256.0.0.1",
            "010.0.0.1",
            "a.b.c.d",
        ] {
            assert!(Ipv4Net::parse(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(Ipv4Net::parse("192.168.1.1/33").is_err());
        assert!(Ipv4Net::parse("192.168.1.1/").is_err());
    }

    #[test]
    fn netmask_and_wildcard_are_complements() {
        let net = Ipv4Net::parse("192.168.1.1/24").expect("parse");
        assert_eq!(
            net.netmask(),
            "255.255.255.0".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(net.wildcard(), "0.0.0.255".parse::<Ipv4Addr>().expect("ip"));

        let slash_zero = Ipv4Net::parse("0.0.0.0/0").expect("parse");
        assert_eq!(slash_zero.netmask(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(
            slash_zero.wildcard(),
            "255.255.255.255".parse::<Ipv4Addr>().expect("ip")
        );
    }

    #[test]
    fn network_and_broadcast_are_masked_correctly() {
        let net = Ipv4Net::parse("192.168.1.37/24").expect("parse");
        assert_eq!(
            net.network(),
            "192.168.1.0".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(
            net.broadcast(),
            "192.168.1.255".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(net.network_cidr(), "192.168.1.0/24");
        // Display preserves the operator's input, network_cidr() normalises.
        assert_eq!(net.to_string(), "192.168.1.37/24");
    }

    #[test]
    fn host_range_excludes_network_and_broadcast() {
        let net = Ipv4Net::parse("192.168.1.0/24").expect("parse");
        assert_eq!(
            net.first_host(),
            "192.168.1.1".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(
            net.last_host(),
            "192.168.1.254".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(net.usable_host_count(), 254);
    }

    #[test]
    fn point_to_point_prefixes_use_both_addresses() {
        let slash31 = Ipv4Net::parse("10.0.0.0/31").expect("parse");
        assert_eq!(
            slash31.first_host(),
            "10.0.0.0".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(
            slash31.last_host(),
            "10.0.0.1".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(slash31.usable_host_count(), 2);

        let slash32 = Ipv4Net::parse("10.0.0.5/32").expect("parse");
        assert_eq!(
            slash32.first_host(),
            "10.0.0.5".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(
            slash32.broadcast(),
            "10.0.0.5".parse::<Ipv4Addr>().expect("ip")
        );
        assert_eq!(slash32.usable_host_count(), 1);
    }

    #[test]
    fn slash_zero_spans_everything() {
        let all = Ipv4Net::parse("0.0.0.0/0").expect("parse");
        assert!(all.contains("203.0.113.9".parse::<Ipv4Addr>().expect("ip")));
        assert!(all.overlaps(&Ipv4Net::parse("10.0.0.0/8").expect("parse")));
    }

    #[test]
    fn contains_respects_boundaries() {
        let net = Ipv4Net::parse("192.168.1.0/24").expect("parse");
        assert!(net.contains("192.168.1.0".parse().expect("ip")));
        assert!(net.contains("192.168.1.255".parse().expect("ip")));
        assert!(!net.contains("192.168.2.0".parse().expect("ip")));
        assert!(!net.contains("192.168.0.255".parse().expect("ip")));
    }

    #[test]
    fn overlap_detection_is_symmetric() {
        let a = Ipv4Net::parse("192.168.1.0/24").expect("parse");
        let b = Ipv4Net::parse("192.168.1.128/25").expect("parse");
        let c = Ipv4Net::parse("192.168.2.0/24").expect("parse");
        assert!(a.overlaps(&b) && b.overlaps(&a), "nested blocks overlap");
        assert!(!a.overlaps(&c) && !c.overlaps(&a), "disjoint blocks do not");
    }

    #[test]
    fn usable_host_count_handles_large_blocks() {
        assert_eq!(
            Ipv4Net::parse("10.0.0.0/8")
                .expect("parse")
                .usable_host_count(),
            16_777_214
        );
        assert_eq!(
            Ipv4Net::parse("0.0.0.0/0")
                .expect("parse")
                .usable_host_count(),
            4_294_967_294
        );
    }

    #[test]
    fn mac_parsing_normalises_separators() {
        let mac = MacAddr::parse("AA-BB-CC-DD-EE-FF").expect("parse");
        assert_eq!(mac.to_string(), "aa:bb:cc:dd:ee:ff");
        // Bare hex and mixed separators are both accepted, then normalised.
        assert_eq!(MacAddr::parse("aabbccddeeff").expect("parse"), mac);
        for bad in [
            "",
            "aa:bb:cc:dd:ee",
            "aa:bb:cc:dd:ee:gg",
            "zz:bb:cc:dd:ee:ff",
        ] {
            assert!(MacAddr::parse(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn port_rejects_zero() {
        assert_eq!(Port::parse("443").expect("port").to_string(), "443");
        assert!(Port::parse("0").is_err());
        assert!(Port::parse("65536").is_err());
        assert!(Port::parse("abc").is_err());
    }

    #[test]
    fn ifname_validation_matches_kernel_limits() {
        assert!(validate_ifname("eth0").is_ok());
        assert!(validate_ifname("br0.10").is_ok());
        assert!(validate_ifname("wlan0").is_ok());
        assert!(validate_ifname(&"a".repeat(15)).is_ok());
        for bad in [
            "",
            &"a".repeat(16),
            "eth 0",
            "eth/0",
            "eth;rm",
            "$(x)",
            "..",
        ] {
            assert!(validate_ifname(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
