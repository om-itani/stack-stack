//! Cloudflare DDNS.
//!
//! The WAN address comes from the router document (the interface marked WAN) —
//! never from a third-party echo service. Updates are plain `curl` calls to the
//! Cloudflare API: list the record, compare, `PUT` when it changed. The API
//! token lives in its own 0600 document, is handed to `curl` as a header built
//! in-process, and is redacted from every log line.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::{NetworkConfig, Problem};
use crate::networking::store::{self, write_atomic};
use crate::runner::Runner;

pub const DEFAULT_INTERVAL_MIN: u32 = 15;
pub const DEFAULT_TTL: u32 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DdnsConfig {
    pub enabled: bool,
    pub zone_id: String,
    /// Fully-qualified record names, e.g. `home.example.com`.
    pub record_names: Vec<String>,
    pub proxied: bool,
    pub interval_min: u32,
    pub ttl: u32,
    /// Last address pushed, so unchanged records are not written every tick.
    pub last_seen_ip: Option<String>,
    pub last_ok_at: Option<u64>,
}

impl Default for DdnsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            zone_id: String::new(),
            record_names: Vec::new(),
            proxied: false,
            interval_min: DEFAULT_INTERVAL_MIN,
            ttl: DEFAULT_TTL,
            last_seen_ip: None,
            last_ok_at: None,
        }
    }
}

impl DdnsConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        if !self.enabled {
            return problems;
        }
        if self.zone_id.trim().is_empty()
            || !self.zone_id.trim().chars().all(|c| c.is_ascii_hexdigit())
        {
            problems.push(Problem {
                field: "zone_id".to_string(),
                message: "zone id must be the 32-hex identifier from the Cloudflare dashboard"
                    .to_string(),
            });
        }
        if self.record_names.is_empty() {
            problems.push(Problem {
                field: "record_names".to_string(),
                message: "at least one record name is required".to_string(),
            });
        }
        for (index, name) in self.record_names.iter().enumerate() {
            let name = name.trim();
            let labels: Vec<&str> = name.split('.').collect();
            if labels.len() < 3
                || !labels.iter().all(|label| {
                    !label.is_empty()
                        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })
            {
                problems.push(Problem {
                    field: format!("record_names[{index}]"),
                    message: format!(
                        "{name:?} is not a fully-qualified name like home.example.com"
                    ),
                });
            }
        }
        if self.interval_min < 1 || self.interval_min > 1440 {
            problems.push(Problem {
                field: "interval_min".to_string(),
                message: "interval must be 1-1440 minutes".to_string(),
            });
        }
        if self.ttl != 1 && !(60..=86400).contains(&self.ttl) {
            problems.push(Problem {
                field: "ttl".to_string(),
                message: "ttl must be 1 (auto) or 60-86400 seconds".to_string(),
            });
        }
        problems
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DdnsSecrets {
    pub api_token: String,
}

pub fn load_secrets(path: &Path) -> DdnsSecrets {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_secrets(path: &Path, secrets: &DdnsSecrets) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(secrets)
        .map_err(|err| ApiError::internal(format!("cannot serialise ddns secrets: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))?;
    restrict_600(path);
    Ok(())
}

pub fn load(path: &Path) -> (DdnsConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<DdnsConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                DdnsConfig::default(),
                Some(format!("{} is not valid ddns JSON: {err}", path.display())),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (DdnsConfig::default(), None),
        Err(err) => (
            DdnsConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &Path, config: &DdnsConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise ddns config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

/// The WAN address from the router document, or `None` when the WAN is on DHCP
/// without a lease recorded.
pub fn wan_ip(network: &NetworkConfig) -> Option<String> {
    let wan = network.wan_interface()?;
    let iface = network.interfaces.iter().find(|iface| iface.name == wan)?;
    let address = iface.address.as_deref()?.trim().to_string();
    if address.is_empty() || address == "0.0.0.0" {
        return None;
    }
    // `a.b.c.d/n` → `a.b.c.d`.
    Some(address.split('/').next().unwrap_or(&address).to_string())
}

#[derive(Clone)]
pub struct DdnsContext {
    pub config_path: PathBuf,
    pub secrets_path: PathBuf,
    pub network_path: PathBuf,
    pub runner: Arc<dyn Runner>,
}

impl DdnsContext {
    pub fn new(
        config_path: PathBuf,
        secrets_path: PathBuf,
        network_path: PathBuf,
        runner: Arc<dyn Runner>,
    ) -> Self {
        Self {
            config_path,
            secrets_path,
            network_path,
            runner,
        }
    }

    pub fn config(&self) -> DdnsConfig {
        load(&self.config_path).0
    }

    pub fn network(&self) -> NetworkConfig {
        store::load(&self.network_path).0
    }

    pub fn wan_ip(&self) -> Option<String> {
        wan_ip(&self.network())
    }

    /// True when a token is stored.
    pub fn has_token(&self) -> bool {
        !load_secrets(&self.secrets_path).api_token.trim().is_empty()
    }
}

/// One Cloudflare A record as returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub proxied: bool,
    #[serde(default)]
    pub ttl: u32,
    #[serde(rename = "type")]
    pub kind: String,
}

/// Parse the `result` array of a record list response.
pub fn parse_records(json: &str) -> Result<Vec<DnsRecord>, ApiError> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|err| ApiError::internal(format!("cannot read the Cloudflare response: {err}")))?;
    if value.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let detail = value
            .get("errors")
            .map(|errors| errors.to_string())
            .unwrap_or_else(|| "unknown error".to_string());
        return Err(ApiError::internal(format!(
            "Cloudflare rejected the request: {}",
            detail.chars().take(300).collect::<String>()
        )));
    }
    let result = value
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let records = result
        .iter()
        .filter_map(|entry| {
            Some(DnsRecord {
                id: entry.get("id")?.as_str()?.to_string(),
                name: entry.get("name")?.as_str()?.to_string(),
                content: entry.get("content")?.as_str()?.to_string(),
                proxied: entry
                    .get("proxied")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                ttl: entry.get("ttl").and_then(|v| v.as_u64()).unwrap_or(300) as u32,
                kind: entry
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("A")
                    .to_string(),
            })
        })
        .collect();
    Ok(records)
}

/// One update attempt across every record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateOutcome {
    pub name: String,
    pub changed: bool,
    pub ok: bool,
    pub detail: String,
}

impl DdnsContext {
    /// Push the current WAN address to every configured record.
    pub async fn update(&self) -> Vec<UpdateOutcome> {
        let config = self.config();
        let Some(ip) = self.wan_ip() else {
            return vec![UpdateOutcome {
                name: "(wan)".to_string(),
                changed: false,
                ok: false,
                detail: "no WAN address is recorded in the interface document".to_string(),
            }];
        };
        let token = load_secrets(&self.secrets_path).api_token;
        if token.trim().is_empty() {
            return vec![UpdateOutcome {
                name: "(token)".to_string(),
                changed: false,
                ok: false,
                detail: "no API token is stored".to_string(),
            }];
        }

        let mut outcomes = Vec::new();
        for name in &config.record_names {
            let name = name.trim().to_string();
            let outcome = self.update_one(&name, &ip, &config, token.trim()).await;
            outcomes.push(outcome);
        }

        // Persist the "last seen" bookkeeping only when everything worked.
        if outcomes.iter().all(|outcome| outcome.ok) {
            let mut updated = config;
            let changed = updated.last_seen_ip.as_deref() != Some(ip.as_str());
            updated.last_seen_ip = Some(ip.clone());
            updated.last_ok_at = Some(now_secs());
            if save(Path::new(&self.config_path), &updated).is_ok() && changed {
                tracing::info!(%ip, "DDNS updated");
            }
        }
        outcomes
    }

    async fn update_one(
        &self,
        name: &str,
        ip: &str,
        config: &DdnsConfig,
        token: &str,
    ) -> UpdateOutcome {
        let runner = Arc::clone(&self.runner);
        let zone = config.zone_id.trim().to_string();
        let proxied = config.proxied;
        let ttl = config.ttl;
        let record_name = name.to_string();
        let ip_owned = ip.to_string();
        let token_owned = token.to_string();

        let result = tokio::task::spawn_blocking(move || {
            // curl config lines are read from stdin (`-K -`) so the API token
            // never appears in the process argument list, where `ps` would show
            // it to every local user.
            let base = vec![
                "-s".to_string(),
                "--fail".to_string(),
                "--max-time".to_string(),
                "20".to_string(),
                "-K".to_string(),
                "-".to_string(),
            ];

            // 1. Find the record id.
            let list_config = format!(
                "url = \"https://api.cloudflare.com/client/v4/zones/{zone}/dns_records?type=A&name={record_name}\"\n\
                 header = \"Authorization: Bearer {token_owned}\"\n"
            );
            let list = runner
                .run_with_input("curl", &base, Duration::from_secs(30), list_config.as_bytes())
                .map_err(|err| format!("curl is not available: {err}"))?;
            if !list.success() {
                return Err(trimmed(&list.combined()));
            }
            let records = parse_records(&list.stdout).map_err(|err| err.to_string())?;
            let Some(record) = records.into_iter().find(|record| record.kind == "A") else {
                return Err(format!("no A record named {record_name} exists in the zone"));
            };
            if record.content == ip_owned {
                return Ok((false, format!("{record_name} already points at {ip_owned}")));
            }

            // 2. Write the new address.
            let body = serde_json::json!({
                "type": "A",
                "name": record_name,
                "content": ip_owned,
                "ttl": ttl,
                "proxied": proxied,
            })
            .to_string();
            let put_config = format!(
                "url = \"https://api.cloudflare.com/client/v4/zones/{zone}/dns_records/{}\"\n\
                 header = \"Authorization: Bearer {token_owned}\"\n\
                 request = \"PUT\"\n\
                 data = \"{body}\"\n",
                record.id
            );
            let put = runner
                .run_with_input("curl", &base, Duration::from_secs(30), put_config.as_bytes())
                .map_err(|err| format!("curl is not available: {err}"))?;
            if !put.success() {
                return Err(trimmed(&put.combined()));
            }
            parse_records(&put.stdout)
                .map(|_| (true, format!("{record_name} → {ip_owned}")))
                .map_err(|err| err.to_string())
        })
        .await;

        match result {
            Ok(Ok((changed, detail))) => UpdateOutcome {
                name: name.to_string(),
                changed,
                ok: true,
                detail,
            },
            Ok(Err(detail)) => UpdateOutcome {
                name: name.to_string(),
                changed: false,
                ok: false,
                detail,
            },
            Err(err) => UpdateOutcome {
                name: name.to_string(),
                changed: false,
                ok: false,
                detail: format!("ddns task failed: {err}"),
            },
        }
    }
}

fn trimmed(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "no output".to_string()
    } else if text.len() > 300 {
        text[..300].to_string()
    } else {
        text.to_string()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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
    use crate::networking::model::InterfaceRole;
    use crate::runner::fake::FakeRunner;

    const LIST_JSON: &str = r#"{"success":true,"result":[{"id":"abc123","type":"A","name":"home.example.com","content":"1.2.3.4","proxied":false,"ttl":300}]}"#;

    fn network(wan: Option<&str>) -> NetworkConfig {
        let mut config = NetworkConfig::default();
        if let Some((name, address)) = wan.map(|value| value.split_once('=').unwrap_or((value, "")))
        {
            let iface = crate::networking::model::InterfaceConfig {
                name: name.to_string(),
                role: InterfaceRole::Wan,
                address: (!address.is_empty()).then(|| address.to_string()),
            };
            config.interfaces.push(iface);
        }
        config
    }

    #[test]
    fn default_config_validates_clean() {
        assert!(DdnsConfig::default().validate().is_empty());
    }

    #[test]
    fn enabled_config_is_validated() {
        let config = DdnsConfig {
            enabled: true,
            zone_id: "not-hex!".to_string(),
            record_names: vec!["home".to_string()],
            interval_min: 0,
            ttl: 10,
            ..DdnsConfig::default()
        };
        let fields: Vec<String> = config.validate().iter().map(|p| p.field.clone()).collect();
        for expected in ["zone_id", "record_names[0]", "interval_min", "ttl"] {
            assert!(fields.iter().any(|field| field == expected), "{fields:?}");
        }
    }

    #[test]
    fn wan_ip_comes_from_the_interface_document() {
        assert_eq!(
            wan_ip(&network(Some("eth0=203.0.113.7/24"))).as_deref(),
            Some("203.0.113.7")
        );
        assert_eq!(wan_ip(&network(Some("eth0="))), None);
        assert_eq!(wan_ip(&network(None)), None);
    }

    #[test]
    fn records_are_parsed_from_the_api_response() {
        let records = parse_records(LIST_JSON).expect("parse");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "abc123");
        assert_eq!(records[0].content, "1.2.3.4");
    }

    #[test]
    fn api_errors_surface_the_cloudflare_message() {
        let json = r#"{"success":false,"errors":[{"code":9103,"message":"Unknown X-Auth-Key"}]}"#;
        let err = parse_records(json).expect_err("rejected");
        assert!(err.to_string().contains("9103"), "{err}");
    }

    fn context(
        dir: &Path,
        network_path: &Path,
        secrets_path: &Path,
        runner: FakeRunner,
    ) -> (DdnsContext, Arc<FakeRunner>) {
        let runner = Arc::new(runner);
        (
            DdnsContext::new(
                dir.join("ddns.json"),
                secrets_path.to_path_buf(),
                network_path.to_path_buf(),
                runner.clone() as Arc<dyn Runner>,
            ),
            runner,
        )
    }

    fn write_config(dir: &Path, zone: &str, records: &[&str]) {
        let config = DdnsConfig {
            enabled: true,
            zone_id: zone.to_string(),
            record_names: records.iter().map(|name| (*name).to_string()).collect(),
            ..DdnsConfig::default()
        };
        save(&dir.join("ddns.json"), &config).expect("config");
    }

    #[test]
    fn unchanged_records_are_not_written() {
        let dir = std::env::temp_dir().join(format!("ss-ddns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let network_path = dir.join("network.json");
        store::save(&network_path, &network(Some("eth0=1.2.3.4/24"))).expect("save");
        let secrets_path = dir.join("ddns.secrets.json");
        save_secrets(
            &secrets_path,
            &DdnsSecrets {
                api_token: "tok".to_string(),
            },
        )
        .expect("save");
        write_config(&dir, "zone1", &["home.example.com"]);

        let (context, runner) = context(
            &dir,
            &network_path,
            &secrets_path,
            FakeRunner::new().respond("curl", Some(0), LIST_JSON, ""),
        );
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        let outcomes = runtime.block_on(context.update());
        assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");
        assert!(outcomes.iter().all(|o| !o.changed), "address unchanged");
        assert_eq!(runner.calls().len(), 1, "only the list call runs");
    }

    #[test]
    fn changed_records_are_written() {
        let dir = std::env::temp_dir().join(format!("ss-ddns2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let network_path = dir.join("network.json");
        store::save(&network_path, &network(Some("eth0=9.9.9.9/24"))).expect("save");
        let secrets_path = dir.join("ddns.secrets.json");
        save_secrets(
            &secrets_path,
            &DdnsSecrets {
                api_token: "tok".to_string(),
            },
        )
        .expect("save");
        write_config(&dir, "zone1", &["home.example.com"]);

        let (context, runner) = context(
            &dir,
            &network_path,
            &secrets_path,
            FakeRunner::new().respond("curl", Some(0), LIST_JSON, ""),
        );
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        let outcomes = runtime.block_on(context.update());
        assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");
        assert!(outcomes.iter().all(|o| o.changed));
        assert_eq!(runner.calls().len(), 2, "list then put");
        // The token must never appear in an argument list.
        assert!(
            runner
                .calls()
                .iter()
                .all(|(_, args)| !args.iter().any(|arg| arg.contains("tok"))),
            "the token travelled via a header, not argv"
        );
    }

    #[test]
    fn a_missing_token_is_reported() {
        let dir = std::env::temp_dir().join(format!("ss-ddns3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let network_path = dir.join("network.json");
        store::save(&network_path, &network(Some("eth0=1.2.3.4/24"))).expect("save");
        let context = DdnsContext::new(
            dir.join("ddns.json"),
            dir.join("secrets.json"),
            network_path,
            Arc::new(FakeRunner::new()),
        );
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        let outcomes = runtime.block_on(context.update());
        assert!(outcomes.iter().all(|o| !o.ok));
        assert!(outcomes[0].detail.contains("token"));
    }
}
