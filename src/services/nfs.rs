//! NFS exports.
//!
//! The document is rendered into a complete `/etc/exports`, checked with
//! `exportfs -ra` before `nfs-server` is restarted, and client lists are
//! validated with the crate's own IPv4 arithmetic rather than a regex.

use std::path::PathBuf;
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

pub const EXPORTS: &str = "/etc/exports";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NfsExport {
    pub path: String,
    /// Client expressions: `192.168.1.0/24` or a bare address (treated as /32).
    pub clients: Vec<String>,
    pub ro: bool,
    pub sync: bool,
    pub no_subtree_check: bool,
    pub root_squash: bool,
}

impl Default for NfsExport {
    fn default() -> Self {
        Self {
            path: String::new(),
            clients: Vec::new(),
            ro: false,
            sync: true,
            no_subtree_check: true,
            root_squash: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NfsConfig {
    pub enabled: bool,
    pub v4: bool,
    pub exports: Vec<NfsExport>,
}

impl Default for NfsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            v4: true,
            exports: Vec::new(),
        }
    }
}

/// Normalise a client expression to canonical CIDR form.
pub fn canonical_client(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("an empty client is not allowed".to_string());
    }
    if trimmed.contains('/') {
        let net = Ipv4Net::parse(trimmed)?;
        Ok(net.network_cidr())
    } else {
        let addr = parse_ipv4(trimmed)?;
        Ok(format!("{addr}/32"))
    }
}

fn export_options(export: &NfsExport) -> String {
    let mut parts: Vec<&str> = Vec::new();
    parts.push(if export.ro { "ro" } else { "rw" });
    parts.push(if export.sync { "sync" } else { "async" });
    parts.push(if export.no_subtree_check {
        "no_subtree_check"
    } else {
        "subtree_check"
    });
    parts.push(if export.root_squash {
        "root_squash"
    } else {
        "no_root_squash"
    });
    parts.join(",")
}

impl NfsConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        if !self.enabled {
            return problems;
        }
        let mut seen: Vec<(String, String)> = Vec::new();

        for (index, export) in self.exports.iter().enumerate() {
            let field = format!("exports[{index}]");
            if export.path.trim().is_empty() || !export.path.trim().starts_with('/') {
                problems.push(Problem {
                    field: format!("{field}.path"),
                    message: "export path must be an absolute path".to_string(),
                });
            }
            if export.clients.is_empty() {
                problems.push(Problem {
                    field: format!("{field}.clients"),
                    message: "at least one client is required".to_string(),
                });
            }
            for client in &export.clients {
                match canonical_client(client) {
                    Ok(canonical) => {
                        let key = (export.path.trim().to_string(), canonical.clone());
                        if seen.contains(&key) {
                            problems.push(Problem {
                                field: format!("{field}.clients"),
                                message: format!("{canonical} is listed twice"),
                            });
                        }
                        seen.push(key);
                    }
                    Err(message) => problems.push(Problem {
                        field: format!("{field}.clients"),
                        message: format!("{client:?}: {message}"),
                    }),
                }
            }
        }
        problems
    }

    /// Render a complete `/etc/exports`.
    pub fn render(&self) -> String {
        let mut out =
            String::from("# Managed by stack-stack. Edits are overwritten on the next apply.\n");
        for export in &self.exports {
            let options = export_options(export);
            let mut line = export.path.trim().to_string();
            for client in &export.clients {
                let canonical =
                    canonical_client(client).unwrap_or_else(|_| client.trim().to_string());
                line.push(' ');
                line.push_str(&canonical);
                line.push('(');
                line.push_str(&options);
                line.push(')');
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

#[derive(Clone)]
pub struct NfsContext {
    pub config_path: PathBuf,
    pub runner: Arc<dyn Runner>,
    pub exports_path: PathBuf,
}

impl NfsContext {
    pub fn new(config_path: PathBuf, exports_path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self {
            config_path,
            runner,
            exports_path,
        }
    }
}

/// One row of `showmount --exports`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveExport {
    pub path: String,
    pub clients: String,
}

pub fn parse_showmount(output: &str) -> Vec<ActiveExport> {
    output
        .lines()
        .skip(1) // header
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            match line.split_once(char::is_whitespace) {
                Some((path, clients)) => Some(ActiveExport {
                    path: path.trim().to_string(),
                    clients: clients.trim().to_string(),
                }),
                None => Some(ActiveExport {
                    path: line.to_string(),
                    clients: String::new(),
                }),
            }
        })
        .collect()
}

pub fn active_exports(runner: &dyn Runner) -> Result<Vec<ActiveExport>, String> {
    let output = runner
        .run_timeout(
            "showmount",
            &["--exports".to_string()],
            Duration::from_secs(15),
        )
        .map_err(|err| format!("showmount is not available: {err}"))?;
    if !output.success() {
        return Err(format!("showmount failed: {}", trimmed(&output.combined())));
    }
    Ok(parse_showmount(&output.stdout))
}

pub async fn apply(context: &NfsContext, config: &NfsConfig) -> Vec<StepResult> {
    let context = context.clone();
    let config = config.clone();
    match tokio::task::spawn_blocking(move || apply_sync(&context, &config)).await {
        Ok(steps) => steps,
        Err(err) => vec![StepResult {
            subsystem: "nfs".to_string(),
            ok: false,
            detail: format!("nfs apply task failed: {err}"),
        }],
    }
}

fn apply_sync(context: &NfsContext, config: &NfsConfig) -> Vec<StepResult> {
    let mut steps = Vec::new();

    let rendered = config.render();
    let write = (|| -> Result<String, String> {
        write_atomic(&context.exports_path, rendered.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", context.exports_path.display()))?;
        Ok(format!("{} export(s)", config.exports.len()))
    })();
    steps.push(step("exports", write));

    if !config.enabled {
        let _ =
            context
                .runner
                .run_timeout("exportfs", &["-ua".to_string()], Duration::from_secs(30));
        steps.push(step(
            "exportfs",
            Ok("NFS disabled; existing exports withdrawn".to_string()),
        ));
        return steps;
    }

    let reload = (|| -> Result<String, String> {
        let output = context
            .runner
            .run_timeout("exportfs", &["-ra".to_string()], Duration::from_secs(60))
            .map_err(|err| format!("exportfs is not available: {err}"))?;
        if !output.success() {
            return Err(format!("exportfs -ra: {}", trimmed(&output.combined())));
        }
        Ok("exportfs reloaded".to_string())
    })();
    steps.push(step("exportfs", reload));

    // Debian ships the unit as `nfs-server` (an alias of nfs-kernel-server).
    let restart = (|| -> Result<String, String> {
        let output = context
            .runner
            .run_timeout(
                "systemctl",
                &["restart".to_string(), "nfs-server".to_string()],
                Duration::from_secs(60),
            )
            .map_err(|err| format!("systemctl is not available: {err}"))?;
        if !output.success() {
            return Err(format!(
                "systemctl restart nfs-server: {}",
                trimmed(&output.combined())
            ));
        }
        Ok("nfs-server restarted".to_string())
    })();
    steps.push(step("nfs-server", restart));

    steps
}

pub fn load(path: &std::path::Path) -> (NfsConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<NfsConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                NfsConfig::default(),
                Some(format!("{} is not valid nfs JSON: {err}", path.display())),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (NfsConfig::default(), None),
        Err(err) => (
            NfsConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &std::path::Path, config: &NfsConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise nfs config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    fn export(path: &str, clients: &[&str]) -> NfsExport {
        NfsExport {
            path: path.to_string(),
            clients: clients.iter().map(|client| (*client).to_string()).collect(),
            ..NfsExport::default()
        }
    }

    #[test]
    fn default_config_validates_clean() {
        assert!(NfsConfig::default().validate().is_empty());
        assert!(
            NfsConfig {
                enabled: true,
                ..NfsConfig::default()
            }
            .validate()
            .is_empty()
        );
    }

    #[test]
    fn clients_are_validated_and_canonicalised() {
        assert_eq!(
            canonical_client("192.168.1.0/24").expect("cidr"),
            "192.168.1.0/24"
        );
        assert_eq!(
            canonical_client("192.168.1.5").expect("ip"),
            "192.168.1.5/32"
        );
        assert_eq!(
            canonical_client("10.0.0.7/24").expect("host bits"),
            "10.0.0.0/24"
        );
        for bad in ["", "*", "example.com", "300.1.1.1"] {
            assert!(canonical_client(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn duplicate_path_client_pairs_are_rejected() {
        let config = NfsConfig {
            enabled: true,
            exports: vec![export("/srv/media", &["192.168.1.0/24", "192.168.1.0/24"])],
            ..NfsConfig::default()
        };
        let problems = config.validate();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("twice"));
    }

    #[test]
    fn render_writes_one_line_per_export_with_options() {
        let config = NfsConfig {
            enabled: true,
            exports: vec![NfsExport {
                ro: true,
                ..export("/srv/media", &["192.168.1.0/24", "10.0.0.5"])
            }],
            ..NfsConfig::default()
        };
        let rendered = config.render();
        assert!(rendered.contains("/srv/media 192.168.1.0/24(ro,sync,no_subtree_check,root_squash) 10.0.0.5/32(ro,sync,no_subtree_check,root_squash)"));
    }

    #[test]
    fn showmount_output_is_parsed() {
        let output = "Export list for host:\n/srv/media 192.168.1.0/24\n/srv/pub *\n";
        let exports = parse_showmount(output);
        assert_eq!(exports.len(), 2);
        assert_eq!(exports[0].path, "/srv/media");
        assert_eq!(exports[0].clients, "192.168.1.0/24");
    }

    #[test]
    fn apply_reloads_and_restarts() {
        let dir = std::env::temp_dir().join(format!("ss-nfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runner = Arc::new(
            FakeRunner::new()
                .respond("exportfs", Some(0), "", "")
                .respond("systemctl", Some(0), "", ""),
        );
        let context = NfsContext::new(
            dir.join("nfs.json"),
            dir.join("exports"),
            runner.clone() as Arc<dyn Runner>,
        );

        let config = NfsConfig {
            enabled: true,
            exports: vec![export("/srv/media", &["192.168.1.0/24"])],
            ..NfsConfig::default()
        };
        let steps = apply_sync(&context, &config);
        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
        assert!(runner.ran_with("exportfs", &["-ra"]));
        assert!(runner.ran_with("systemctl", &["restart", "nfs-server"]));
    }

    #[test]
    fn disabled_config_withdraws_exports_without_restarting() {
        let dir = std::env::temp_dir().join(format!("ss-nfs-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runner = Arc::new(
            FakeRunner::new()
                .respond("exportfs", Some(0), "", "")
                .respond("systemctl", Some(0), "", ""),
        );
        let context = NfsContext::new(
            dir.join("nfs.json"),
            dir.join("exports"),
            runner.clone() as Arc<dyn Runner>,
        );

        let steps = apply_sync(&context, &NfsConfig::default());
        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
        assert!(runner.ran_with("exportfs", &["-ua"]));
        assert!(!runner.ran_with("systemctl", &["restart", "nfs-server"]));
    }
}
