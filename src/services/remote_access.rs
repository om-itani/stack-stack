//! Remote access integrations for zero-config mesh networks: Tailscale, NetBird, and Cloudflare.

use serde::{Deserialize, Serialize};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteAccessOverview {
    pub tailscale: TailscaleStatus,
    pub netbird: NetBirdStatus,
    pub cloudflared: CloudflaredStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailscaleStatus {
    pub installed: bool,
    pub running: bool,
    pub ip: Option<String>,
    pub hostname: Option<String>,
    pub peers_count: usize,
    pub auth_url: Option<String>,
    pub raw: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetBirdStatus {
    pub installed: bool,
    pub running: bool,
    pub ip: Option<String>,
    pub peers_count: usize,
    pub management_url: Option<String>,
    pub raw: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflaredStatus {
    pub installed: bool,
    pub active: bool,
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TailscaleUpRequest {
    pub auth_key: String,
    pub hostname: Option<String>,
    pub exit_node: Option<bool>,
    pub accept_routes: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct NetBirdUpRequest {
    pub setup_key: String,
    pub management_url: Option<String>,
}

pub fn get_overview() -> RemoteAccessOverview {
    RemoteAccessOverview {
        tailscale: tailscale_status(),
        netbird: netbird_status(),
        cloudflared: cloudflared_status(),
    }
}

pub fn tailscale_status() -> TailscaleStatus {
    let output = match crate::proc::run(
        "tailscale",
        &["status".to_string(), "--json".to_string()],
        None,
        Duration::from_secs(5),
    ) {
        Ok(out) => out,
        Err(_) => {
            return TailscaleStatus {
                installed: false,
                running: false,
                ip: None,
                hostname: None,
                peers_count: 0,
                auth_url: None,
                raw: None,
            };
        }
    };

    let mut status = TailscaleStatus {
        installed: true,
        running: false,
        ip: None,
        hostname: None,
        peers_count: 0,
        auth_url: None,
        raw: Some(output.combined()),
    };

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&output.stdout) {
        let backend_state = val
            .get("BackendState")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        status.running = backend_state.eq_ignore_ascii_case("running");
        status.auth_url = val
            .get("AuthURL")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(self_node) = val.get("Self") {
            if let Some(ips) = self_node.get("TailscaleIPs").and_then(|v| v.as_array()) {
                status.ip = ips.first().and_then(|v| v.as_str()).map(|s| s.to_string());
            }
            if let Some(h) = self_node.get("HostName").and_then(|v| v.as_str()) {
                status.hostname = Some(h.to_string());
            }
        }

        if let Some(peers) = val.get("Peer").and_then(|v| v.as_object()) {
            status.peers_count = peers.len();
        }
    }

    status
}

pub fn tailscale_up(req: &TailscaleUpRequest) -> Result<String, String> {
    let key = req.auth_key.trim();
    if key.is_empty() {
        return Err("Tailscale auth key cannot be empty".to_string());
    }

    let mut args = vec!["up".to_string(), "--authkey".to_string(), key.to_string()];
    if let Some(host) = &req.hostname
        && !host.trim().is_empty()
    {
        args.push("--hostname".to_string());
        args.push(host.trim().to_string());
    }
    if req.exit_node.unwrap_or(false) {
        args.push("--advertise-exit-node".to_string());
    }
    if req.accept_routes.unwrap_or(false) {
        args.push("--accept-routes".to_string());
    }

    let out = crate::proc::run("tailscale", &args, None, TIMEOUT)
        .map_err(|e| format!("Failed to invoke tailscale: {e}"))?;

    if out.success() {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(out.combined().trim().to_string())
    }
}

pub fn tailscale_down() -> Result<String, String> {
    let out = crate::proc::run(
        "tailscale",
        &["down".to_string()],
        None,
        Duration::from_secs(10),
    )
    .map_err(|e| format!("Failed to invoke tailscale: {e}"))?;

    if out.success() {
        Ok("Tailscale disconnected".to_string())
    } else {
        Err(out.combined().trim().to_string())
    }
}

pub fn netbird_status() -> NetBirdStatus {
    let output = match crate::proc::run(
        "netbird",
        &["status".to_string()],
        None,
        Duration::from_secs(5),
    ) {
        Ok(out) => out,
        Err(_) => {
            return NetBirdStatus {
                installed: false,
                running: false,
                ip: None,
                peers_count: 0,
                management_url: None,
                raw: None,
            };
        }
    };

    let raw = output.combined();
    let mut status = NetBirdStatus {
        installed: true,
        running: false,
        ip: None,
        peers_count: 0,
        management_url: None,
        raw: Some(raw.clone()),
    };

    parse_netbird_text(&raw, &mut status);
    status
}

pub fn parse_netbird_text(raw: &str, status: &mut NetBirdStatus) {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Management:") && trimmed.contains("Connected") {
            status.running = true;
        } else if let Some(ip_part) = trimmed.strip_prefix("NetBird IP:") {
            let ip = ip_part.split('/').next().unwrap_or("").trim();
            if !ip.is_empty() {
                status.ip = Some(ip.to_string());
            }
        } else if let Some(count_part) = trimmed.strip_prefix("Peers count:")
            && let Some(num_str) = count_part.split('/').next()
            && let Ok(n) = num_str.trim().parse::<usize>()
        {
            status.peers_count = n;
        } else if let Some(url) = trimmed.strip_prefix("Management URL:") {
            status.management_url = Some(url.trim().to_string());
        }
    }
}

pub fn netbird_up(req: &NetBirdUpRequest) -> Result<String, String> {
    let key = req.setup_key.trim();
    if key.is_empty() {
        return Err("NetBird setup key cannot be empty".to_string());
    }

    let mut args = vec!["up".to_string(), "--setup-key".to_string(), key.to_string()];
    if let Some(url) = &req.management_url
        && !url.trim().is_empty()
    {
        args.push("--management-url".to_string());
        args.push(url.trim().to_string());
    }

    let out = crate::proc::run("netbird", &args, None, TIMEOUT)
        .map_err(|e| format!("Failed to invoke netbird: {e}"))?;

    if out.success() {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(out.combined().trim().to_string())
    }
}

pub fn netbird_down() -> Result<String, String> {
    let out = crate::proc::run(
        "netbird",
        &["down".to_string()],
        None,
        Duration::from_secs(10),
    )
    .map_err(|e| format!("Failed to invoke netbird: {e}"))?;

    if out.success() {
        Ok("NetBird disconnected".to_string())
    } else {
        Err(out.combined().trim().to_string())
    }
}

pub fn cloudflared_status() -> CloudflaredStatus {
    let ver_out = crate::proc::run(
        "cloudflared",
        &["--version".to_string()],
        None,
        Duration::from_secs(3),
    );
    let installed = ver_out.as_ref().map(|o| o.success()).unwrap_or(false);
    let version = ver_out
        .ok()
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty());

    let act_out = crate::proc::run(
        "systemctl",
        &["is-active".to_string(), "cloudflared".to_string()],
        None,
        Duration::from_secs(3),
    );
    let active = act_out
        .as_ref()
        .map(|o| o.stdout.trim() == "active")
        .unwrap_or(false);

    CloudflaredStatus {
        installed,
        active,
        version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_netbird_status_text() {
        let sample = "Management: Connected\nSignal: Connected\nNetBird IP: 100.64.0.5/16\nPeers count: 4/4 Connected\nManagement URL: https://api.netbird.io";
        let mut status = NetBirdStatus {
            installed: true,
            running: false,
            ip: None,
            peers_count: 0,
            management_url: None,
            raw: None,
        };
        parse_netbird_text(sample, &mut status);
        assert!(status.running);
        assert_eq!(status.ip.as_deref(), Some("100.64.0.5"));
        assert_eq!(status.peers_count, 4);
        assert_eq!(
            status.management_url.as_deref(),
            Some("https://api.netbird.io")
        );
    }
}
