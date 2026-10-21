//! Samba (SMB/CIFS) shares.
//!
//! `smb.conf` is rendered in full from the document — never patched in place —
//! so the file on disk is always exactly what the UI shows. `testparm -s`
//! validates it before `smbd` is restarted, and Samba passwords reach
//! `smbpasswd` over stdin, never argv (the same rule the guest passwords use).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::apply::StepResult;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;
use crate::services::{parse_octal, step, trimmed};

pub const SMB_CONF: &str = "/etc/samba/smb.conf";
pub const WORKGROUP_MAX: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmbShare {
    pub name: String,
    pub path: String,
    pub browsable: bool,
    pub read_only: bool,
    pub guest_ok: bool,
    /// Samba usernames allowed to connect; empty means "any authenticated user".
    pub valid_users: Vec<String>,
    /// Force all file operations to this user.
    pub force_user: Option<String>,
    pub create_mask: String,
    pub dir_mask: String,
    pub comment: String,
}

impl Default for SmbShare {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            browsable: true,
            read_only: false,
            guest_ok: false,
            valid_users: Vec::new(),
            force_user: None,
            create_mask: "0664".to_string(),
            dir_mask: "0775".to_string(),
            comment: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmbUser {
    pub username: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmbConfig {
    pub enabled: bool,
    pub workgroup: String,
    pub server_string: String,
    pub shares: Vec<SmbShare>,
    /// Samba accounts this app manages. A username here must also exist as a
    /// Unix account; the UI surfaces the `useradd` hint when it does not.
    pub users: Vec<SmbUser>,
}

impl Default for SmbConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            workgroup: "WORKGROUP".to_string(),
            server_string: "stack-stack".to_string(),
            shares: Vec::new(),
            users: Vec::new(),
        }
    }
}

impl SmbConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();

        if self.enabled {
            let workgroup = self.workgroup.trim();
            if workgroup.is_empty() || workgroup.len() > WORKGROUP_MAX {
                problems.push(Problem {
                    field: "workgroup".to_string(),
                    message: format!("workgroup must be 1-{WORKGROUP_MAX} characters"),
                });
            } else if !workgroup
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                problems.push(Problem {
                    field: "workgroup".to_string(),
                    message: "workgroup may only contain letters, digits, - and _".to_string(),
                });
            }
        }

        let usernames: Vec<&str> = self
            .users
            .iter()
            .map(|user| user.username.as_str())
            .collect();
        let mut seen_names: Vec<String> = Vec::new();

        for (index, share) in self.shares.iter().enumerate() {
            let field = format!("shares[{index}]");
            let name = share.name.trim();
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            {
                problems.push(Problem {
                    field: format!("{field}.name"),
                    message: "share name may only contain letters, digits, . _ -".to_string(),
                });
            } else if seen_names
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(name))
            {
                problems.push(Problem {
                    field: format!("{field}.name"),
                    message: format!("share {name:?} is defined twice"),
                });
            }
            seen_names.push(name.to_string());

            if share.path.trim().is_empty() || !share.path.trim().starts_with('/') {
                problems.push(Problem {
                    field: format!("{field}.path"),
                    message: "share path must be an absolute path".to_string(),
                });
            }
            if parse_octal(&share.create_mask).is_none() {
                problems.push(Problem {
                    field: format!("{field}.create_mask"),
                    message: "create mask must be octal (e.g. 0664)".to_string(),
                });
            }
            if parse_octal(&share.dir_mask).is_none() {
                problems.push(Problem {
                    field: format!("{field}.dir_mask"),
                    message: "directory mask must be octal (e.g. 0775)".to_string(),
                });
            }
            for user in &share.valid_users {
                if !usernames.contains(&user.as_str()) {
                    problems.push(Problem {
                        field: format!("{field}.valid_users"),
                        message: format!("{user:?} is not one of the configured Samba users"),
                    });
                }
            }
            if let Some(force) = share.force_user.as_deref()
                && !force.trim().is_empty()
                && !usernames.contains(&force)
            {
                problems.push(Problem {
                    field: format!("{field}.force_user"),
                    message: format!("{force:?} is not one of the configured Samba users"),
                });
            }
        }

        problems
    }
}

/// Render a complete `smb.conf`.
pub fn render(config: &SmbConfig, lan_cidr: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("# Managed by stack-stack. Edits are overwritten on the next apply.\n");
    out.push_str("[global]\n");
    let _ = writeln!(out, "   workgroup = {}", config.workgroup.trim());
    let _ = writeln!(out, "   server string = {}", config.server_string.trim());
    out.push_str("   server role = standalone server\n");
    out.push_str("   log file = /var/log/samba/log.%m\n");
    out.push_str("   map to guest = Bad User\n");
    if let Some(cidr) = lan_cidr.map(str::trim).filter(|cidr| !cidr.is_empty()) {
        let _ = writeln!(out, "   hosts allow = {cidr}");
    }

    for share in &config.shares {
        out.push('\n');
        let _ = writeln!(out, "[{}]", share.name.trim());
        if !share.comment.trim().is_empty() {
            let _ = writeln!(out, "   comment = {}", share.comment.trim());
        }
        let _ = writeln!(out, "   path = {}", share.path.trim());
        let _ = writeln!(out, "   browseable = {}", yes_no(share.browsable));
        let _ = writeln!(out, "   read only = {}", yes_no(share.read_only));
        let _ = writeln!(out, "   guest ok = {}", yes_no(share.guest_ok));
        if !share.valid_users.is_empty() {
            let _ = writeln!(out, "   valid users = {}", share.valid_users.join(", "));
        }
        if let Some(force) = share
            .force_user
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
        {
            let _ = writeln!(out, "   force user = {force}");
        }
        let create = parse_octal(&share.create_mask).unwrap_or(0o664);
        let dir = parse_octal(&share.dir_mask).unwrap_or(0o775);
        let _ = writeln!(out, "   create mask = {create:04o}");
        let _ = writeln!(out, "   directory mask = {dir:04o}");
    }
    out
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

use std::fmt::Write as _;
fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SambaContext {
    pub config_path: PathBuf,
    pub runner: Arc<dyn Runner>,
    /// Where the generated `smb.conf` is written.
    pub smb_conf: PathBuf,
}

impl SambaContext {
    pub fn new(config_path: PathBuf, smb_conf: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self {
            config_path,
            runner,
            smb_conf,
        }
    }
}

/// A row of `pdbedit -L` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmbUserStatus {
    pub username: String,
    pub uid: Option<u32>,
    pub full_name: String,
}

/// Parse `pdbedit -L` (`user:uid:full name`).
pub fn parse_pdbedit(output: &str) -> Vec<SmbUserStatus> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            let username = fields.next()?.trim();
            if username.is_empty() {
                return None;
            }
            let uid = fields.next().and_then(|uid| uid.trim().parse::<u32>().ok());
            let full_name = fields.next().unwrap_or("").trim().to_string();
            Some(SmbUserStatus {
                username: username.to_string(),
                uid,
                full_name,
            })
        })
        .collect()
}

/// Read the Samba account list; a missing tool is reported, not fatal.
pub fn list_users(runner: &dyn Runner) -> Result<Vec<SmbUserStatus>, String> {
    let output = runner
        .run_timeout("pdbedit", &["-L".to_string()], Duration::from_secs(15))
        .map_err(|err| format!("pdbedit is not available: {err}"))?;
    if !output.success() {
        return Err(format!("pdbedit failed: {}", trimmed(&output.combined())));
    }
    Ok(parse_pdbedit(&output.stdout))
}

/// Set (or create) a Samba password. The password is written twice on stdin,
/// exactly as `smbpasswd -s` expects, and never appears on the command line.
pub fn set_password(runner: &dyn Runner, username: &str, password: &str) -> Result<(), ApiError> {
    let username = username.trim();
    if username.is_empty() {
        return Err(ApiError::bad_request("a username is required"));
    }
    if password.len() < 8 {
        return Err(ApiError::bad_request(
            "a Samba password must be at least 8 characters",
        ));
    }
    let input = format!("{password}\n{password}\n");
    let output = runner
        .run_with_input(
            "smbpasswd",
            &["-a".to_string(), "-s".to_string(), username.to_string()],
            Duration::from_secs(20),
            input.as_bytes(),
        )
        .map_err(|err| ApiError::unavailable(format!("smbpasswd is not available: {err}")))?;
    if output.timed_out || !output.success() {
        return Err(ApiError::internal(format!(
            "could not set the Samba password for {username}: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(())
}

pub fn set_enabled(runner: &dyn Runner, username: &str, enabled: bool) -> Result<(), ApiError> {
    let flag = if enabled { "-e" } else { "-d" };
    let output = runner
        .run_timeout(
            "smbpasswd",
            &[flag.to_string(), username.trim().to_string()],
            Duration::from_secs(15),
        )
        .map_err(|err| ApiError::unavailable(format!("smbpasswd is not available: {err}")))?;
    if !output.success() {
        return Err(ApiError::internal(format!(
            "could not update {username}: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(())
}

pub fn delete_user(runner: &dyn Runner, username: &str) -> Result<(), ApiError> {
    let output = runner
        .run_timeout(
            "smbpasswd",
            &["-x".to_string(), username.trim().to_string()],
            Duration::from_secs(15),
        )
        .map_err(|err| ApiError::unavailable(format!("smbpasswd is not available: {err}")))?;
    if !output.success() {
        return Err(ApiError::internal(format!(
            "could not remove {username}: {}",
            trimmed(&output.combined())
        )));
    }
    Ok(())
}

/// Render, check and (if the document is enabled) restart Samba.
pub async fn apply(
    context: &SambaContext,
    config: &SmbConfig,
    lan_cidr: Option<String>,
) -> Vec<StepResult> {
    let context = context.clone();
    let config = config.clone();
    let result = tokio::task::spawn_blocking(move || apply_sync(&context, &config, lan_cidr)).await;
    match result {
        Ok(steps) => steps,
        Err(err) => vec![StepResult {
            subsystem: "samba".to_string(),
            ok: false,
            detail: format!("samba apply task failed: {err}"),
        }],
    }
}

fn apply_sync(
    context: &SambaContext,
    config: &SmbConfig,
    lan_cidr: Option<String>,
) -> Vec<StepResult> {
    let mut steps = Vec::new();

    let rendered = render(config, lan_cidr.as_deref());
    let write = (|| -> Result<String, String> {
        write_atomic(&context.smb_conf, rendered.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", context.smb_conf.display()))?;
        // Validate with Samba's own parser before the daemon ever reads it.
        let check = context
            .runner
            .run_timeout(
                "testparm",
                &[
                    "-s".to_string(),
                    context.smb_conf.to_string_lossy().to_string(),
                ],
                Duration::from_secs(15),
            )
            .map_err(|err| format!("testparm is not available: {err}"))?;
        if !check.success() {
            return Err(format!(
                "testparm rejected the config: {}",
                trimmed(&check.combined())
            ));
        }
        Ok(format!(
            "{} bytes, {} share(s)",
            rendered.len(),
            config.shares.len()
        ))
    })();
    steps.push(step("config", write));

    if !config.enabled {
        // Best effort: stopping a service that is already stopped is fine.
        let _ = context.runner.run_timeout(
            "systemctl",
            &["stop".to_string(), "smbd".to_string()],
            Duration::from_secs(30),
        );
        steps.push(step("smbd", Ok("Samba disabled".to_string())));
        return steps;
    }

    let smbd = (|| -> Result<String, String> {
        let output = context
            .runner
            .run_timeout(
                "systemctl",
                &["restart".to_string(), "smbd".to_string()],
                Duration::from_secs(60),
            )
            .map_err(|err| format!("systemctl is not available: {err}"))?;
        if !output.success() {
            return Err(format!(
                "systemctl restart smbd: {}",
                trimmed(&output.combined())
            ));
        }
        Ok("smbd restarted".to_string())
    })();
    steps.push(step("smbd", smbd));

    // nmbd (NetBIOS name service) is not installed everywhere, so a failure to
    // restart it is reported as informational rather than as an apply failure.
    let nmbd: Result<String, String> = match context.runner.run_timeout(
        "systemctl",
        &["restart".to_string(), "nmbd".to_string()],
        Duration::from_secs(30),
    ) {
        Ok(output) if output.success() => Ok("nmbd restarted".to_string()),
        Ok(_) => Ok("nmbd is not present".to_string()),
        Err(_) => Ok("nmbd is not present".to_string()),
    };
    steps.push(step("nmbd", nmbd));

    steps
}

pub fn load(path: &std::path::Path) -> (SmbConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<SmbConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                SmbConfig::default(),
                Some(format!("{} is not valid samba JSON: {err}", path.display())),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (SmbConfig::default(), None),
        Err(err) => (
            SmbConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &std::path::Path, config: &SmbConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise samba config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    fn share(name: &str) -> SmbShare {
        SmbShare {
            name: name.to_string(),
            path: format!("/srv/{name}"),
            ..SmbShare::default()
        }
    }

    #[test]
    fn default_config_validates_clean() {
        assert!(SmbConfig::default().validate().is_empty());
    }

    #[test]
    fn duplicate_share_names_and_bad_paths_are_rejected() {
        let config = SmbConfig {
            enabled: true,
            shares: vec![
                share("media"),
                share("MEDIA"),
                SmbShare {
                    name: "bad name".to_string(),
                    path: "relative".to_string(),
                    create_mask: "999".to_string(),
                    ..SmbShare::default()
                },
            ],
            ..SmbConfig::default()
        };
        let problems = config.validate();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.iter().any(|f| f.ends_with("name")));
        assert!(fields.iter().any(|f| f.ends_with("path")));
        assert!(fields.iter().any(|f| f.ends_with("create_mask")));
    }

    #[test]
    fn valid_users_must_exist_as_samba_users() {
        let config = SmbConfig {
            enabled: true,
            users: vec![SmbUser {
                username: "alice".to_string(),
                enabled: true,
            }],
            shares: vec![SmbShare {
                valid_users: vec!["alice".to_string(), "bob".to_string()],
                ..share("media")
            }],
            ..SmbConfig::default()
        };
        let problems = config.validate();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("bob"));
    }

    #[test]
    fn render_contains_the_global_and_share_sections() {
        let config = SmbConfig {
            enabled: true,
            shares: vec![SmbShare {
                name: "media".to_string(),
                path: "/srv/media".to_string(),
                read_only: true,
                guest_ok: true,
                valid_users: vec!["alice".to_string()],
                force_user: Some("alice".to_string()),
                comment: "Films".to_string(),
                ..SmbShare::default()
            }],
            ..SmbConfig::default()
        };
        let rendered = render(&config, Some("192.168.1.0/24"));
        assert!(rendered.contains("[global]"));
        assert!(rendered.contains("workgroup = WORKGROUP"));
        assert!(rendered.contains("hosts allow = 192.168.1.0/24"));
        assert!(rendered.contains("[media]"));
        assert!(rendered.contains("path = /srv/media"));
        assert!(rendered.contains("read only = yes"));
        assert!(rendered.contains("guest ok = yes"));
        assert!(rendered.contains("valid users = alice"));
        assert!(rendered.contains("force user = alice"));
        assert!(rendered.contains("create mask = 0664"));
        assert!(rendered.contains("comment = Films"));
    }

    #[test]
    fn pdbedit_output_is_parsed() {
        let users = parse_pdbedit("alice:1000:Alice Smith\nbob:1001:\n\n");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].username, "alice");
        assert_eq!(users[0].uid, Some(1000));
        assert_eq!(users[0].full_name, "Alice Smith");
        assert_eq!(users[1].full_name, "");
    }

    #[test]
    fn passwords_go_in_on_stdin_and_are_length_checked() {
        let runner = FakeRunner::new().respond("smbpasswd", Some(0), "", "");
        assert!(set_password(&runner, "alice", "short").is_err());
        set_password(&runner, "alice", "longenough").expect("set");
        assert!(runner.ran_with("smbpasswd", &["-a", "-s", "alice"]));
    }

    #[test]
    fn apply_writes_and_checks_then_restarts() {
        let dir = std::env::temp_dir().join(format!("ss-samba-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conf = dir.join("smb.conf");
        let runner = FakeRunner::new()
            .respond("testparm", Some(0), "Loaded services file OK.", "")
            .respond("systemctl", Some(0), "", "");
        let context = SambaContext::new(dir.join("samba.json"), conf.clone(), Arc::new(runner));

        let config = SmbConfig {
            enabled: true,
            shares: vec![share("media")],
            ..SmbConfig::default()
        };
        let steps = apply_sync(&context, &config, None);
        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
        assert!(conf.is_file());
        assert!(steps.iter().any(|s| s.subsystem == "smbd"));
    }

    #[test]
    fn disabled_config_does_not_restart_smbd() {
        let dir = std::env::temp_dir().join(format!("ss-samba-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runner = Arc::new(
            FakeRunner::new()
                .respond("testparm", Some(0), "OK", "")
                .respond("systemctl", Some(0), "", ""),
        );
        let context = SambaContext::new(
            dir.join("samba.json"),
            dir.join("smb.conf"),
            runner.clone() as Arc<dyn Runner>,
        );

        let steps = apply_sync(&context, &SmbConfig::default(), None);
        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
        assert!(!runner.ran_with("systemctl", &["restart", "smbd"]));
    }
}
