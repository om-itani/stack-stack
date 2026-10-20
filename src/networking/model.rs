//! The router configuration document and its validator.
//!
//! Everything the generators consume is read from here, and every value that
//! reaches a generated config file has first passed [`NetworkConfig::validate`].
//! Validation is deliberately strict: a router that half-applies is worse than
//! one that refuses.

use serde::{Deserialize, Serialize};

use crate::networking::ipmath::{Ipv4Net, MacAddr, Port, validate_ifname};

// ---------------------------------------------------------------------------
// Interfaces, VLANs, bridges, WiFi
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceRole {
    Wan,
    Lan,
    #[default]
    Unused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceConfig {
    pub name: String,
    #[serde(default)]
    pub role: InterfaceRole,
    /// Static address as `a.b.c.d/n`. `None` means "leave to DHCP" on a WAN and
    /// "unconfigured" elsewhere.
    #[serde(default)]
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VlanConfig {
    /// Kernel interface name to create, e.g. `eth1.10`.
    pub name: String,
    /// Trunk interface the tag rides on.
    pub parent: String,
    pub id: u16,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
    /// L3 address of the bridge; a bridge without one is pure L2.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub stp: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Wireless interface to use as the access point, e.g. `wlan0`.
    #[serde(default)]
    pub interface: String,
    #[serde(default)]
    pub ssid: String,
    #[serde(default)]
    pub passphrase: String,
    /// `2g` or `5g`.
    #[serde(default = "default_band")]
    pub band: String,
    /// 0 selects the driver default.
    #[serde(default)]
    pub channel: u8,
    #[serde(default = "default_country")]
    pub country: String,
    /// Bridge the AP is added to, usually the LAN bridge.
    #[serde(default = "default_lan_bridge")]
    pub bridge: String,
    /// `wpa2`, `wpa3` or `open`.
    #[serde(default = "default_security")]
    pub security: String,
}

impl Default for WifiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: String::new(),
            ssid: String::new(),
            passphrase: String::new(),
            band: default_band(),
            channel: 0,
            country: default_country(),
            bridge: default_lan_bridge(),
            security: default_security(),
        }
    }
}

// ---------------------------------------------------------------------------
// Firewall
// ---------------------------------------------------------------------------

/// Default deny: an unspecified input policy must never be permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirewallAction {
    Accept,
    #[default]
    Drop,
    Reject,
}

impl FirewallAction {
    pub fn as_verdict(&self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Drop => "drop",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallRule {
    pub id: String,
    #[serde(default)]
    pub action: FirewallAction,
    /// `tcp`, `udp`, `icmp` or `any`.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// Source CIDR; `None` means any source.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub comment: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallConfig {
    #[serde(default = "default_input_policy")]
    pub default_input: FirewallAction,
    #[serde(default = "default_true")]
    pub allow_established: bool,
    #[serde(default = "default_true")]
    pub allow_icmp: bool,
    /// Ports always reachable from the LAN so the operator cannot lock
    /// themselves out of SSH or the web UI.
    #[serde(default = "default_management_ports")]
    pub management_ports: Vec<u16>,
    #[serde(default)]
    pub rules: Vec<FirewallRule>,
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            default_input: FirewallAction::Drop,
            allow_established: true,
            allow_icmp: true,
            management_ports: default_management_ports(),
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForward {
    pub id: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    pub external_port: u16,
    pub internal_ip: String,
    pub internal_port: u16,
    #[serde(default)]
    pub comment: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// DHCP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DhcpReservation {
    pub mac: String,
    pub ip: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DhcpConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Bridge (or interface) leases are served on.
    #[serde(default)]
    pub interface: String,
    #[serde(default)]
    pub range_start: String,
    #[serde(default)]
    pub range_end: String,
    #[serde(default = "default_lease")]
    pub lease: String,
    #[serde(default)]
    pub gateway: String,
    #[serde(default)]
    pub dns_servers: Vec<String>,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub reservations: Vec<DhcpReservation>,
}

impl Default for DhcpConfig {
    fn default() -> Self {
        Self {
            // Opt-in: a DHCP server with no interface chosen is meaningless, and
            // the default document must validate clean so a fresh install does
            // not open with a wall of errors.
            enabled: false,
            interface: String::new(),
            range_start: String::new(),
            range_end: String::new(),
            lease: default_lease(),
            gateway: String::new(),
            dns_servers: Vec::new(),
            domain: String::new(),
            reservations: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// DNS (unbound)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsRewrite {
    pub host: String,
    pub address: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_dns_listen")]
    pub listen: Vec<String>,
    #[serde(default)]
    pub upstreams: Vec<String>,
    #[serde(default = "default_true")]
    pub dnssec: bool,
    #[serde(default = "default_true")]
    pub qname_minimisation: bool,
    /// Answer these names locally instead of resolving them upstream.
    #[serde(default)]
    pub rewrites: Vec<DnsRewrite>,
    /// Domains answered with NXDOMAIN (a hand-managed blocklist).
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Optional on-disk blocklist file pulled in with `include:`.
    #[serde(default)]
    pub blocklist_file: Option<String>,
    #[serde(default = "default_cache_min")]
    pub cache_min_ttl: u32,
    #[serde(default = "default_cache_max")]
    pub cache_max_ttl: u32,
    #[serde(default = "default_cache_size")]
    pub cache_size_mb: u32,
    #[serde(default = "default_threads")]
    pub threads: u8,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: default_dns_listen(),
            upstreams: Vec::new(),
            dnssec: true,
            qname_minimisation: true,
            rewrites: Vec::new(),
            blocked_domains: Vec::new(),
            blocklist_file: None,
            cache_min_ttl: default_cache_min(),
            cache_max_ttl: default_cache_max(),
            cache_size_mb: default_cache_size(),
            threads: default_threads(),
        }
    }
}

// ---------------------------------------------------------------------------
// SQM (CAKE)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqmConfig {
    #[serde(default)]
    pub enabled: bool,
    /// WAN interface to shape.
    #[serde(default)]
    pub interface: String,
    #[serde(default = "default_download_kbps")]
    pub download_kbps: u32,
    #[serde(default = "default_upload_kbps")]
    pub upload_kbps: u32,
    /// `ethernet`, `atm` or `none`.
    #[serde(default = "default_link_layer")]
    pub link_layer: String,
    /// Per-packet overhead in bytes, as required by CAKE.
    #[serde(default)]
    pub overhead: i32,
    #[serde(default = "default_true")]
    pub diffserv4: bool,
    /// Intermediate queueing device used to shape ingress.
    #[serde(default = "default_ifb")]
    pub ifb: String,
}

impl Default for SqmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: String::new(),
            download_kbps: default_download_kbps(),
            upload_kbps: default_upload_kbps(),
            link_layer: default_link_layer(),
            overhead: 0,
            diffserv4: true,
            ifb: default_ifb(),
        }
    }
}

// ---------------------------------------------------------------------------
// Cloudflare tunnel
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelIngress {
    pub hostname: String,
    /// Origin service, e.g. `http://192.168.1.10:8096`.
    pub service: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tunnel_id: String,
    #[serde(default = "default_credentials_file")]
    pub credentials_file: String,
    #[serde(default)]
    pub ingress: Vec<TunnelIngress>,
    /// Also publish the stack-stack UI through the tunnel.
    #[serde(default)]
    pub expose_ui: bool,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tunnel_id: String::new(),
            credentials_file: default_credentials_file(),
            ingress: Vec::new(),
            expose_ui: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Certificates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CertMode {
    /// No local certificate; TLS terminates upstream (e.g. Cloudflare edge).
    #[default]
    None,
    /// Self-signed certificate generated locally for the listed domains.
    SelfSigned,
    /// Certificate/key pair provided by the operator (e.g. Cloudflare Origin).
    Imported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertConfig {
    #[serde(default)]
    pub mode: CertMode,
    /// Domains the certificate should cover; `*.example.com` is allowed.
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default = "default_cert_directory")]
    pub directory: String,
    /// File stem (without extension) of the active certificate pair.
    #[serde(default)]
    pub active: Option<String>,
}

impl Default for CertConfig {
    fn default() -> Self {
        Self {
            mode: CertMode::None,
            domains: Vec::new(),
            directory: default_cert_directory(),
            active: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Root document
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default)]
    pub interfaces: Vec<InterfaceConfig>,
    #[serde(default)]
    pub vlans: Vec<VlanConfig>,
    #[serde(default)]
    pub bridges: Vec<BridgeConfig>,
    #[serde(default)]
    pub wifi: WifiConfig,
    #[serde(default)]
    pub firewall: FirewallConfig,
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
    #[serde(default)]
    pub dhcp: DhcpConfig,
    #[serde(default)]
    pub dns: DnsConfig,
    #[serde(default)]
    pub sqm: SqmConfig,
    #[serde(default)]
    pub tunnel: TunnelConfig,
    #[serde(default)]
    pub certs: CertConfig,
}

/// One validation failure. `field` is a dotted path so the UI can highlight it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub field: String,
    pub message: String,
}

impl Problem {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl NetworkConfig {
    /// Every name this box will own: physical interfaces, VLANs and bridges.
    fn known_interfaces(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .interfaces
            .iter()
            .map(|iface| iface.name.as_str())
            .collect();
        names.extend(self.vlans.iter().map(|vlan| vlan.name.as_str()));
        names.extend(self.bridges.iter().map(|bridge| bridge.name.as_str()));
        names
    }

    /// Static LAN subnets, de-duplicated. Bridges win over their members
    /// because a member interface in a bridge carries no address of its own.
    pub fn lan_networks(&self) -> Vec<Ipv4Net> {
        let mut nets: Vec<Ipv4Net> = Vec::new();
        let mut push = |value: &Option<String>| {
            if let Some(address) = value
                && let Ok(net) = Ipv4Net::parse(address)
                && !nets.contains(&net)
            {
                nets.push(net);
            }
        };
        for bridge in &self.bridges {
            push(&bridge.address);
        }
        for vlan in &self.vlans {
            push(&vlan.address);
        }
        for iface in &self.interfaces {
            if iface.role == InterfaceRole::Lan {
                push(&iface.address);
            }
        }
        nets
    }

    /// Interfaces that carry a LAN address and therefore anchor the LAN
    /// firewall rules.
    ///
    /// Bridge members are excluded on purpose: a member is pure layer 2 and has
    /// no address of its own, so matching it would both duplicate rules and keep
    /// matching an interface that has no routable role.
    pub fn lan_interfaces(&self) -> Vec<String> {
        let members: Vec<&str> = self
            .bridges
            .iter()
            .flat_map(|bridge| bridge.members.iter().map(String::as_str))
            .collect();

        let mut names: Vec<String> = Vec::new();
        let push = |name: &str, names: &mut Vec<String>| {
            if !members.contains(&name) && !names.iter().any(|existing| existing == name) {
                names.push(name.to_string());
            }
        };

        for bridge in &self.bridges {
            if bridge.address.is_some() {
                push(&bridge.name, &mut names);
            }
        }
        for vlan in &self.vlans {
            if vlan.address.is_some() {
                push(&vlan.name, &mut names);
            }
        }
        for iface in &self.interfaces {
            if iface.role == InterfaceRole::Lan {
                push(&iface.name, &mut names);
            }
        }
        names
    }

    pub fn wan_interface(&self) -> Option<&str> {
        self.interfaces
            .iter()
            .find(|iface| iface.role == InterfaceRole::Wan)
            .map(|iface| iface.name.as_str())
    }

    /// Full validation. An empty result means "safe to render and apply".
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        self.validate_interfaces(&mut problems);
        self.validate_firewall(&mut problems);
        self.validate_dhcp(&mut problems);
        self.validate_dns(&mut problems);
        self.validate_sqm(&mut problems);
        self.validate_tunnel(&mut problems);
        self.validate_certs(&mut problems);
        self.validate_subnets(&mut problems);
        problems
    }

    fn validate_interfaces(&self, problems: &mut Vec<Problem>) {
        let mut seen: Vec<&str> = Vec::new();
        for (index, iface) in self.interfaces.iter().enumerate() {
            let field = format!("interfaces[{index}]");
            if let Err(err) = validate_ifname(&iface.name) {
                problems.push(Problem::new(format!("{field}.name"), err));
            }
            if seen.contains(&iface.name.as_str()) {
                problems.push(Problem::new(
                    format!("{field}.name"),
                    format!("interface {:?} is declared twice", iface.name),
                ));
            }
            seen.push(iface.name.as_str());
            check_optional_address(&iface.address, &format!("{field}.address"), problems);
        }

        let wan_count = self
            .interfaces
            .iter()
            .filter(|iface| iface.role == InterfaceRole::Wan)
            .count();
        if wan_count > 1 {
            problems.push(Problem::new(
                "interfaces",
                format!("{wan_count} interfaces are marked as WAN; exactly one is supported"),
            ));
        }

        let mut vlan_names: Vec<&str> = Vec::new();
        for (index, vlan) in self.vlans.iter().enumerate() {
            let field = format!("vlans[{index}]");
            if let Err(err) = validate_ifname(&vlan.name) {
                problems.push(Problem::new(format!("{field}.name"), err));
            }
            if vlan_names.contains(&vlan.name.as_str()) {
                problems.push(Problem::new(
                    format!("{field}.name"),
                    format!("VLAN {:?} is declared twice", vlan.name),
                ));
            }
            vlan_names.push(vlan.name.as_str());

            if vlan.id == 0 || vlan.id > 4094 {
                problems.push(Problem::new(
                    format!("{field}.id"),
                    format!("VLAN id {} is outside 1-4094", vlan.id),
                ));
            }
            if self.vlans.iter().any(|other| {
                other.id == vlan.id && other.parent == vlan.parent && other.name != vlan.name
            }) {
                problems.push(Problem::new(
                    format!("{field}.id"),
                    format!("VLAN id {} is already used on {}", vlan.id, vlan.parent),
                ));
            }
            if !self
                .interfaces
                .iter()
                .any(|iface| iface.name == vlan.parent)
            {
                problems.push(Problem::new(
                    format!("{field}.parent"),
                    format!("parent interface {:?} is not declared", vlan.parent),
                ));
            }
            check_optional_address(&vlan.address, &format!("{field}.address"), problems);
        }

        let mut bridge_names: Vec<&str> = Vec::new();
        for (index, bridge) in self.bridges.iter().enumerate() {
            let field = format!("bridges[{index}]");
            if let Err(err) = validate_ifname(&bridge.name) {
                problems.push(Problem::new(format!("{field}.name"), err));
            }
            if bridge_names.contains(&bridge.name.as_str()) {
                problems.push(Problem::new(
                    format!("{field}.name"),
                    format!("bridge {:?} is declared twice", bridge.name),
                ));
            }
            bridge_names.push(bridge.name.as_str());
            check_optional_address(&bridge.address, &format!("{field}.address"), problems);

            let mut members: Vec<&str> = Vec::new();
            for member in &bridge.members {
                if member == &bridge.name {
                    problems.push(Problem::new(
                        format!("{field}.members"),
                        format!("bridge {:?} cannot contain itself", bridge.name),
                    ));
                }
                if members.contains(&member.as_str()) {
                    problems.push(Problem::new(
                        format!("{field}.members"),
                        format!("{:?} is listed twice", member),
                    ));
                }
                members.push(member.as_str());
                if !seen.contains(&member.as_str()) && !vlan_names.contains(&member.as_str()) {
                    problems.push(Problem::new(
                        format!("{field}.members"),
                        format!("member {:?} is not a declared interface or VLAN", member),
                    ));
                }
                // An address belongs on the bridge, not on a pure L2 member.
                if self.member_has_address(member) {
                    problems.push(Problem::new(
                        format!("{field}.members"),
                        format!(
                            "{member} has its own address; move it to the bridge {:?}",
                            bridge.name
                        ),
                    ));
                }
            }
            if bridge.members.is_empty() {
                problems.push(Problem::new(
                    format!("{field}.members"),
                    format!("bridge {:?} has no members", bridge.name),
                ));
            }
        }

        // A physical interface cannot belong to two bridges.
        for (index, bridge) in self.bridges.iter().enumerate() {
            for member in &bridge.members {
                let owners = self
                    .bridges
                    .iter()
                    .filter(|other| other.members.contains(member))
                    .count();
                if owners > 1 {
                    problems.push(Problem::new(
                        format!("bridges[{index}].members"),
                        format!("{member} is a member of more than one bridge"),
                    ));
                }
            }
        }

        self.validate_wifi(problems);
    }

    fn validate_wifi(&self, problems: &mut Vec<Problem>) {
        let wifi = &self.wifi;
        if !wifi.enabled {
            return;
        }
        if let Err(err) = validate_ifname(&wifi.interface) {
            problems.push(Problem::new("wifi.interface", err));
        }
        if wifi.ssid.is_empty() || wifi.ssid.len() > 32 {
            problems.push(Problem::new("wifi.ssid", "SSID must be 1-32 characters"));
        }
        if wifi.security != "open" && wifi.passphrase.len() < 8 {
            problems.push(Problem::new(
                "wifi.passphrase",
                "WPA passphrase must be at least 8 characters",
            ));
        }
        if wifi.passphrase.len() > 63 {
            problems.push(Problem::new(
                "wifi.passphrase",
                "WPA passphrase must be at most 63 characters",
            ));
        }
        if !matches!(wifi.band.as_str(), "2g" | "5g") {
            problems.push(Problem::new("wifi.band", "band must be \"2g\" or \"5g\""));
        }
        if !matches!(wifi.security.as_str(), "wpa2" | "wpa3" | "open") {
            problems.push(Problem::new(
                "wifi.security",
                "security must be wpa2, wpa3 or open",
            ));
        }
        if wifi.country.len() != 2 || !wifi.country.chars().all(|c| c.is_ascii_alphabetic()) {
            problems.push(Problem::new(
                "wifi.country",
                "country must be a two-letter regulatory code, e.g. GB",
            ));
        }
        if wifi.channel > 196 {
            problems.push(Problem::new("wifi.channel", "channel is out of range"));
        }
        if !wifi.bridge.is_empty() && !self.bridges.iter().any(|b| b.name == wifi.bridge) {
            problems.push(Problem::new(
                "wifi.bridge",
                format!("bridge {:?} is not declared", wifi.bridge),
            ));
        }
    }

    fn validate_firewall(&self, problems: &mut Vec<Problem>) {
        for (index, port) in self.firewall.management_ports.iter().enumerate() {
            if Port::new(*port).is_err() {
                problems.push(Problem::new(
                    format!("firewall.management_ports[{index}]"),
                    "management ports must be 1-65535",
                ));
            }
        }

        let mut ids: Vec<&str> = Vec::new();
        for (index, rule) in self.firewall.rules.iter().enumerate() {
            let field = format!("firewall.rules[{index}]");
            if rule.id.trim().is_empty() {
                problems.push(Problem::new(
                    format!("{field}.id"),
                    "rule id must not be empty",
                ));
            }
            if ids.contains(&rule.id.as_str()) {
                problems.push(Problem::new(
                    format!("{field}.id"),
                    format!("rule id {:?} is used twice", rule.id),
                ));
            }
            ids.push(rule.id.as_str());

            if !matches!(rule.protocol.as_str(), "tcp" | "udp" | "icmp" | "any") {
                problems.push(Problem::new(
                    format!("{field}.protocol"),
                    "protocol must be tcp, udp, icmp or any",
                ));
            }
            if let Some(port) = rule.port
                && Port::new(port).is_err()
            {
                problems.push(Problem::new(
                    format!("{field}.port"),
                    "port must be 1-65535",
                ));
            }
            if rule.port.is_some() && !matches!(rule.protocol.as_str(), "tcp" | "udp") {
                problems.push(Problem::new(
                    format!("{field}.port"),
                    "a port only applies to tcp or udp rules",
                ));
            }
            if let Some(source) = &rule.source
                && let Err(err) = Ipv4Net::parse(source)
            {
                problems.push(Problem::new(format!("{field}.source"), err));
            }
        }

        let lan_nets = self.lan_networks();
        for (index, forward) in self.port_forwards.iter().enumerate() {
            let field = format!("port_forwards[{index}]");
            if forward.id.trim().is_empty() {
                problems.push(Problem::new(format!("{field}.id"), "id must not be empty"));
            }
            if !matches!(forward.protocol.as_str(), "tcp" | "udp") {
                problems.push(Problem::new(
                    format!("{field}.protocol"),
                    "protocol must be tcp or udp",
                ));
            }
            for (name, value) in [
                ("external_port", forward.external_port),
                ("internal_port", forward.internal_port),
            ] {
                if Port::new(value).is_err() {
                    problems.push(Problem::new(
                        format!("{field}.{name}"),
                        "port must be 1-65535",
                    ));
                }
            }
            match crate::networking::ipmath::parse_ipv4(&forward.internal_ip) {
                Ok(ip) => {
                    if !lan_nets.is_empty() && !lan_nets.iter().any(|net| net.contains(ip)) {
                        problems.push(Problem::new(
                            format!("{field}.internal_ip"),
                            format!("{ip} is not inside any configured LAN subnet"),
                        ));
                    }
                }
                Err(err) => problems.push(Problem::new(format!("{field}.internal_ip"), err)),
            }
        }

        // Two forwards cannot share the same external port/protocol.
        for (index, forward) in self.port_forwards.iter().enumerate() {
            let clashes = self
                .port_forwards
                .iter()
                .enumerate()
                .filter(|(other, item)| {
                    *other != index
                        && item.enabled
                        && forward.enabled
                        && item.protocol == forward.protocol
                        && item.external_port == forward.external_port
                })
                .count();
            if clashes > 0 {
                problems.push(Problem::new(
                    format!("port_forwards[{index}].external_port"),
                    format!(
                        "{}:{} is forwarded more than once",
                        forward.protocol, forward.external_port
                    ),
                ));
            }
        }
    }

    fn validate_dhcp(&self, problems: &mut Vec<Problem>) {
        let dhcp = &self.dhcp;
        if !dhcp.enabled {
            return;
        }

        let subnet = match self.subnet_for_interface(&dhcp.interface) {
            Some(subnet) => Some(subnet),
            None => {
                problems.push(Problem::new(
                    "dhcp.interface",
                    format!(
                        "interface {:?} has no static LAN address to serve DHCP from",
                        dhcp.interface
                    ),
                ));
                None
            }
        };

        if let Some(subnet) = subnet
            && subnet.usable_host_count() < 3
        {
            problems.push(Problem::new(
                "dhcp.interface",
                format!(
                    "{} has only {} usable address(es); far too small to serve DHCP",
                    subnet.network_cidr(),
                    subnet.usable_host_count()
                ),
            ));
        }

        let start = parse_required(&dhcp.range_start, "dhcp.range_start", problems);
        let end = parse_required(&dhcp.range_end, "dhcp.range_end", problems);
        let gateway = parse_required(&dhcp.gateway, "dhcp.gateway", problems);

        if let (Some(start), Some(end)) = (start, end) {
            if u32::from(start) > u32::from(end) {
                problems.push(Problem::new(
                    "dhcp.range_start",
                    "range start is above range end",
                ));
            }
            if let Some(subnet) = subnet {
                let first = subnet.first_host();
                let last = subnet.last_host();
                if u32::from(start) < u32::from(first) || u32::from(end) > u32::from(last) {
                    problems.push(Problem::new(
                        "dhcp.range_start",
                        format!(
                            "range must lie inside {} ({} - {})",
                            subnet.network_cidr(),
                            first,
                            last
                        ),
                    ));
                }
            }
        }

        if let (Some(gateway), Some(subnet)) = (gateway, subnet)
            && !subnet.contains(gateway)
        {
            problems.push(Problem::new(
                "dhcp.gateway",
                format!("gateway {gateway} is outside {}", subnet.network_cidr()),
            ));
        }

        if dhcp.lease.trim().is_empty() {
            problems.push(Problem::new("dhcp.lease", "lease time must not be empty"));
        }
        if !dhcp.domain.is_empty() && !is_valid_domain(&dhcp.domain) {
            problems.push(Problem::new(
                "dhcp.domain",
                "domain is not a valid DNS name",
            ));
        }

        for (index, server) in dhcp.dns_servers.iter().enumerate() {
            if crate::networking::ipmath::parse_ipv4(server).is_err() {
                problems.push(Problem::new(
                    format!("dhcp.dns_servers[{index}]"),
                    format!("{server:?} is not a valid IPv4 address"),
                ));
            }
        }

        let mut seen_macs: Vec<String> = Vec::new();
        let mut seen_ips: Vec<String> = Vec::new();
        for (index, reservation) in dhcp.reservations.iter().enumerate() {
            let field = format!("dhcp.reservations[{index}]");
            match MacAddr::parse(&reservation.mac) {
                Ok(mac) => {
                    let normalised = mac.to_string();
                    if seen_macs.contains(&normalised) {
                        problems.push(Problem::new(
                            format!("{field}.mac"),
                            format!("{normalised} already has a reservation"),
                        ));
                    }
                    seen_macs.push(normalised);
                }
                Err(err) => problems.push(Problem::new(format!("{field}.mac"), err)),
            }

            match crate::networking::ipmath::parse_ipv4(&reservation.ip) {
                Ok(ip) => {
                    let key = ip.to_string();
                    if seen_ips.contains(&key) {
                        problems.push(Problem::new(
                            format!("{field}.ip"),
                            format!("{key} is reserved more than once"),
                        ));
                    }
                    seen_ips.push(key);

                    if let Some(subnet) = subnet
                        && !subnet.contains(ip)
                    {
                        problems.push(Problem::new(
                            format!("{field}.ip"),
                            format!("{ip} is outside {}", subnet.network_cidr()),
                        ));
                    }
                    if let Some(gateway) = gateway
                        && gateway == ip
                    {
                        problems.push(Problem::new(
                            format!("{field}.ip"),
                            format!("{ip} is the gateway address"),
                        ));
                    }
                    // A reservation inside the dynamic pool can be handed out twice.
                    if let (Some(start), Some(end)) = (start, end)
                        && u32::from(ip) >= u32::from(start)
                        && u32::from(ip) <= u32::from(end)
                    {
                        problems.push(Problem::new(
                            format!("{field}.ip"),
                            format!("{ip} is inside the dynamic DHCP range"),
                        ));
                    }
                }
                Err(err) => problems.push(Problem::new(format!("{field}.ip"), err)),
            }
        }
    }

    fn validate_dns(&self, problems: &mut Vec<Problem>) {
        let dns = &self.dns;
        if !dns.enabled {
            return;
        }

        if dns.listen.is_empty() {
            problems.push(Problem::new(
                "dns.listen",
                "at least one listen address is required",
            ));
        }
        for (index, address) in dns.listen.iter().enumerate() {
            if crate::networking::ipmath::parse_ipv4(address).is_err() {
                problems.push(Problem::new(
                    format!("dns.listen[{index}]"),
                    format!("{address:?} is not a valid IPv4 address"),
                ));
            }
        }

        for (index, upstream) in dns.upstreams.iter().enumerate() {
            if crate::networking::ipmath::parse_ipv4(upstream).is_err() {
                problems.push(Problem::new(
                    format!("dns.upstreams[{index}]"),
                    format!("{upstream:?} is not a valid IPv4 address"),
                ));
            }
        }

        if dns.cache_min_ttl > dns.cache_max_ttl {
            problems.push(Problem::new(
                "dns.cache_min_ttl",
                "minimum TTL is above the maximum TTL",
            ));
        }
        if dns.threads == 0 {
            problems.push(Problem::new("dns.threads", "threads must be at least 1"));
        }
        if dns.cache_size_mb == 0 {
            problems.push(Problem::new(
                "dns.cache_size_mb",
                "cache size must be at least 1 MiB",
            ));
        }

        let mut hosts: Vec<String> = Vec::new();
        for (index, rewrite) in dns.rewrites.iter().enumerate() {
            let field = format!("dns.rewrites[{index}]");
            if !is_valid_hostname(&rewrite.host) {
                problems.push(Problem::new(
                    format!("{field}.host"),
                    format!("{:?} is not a valid host name", rewrite.host),
                ));
            }
            let normalised = rewrite.host.to_ascii_lowercase();
            if hosts.iter().any(|host| host == &normalised) {
                problems.push(Problem::new(
                    format!("{field}.host"),
                    format!("{:?} is rewritten twice", rewrite.host),
                ));
            }
            hosts.push(normalised);
            if let Err(err) = crate::networking::ipmath::parse_ipv4(&rewrite.address) {
                problems.push(Problem::new(format!("{field}.address"), err));
            }
        }

        for (index, domain) in dns.blocked_domains.iter().enumerate() {
            if !is_valid_domain(domain) {
                problems.push(Problem::new(
                    format!("dns.blocked_domains[{index}]"),
                    format!("{domain:?} is not a valid domain"),
                ));
            }
        }

        // DHCP clients must be able to reach whatever address DNS listens on.
        if self.dhcp.enabled && !self.dhcp.dns_servers.is_empty() {
            for (index, server) in self.dhcp.dns_servers.iter().enumerate() {
                if !dns.listen.iter().any(|listen| listen == server) {
                    problems.push(Problem::new(
                        format!("dhcp.dns_servers[{index}]"),
                        format!("DNS server {server} is not in dns.listen"),
                    ));
                }
            }
        }
    }

    fn validate_sqm(&self, problems: &mut Vec<Problem>) {
        let sqm = &self.sqm;
        if !sqm.enabled {
            return;
        }
        if self.wan_interface().is_none() {
            problems.push(Problem::new(
                "sqm.interface",
                "shape the WAN, but no interface is marked as WAN",
            ));
        } else if sqm.interface.is_empty() {
            problems.push(Problem::new("sqm.interface", "SQM needs a WAN interface"));
        } else if !self.known_interfaces().contains(&sqm.interface.as_str()) {
            problems.push(Problem::new(
                "sqm.interface",
                format!("interface {:?} is not declared", sqm.interface),
            ));
        }
        if sqm.download_kbps == 0 || sqm.upload_kbps == 0 {
            problems.push(Problem::new(
                "sqm.download_kbps",
                "both bandwidths must be greater than zero",
            ));
        }
        if !matches!(sqm.link_layer.as_str(), "ethernet" | "atm" | "none") {
            problems.push(Problem::new(
                "sqm.link_layer",
                "link layer must be ethernet, atm or none",
            ));
        }
        if let Err(err) = validate_ifname(&sqm.ifb) {
            problems.push(Problem::new("sqm.ifb", err));
        }
    }

    fn validate_tunnel(&self, problems: &mut Vec<Problem>) {
        let tunnel = &self.tunnel;
        if !tunnel.enabled {
            return;
        }
        if tunnel.tunnel_id.trim().is_empty() {
            problems.push(Problem::new(
                "tunnel.tunnel_id",
                "a tunnel id (or name) is required",
            ));
        }
        if tunnel.credentials_file.trim().is_empty() {
            problems.push(Problem::new(
                "tunnel.credentials_file",
                "the credentials file path must not be empty",
            ));
        }
        if tunnel.ingress.is_empty() {
            problems.push(Problem::new(
                "tunnel.ingress",
                "at least one public hostname is required",
            ));
        }

        let mut hostnames: Vec<String> = Vec::new();
        for (index, ingress) in tunnel.ingress.iter().enumerate() {
            let field = format!("tunnel.ingress[{index}]");
            if !is_valid_hostname(&ingress.hostname) {
                problems.push(Problem::new(
                    format!("{field}.hostname"),
                    format!("{:?} is not a valid host name", ingress.hostname),
                ));
            }
            let normalised = ingress.hostname.to_ascii_lowercase();
            if hostnames.contains(&normalised) {
                problems.push(Problem::new(
                    format!("{field}.hostname"),
                    format!("{:?} is published twice", ingress.hostname),
                ));
            }
            hostnames.push(normalised);

            if !is_valid_service(&ingress.service) {
                problems.push(Problem::new(
                    format!("{field}.service"),
                    "service must look like http://host:port, https://host:port, tcp://host:port, ssh://host:22 or http_status:404",
                ));
            }
        }
    }

    fn validate_certs(&self, problems: &mut Vec<Problem>) {
        let certs = &self.certs;
        if certs.mode == CertMode::None {
            return;
        }
        if certs.domains.is_empty() {
            problems.push(Problem::new(
                "certs.domains",
                "at least one domain is required",
            ));
        }
        for (index, domain) in certs.domains.iter().enumerate() {
            if !is_valid_hostname(domain) {
                problems.push(Problem::new(
                    format!("certs.domains[{index}]"),
                    format!("{domain:?} is not a valid domain"),
                ));
            }
        }
        if certs.mode == CertMode::SelfSigned && certs.directory.trim().is_empty() {
            problems.push(Problem::new(
                "certs.directory",
                "certificate directory must not be empty",
            ));
        }
    }

    /// Cross-checks that need the whole picture: LAN subnets must not collide
    /// with each other (which silently breaks routing), and there must be at
    /// most one WAN default route.
    fn validate_subnets(&self, problems: &mut Vec<Problem>) {
        let nets: Vec<(String, Ipv4Net)> = self
            .interfaces
            .iter()
            .filter_map(|iface| {
                iface
                    .address
                    .as_deref()
                    .and_then(|address| Ipv4Net::parse(address).ok())
                    .map(|net| (iface.name.clone(), net))
            })
            .chain(self.vlans.iter().filter_map(|vlan| {
                vlan.address
                    .as_deref()
                    .and_then(|address| Ipv4Net::parse(address).ok())
                    .map(|net| (vlan.name.clone(), net))
            }))
            .chain(self.bridges.iter().filter_map(|bridge| {
                bridge
                    .address
                    .as_deref()
                    .and_then(|address| Ipv4Net::parse(address).ok())
                    .map(|net| (bridge.name.clone(), net))
            }))
            .collect();

        for (index, (name, net)) in nets.iter().enumerate() {
            for (other_name, other) in nets.iter().skip(index + 1) {
                if net.overlaps(other) {
                    problems.push(Problem::new(
                        "interfaces",
                        format!(
                            "{name} ({} ) overlaps {other_name} ({})",
                            net.addr_cidr(),
                            other.addr_cidr()
                        ),
                    ));
                }
            }
        }
    }

    /// True when a bridge member also carries a static address.
    fn member_has_address(&self, name: &str) -> bool {
        self.interfaces
            .iter()
            .any(|iface| iface.name == name && iface.address.is_some())
            || self
                .vlans
                .iter()
                .any(|vlan| vlan.name == name && vlan.address.is_some())
    }

    /// The static subnet a named interface carries, if any.
    fn subnet_for_interface(&self, name: &str) -> Option<Ipv4Net> {
        self.bridges
            .iter()
            .find(|bridge| bridge.name == name)
            .and_then(|bridge| bridge.address.as_deref())
            .or_else(|| {
                self.vlans
                    .iter()
                    .find(|vlan| vlan.name == name)
                    .and_then(|vlan| vlan.address.as_deref())
            })
            .or_else(|| {
                self.interfaces
                    .iter()
                    .find(|iface| iface.name == name)
                    .and_then(|iface| iface.address.as_deref())
            })
            .and_then(|address| Ipv4Net::parse(address).ok())
    }
}

fn check_optional_address(value: &Option<String>, field: &str, problems: &mut Vec<Problem>) {
    if let Some(address) = value
        && let Err(err) = Ipv4Net::parse(address)
    {
        problems.push(Problem::new(field, err));
    }
}

fn parse_required(
    value: &str,
    field: &str,
    problems: &mut Vec<Problem>,
) -> Option<std::net::Ipv4Addr> {
    if value.trim().is_empty() {
        problems.push(Problem::new(field, "value is required"));
        return None;
    }
    match crate::networking::ipmath::parse_ipv4(value) {
        Ok(ip) => Some(ip),
        Err(err) => {
            problems.push(Problem::new(field, err));
            None
        }
    }
}

/// A DNS domain: at least two labels, no underscores, no leading/trailing dot.
pub fn is_valid_domain(value: &str) -> bool {
    let trimmed = value.trim().trim_end_matches('.');
    if trimmed.is_empty() || trimmed.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = trimmed.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// A hostname, which additionally allows a leading `*.` wildcard.
pub fn is_valid_hostname(value: &str) -> bool {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix("*.") {
        return is_valid_domain(rest);
    }
    is_valid_domain(trimmed)
}

/// Cloudflared origin services: `scheme://host[:port]` or `http_status:NNN`.
fn is_valid_service(value: &str) -> bool {
    let trimmed = value.trim();
    if let Some(code) = trimmed.strip_prefix("http_status:") {
        return code
            .parse::<u16>()
            .map(|code| (100..=599).contains(&code))
            .unwrap_or(false);
    }
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https" | "tcp" | "ssh" | "rdp" | "unix") {
        return false;
    }
    if rest.is_empty() || rest.contains(char::is_whitespace) || rest.contains('/') {
        return false;
    }
    true
}

fn default_true() -> bool {
    true
}

fn default_protocol() -> String {
    "tcp".to_string()
}

fn default_band() -> String {
    "5g".to_string()
}

fn default_country() -> String {
    "GB".to_string()
}

fn default_lan_bridge() -> String {
    "br0".to_string()
}

fn default_security() -> String {
    "wpa2".to_string()
}

fn default_input_policy() -> FirewallAction {
    FirewallAction::Drop
}

fn default_management_ports() -> Vec<u16> {
    vec![22, 8553]
}

fn default_lease() -> String {
    "12h".to_string()
}

fn default_dns_listen() -> Vec<String> {
    vec!["127.0.0.1".to_string()]
}

fn default_cache_min() -> u32 {
    60
}

fn default_cache_max() -> u32 {
    86_400
}

fn default_cache_size() -> u32 {
    64
}

fn default_threads() -> u8 {
    2
}

fn default_download_kbps() -> u32 {
    100_000
}

fn default_upload_kbps() -> u32 {
    20_000
}

fn default_link_layer() -> String {
    "ethernet".to_string()
}

fn default_ifb() -> String {
    "ifb0".to_string()
}

fn default_credentials_file() -> String {
    "/etc/cloudflared/credentials.json".to_string()
}

fn default_cert_directory() -> String {
    "/etc/stack-stack/certs".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lan_router() -> NetworkConfig {
        NetworkConfig {
            interfaces: vec![
                InterfaceConfig {
                    name: "eth0".to_string(),
                    role: InterfaceRole::Wan,
                    address: None,
                },
                InterfaceConfig {
                    name: "eth1".to_string(),
                    role: InterfaceRole::Lan,
                    address: None,
                },
            ],
            bridges: vec![BridgeConfig {
                name: "br0".to_string(),
                members: vec!["eth1".to_string()],
                address: Some("192.168.1.1/24".to_string()),
                stp: false,
                comment: String::new(),
            }],
            dhcp: DhcpConfig {
                enabled: true,
                interface: "br0".to_string(),
                range_start: "192.168.1.100".to_string(),
                range_end: "192.168.1.250".to_string(),
                gateway: "192.168.1.1".to_string(),
                dns_servers: vec!["192.168.1.1".to_string()],
                ..DhcpConfig::default()
            },
            dns: DnsConfig {
                listen: vec!["192.168.1.1".to_string(), "127.0.0.1".to_string()],
                ..DnsConfig::default()
            },
            ..NetworkConfig::default()
        }
    }

    #[test]
    fn default_document_deserialises_from_empty_json() {
        let cfg: NetworkConfig = serde_json::from_str("{}").expect("parse");
        assert!(!cfg.wifi.enabled);
        assert_eq!(cfg.firewall.default_input, FirewallAction::Drop);
        assert_eq!(cfg.firewall.management_ports, vec![22, 8553]);
        assert!(!cfg.dhcp.enabled, "DHCP is opt-in");
        assert!(cfg.dns.enabled);
        assert_eq!(cfg.dns.listen, vec!["127.0.0.1".to_string()]);
    }

    #[test]
    fn default_document_is_valid() {
        // A fresh install must not greet the operator with validation errors.
        let problems = NetworkConfig::default().validate();
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn partial_section_keeps_secure_defaults() {
        // Omitting allow_established must not silently flip it to false.
        let cfg: NetworkConfig =
            serde_json::from_str(r#"{"firewall":{"rules":[]}}"#).expect("parse");
        assert!(cfg.firewall.allow_established);
        assert!(cfg.firewall.allow_icmp);
        assert_eq!(cfg.firewall.default_input, FirewallAction::Drop);
    }

    #[test]
    fn sane_lan_router_validates_clean() {
        let problems = lan_router().validate();
        assert!(problems.is_empty(), "unexpected problems: {problems:?}");
    }

    #[test]
    fn dhcp_range_outside_subnet_is_rejected() {
        let mut cfg = lan_router();
        cfg.dhcp.range_end = "192.168.2.50".to_string();
        let problems = cfg.validate();
        assert!(
            problems.iter().any(|p| p.field == "dhcp.range_start"),
            "{problems:?}"
        );
    }

    #[test]
    fn reservation_inside_dynamic_pool_is_rejected() {
        let mut cfg = lan_router();
        cfg.dhcp.reservations.push(DhcpReservation {
            mac: "aa:bb:cc:dd:ee:ff".to_string(),
            ip: "192.168.1.120".to_string(),
            name: "nas".to_string(),
        });
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("inside the dynamic DHCP range")),
            "{problems:?}"
        );
    }

    #[test]
    fn overlapping_lan_subnets_are_rejected() {
        let mut cfg = lan_router();
        cfg.vlans.push(VlanConfig {
            name: "eth1.10".to_string(),
            parent: "eth1".to_string(),
            id: 10,
            address: Some("192.168.1.128/25".to_string()),
            comment: String::new(),
        });
        let problems = cfg.validate();
        assert!(
            problems.iter().any(|p| p.message.contains("overlaps")),
            "{problems:?}"
        );
    }

    #[test]
    fn vlan_parent_must_exist() {
        let mut cfg = lan_router();
        cfg.vlans.push(VlanConfig {
            name: "eth9.10".to_string(),
            parent: "eth9".to_string(),
            id: 10,
            address: Some("10.10.0.1/24".to_string()),
            comment: String::new(),
        });
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("parent interface")),
            "{problems:?}"
        );
    }

    #[test]
    fn two_wan_interfaces_are_rejected() {
        let mut cfg = lan_router();
        cfg.interfaces[1].role = InterfaceRole::Wan;
        let problems = cfg.validate();
        assert!(
            problems.iter().any(|p| p.message.contains("marked as WAN")),
            "{problems:?}"
        );
    }

    #[test]
    fn port_forward_outside_lan_is_rejected() {
        let mut cfg = lan_router();
        cfg.port_forwards.push(PortForward {
            id: "web".to_string(),
            protocol: "tcp".to_string(),
            external_port: 443,
            internal_ip: "10.9.9.9".to_string(),
            internal_port: 443,
            comment: String::new(),
            enabled: true,
        });
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("not inside any configured LAN")),
            "{problems:?}"
        );
    }

    #[test]
    fn duplicate_external_port_is_rejected() {
        let mut cfg = lan_router();
        for id in ["a", "b"] {
            cfg.port_forwards.push(PortForward {
                id: id.to_string(),
                protocol: "tcp".to_string(),
                external_port: 443,
                internal_ip: "192.168.1.10".to_string(),
                internal_port: 443,
                comment: String::new(),
                enabled: true,
            });
        }
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("forwarded more than once")),
            "{problems:?}"
        );
    }

    #[test]
    fn dhcp_dns_server_must_be_served_locally() {
        let mut cfg = lan_router();
        cfg.dhcp.dns_servers = vec!["1.1.1.1".to_string()];
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("is not in dns.listen")),
            "{problems:?}"
        );
    }

    #[test]
    fn wifi_requires_a_long_enough_passphrase() {
        let mut cfg = lan_router();
        cfg.wifi = WifiConfig {
            enabled: true,
            interface: "wlan0".to_string(),
            ssid: "home".to_string(),
            passphrase: "short".to_string(),
            bridge: "br0".to_string(),
            ..WifiConfig::default()
        };
        let problems = cfg.validate();
        assert!(
            problems.iter().any(|p| p.field == "wifi.passphrase"),
            "{problems:?}"
        );
    }

    #[test]
    fn sqm_requires_a_wan() {
        let mut cfg = lan_router();
        cfg.interfaces[0].role = InterfaceRole::Unused;
        cfg.sqm = SqmConfig {
            enabled: true,
            interface: "eth0".to_string(),
            ..SqmConfig::default()
        };
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("no interface is marked as WAN")),
            "{problems:?}"
        );
    }

    #[test]
    fn tunnel_ingress_service_is_validated() {
        let mut cfg = lan_router();
        cfg.tunnel = TunnelConfig {
            enabled: true,
            tunnel_id: "abc".to_string(),
            ingress: vec![TunnelIngress {
                hostname: "adguard.omaritani.dev".to_string(),
                service: "not a url".to_string(),
                comment: String::new(),
            }],
            ..TunnelConfig::default()
        };
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.field == "tunnel.ingress[0].service"),
            "{problems:?}"
        );
    }

    #[test]
    fn dns_rewrite_hosts_must_be_unique() {
        let mut cfg = lan_router();
        for _ in 0..2 {
            cfg.dns.rewrites.push(DnsRewrite {
                host: "adguard.omaritani.dev".to_string(),
                address: "192.168.1.10".to_string(),
                comment: String::new(),
            });
        }
        let problems = cfg.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("rewritten twice")),
            "{problems:?}"
        );
    }

    #[test]
    fn wildcard_domains_are_valid_for_certs() {
        assert!(is_valid_hostname("*.omaritani.dev"));
        assert!(is_valid_hostname("adguard.omaritani.dev"));
        assert!(!is_valid_hostname("localhost"));
        assert!(!is_valid_hostname("*.dev"));
        assert!(!is_valid_hostname("bad..domain.dev"));
    }
}
