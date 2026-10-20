//! Read-only probes for the "is it actually running" panel.
//!
//! Everything here is best-effort: a missing `systemctl` or `nft` on a
//! development host must produce an "unavailable" entry, never an error.

use serde::Serialize;

use crate::networking::model::NetworkConfig;
use crate::networking::runner::Runner;

/// Daemons the network page reports on.
const UNITS: [&str; 3] = ["dnsmasq", "unbound", "cloudflared"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub unit: String,
    /// systemd's own word: `active`, `inactive`, `failed`, `unknown`, …
    pub state: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirewallStatus {
    pub loaded: bool,
    /// Rules matching the generated table, counted by verdict keyword.
    pub rules: usize,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SqmStatus {
    pub active: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkStatus {
    pub services: Vec<ServiceStatus>,
    pub firewall: FirewallStatus,
    pub sqm: SqmStatus,
}

pub fn probe(config: &NetworkConfig, runner: &dyn Runner) -> NetworkStatus {
    NetworkStatus {
        services: UNITS.iter().map(|unit| service(runner, unit)).collect(),
        firewall: firewall(runner),
        sqm: sqm(config, runner),
    }
}

fn service(runner: &dyn Runner, unit: &str) -> ServiceStatus {
    let state = match runner.call("systemctl", &["is-active", unit]) {
        Ok(output) => {
            let reported = output.stdout.trim();
            if reported.is_empty() {
                "unknown".to_string()
            } else {
                reported.to_string()
            }
        }
        Err(_) => "unavailable".to_string(),
    };

    ServiceStatus {
        name: unit.to_string(),
        unit: format!("{unit}.service"),
        active: state == "active",
        state,
    }
}

fn firewall(runner: &dyn Runner) -> FirewallStatus {
    match runner.call("nft", &["list", "table", "inet", super::nftables::TABLE]) {
        Ok(output) if output.success() => {
            let rules = output
                .stdout
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    !trimmed.is_empty()
                        && !trimmed.starts_with('#')
                        && trimmed != "{"
                        && trimmed != "}"
                        && ["accept", "drop", "reject", "masquerade", "dnat"]
                            .iter()
                            .any(|verdict| trimmed.contains(verdict))
                })
                .count();
            FirewallStatus {
                loaded: true,
                rules,
                detail: format!("{rules} rules loaded"),
            }
        }
        Ok(output) => FirewallStatus {
            loaded: false,
            rules: 0,
            detail: if output.timed_out {
                "timed out".to_string()
            } else {
                "ruleset not loaded".to_string()
            },
        },
        Err(_) => FirewallStatus {
            loaded: false,
            rules: 0,
            detail: "nftables unavailable".to_string(),
        },
    }
}

fn sqm(config: &NetworkConfig, runner: &dyn Runner) -> SqmStatus {
    let interface = if config.sqm.interface.is_empty() {
        config.wan_interface().unwrap_or_default()
    } else {
        config.sqm.interface.as_str()
    };
    if interface.is_empty() {
        return SqmStatus {
            active: false,
            detail: "no WAN interface".to_string(),
        };
    }

    match runner.call("tc", &["qdisc", "show", "dev", interface]) {
        Ok(output) if output.success() => {
            let active = output.stdout.contains("cake");
            SqmStatus {
                active,
                detail: if active {
                    format!("cake active on {interface}")
                } else {
                    format!("no cake qdisc on {interface}")
                },
            }
        }
        _ => SqmStatus {
            active: false,
            detail: "tc unavailable".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::model::{InterfaceConfig, InterfaceRole, SqmConfig};
    use crate::networking::runner::fake::FakeRunner;

    fn config() -> NetworkConfig {
        NetworkConfig {
            interfaces: vec![InterfaceConfig {
                name: "eth0".to_string(),
                role: InterfaceRole::Wan,
                address: None,
            }],
            sqm: SqmConfig {
                enabled: true,
                interface: "eth0".to_string(),
                ..SqmConfig::default()
            },
            ..NetworkConfig::default()
        }
    }

    #[test]
    fn reports_systemd_state_per_unit() {
        let runner = FakeRunner::new()
            .respond("systemctl", Some(0), "active\n", "")
            .respond_when("systemctl", "is-active unbound", Some(3), "inactive\n", "");

        let status = probe(&config(), &runner);
        let dnsmasq = status
            .services
            .iter()
            .find(|s| s.unit == "dnsmasq.service")
            .expect("dnsmasq");
        let unbound = status
            .services
            .iter()
            .find(|s| s.unit == "unbound.service")
            .expect("unbound");
        assert!(dnsmasq.active);
        assert_eq!(dnsmasq.state, "active");
        assert!(!unbound.active);
        assert_eq!(unbound.state, "inactive");
    }

    #[test]
    fn missing_systemctl_is_reported_as_unavailable() {
        let runner = FakeRunner::new().missing("systemctl");
        let status = probe(&config(), &runner);
        assert!(status.services.iter().all(|s| s.state == "unavailable"));
    }

    #[test]
    fn firewall_rule_count_ignores_boilerplate() {
        let table = "\
table inet stackstack {
\tchain input {
\t\ttype filter hook input priority filter; policy drop;
\t\tct state established,related accept
\t\tiifname \"lo\" accept
\t}
\tchain forward {
\t\ttype filter hook forward priority filter; policy drop;
\t}
}
";
        let runner = FakeRunner::new().respond("nft", Some(0), table, "");
        let status = probe(&config(), &runner);
        assert!(status.firewall.loaded);
        // Two accepts plus two "; policy drop;" policy lines.
        assert_eq!(status.firewall.rules, 4, "{:?}", status.firewall);
    }

    #[test]
    fn unloaded_firewall_is_not_an_error() {
        let runner = FakeRunner::new().respond("nft", Some(1), "", "No such file or directory");
        let status = probe(&config(), &runner);
        assert!(!status.firewall.loaded);
        assert_eq!(status.firewall.detail, "ruleset not loaded");
    }

    #[test]
    fn sqm_status_follows_the_qdisc() {
        let cake = FakeRunner::new().respond(
            "tc",
            Some(0),
            "qdisc cake 8001: root refcnt 2 bandwidth 20Mbit diffserv4 dual-srchost\n",
            "",
        );
        assert!(probe(&config(), &cake).sqm.active);

        let none =
            FakeRunner::new().respond("tc", Some(0), "qdisc pfifo_fast 0: root refcnt 2\n", "");
        assert!(!probe(&config(), &none).sqm.active);
    }
}
