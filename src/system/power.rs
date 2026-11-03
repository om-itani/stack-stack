//! Universal CPU Power Profiles (Quiet/Silent, Balanced, Performance) and Machine Hardening.
//!
//! Supports:
//! - Intel (`intel_pstate`, `intel_cpufreq`) with Energy Performance Preference (EPP)
//! - AMD (`amd-pstate-epp`, `amd-pstate`, `acpi-cpufreq`) with EPP or classical governors
//! - ARM64 (`cpufreq-dt`, Raspberry Pi, Ampere, Rockchip)
//! - Generic fallback for virtualized or bare-metal machines

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
    pub vendor: String,
    pub driver: String,
    pub governor: String,
    pub energy_preference: Option<String>,
    pub available_governors: Vec<String>,
    pub available_preferences: Vec<String>,
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

pub fn cpu_vendor() -> String {
    if let Ok(info) = fs::read_to_string("/proc/cpuinfo") {
        for line in info.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                let v = v.trim();
                if k == "vendor_id" {
                    if v.contains("AMD") || v.contains("AuthenticAMD") {
                        return "AMD".to_string();
                    } else if v.contains("Intel") || v.contains("GenuineIntel") {
                        return "Intel".to_string();
                    }
                    return v.to_string();
                } else if (k == "CPU implementer" || k == "Hardware")
                    && (v.contains("0x41") || v.to_ascii_lowercase().contains("arm"))
                {
                    return "ARM".to_string();
                }
            }
        }
    }
    std::env::consts::ARCH.to_uppercase()
}

pub fn cpufreq_driver() -> String {
    fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_driver")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "generic".to_string())
}

pub fn get_power_profile(_runner: &dyn Runner) -> PowerProfileStatus {
    let vendor = cpu_vendor();
    let driver = cpufreq_driver();

    let gov = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "powersave".to_string());

    let epp =
        fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
            .map(|s| s.trim().to_string())
            .ok();

    let avail_gov_str =
        fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors")
            .unwrap_or_default();
    let available_governors: Vec<String> = avail_gov_str
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let avail_pref_str = fs::read_to_string(
        "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_available_preferences",
    )
    .unwrap_or_default();
    let available_preferences: Vec<String> = avail_pref_str
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let current_profile = if let Some(pref) = epp.as_deref() {
        match (gov.as_str(), pref) {
            ("performance", _) | (_, "performance") => PowerProfile::Performance,
            (_, "power" | "balance_power") => PowerProfile::Quiet,
            _ => PowerProfile::Balanced,
        }
    } else {
        match gov.as_str() {
            "performance" => PowerProfile::Performance,
            "conservative" | "powersave" => PowerProfile::Quiet,
            _ => PowerProfile::Balanced,
        }
    };

    let cpu_temp_c = crate::system::metrics::cpu_temperature_c();
    let freq_mhz = crate::system::metrics::cpu_frequency_mhz();

    PowerProfileStatus {
        current_profile,
        vendor,
        driver,
        governor: gov,
        energy_preference: epp,
        available_governors,
        available_preferences,
        cpu_temp_c,
        freq_mhz,
    }
}

pub fn set_power_profile(runner: &dyn Runner, profile: PowerProfile) -> Result<String, ApiError> {
    let avail_govs_str =
        fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors")
            .unwrap_or_default();
    let avail_govs: Vec<&str> = avail_govs_str.split_whitespace().collect();

    let avail_prefs_str = fs::read_to_string(
        "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_available_preferences",
    )
    .unwrap_or_default();
    let avail_prefs: Vec<&str> = avail_prefs_str.split_whitespace().collect();

    let has_epp = !avail_prefs.is_empty()
        || fs::metadata("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
            .is_ok();

    let (target_gov, target_epp) = if has_epp {
        // Modern Intel (intel_pstate) or modern AMD (amd-pstate-epp)
        let gov = if profile == PowerProfile::Performance {
            if avail_govs.contains(&"performance") {
                "performance"
            } else {
                "powersave"
            }
        } else if avail_govs.contains(&"powersave") {
            "powersave"
        } else {
            avail_govs.first().copied().unwrap_or("powersave")
        };

        let epp = match profile {
            PowerProfile::Quiet => {
                if avail_prefs.contains(&"power") {
                    "power"
                } else if avail_prefs.contains(&"balance_power") {
                    "balance_power"
                } else {
                    "default"
                }
            }
            PowerProfile::Balanced => {
                if avail_prefs.contains(&"balance_performance") {
                    "balance_performance"
                } else if avail_prefs.contains(&"balance_power") {
                    "balance_power"
                } else {
                    "default"
                }
            }
            PowerProfile::Performance => {
                if avail_prefs.contains(&"performance") {
                    "performance"
                } else {
                    "default"
                }
            }
        };
        (gov, Some(epp))
    } else {
        // Classical governors: AMD (amd-pstate passive, acpi-cpufreq), ARM (cpufreq-dt), or generic
        let gov = match profile {
            PowerProfile::Quiet => {
                if avail_govs.contains(&"conservative") {
                    "conservative"
                } else if avail_govs.contains(&"powersave") {
                    "powersave"
                } else if avail_govs.contains(&"schedutil") {
                    "schedutil"
                } else {
                    avail_govs.first().copied().unwrap_or("powersave")
                }
            }
            PowerProfile::Balanced => {
                if avail_govs.contains(&"schedutil") {
                    "schedutil"
                } else if avail_govs.contains(&"ondemand") {
                    "ondemand"
                } else if avail_govs.contains(&"powersave") {
                    "powersave"
                } else {
                    avail_govs.first().copied().unwrap_or("powersave")
                }
            }
            PowerProfile::Performance => {
                if avail_govs.contains(&"performance") {
                    "performance"
                } else if avail_govs.contains(&"schedutil") {
                    "schedutil"
                } else {
                    avail_govs.first().copied().unwrap_or("performance")
                }
            }
        };
        (gov, None)
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
                    if let Some(epp) = target_epp {
                        let _ = fs::write(
                            cpufreq.join("energy_performance_preference"),
                            epp.as_bytes(),
                        );
                    }
                    applied_sysfs = true;
                }
            }
        }
    }

    if !applied_sysfs {
        let _ = runner.call("cpupower", &["frequency-set", "-g", target_gov]);
    }

    let vendor = cpu_vendor();
    let driver = cpufreq_driver();
    let epp_str = target_epp
        .map(|e| format!(", epp: {e}"))
        .unwrap_or_default();
    Ok(format!(
        "CPU power profile set to {:?} [{vendor} / {driver}] (governor: {}{})",
        profile, target_gov, epp_str
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
