//! Host capability probe.
//!
//! Answers the questions the UI must show before it offers to create anything:
//! is there a KVM device, does QEMU exist and which accelerators does it have,
//! how many CPUs and how much RAM is there, are hugepages available, which
//! bridges can a guest join, and is LXC present. Everything is best-effort and
//! degrades to "unavailable" on a host (or OS) without these files.
//!
//! CPU and memory come straight from `/proc`, but the probes that need a binary
//! go through [`Runner`], so this is unit-testable with a fake.

use serde::Serialize;

use crate::networking::interfaces::{self, InterfaceKind};
use crate::networking::runner::Runner;
use crate::virtualization::qemu::OvmfPaths;

/// Candidate OVMF firmware pairs, most specific first.
const OVMF_PAIRS: [(&str, &str); 6] = [
    (
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ),
    (
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_VARS.fd",
    ),
    (
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    ),
    (
        "/usr/share/edk2/x64/OVMF_CODE.fd",
        "/usr/share/edk2/x64/OVMF_VARS.fd",
    ),
    (
        "/usr/share/qemu/OVMF_CODE.fd",
        "/usr/share/qemu/OVMF_VARS.fd",
    ),
    (
        "/usr/share/AAVMF/AAVMF_CODE.fd",
        "/usr/share/AAVMF/AAVMF_VARS.fd",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CpuInfo {
    pub vendor: Option<String>,
    pub model: Option<String>,
    /// Logical CPUs the kernel sees (`processor` lines).
    pub logical_cpus: u32,
    /// `cpu cores` from the first processor block, when present.
    pub physical_cores: Option<u32>,
    /// `vmx` or `svm`, when the CPU can virtualise.
    pub virtualization_flag: Option<String>,
    pub has_aes: bool,
    pub has_avx2: bool,
}

impl Default for CpuInfo {
    fn default() -> Self {
        Self {
            vendor: None,
            model: None,
            logical_cpus: 1,
            physical_cores: None,
            virtualization_flag: None,
            has_aes: false,
            has_avx2: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolInfo {
    pub program: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostCapabilities {
    pub arch: String,
    /// `/dev/kvm` is present and usable.
    pub kvm: bool,
    /// Nested virtualisation is enabled in the host KVM module.
    pub nested: bool,
    pub cpu: CpuInfo,
    pub memory_mb: u64,
    pub hugepages_2m_total: u64,
    pub hugepages_2m_free: u64,
    pub qemu: Option<ToolInfo>,
    /// Accelerators the QEMU binary was built with (`kvm`, `tcg`, …).
    pub accelerators: Vec<String>,
    pub lxc: Option<ToolInfo>,
    pub subuid_mapped: bool,
    /// Bridges a guest can join, plus the `user` pseudo-bridge for slirp.
    pub bridges: Vec<String>,
    pub uefi: bool,
}

impl Default for HostCapabilities {
    fn default() -> Self {
        Self {
            arch: "amd64".to_string(),
            kvm: false,
            nested: false,
            cpu: CpuInfo::default(),
            memory_mb: 0,
            hugepages_2m_total: 0,
            hugepages_2m_free: 0,
            qemu: None,
            accelerators: Vec::new(),
            lxc: None,
            subuid_mapped: false,
            bridges: Vec::new(),
            uefi: false,
        }
    }
}

impl HostCapabilities {
    /// A guest can only start when QEMU exists and either KVM or TCG is present.
    pub fn can_run_vms(&self) -> bool {
        self.qemu.is_some() && (self.kvm || self.accelerators.iter().any(|a| a == "tcg"))
    }

    pub fn can_run_containers(&self) -> bool {
        self.lxc.is_some()
    }
}

/// Probe the host. Never fails; missing pieces are reported as absent.
pub fn probe(runner: &dyn Runner) -> HostCapabilities {
    let arch = host_arch();
    let mut caps = HostCapabilities {
        arch: arch.clone(),
        kvm: kvm_available(),
        nested: nested_enabled(),
        cpu: read_cpuinfo().unwrap_or_default(),
        ..HostCapabilities::default()
    };

    if let Some(meminfo) = read_meminfo() {
        caps.memory_mb = meminfo.mem_total_mb;
        caps.hugepages_2m_total = meminfo.hugepages_2m_total;
        caps.hugepages_2m_free = meminfo.hugepages_2m_free;
    }

    let qemu_program = if arch == "arm64" {
        "qemu-system-aarch64"
    } else {
        "qemu-system-x86_64"
    };
    caps.qemu = tool_version(runner, qemu_program, &["--version"]);
    caps.accelerators = if caps.qemu.is_some() {
        parse_accelerators(
            &runner
                .call(qemu_program, &["-accel", "help"])
                .map(|o| o.stdout)
                .unwrap_or_default(),
        )
    } else {
        Vec::new()
    };
    caps.lxc = tool_version(runner, "lxc-create", &["--version"])
        .or_else(|| tool_version(runner, "lxc-ls", &["--version"]));
    caps.uefi = detect_ovmf().is_some();
    caps.bridges = discover_bridges(runner);
    caps.subuid_mapped = subuid_mapped();
    caps
}

/// Firmware pair for a UEFI guest, if any is installed.
pub fn detect_ovmf() -> Option<OvmfPaths> {
    detect_ovmf_in(&OVMF_PAIRS)
}

/// Testable core of [`detect_ovmf`].
pub fn detect_ovmf_in(pairs: &[(&str, &str)]) -> Option<OvmfPaths> {
    pairs.iter().find_map(|(code, vars)| {
        let code = std::path::Path::new(code);
        let vars = std::path::Path::new(vars);
        if code.is_file() && vars.is_file() {
            Some(OvmfPaths {
                code: code.to_path_buf(),
                vars: vars.to_path_buf(),
            })
        } else {
            None
        }
    })
}

fn host_arch() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "arm64".to_string(),
        _ => "amd64".to_string(),
    }
}

#[cfg(target_os = "linux")]
fn kvm_available() -> bool {
    // A char device at /dev/kvm that we can open is the only reliable signal.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

#[cfg(not(target_os = "linux"))]
fn kvm_available() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn nested_enabled() -> bool {
    for path in [
        "/sys/module/kvm_intel/parameters/nested",
        "/sys/module/kvm_amd/parameters/nested",
    ] {
        if let Ok(value) = std::fs::read_to_string(path) {
            let value = value.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "1" | "y" | "yes" | "on") {
                return true;
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn nested_enabled() -> bool {
    false
}

fn subuid_mapped() -> bool {
    for path in ["/etc/subuid", "/etc/subgid"] {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                if !contents
                    .lines()
                    .any(|line| line.trim_start().starts_with("root:"))
                {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

fn tool_version(runner: &dyn Runner, program: &str, args: &[&str]) -> Option<ToolInfo> {
    let output = runner.call(program, args).ok()?;
    if output.timed_out {
        return None;
    }
    let first_line = output
        .combined()
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    Some(ToolInfo {
        program: program.to_string(),
        version: first_line,
    })
}

/// Parse `qemu-system-* -accel help`: a header line, then one accelerator per
/// line.
pub fn parse_accelerators(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.to_ascii_lowercase().contains("accelerator"))
        .map(|line| line.to_ascii_lowercase())
        .collect()
}

fn discover_bridges(runner: &dyn Runner) -> Vec<String> {
    let mut bridges: Vec<String> = interfaces::discover(runner)
        .into_iter()
        .filter(|iface| iface.kind == InterfaceKind::Bridge)
        .map(|iface| iface.name)
        .collect();
    bridges.sort();
    // `user` is not a real bridge but a legitimate slirp target.
    bridges.push("user".to_string());
    bridges
}

// ---------------------------------------------------------------------------
// /proc parsing
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn read_cpuinfo() -> Option<CpuInfo> {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map(|raw| parse_cpuinfo(&raw))
}

#[cfg(not(target_os = "linux"))]
fn read_cpuinfo() -> Option<CpuInfo> {
    None
}

/// Parse the `key : value` blocks of `/proc/cpuinfo`. Only the first `cpu cores`
/// is kept (they repeat per socket) and the flag set is taken from any block.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_cpuinfo(raw: &str) -> CpuInfo {
    let mut info = CpuInfo::default();
    let mut logical = 0u32;
    for line in raw.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "processor" => logical += 1,
            "vendor_id" if info.vendor.is_none() => info.vendor = Some(value.to_string()),
            "model name" if info.model.is_none() => info.model = Some(value.to_string()),
            "cpu cores" if info.physical_cores.is_none() => {
                info.physical_cores = value.parse::<u32>().ok();
            }
            "flags" | "Features" => {
                for flag in value.split_whitespace() {
                    match flag {
                        "vmx" | "svm" => {
                            if info.virtualization_flag.is_none() {
                                info.virtualization_flag = Some(flag.to_string());
                            }
                        }
                        "aes" => info.has_aes = true,
                        "avx2" => info.has_avx2 = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    info.logical_cpus = logical.max(1);
    info
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemInfo {
    pub mem_total_mb: u64,
    pub hugepages_2m_total: u64,
    pub hugepages_2m_free: u64,
}

#[cfg(target_os = "linux")]
fn read_meminfo() -> Option<MemInfo> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .map(|raw| parse_meminfo(&raw))
}

#[cfg(not(target_os = "linux"))]
fn read_meminfo() -> Option<MemInfo> {
    None
}

/// `/proc/meminfo` values are in KiB.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_meminfo(raw: &str) -> MemInfo {
    let mut info = MemInfo::default();
    for line in raw.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kib: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        match key.trim() {
            "MemTotal" => info.mem_total_mb = kib / 1024,
            "Hugepages_Total" => info.hugepages_2m_total = kib,
            "Hugepages_Free" => info.hugepages_2m_free = kib,
            _ => {}
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::runner::fake::FakeRunner;

    #[test]
    fn cpuinfo_parsing_picks_the_fields_we_use() {
        let raw = "\
processor\t: 0
vendor_id\t: GenuineIntel
cpu family\t: 6
model name\t: Intel(R) Core(TM) i5-6500 CPU @ 3.20GHz
cpu cores\t: 4
flags\t\t: fpu vme de pse mce cx8 apic vmx aes avx2
processor\t: 1
model name\t: Intel(R) Core(TM) i5-6500 CPU @ 3.20GHz
cpu cores\t: 4
flags\t\t: fpu vmx aes avx2
";
        let info = parse_cpuinfo(raw);
        assert_eq!(info.logical_cpus, 2);
        assert_eq!(info.physical_cores, Some(4));
        assert_eq!(info.vendor.as_deref(), Some("GenuineIntel"));
        assert_eq!(info.virtualization_flag.as_deref(), Some("vmx"));
        assert!(info.has_aes && info.has_avx2);
    }

    #[test]
    fn amd_boxes_report_svm() {
        let raw = "processor : 0\nflags : svm aes avx2\n";
        let info = parse_cpuinfo(raw);
        assert_eq!(info.virtualization_flag.as_deref(), Some("svm"));
    }

    #[test]
    fn meminfo_parsing_converts_kib_to_mib() {
        let raw = "\
MemTotal:       16330816 kB
MemFree:         1234567 kB
Hugepages_Total:      64
Hugepages_Free:       64
";
        let info = parse_meminfo(raw);
        assert_eq!(info.mem_total_mb, 15948);
        assert_eq!(info.hugepages_2m_total, 64);
        assert_eq!(info.hugepages_2m_free, 64);
    }

    #[test]
    fn accelerator_help_is_parsed() {
        let text = "Accelerators supported in QEMU binary:\ntcg\nkvm\n";
        assert_eq!(parse_accelerators(text), vec!["tcg", "kvm"]);
    }

    #[test]
    fn probe_reports_tools_and_bridges() {
        let runner = FakeRunner::new()
            .respond(
                "qemu-system-x86_64",
                Some(0),
                "QEMU emulator version 8.2.0\n",
                "",
            )
            .respond_when(
                "qemu-system-x86_64",
                "-accel help",
                Some(0),
                "Accelerators supported in QEMU binary:\nkvm\ntcg\n",
                "",
            )
            .respond("lxc-create", Some(0), "5.0.2\n", "")
            .respond(
                "ip",
                Some(0),
                r#"[{"ifname":"br0","addr_info":[]},{"ifname":"eth0","addr_info":[]}]"#,
                "",
            );

        // Fake sysfs so `ip` addresses and interface kinds resolve: the kind
        // classification needs real directories, so just assert the parts that
        // do not depend on the filesystem.
        let caps = probe(&runner);
        assert!(caps.qemu.is_some());
        assert_eq!(caps.accelerators, vec!["kvm", "tcg"]);
        assert_eq!(
            caps.qemu.as_ref().map(|q| q.version.as_str()),
            Some("QEMU emulator version 8.2.0")
        );
        assert!(caps.lxc.is_some());
        assert!(caps.can_run_vms());
        assert!(caps.can_run_containers());
        // `user` is always offered as a networking fallback.
        assert!(caps.bridges.contains(&"user".to_string()));
    }

    #[test]
    fn probe_degrades_without_any_tools() {
        let runner = FakeRunner::new()
            .missing("qemu-system-x86_64")
            .missing("lxc-create")
            .missing("lxc-ls")
            .missing("ip");
        let caps = probe(&runner);
        assert!(caps.qemu.is_none());
        assert!(caps.lxc.is_none());
        assert!(!caps.can_run_vms());
        assert!(!caps.can_run_containers());
        assert_eq!(caps.bridges, vec!["user".to_string()]);
    }

    #[test]
    fn ovmf_detection_uses_the_first_existing_pair() {
        let dir = std::env::temp_dir().join(format!("stack-stack-ovmf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let code = dir.join("CODE.fd");
        let vars = dir.join("VARS.fd");
        std::fs::write(&code, b"code").expect("write");
        std::fs::write(&vars, b"vars").expect("write");

        let found = detect_ovmf_in(&[
            ("/nonexistent/CODE.fd", "/nonexistent/VARS.fd"),
            (code.to_str().expect("utf8"), vars.to_str().expect("utf8")),
        ])
        .expect("a pair");
        assert_eq!(found.code, code);
        assert_eq!(found.vars, vars);
    }
}
