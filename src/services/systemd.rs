//! Host systemd service management.
//!
//! Provides inspection and lifecycle control (start, stop, restart, enable, disable)
//! of systemd units on the host without requiring terminal or SSH access.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

use crate::error::ApiError;
use crate::networking::store::write_atomic;
use crate::runner::Runner;

const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceUnit {
    pub name: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub enabled: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub name: String,
    pub description: String,
    pub exec_start: String,
    #[serde(default = "default_restart")]
    pub restart: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default)]
    pub working_dir: Option<String>,
}

fn default_restart() -> String {
    "always".to_string()
}

fn default_user() -> String {
    "root".to_string()
}

/// Validate that a unit name is safe to pass to systemctl.
pub fn validate_unit_name(unit: &str) -> Result<String, ApiError> {
    let trimmed = unit.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(ApiError::bad_request("invalid unit name length"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '@')
    {
        return Err(ApiError::bad_request(
            "unit name contains invalid characters",
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(ApiError::bad_request(
            "unit name must not contain path traversal characters",
        ));
    }
    let full_name = if trimmed.ends_with(".service") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.service")
    };
    Ok(full_name)
}

/// Query systemd for loaded services.
pub fn list_services(runner: &dyn Runner) -> Result<Vec<ServiceUnit>, ApiError> {
    if !runner.available("systemctl", &["--version"]) {
        return Ok(Vec::new());
    }

    let output = runner
        .run(
            "systemctl",
            &[
                "list-units".to_string(),
                "--type=service".to_string(),
                "--all".to_string(),
                "--no-pager".to_string(),
                "--no-legend".to_string(),
                "--plain".to_string(),
            ],
        )
        .map_err(|err| ApiError::internal(format!("cannot query systemctl: {err}")))?;

    if !output.success() {
        return Err(ApiError::internal(format!(
            "systemctl list-units failed: {}",
            output.combined().trim()
        )));
    }

    let mut units = Vec::new();
    for line in output.stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Each line format: UNIT LOAD ACTIVE SUB DESCRIPTION...
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }

        let unit_name = parts[0].trim_start_matches(['●', '*']).trim();
        if !unit_name.ends_with(".service") {
            continue;
        }

        let load_state = parts[1].to_string();
        let active_state = parts[2].to_string();
        let sub_state = parts[3].to_string();
        let description = if parts.len() > 4 {
            parts[4..].join(" ")
        } else {
            String::new()
        };

        units.push(ServiceUnit {
            name: unit_name.to_string(),
            description,
            load_state,
            active_state,
            sub_state,
            enabled: "unknown".to_string(),
        });
    }

    Ok(units)
}

/// Perform an action (`start`, `stop`, `restart`, `enable`, `disable`) on a service unit.
pub fn service_action(runner: &dyn Runner, unit: &str, action: &str) -> Result<String, ApiError> {
    let full_unit = validate_unit_name(unit)?;
    let action = action.trim().to_lowercase();
    let valid_actions = ["start", "stop", "restart", "enable", "disable", "reload"];

    if !valid_actions.contains(&action.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid action '{action}', must be one of: {}",
            valid_actions.join(", ")
        )));
    }

    if !runner.available("systemctl", &["--version"]) {
        return Err(ApiError::unavailable(
            "systemctl is not available on this host",
        ));
    }

    let output = runner
        .run_with_input(
            "systemctl",
            &[action.clone(), full_unit.clone()],
            SYSTEMCTL_TIMEOUT,
            b"",
        )
        .map_err(|err| ApiError::internal(format!("failed to execute systemctl: {err}")))?;

    if !output.success() {
        return Err(ApiError::internal(format!(
            "systemctl {action} {full_unit} failed: {}",
            output.combined().trim()
        )));
    }

    Ok(format!("{action} executed successfully for {full_unit}"))
}

/// Create a new systemd unit file under `/etc/systemd/system/`.
pub fn create_service(
    runner: &dyn Runner,
    req: &CreateServiceRequest,
    unit_dir: &Path,
) -> Result<String, ApiError> {
    let full_name = validate_unit_name(&req.name)?;
    let unit_path = unit_dir.join(&full_name);

    if unit_path.exists() {
        return Err(ApiError::bad_request(format!(
            "service file '{full_name}' already exists"
        )));
    }

    let working_dir_line = req
        .working_dir
        .as_ref()
        .filter(|w| !w.trim().is_empty())
        .map(|w| format!("WorkingDirectory={}\n", w.trim()))
        .unwrap_or_default();

    let service_content = format!(
        "[Unit]\nDescription={desc}\nAfter=network.target\n\n[Service]\nType=simple\nUser={user}\nExecStart={exec}\nRestart={restart}\n{workdir}\n[Install]\nWantedBy=multi-user.target\n",
        desc = req.description.trim(),
        user = req.user.trim(),
        exec = req.exec_start.trim(),
        restart = req.restart.trim(),
        workdir = working_dir_line,
    );

    write_atomic(&unit_path, service_content.as_bytes())
        .map_err(|err| ApiError::internal(format!("cannot write unit file: {err}")))?;

    if runner.available("systemctl", &["--version"]) {
        let _ = runner.run("systemctl", &["daemon-reload".to_string()]);
    }

    Ok(full_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    #[test]
    fn validate_unit_name_rules() {
        assert_eq!(validate_unit_name("docker").unwrap(), "docker.service");
        assert_eq!(
            validate_unit_name("docker.service").unwrap(),
            "docker.service"
        );
        assert_eq!(
            validate_unit_name("my-app_1@test").unwrap(),
            "my-app_1@test.service"
        );
        assert!(validate_unit_name("").is_err());
        assert!(validate_unit_name("../escaped").is_err());
        assert!(validate_unit_name("evil;rm").is_err());
    }

    #[test]
    fn list_services_parses_systemctl_output() {
        let runner = FakeRunner::new()
            .respond("systemctl", Some(0), "systemd 252\n", "")
            .respond(
                "systemctl",
                Some(0),
                "cron.service loaded active running Regular background processing\ndocker.service loaded active running Docker Engine\n",
                "",
            );

        let units = list_services(&runner).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].name, "cron.service");
        assert_eq!(units[0].active_state, "active");
        assert_eq!(units[1].name, "docker.service");
    }

    #[test]
    fn service_action_executes_systemctl() {
        let runner = FakeRunner::new()
            .respond("systemctl", Some(0), "systemd 252\n", "")
            .respond("systemctl", Some(0), "", "");

        let res = service_action(&runner, "docker", "restart").unwrap();
        assert!(res.contains("restart"));
    }
}
