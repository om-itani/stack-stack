//! WireGuard road-home VPN.
//!
//! One interface (`wg0` by default) with a generated `wg-quick` configuration.
//! The server's private key lives in its own 0600 document and is never part of
//! the shareable topology document; a client's private key is generated on
//! demand for an export, shown once, and never stored.
//!
//! Keys are produced by the `wg` tool itself (`wg genkey`, and the public key by
//! piping a private key into `wg pubkey`), so this crate never implements
//! cryptography. Passwords and keys reach the tool over stdin.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::apply::StepResult;
use crate::networking::ipmath::{Ipv4Net, parse_ipv4};
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;
use crate::services::{step, trimmed};

pub const WG_QUICK_DIR: &str = "/etc/wireguard";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WgPeer {
    pub name: String,
    /// Base64 public key, managed by the UI (generated on this host or pasted).
    pub public_key: String,
    /// Routes pushed into the tunnel for this peer, e.g. the peer's tunnel IP
    /// plus the LAN CIDR for a full-access road home.
    pub allowed_ips: Vec<String>,
    /// Optional DNS server handed to the client (the host's LAN IP by default).
    pub dns: Option<String>,
    pub persistent_keepalive: u32,
    pub enabled: bool,
}

impl Default for WgPeer {
    fn default() -> Self {
        Self {
            name: String::new(),
            public_key: String::new(),
            allowed_ips: Vec::new(),
            dns: None,
            persistent_keepalive: 25,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WgConfig {
    pub enabled: bool,
    pub interface: String,
    pub address_cidr: String,
    pub listen_port: u16,
    pub mtu: u16,
    pub peers: Vec<WgPeer>,
}

impl Default for WgConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: "wg0".to_string(),
            address_cidr: "10.10.0.1/24".to_string(),
            listen_port: 51_820,
            mtu: 1420,
            peers: Vec::new(),
        }
    }
}

impl WgConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        if !self.enabled {
            return problems;
        }
        if !self.interface.starts_with("wg")
            || self.interface.len() > 15
            || !self.interface[2..].chars().all(|c| c.is_ascii_digit())
        {
            problems.push(Problem {
                field: "interface".to_string(),
                message: "interface must be wg0, wg1, …".to_string(),
            });
        }
        if Ipv4Net::parse(&self.address_cidr).is_err() {
            problems.push(Problem {
                field: "address_cidr".to_string(),
                message: "address must be an IPv4 CIDR (e.g. 10.10.0.1/24)".to_string(),
            });
        }
        if crate::networking::ipmath::Port::new(self.listen_port).is_err() {
            problems.push(Problem {
                field: "listen_port".to_string(),
                message: "listen port must be 1-65535".to_string(),
            });
        }
        if !(576..=1500).contains(&self.mtu) {
            problems.push(Problem {
                field: "mtu".to_string(),
                message: "mtu must be between 576 and 1500".to_string(),
            });
        }

        let tunnel = Ipv4Net::parse(&self.address_cidr).ok();
        let mut names: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();

        for (index, peer) in self.peers.iter().enumerate() {
            let field = format!("peers[{index}]");
            let name = peer.name.trim();
            if name.is_empty() {
                problems.push(Problem {
                    field: format!("{field}.name"),
                    message: "a peer name is required".to_string(),
                });
            } else if names.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
                problems.push(Problem {
                    field: format!("{field}.name"),
                    message: format!("peer {name:?} is defined twice"),
                });
            }
            names.push(name.to_string());

            if !is_wg_key(&peer.public_key) {
                problems.push(Problem {
                    field: format!("{field}.public_key"),
                    message: "public key must be 44 base64 characters ending in '='".to_string(),
                });
            } else if keys.contains(&peer.public_key) {
                problems.push(Problem {
                    field: format!("{field}.public_key"),
                    message: "this public key is already used by another peer".to_string(),
                });
            }
            keys.push(peer.public_key.clone());

            if peer.allowed_ips.is_empty() {
                problems.push(Problem {
                    field: format!("{field}.allowed_ips"),
                    message: "at least one allowed IP is required".to_string(),
                });
            }
            for allowed in &peer.allowed_ips {
                if Ipv4Net::parse(allowed).is_err() {
                    problems.push(Problem {
                        field: format!("{field}.allowed_ips"),
                        message: format!("{allowed:?} is not an IPv4 CIDR"),
                    });
                }
            }
            // The peer's tunnel address must sit inside the interface network.
            if let (Some(tunnel), Some(first)) = (tunnel, peer.allowed_ips.first())
                && let Ok(net) = Ipv4Net::parse(first)
                && !tunnel.contains(net.addr())
            {
                problems.push(Problem {
                    field: format!("{field}.allowed_ips"),
                    message: format!(
                        "{} is outside the tunnel network {}",
                        first, self.address_cidr
                    ),
                });
            }
            if let Some(dns) = peer.dns.as_deref().map(str::trim).filter(|d| !d.is_empty())
                && parse_ipv4(dns).is_err()
            {
                problems.push(Problem {
                    field: format!("{field}.dns"),
                    message: "dns must be an IPv4 address".to_string(),
                });
            }
        }
        problems
    }

    /// Render the `wg-quick` configuration for the interface.
    ///
    /// `private_key` must already have been loaded from the secrets document;
    /// the topology document itself never carries it.
    pub fn render(&self, private_key: &str) -> String {
        let mut out = String::new();
        out.push_str("# Managed by stack-stack. Contains a private key; keep it 0600.\n");
        out.push_str("[Interface]\n");
        let address = Ipv4Net::parse(&self.address_cidr)
            .map(|net| net.addr_cidr())
            .unwrap_or_else(|_| self.address_cidr.clone());
        let _ = writeln!(out, "Address = {address}");
        let _ = writeln!(out, "ListenPort = {}", self.listen_port);
        let _ = writeln!(out, "PrivateKey = {}", private_key.trim());
        let _ = writeln!(out, "MTU = {}", self.mtu);

        for peer in self.peers.iter().filter(|peer| peer.enabled) {
            out.push('\n');
            let _ = writeln!(out, "[Peer]");
            if !peer.name.trim().is_empty() {
                let _ = writeln!(out, "# {}", peer.name.trim());
            }
            let _ = writeln!(out, "PublicKey = {}", peer.public_key.trim());
            let allowed = peer
                .allowed_ips
                .iter()
                .filter_map(|ip| Ipv4Net::parse(ip).ok().map(|net| net.network_cidr()))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(out, "AllowedIPs = {allowed}");
            if peer.persistent_keepalive > 0 {
                let _ = writeln!(out, "PersistentKeepalive = {}", peer.persistent_keepalive);
            }
        }
        out
    }
}

/// WireGuard keys are 32 bytes of base64: 43 characters plus '='.
pub fn is_wg_key(key: &str) -> bool {
    let key = key.trim();
    key.len() == 44
        && key.ends_with('=')
        && key[..43]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/'))
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct WgContext {
    pub config_path: PathBuf,
    pub secrets_path: PathBuf,
    pub runner: Arc<dyn Runner>,
    /// Where generated `<interface>.conf` files are written.
    pub conf_dir: PathBuf,
}

impl WgContext {
    pub fn new(config_path: PathBuf, secrets_path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self {
            config_path,
            secrets_path,
            runner,
            conf_dir: PathBuf::from(WG_QUICK_DIR),
        }
    }

    /// Override the configuration directory (tests point it at a temp dir).
    #[cfg(test)]
    pub fn with_conf_dir(mut self, dir: PathBuf) -> Self {
        self.conf_dir = dir;
        self
    }

    /// Derive the server public key from the stored private key. Empty when no
    /// key is stored yet.
    pub fn public_key(&self) -> Result<String, String> {
        let secrets = load_secrets(&self.secrets_path);
        if !is_wg_key(&secrets.private_key) {
            return Ok(String::new());
        }
        derive_public(self.runner.as_ref(), &secrets.private_key).map_err(|err| err.to_string())
    }

    /// The current topology document.
    pub fn config(&self) -> WgConfig {
        load(&self.config_path).0
    }

    /// Path of the generated `wg-quick` configuration.
    pub fn conf_path(&self, interface: &str) -> PathBuf {
        self.conf_dir.join(format!("{interface}.conf"))
    }
}

/// The server key document (0600).
pub fn load(path: &Path) -> (WgConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<WgConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                WgConfig::default(),
                Some(format!(
                    "{} is not valid wireguard JSON: {err}",
                    path.display()
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (WgConfig::default(), None),
        Err(err) => (
            WgConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &Path, config: &WgConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise wireguard config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WgSecrets {
    pub private_key: String,
}

pub fn load_secrets(path: &Path) -> WgSecrets {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_secrets(path: &Path, secrets: &WgSecrets) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(secrets)
        .map_err(|err| ApiError::internal(format!("cannot serialise wireguard secrets: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))?;
    restrict_600(path);
    Ok(())
}

/// Generate a fresh keypair with the `wg` tool. The private key is returned
/// once; callers decide whether to persist it (server) or just show it (client).
pub fn generate_keypair(runner: &dyn Runner) -> Result<(String, String), ApiError> {
    let private = runner
        .call("wg", &["genkey"])
        .map_err(|err| ApiError::unavailable(format!("wg is not available: {err}")))?;
    if !private.success() || private.stdout.trim().is_empty() {
        return Err(ApiError::internal(format!(
            "wg genkey failed: {}",
            trimmed(&private.combined())
        )));
    }
    let private_key = private.stdout.trim().to_string();
    let public_key = derive_public(runner, &private_key)?;
    Ok((private_key, public_key))
}

/// Derive the public key from a private key over stdin.
pub fn derive_public(runner: &dyn Runner, private_key: &str) -> Result<String, ApiError> {
    let output = runner
        .run_with_input(
            "wg",
            &["pubkey".to_string()],
            Duration::from_secs(15),
            format!("{private_key}\n").as_bytes(),
        )
        .map_err(|err| ApiError::unavailable(format!("wg is not available: {err}")))?;
    if !output.success() || output.stdout.trim().is_empty() {
        return Err(ApiError::internal(format!(
            "wg pubkey failed: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(output.stdout.trim().to_string())
}

/// One peer row of `wg show <iface> dump`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerStatus {
    pub public_key: String,
    pub endpoint: String,
    pub allowed_ips: String,
    /// Unix seconds of the latest handshake; 0 means "never".
    pub latest_handshake: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct InterfaceStatus {
    pub public_key: String,
    pub listen_port: u16,
    pub peers: Vec<PeerStatus>,
}

/// Parse `wg show <iface> dump`: line 0 is the interface, the rest are peers.
pub fn parse_dump(output: &str) -> InterfaceStatus {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let Some(interface) = lines.next() else {
        return InterfaceStatus::default();
    };
    let fields: Vec<&str> = interface.split('\t').collect();
    // `wg show <iface> dump` line 0: private_key, public_key, listen_port, fwmark.
    let status = InterfaceStatus {
        public_key: fields.get(2).unwrap_or(&"").trim().to_string(),
        listen_port: fields
            .get(3)
            .and_then(|port| port.trim().parse::<u16>().ok())
            .unwrap_or(0),
        peers: Vec::new(),
    };

    let peers = lines
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 8 {
                return None;
            }
            Some(PeerStatus {
                public_key: fields[0].trim().to_string(),
                endpoint: fields[2].trim().to_string(),
                allowed_ips: fields[3].trim().to_string(),
                latest_handshake: fields[4].trim().parse::<u64>().unwrap_or(0),
                rx_bytes: fields[5].trim().parse::<u64>().unwrap_or(0),
                tx_bytes: fields[6].trim().parse::<u64>().unwrap_or(0),
            })
        })
        .collect();

    InterfaceStatus { peers, ..status }
}

pub fn status(runner: &dyn Runner, interface: &str) -> Result<InterfaceStatus, String> {
    let output = runner
        .run_timeout(
            "wg",
            &[
                "show".to_string(),
                interface.to_string(),
                "dump".to_string(),
            ],
            Duration::from_secs(15),
        )
        .map_err(|err| format!("wg is not available: {err}"))?;
    if !output.success() {
        return Err(format!("wg show failed: {}", trimmed(&output.combined())));
    }
    Ok(parse_dump(&output.stdout))
}

/// Render a client configuration. `client_private` is generated on demand and
/// never persisted; `endpoint` is `host:port`. `AllowedIPs` routes everything
/// through the tunnel: a full-access road home.
pub fn render_client(
    client_private: &str,
    client_address: &str,
    server_public: &str,
    endpoint: &str,
    dns: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("[Interface]\n");
    let _ = writeln!(out, "PrivateKey = {}", client_private.trim());
    let _ = writeln!(out, "Address = {}", client_address.trim());
    if let Some(dns) = dns.map(str::trim).filter(|d| !d.is_empty()) {
        let _ = writeln!(out, "DNS = {dns}");
    }
    out.push('\n');
    out.push_str("[Peer]\n");
    let _ = writeln!(out, "PublicKey = {}", server_public.trim());
    let _ = writeln!(out, "AllowedIPs = 0.0.0.0/0");
    let _ = writeln!(out, "Endpoint = {}", endpoint.trim());
    let _ = writeln!(out, "PersistentKeepalive = 25");
    out
}

/// A QR code as UTF-8 block characters via `qrencode`, when installed.
pub fn qr_text(runner: &dyn Runner, payload: &str) -> Result<String, ApiError> {
    let output = runner
        .run_with_input(
            "qrencode",
            &["-t".to_string(), "ansiutf8".to_string()],
            Duration::from_secs(15),
            format!("{payload}\n").as_bytes(),
        )
        .map_err(|err| ApiError::unavailable(format!("qrencode is not available: {err}")))?;
    if !output.success() {
        return Err(ApiError::internal(format!(
            "qrencode failed: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(output.stdout)
}

/// Apply: write the conf, then bring the interface up or down.
pub async fn apply(context: &WgContext, config: &WgConfig) -> Vec<StepResult> {
    let context = context.clone();
    let config = config.clone();
    match tokio::task::spawn_blocking(move || apply_sync(&context, &config)).await {
        Ok(steps) => steps,
        Err(err) => vec![StepResult {
            subsystem: "wireguard".to_string(),
            ok: false,
            detail: format!("wireguard apply task failed: {err}"),
        }],
    }
}

fn apply_sync(context: &WgContext, config: &WgConfig) -> Vec<StepResult> {
    let mut steps = Vec::new();
    let conf = context.conf_path(&config.interface);

    let write = (|| -> Result<String, String> {
        if !config.enabled {
            return Ok("wireguard disabled; configuration left in place".to_string());
        }
        let secrets = load_secrets(&context.secrets_path);
        if !is_wg_key(&secrets.private_key) {
            return Err("no server private key has been generated yet".to_string());
        }
        write_atomic(&conf, config.render(&secrets.private_key).as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", conf.display()))?;
        restrict_600(&conf);
        Ok(format!("{} written", conf.display()))
    })();
    steps.push(step("config", write));

    let quick = (|| -> Result<String, String> {
        let action = if config.enabled { "up" } else { "down" };
        let output = context
            .runner
            .run_timeout(
                "wg-quick",
                &[action.to_string(), config.interface.clone()],
                Duration::from_secs(60),
            )
            .map_err(|err| format!("wg-quick is not available: {err}"))?;
        let already = output.combined().contains("already exists")
            || output.combined().contains("is not present");
        if !output.success() && !already {
            return Err(format!(
                "wg-quick {action}: {}",
                trimmed(&output.combined())
            ));
        }
        if config.enabled {
            Ok(format!("{} is up", config.interface))
        } else {
            Ok(format!("{} is down", config.interface))
        }
    })();
    steps.push(step("interface", quick));

    steps
}

#[cfg(unix)]
fn restrict_600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_600(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    const PRIVATE: &str = "8Bj1dZ0SzLwCFLURvY6R7C0zL0dH0p3vWq0kS0l0rH4=";
    const PUBLIC: &str = "Fv0nS0m3P1bl1cK3yV4l1dT3stK3y00000000000000=";

    fn peer() -> WgPeer {
        WgPeer {
            name: "phone".to_string(),
            public_key: PUBLIC.to_string(),
            allowed_ips: vec!["10.10.0.2/32".to_string()],
            dns: Some("192.168.1.1".to_string()),
            persistent_keepalive: 25,
            enabled: true,
        }
    }

    fn config() -> WgConfig {
        WgConfig {
            enabled: true,
            peers: vec![peer()],
            ..WgConfig::default()
        }
    }

    #[test]
    fn default_config_validates_clean() {
        assert!(WgConfig::default().validate().is_empty());
        assert!(config().validate().is_empty());
    }

    #[test]
    fn validation_rejects_bad_keys_addresses_and_duplicates() {
        let config = WgConfig {
            interface: "eth9".to_string(),
            address_cidr: "10.10.0.0/33".to_string(),
            listen_port: 0,
            mtu: 10,
            peers: vec![
                peer(),
                WgPeer {
                    public_key: "not-a-key".to_string(),
                    allowed_ips: vec!["10.10.9.9/32".to_string()],
                    ..peer()
                },
            ],
            ..config()
        };
        let fields: Vec<String> = config.validate().iter().map(|p| p.field.clone()).collect();
        for expected in [
            "interface",
            "address_cidr",
            "listen_port",
            "mtu",
            "peers[1].public_key",
        ] {
            assert!(fields.iter().any(|field| field == expected), "{fields:?}");
        }
    }

    #[test]
    fn duplicate_peer_names_and_keys_are_rejected() {
        let config = WgConfig {
            peers: vec![
                peer(),
                WgPeer {
                    name: "PHONE".to_string(),
                    ..peer()
                },
            ],
            ..config()
        };
        let fields: Vec<String> = config.validate().iter().map(|p| p.field.clone()).collect();
        assert!(fields.iter().any(|field| field.ends_with("name")));
        assert!(fields.iter().any(|field| field.ends_with("public_key")));
    }

    #[test]
    fn render_contains_interface_and_peer_sections() {
        let rendered = config().render(PRIVATE);
        assert!(rendered.contains("[Interface]"));
        assert!(rendered.contains("Address = 10.10.0.1/24"));
        assert!(rendered.contains("ListenPort = 51820"));
        assert!(rendered.contains("PrivateKey = 8Bj1dZ0"));
        assert!(rendered.contains("MTU = 1420"));
        assert!(rendered.contains("[Peer]"));
        assert!(rendered.contains("# phone"));
        assert!(rendered.contains("PublicKey = Fv0nS0m3"));
        assert!(rendered.contains("AllowedIPs = 10.10.0.2/32"));
        assert!(rendered.contains("PersistentKeepalive = 25"));
    }

    #[test]
    fn key_shape_is_checked() {
        assert!(is_wg_key(PRIVATE));
        assert!(!is_wg_key("short"));
        assert!(!is_wg_key(&format!("{}x", PRIVATE)));
        assert!(!is_wg_key(&PRIVATE[..43]));
    }

    #[test]
    fn dump_output_is_parsed() {
        let dump = format!(
            "wg0\t{PRIVATE}\tFv0nServerKey=\t51820\t0\n{PUBLIC}\t(none)\t1.2.3.4:51820\t10.10.0.2/32\t1700000000\t1024\t2048\t25\n"
        );
        let status = parse_dump(&dump);
        assert_eq!(status.listen_port, 51820);
        assert_eq!(status.peers.len(), 1);
        assert_eq!(status.peers[0].endpoint, "1.2.3.4:51820");
        assert_eq!(status.peers[0].latest_handshake, 1_700_000_000);
        assert_eq!(status.peers[0].rx_bytes, 1024);
        assert_eq!(status.peers[0].tx_bytes, 2048);
    }

    #[test]
    fn dump_with_no_peers_yields_an_empty_list() {
        let status = parse_dump("wg0\tkey\tpub\t51820\t0\n");
        assert!(status.peers.is_empty());
    }

    #[test]
    fn keys_are_generated_via_wg_and_pubkey_over_stdin() {
        let runner = FakeRunner::new()
            .respond("wg", Some(0), &format!("{PRIVATE}\n"), "")
            .respond_when("wg", "pubkey", Some(0), &format!("{PUBLIC}\n"), "");
        let (private, public) = generate_keypair(&runner).expect("keypair");
        assert_eq!(private, PRIVATE);
        assert_eq!(public, PUBLIC);
        assert!(runner.ran_with("wg", &["genkey"]));
        assert!(runner.ran_with("wg", &["pubkey"]));
    }

    #[test]
    fn apply_writes_a_conf_and_brings_the_interface_up() {
        let dir = std::env::temp_dir().join(format!("ss-wg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conf_dir = dir.join("wireguard");
        std::fs::create_dir_all(&conf_dir).expect("mkdir");
        let secrets_path = dir.join("wg.secrets.json");
        save_secrets(
            &secrets_path,
            &WgSecrets {
                private_key: PRIVATE.to_string(),
            },
        )
        .expect("save");

        let runner = Arc::new(FakeRunner::new().respond("wg-quick", Some(0), "", ""));
        let context = WgContext::new(
            dir.join("wg.json"),
            secrets_path,
            runner.clone() as Arc<dyn Runner>,
        )
        .with_conf_dir(conf_dir.clone());
        let steps = apply_sync(&context, &config());
        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
        assert!(runner.ran_with("wg-quick", &["up", "wg0"]));
        let conf = std::fs::read_to_string(conf_dir.join("wg0.conf")).expect("conf");
        assert!(conf.contains("PrivateKey = 8Bj1dZ0"));
    }

    #[test]
    fn apply_without_a_key_fails_cleanly() {
        let dir = std::env::temp_dir().join(format!("ss-wg-nokey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let runner = FakeRunner::new().respond("wg-quick", Some(0), "", "");
        let context = WgContext::new(
            dir.join("wg.json"),
            dir.join("secrets.json"),
            Arc::new(runner),
        )
        .with_conf_dir(dir.join("wireguard"));
        let steps = apply_sync(&context, &config());
        let config_step = steps
            .iter()
            .find(|s| s.subsystem == "config")
            .expect("config step");
        assert!(!config_step.ok);
        assert!(config_step.detail.contains("private key"));
    }

    #[test]
    fn client_conf_contains_server_peer_and_full_tunnel() {
        let rendered = render_client(
            PRIVATE,
            "10.10.0.2/32",
            PUBLIC,
            "192.168.1.1:51820",
            Some("192.168.1.1"),
        );
        assert!(rendered.contains("PrivateKey = 8Bj1dZ0"));
        assert!(rendered.contains("Address = 10.10.0.2/32"));
        assert!(rendered.contains("DNS = 192.168.1.1"));
        assert!(rendered.contains("PublicKey = Fv0nS0m3"));
        assert!(rendered.contains("AllowedIPs = 0.0.0.0/0"));
        assert!(rendered.contains("Endpoint = 192.168.1.1:51820"));
        assert!(rendered.contains("PersistentKeepalive = 25"));
    }

    #[test]
    fn qrencode_is_used_when_present() {
        let runner = FakeRunner::new().respond("qrencode", Some(0), "█▀▀", "");
        let qr = qr_text(&runner, "data").expect("qr");
        assert!(qr.contains("█"));
    }
}
