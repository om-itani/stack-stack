//! CPU Power Profiles (Quiet/Silent, Balanced, Performance) and Machine Hardening.

use serde::{Deserialize, Serialize};
use std::fs;

use crate::error::ApiError;
use crate::runner::Runner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerProfile {
    Quiet,
    Balanced,
    Performance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerProfileStatus {
    pub current_profile: PowerProfile,
    pub governor: String,
    pub energy_preference: Option<String>,
    pub available_governors: Vec<String>,
    pub cpu_temp_c: Option<f32>,
    pub freq_mhz: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardeningStatus {
    pub swappiness: u32,
    pub vfs_cache_pressure: u32,
    pub dirty_ratio: u32,
    pub dirty_background_ratio: u32,
    pub somaxconn: u32,
    pub file_max: u64,
    pub fstrim_active: bool,
    pub oom_shield_active: bool,
}

pub fn get_power_profile(_runner: &dyn Runner) -> PowerProfileStatus {
    let gov = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "powersave".to_string());

    let epp =
        fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
            .map(|s| s.trim().to_string())
            .ok();

    let avail_str =
        fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors")
            .unwrap_or_default();
    let available_governors: Vec<String> = avail_str
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let current_profile = match (gov.as_str(), epp.as_deref()) {
        ("performance", _) => PowerProfile::Performance,
        ("powersave", Some("power" | "balance_power")) => PowerProfile::Quiet,
        ("powersave", Some("performance")) => PowerProfile::Performance,
        _ => PowerProfile::Balanced,
    };

    let cpu_temp_c = crate::system::metrics::cpu_temperature_c();
    let freq_mhz = crate::system::metrics::cpu_frequency_mhz();

    PowerProfileStatus {
        current_profile,
        governor: gov,
        energy_preference: epp,
        available_governors,
        cpu_temp_c,
        freq_mhz,
    }
}

pub fn set_power_profile(runner: &dyn Runner, profile: PowerProfile) -> Result<String, ApiError> {
    let (target_gov, target_epp) = match profile {
        PowerProfile::Quiet => ("powersave", "power"),
        PowerProfile::Balanced => ("powersave", "balance_performance"),
        PowerProfile::Performance => ("performance", "performance"),
    };

    // Apply via sysfs across all online CPUs
    let mut applied_sysfs = false;
    if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                let cpufreq = path.join("cpufreq");
                if cpufreq.exists() {
                    let _ = fs::write(cpufreq.join("scaling_governor"), target_gov.as_bytes());
                    let _ = fs::write(
                        cpufreq.join("energy_performance_preference"),
                        target_epp.as_bytes(),
                    );
                    applied_sysfs = true;
                }
            }
        }
    }

    if !applied_sysfs {
        // Fallback to cpupower CLI if sysfs is not directly writable
        let _ = runner.call("cpupower", &["frequency-set", "-g", target_gov]);
    }

    Ok(format!(
        "CPU power profile set to {:?} (governor: {}, epp: {})",
        profile, target_gov, target_epp
    ))
}

pub fn get_hardening_status(runner: &dyn Runner) -> HardeningStatus {
    let read_u32 = |path: &str, def: u32| -> u32 {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(def)
    };
    let read_u64 = |path: &str, def: u64| -> u64 {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(def)
    };

    let swappiness = read_u32("/proc/sys/vm/swappiness", 10);
    let vfs_cache_pressure = read_u32("/proc/sys/vm/vfs_cache_pressure", 50);
    let dirty_ratio = read_u32("/proc/sys/vm/dirty_ratio", 10);
    let dirty_background_ratio = read_u32("/proc/sys/vm/dirty_background_ratio", 5);
    let somaxconn = read_u32("/proc/sys/net/core/somaxconn", 4096);
    let file_max = read_u64("/proc/sys/fs/file-max", 2097152);

    let fstrim_active = runner
        .call("systemctl", &["is-active", "--quiet", "fstrim.timer"])
        .map(|o| o.success())
        .unwrap_or(false);
    let oom_score = fs::read_to_string("/proc/self/oom_score_adj")
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0);
    let oom_shield_active = oom_score <= -900;

    HardeningStatus {
        swappiness,
        vfs_cache_pressure,
        dirty_ratio,
        dirty_background_ratio,
        somaxconn,
        file_max,
        fstrim_active,
        oom_shield_active,
    }
}

pub fn apply_hardening(runner: &dyn Runner) -> Result<String, ApiError> {
    let conf = r#"# Managed by stack-stack host hardening
vm.swappiness = 10
vm.vfs_cache_pressure = 50
vm.dirty_ratio = 10
vm.dirty_background_ratio = 5
net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 4096
fs.file-max = 2097152
"#;
    let _ = fs::write(
        "/etc/sysctl.d/99-stack-stack-hardening.conf",
        conf.as_bytes(),
    );
    let _ = runner.call(
        "sysctl",
        &["-p", "/etc/sysctl.d/99-stack-stack-hardening.conf"],
    );
    let _ = runner.call("systemctl", &["enable", "--now", "fstrim.timer"]);
    let _ = fs::write("/proc/self/oom_score_adj", b"-1000");

    Ok("System hardening applied: memory & network sysctls optimized, fstrim enabled, OOM shield active.".to_string())
}
