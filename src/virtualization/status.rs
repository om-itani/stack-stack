//! Live guest status.
//!
//! VMs are checked through their pidfile and `/proc` (no fork, no QMP round
//! trip just to render a badge); containers through `lxc-info`. Everything is
//! best-effort: a missing pidfile means "stopped", a missing `lxc-info` means
//! "unknown", and neither is ever an error the UI has to handle.

use serde::Serialize;

use crate::config::Config;
use crate::networking::runner::Runner;
use crate::virtualization::model::{CtConfig, VirtualizationConfig, VmConfig};
use crate::virtualization::{lxc, store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestKind {
    Vm,
    Ct,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GuestStatus {
    pub id: String,
    pub kind: GuestKind,
    pub name: String,
    /// `running`, `stopped`, `paused`, `unknown`.
    pub state: String,
    pub pid: Option<u32>,
    pub memory_bytes: Option<u64>,
    /// CPU time consumed by the guest process, in seconds.
    pub cpu_seconds: Option<f64>,
    pub ip: Option<String>,
    pub autostart: bool,
}

/// Probe every guest in the document.
pub fn probe_all(
    cfg: &Config,
    config: &VirtualizationConfig,
    runner: &dyn Runner,
) -> Vec<GuestStatus> {
    let mut statuses: Vec<GuestStatus> = Vec::with_capacity(config.vms.len() + config.cts.len());
    for vm in &config.vms {
        statuses.push(probe_vm(cfg, vm));
    }
    for ct in &config.cts {
        statuses.push(probe_ct(cfg, ct, runner));
    }
    statuses
}

/// VM status from the pidfile plus `/proc`.
pub fn probe_vm(cfg: &Config, vm: &VmConfig) -> GuestStatus {
    let pid = read_pid(&store::pid_path(cfg, &vm.id));
    let running = pid.is_some_and(process_alive);

    GuestStatus {
        id: vm.id.clone(),
        kind: GuestKind::Vm,
        name: vm.display_name().to_string(),
        state: if running { "running" } else { "stopped" }.to_string(),
        pid: if running { pid } else { None },
        memory_bytes: if running {
            pid.and_then(proc_memory_bytes)
        } else {
            None
        },
        cpu_seconds: if running {
            pid.and_then(proc_cpu_seconds)
        } else {
            None
        },
        ip: None,
        autostart: vm.autostart,
    }
}

/// Container status from `lxc-info -H`.
pub fn probe_ct(cfg: &Config, ct: &CtConfig, runner: &dyn Runner) -> GuestStatus {
    let mut status = GuestStatus {
        id: ct.id.clone(),
        kind: GuestKind::Ct,
        name: ct.display_name().to_string(),
        state: "unknown".to_string(),
        pid: None,
        memory_bytes: None,
        cpu_seconds: None,
        ip: None,
        autostart: ct.autostart,
    };

    let Ok(output) = runner.run("lxc-info", &lxc::info_argv(cfg, &ct.id)) else {
        status.state = "unavailable".to_string();
        return status;
    };
    if !output.success() {
        // A container that exists but is stopped returns non-zero here.
        status.state = "stopped".to_string();
        return status;
    }

    let info = parse_lxc_info(&output.stdout);
    status.state = info.state.clone();
    status.pid = info.pid;
    status.memory_bytes = info.memory_bytes;
    status.cpu_seconds = info.cpu_seconds;
    status.ip = info.ip;
    status
}

/// Fields scraped out of `lxc-info -H`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LxcInfo {
    pub state: String,
    pub pid: Option<u32>,
    pub ip: Option<String>,
    pub memory_bytes: Option<u64>,
    pub cpu_seconds: Option<f64>,
}

/// `lxc-info -H` writes one `Key: value` line per attribute.
pub fn parse_lxc_info(text: &str) -> LxcInfo {
    let mut info = LxcInfo {
        state: "unknown".to_string(),
        ..LxcInfo::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "State" => info.state = normalise_state(value),
            "PID" => info.pid = value.parse::<u32>().ok().filter(|pid| *pid > 0),
            "IP" => {
                if info.ip.is_none() && !value.is_empty() {
                    info.ip = Some(value.split_whitespace().next().unwrap_or(value).to_string());
                }
            }
            "Memory use" => info.memory_bytes = parse_size(value),
            "CPU use" => {
                info.cpu_seconds = value
                    .split_whitespace()
                    .next()
                    .and_then(|number| number.parse::<f64>().ok());
            }
            _ => {}
        }
    }
    info
}

fn normalise_state(value: &str) -> String {
    match value.to_ascii_uppercase().as_str() {
        "RUNNING" => "running".to_string(),
        "STOPPED" => "stopped".to_string(),
        "FROZEN" => "paused".to_string(),
        "STARTING" => "starting".to_string(),
        "STOPPING" => "stopping".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// `45.67 MiB` / `1024 bytes` / `1.5 GiB` -> bytes.
pub fn parse_size(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let number: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next().unwrap_or("bytes").to_ascii_lowercase();
    let factor = match unit.as_str() {
        "bytes" | "byte" | "b" => 1.0,
        "kib" | "kb" => 1024.0,
        "mib" | "mb" => 1024.0 * 1024.0,
        "gib" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "tib" | "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    if number < 0.0 {
        return None;
    }
    Some((number * factor) as u64)
}

/// Read a pidfile; a malformed or zero pid is treated as absent.
pub fn read_pid(path: &std::path::Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok().filter(|pid| *pid > 0)
}

/// True when a process with this pid is currently running.
#[cfg(target_os = "linux")]
pub fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
pub fn process_alive(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn proc_memory_bytes(pid: u32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:")
            && let Some(kib) = rest.split_whitespace().next()
            && let Ok(kib) = kib.parse::<u64>()
        {
            return Some(kib * 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn proc_memory_bytes(_pid: u32) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cpu_seconds(pid: u32) -> Option<f64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the (possibly space-containing) comm are 1-indexed from 3.
    let (_, after_comm) = raw.rsplit_once(')')?;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // `fields[0]` is field 3 (state), so utime (field 14) is index 11 and stime
    // (field 15) is index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    // CLK_TCK is 100 on every mainstream Linux configuration.
    Some((utime + stime) as f64 / 100.0)
}

#[cfg(not(target_os = "linux"))]
fn proc_cpu_seconds(_pid: u32) -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::runner::fake::FakeRunner;

    const INFO: &str = "\
Name:           db
State:          RUNNING
PID:            12345
IP:             192.168.1.50
CPU use:        0.42 seconds
BlkIO use:      12.00 MiB
Memory use:     45.67 MiB
KMem use:       0 bytes
";

    #[test]
    fn lxc_info_is_parsed() {
        let info = parse_lxc_info(INFO);
        assert_eq!(info.state, "running");
        assert_eq!(info.pid, Some(12345));
        assert_eq!(info.ip.as_deref(), Some("192.168.1.50"));
        assert_eq!(info.memory_bytes, Some((45.67 * 1024.0 * 1024.0) as u64));
        assert_eq!(info.cpu_seconds, Some(0.42));
    }

    #[test]
    fn a_stopped_container_has_no_pid() {
        let info = parse_lxc_info("Name: db\nState: STOPPED\n");
        assert_eq!(info.state, "stopped");
        assert_eq!(info.pid, None);
    }

    #[test]
    fn sizes_parse_with_every_unit() {
        assert_eq!(parse_size("1024 bytes"), Some(1024));
        assert_eq!(parse_size("1 KiB"), Some(1024));
        assert_eq!(parse_size("1 MiB"), Some(1024 * 1024));
        assert_eq!(parse_size("1 GiB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("nonsense"), None);
    }

    #[test]
    fn probe_all_reports_every_guest() {
        let dir = std::env::temp_dir().join(format!("stack-stack-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = Config {
            guests_dir: dir.to_string_lossy().into_owned(),
            ..Config::default()
        };
        let config = VirtualizationConfig {
            vms: vec![VmConfig {
                id: "web".to_string(),
                ..VmConfig::default()
            }],
            cts: vec![CtConfig {
                id: "db".to_string(),
                template: "debian".to_string(),
                ..CtConfig::default()
            }],
            ..VirtualizationConfig::default()
        };
        let runner = FakeRunner::new().respond_when("lxc-info", "-n db", Some(0), INFO, "");
        let statuses = probe_all(&cfg, &config, &runner);
        assert_eq!(statuses.len(), 2);
        let vm = statuses
            .iter()
            .find(|s| s.kind == GuestKind::Vm)
            .expect("vm");
        assert_eq!(vm.state, "stopped", "no pidfile on this host");
        let ct = statuses
            .iter()
            .find(|s| s.kind == GuestKind::Ct)
            .expect("ct");
        assert_eq!(ct.state, "running");
        assert_eq!(ct.pid, Some(12345));
    }

    #[test]
    fn a_missing_lxc_info_is_unavailable() {
        let cfg = Config::default();
        let ct = CtConfig {
            id: "db".to_string(),
            template: "debian".to_string(),
            ..CtConfig::default()
        };
        let status = probe_ct(&cfg, &ct, &FakeRunner::new().missing("lxc-info"));
        assert_eq!(status.state, "unavailable");
    }
}
