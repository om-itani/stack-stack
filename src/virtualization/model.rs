//! The virtualization document and its validator.
//!
//! One file (`/etc/stack-stack/virtualization.json`) describes every guest this
//! box owns: KVM/QEMU virtual machines and LXC containers. Everything the
//! generators in [`super::qemu`], [`super::lxc`] and [`super::cloudinit`]
//! consume is read from here, and every value that reaches a generated argv,
//! config file or seed has first passed [`VirtualizationConfig::validate`].
//!
//! Validation is strict on purpose: a guest that half-creates is worse than one
//! the box refuses to create, and the arithmetic that decides "is this disk big
//! enough", "does this CPU topology add up" and "is this static address inside
//! its subnet" lives here so the generators stay free of ad-hoc maths.

use serde::{Deserialize, Serialize};

use crate::networking::ipmath::{Ipv4Net, MacAddr, parse_ipv4, validate_ifname};

/// A guest hostname: RFC 1123 labels, a single label allowed (`web`, `db-01`).
pub fn is_valid_hostname(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 253 {
        return false;
    }
    trimmed.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// An SSH public key: `ssh-ed25519 AAAA… comment` / `ssh-rsa …` / `ecdsa-…`.
pub fn is_valid_ssh_key(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return false;
    }
    let mut parts = trimmed.split_whitespace();
    let Some(kind) = parts.next() else {
        return false;
    };
    let Some(body) = parts.next() else {
        return false;
    };
    let algorithms = [
        "ssh-ed25519",
        "ssh-rsa",
        "ssh-dss",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "sk-ssh-ed25519@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    algorithms.contains(&kind) && body.len() >= 8
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Machine {
    #[default]
    Q35,
    Pc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Firmware {
    #[default]
    Bios,
    Uefi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskFormat {
    #[default]
    Qcow2,
    Raw,
}

impl DiskFormat {
    pub fn as_qemu_img(&self) -> &'static str {
        match self {
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskCache {
    #[default]
    None,
    Writeback,
    Directsync,
    Unsafe,
}

impl DiskCache {
    pub fn as_qemu(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Writeback => "writeback",
            Self::Directsync => "directsync",
            Self::Unsafe => "unsafe",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AioMode {
    #[default]
    IoUring,
    Native,
    Threads,
}

impl AioMode {
    pub fn as_qemu(&self) -> &'static str {
        match self {
            Self::IoUring => "io_uring",
            Self::Native => "native",
            Self::Threads => "threads",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CpuModel {
    /// Pass the host CPU through (`-cpu host`): fastest, needs KVM.
    #[default]
    Host,
    /// The best model QEMU can emulate: fastest when there is no KVM.
    Max,
    /// A conservative baseline that boots anywhere.
    Qemu64,
}

/// How a guest gets its IPv4 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    #[default]
    Dhcp,
    Static,
    None,
}

/// Sockets × cores × threads. QEMU wants a topology, not just a count, or
/// Windows guests and some schedulers misread the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuTopology {
    #[serde(default = "one_u16")]
    pub sockets: u16,
    #[serde(default = "one_u16")]
    pub cores: u16,
    #[serde(default = "one_u16")]
    pub threads: u16,
}

impl Default for CpuTopology {
    fn default() -> Self {
        Self {
            sockets: 1,
            cores: 1,
            threads: 1,
        }
    }
}

impl CpuTopology {
    pub fn total(&self) -> u32 {
        u32::from(self.sockets) * u32::from(self.cores) * u32::from(self.threads)
    }

    /// A sensible topology for `vcpus`. When SMT is wanted we only advertise two
    /// threads per core: that is what every x86 hypervisor does and it keeps
    /// `cores` an integer for any even count.
    pub fn auto(vcpus: u16, prefer_smt: bool) -> Self {
        let vcpus = vcpus.max(1);
        let threads = if prefer_smt && vcpus.is_multiple_of(2) {
            2
        } else {
            1
        };
        Self {
            sockets: 1,
            cores: (vcpus / threads).max(1),
            threads,
        }
    }
}

// ---------------------------------------------------------------------------
// CPU pinning
// ---------------------------------------------------------------------------

/// Parse an `taskset`/`cpuset` list like `0-3,8,12-13` into the set of logical
/// CPUs it names. Returns `None` for anything malformed.
pub fn parse_cpuset(value: &str) -> Option<Vec<u16>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut cpus: Vec<u16> = Vec::new();
    for part in trimmed.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.split_once('-') {
            Some((start, end)) => {
                let start: u16 = start.trim().parse().ok()?;
                let end: u16 = end.trim().parse().ok()?;
                if start > end || end - start > 1023 {
                    return None;
                }
                cpus.extend(start..=end);
            }
            None => cpus.push(part.parse().ok()?),
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    if cpus.is_empty() { None } else { Some(cpus) }
}

// ---------------------------------------------------------------------------
// MAC derivation
// ---------------------------------------------------------------------------

/// A stable, locally-administered, unicast MAC derived from the guest id.
///
/// Deterministic so a bridged guest keeps its DHCP lease across recreates, and
/// never a real vendor OUI: the `0x02` bit marks it locally administered and the
/// `0x01` bit is cleared so it is unicast.
pub fn derive_mac(seed: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let bytes = hash.to_be_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]
    )
}

// ---------------------------------------------------------------------------
// Cloud-init (VM provisioning)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudInitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Guest hostname. Empty means "use the guest id".
    #[serde(default)]
    pub hostname: String,
    /// Initial login user (created with sudo and the given password/key).
    #[serde(default = "default_user")]
    pub username: String,
    /// Plain login password. Stored root-only and hashed by cloud-init at boot.
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub ssh_keys: Vec<String>,
    #[serde(default)]
    pub ip_mode: NetMode,
    /// Static address as `a.b.c.d/n` when `ip_mode == static`.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub gateway: String,
    #[serde(default)]
    pub dns: Vec<String>,
}

impl Default for CloudInitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hostname: String::new(),
            username: default_user(),
            password: String::new(),
            ssh_keys: Vec::new(),
            ip_mode: NetMode::Dhcp,
            address: None,
            gateway: String::new(),
            dns: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// VM
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Catalog image id (resolved against [`super::images`]).
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub machine: Machine,
    #[serde(default)]
    pub firmware: Firmware,
    #[serde(default)]
    pub cpu_model: CpuModel,
    /// Extra `-cpu` feature flags, e.g. `+aes`, `-hle`.
    #[serde(default)]
    pub cpu_features: Vec<String>,
    #[serde(default = "default_vcpus")]
    pub vcpus: u16,
    /// Explicit topology; when absent it is derived from `vcpus`.
    #[serde(default)]
    pub cpu_topology: Option<CpuTopology>,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    /// Minimum memory the balloon may shrink to. `None` disables the balloon.
    #[serde(default)]
    pub balloon_mb: Option<u32>,
    #[serde(default)]
    pub hugepages: bool,
    #[serde(default)]
    pub mem_prealloc: bool,
    #[serde(default = "default_disk")]
    pub disk_gb: u32,
    #[serde(default)]
    pub disk_format: DiskFormat,
    #[serde(default)]
    pub disk_cache: DiskCache,
    #[serde(default)]
    pub aio: AioMode,
    #[serde(default = "default_true")]
    pub discard: bool,
    #[serde(default = "default_bridge")]
    pub bridge: String,
    /// Fixed MAC; `None` derives one from the id.
    #[serde(default)]
    pub mac: Option<String>,
    /// Multi-queue virtio-net with one queue per vCPU.
    #[serde(default = "default_true")]
    pub multiqueue: bool,
    #[serde(default = "default_true")]
    pub vnc_enabled: bool,
    /// Fixed VNC port; `None` auto-allocates from `defaults.vnc_port_start`.
    #[serde(default)]
    pub vnc_port: Option<u16>,
    #[serde(default = "default_vnc_bind")]
    pub vnc_bind: String,
    #[serde(default = "default_true")]
    pub serial_console: bool,
    /// Expose host CPU virtualisation flags to the guest (`kvm_amd.nested`).
    #[serde(default)]
    pub nested: bool,
    #[serde(default)]
    pub tpm: bool,
    #[serde(default = "default_true")]
    pub guest_agent: bool,
    #[serde(default = "default_true")]
    pub rng: bool,
    /// Pin the vCPU threads to this `taskset` list, e.g. `0-3,8`.
    #[serde(default)]
    pub cpuset: Option<String>,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub cloud_init: CloudInitConfig,
    /// Raw extra QEMU arguments, appended verbatim and shown in the preview.
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub comment: String,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            image: None,
            arch: default_arch(),
            machine: Machine::Q35,
            firmware: Firmware::Bios,
            cpu_model: CpuModel::Host,
            cpu_features: Vec::new(),
            vcpus: default_vcpus(),
            cpu_topology: None,
            memory_mb: default_memory(),
            balloon_mb: None,
            hugepages: false,
            mem_prealloc: false,
            disk_gb: default_disk(),
            disk_format: DiskFormat::Qcow2,
            disk_cache: DiskCache::None,
            aio: AioMode::IoUring,
            discard: true,
            bridge: default_bridge(),
            mac: None,
            multiqueue: true,
            vnc_enabled: true,
            vnc_port: None,
            vnc_bind: default_vnc_bind(),
            serial_console: true,
            nested: false,
            tpm: false,
            guest_agent: true,
            rng: true,
            cpuset: None,
            autostart: false,
            cloud_init: CloudInitConfig::default(),
            extra_args: Vec::new(),
            tags: Vec::new(),
            comment: String::new(),
        }
    }
}

impl VmConfig {
    /// Display name, falling back to the id.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// Effective CPU topology: the explicit one, or one derived from `vcpus`.
    pub fn topology(&self) -> CpuTopology {
        self.cpu_topology
            .unwrap_or_else(|| CpuTopology::auto(self.vcpus, false))
    }

    /// Effective hostname: the cloud-init hostname, or the id.
    pub fn hostname(&self) -> String {
        if self.cloud_init.hostname.trim().is_empty() {
            self.id.clone()
        } else {
            self.cloud_init.hostname.trim().to_string()
        }
    }

    /// Effective MAC: explicit, or derived from the id.
    pub fn effective_mac(&self) -> String {
        self.mac.clone().unwrap_or_else(|| derive_mac(&self.id))
    }

    pub fn memory_bytes(&self) -> u64 {
        u64::from(self.memory_mb) * 1024 * 1024
    }

    pub fn disk_bytes(&self) -> u64 {
        u64::from(self.disk_gb) * 1024 * 1024 * 1024
    }

    /// True when this VM is a cloud image that gets a NoCloud seed.
    pub fn uses_cloud_init(&self) -> bool {
        self.cloud_init.enabled
    }
}

// ---------------------------------------------------------------------------
// LXC
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindMount {
    /// Host path.
    pub source: String,
    /// Path inside the container.
    pub target: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CtConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub description: String,
    /// Catalog template id (resolved against [`super::images`]).
    #[serde(default)]
    pub template: String,
    #[serde(default = "default_arch")]
    pub arch: String,
    /// CPU allowance in cores; 0 leaves it unlimited.
    #[serde(default = "default_cores")]
    pub cores: u32,
    #[serde(default = "default_ct_memory")]
    pub memory_mb: u32,
    #[serde(default = "default_ct_swap")]
    pub swap_mb: u32,
    #[serde(default = "default_rootfs")]
    pub rootfs_gb: u32,
    #[serde(default = "default_true")]
    pub unprivileged: bool,
    /// Allow nested containers inside this one.
    #[serde(default)]
    pub nesting: bool,
    #[serde(default = "default_bridge")]
    pub bridge: String,
    #[serde(default)]
    pub ip_mode: NetMode,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub gateway: String,
    #[serde(default)]
    pub dns: Vec<String>,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub ssh_keys: Vec<String>,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub startup_order: u32,
    #[serde(default)]
    pub startup_delay: u32,
    #[serde(default)]
    pub binds: Vec<BindMount>,
    /// Linux capabilities to *drop* inside the container, e.g. `NET_ADMIN`.
    #[serde(default)]
    pub cap_drop: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub comment: String,
}

impl Default for CtConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            hostname: String::new(),
            description: String::new(),
            template: String::new(),
            arch: default_arch(),
            cores: default_cores(),
            memory_mb: default_ct_memory(),
            swap_mb: default_ct_swap(),
            rootfs_gb: default_rootfs(),
            unprivileged: true,
            nesting: false,
            bridge: default_bridge(),
            ip_mode: NetMode::Dhcp,
            address: None,
            gateway: String::new(),
            dns: Vec::new(),
            mac: None,
            password: String::new(),
            ssh_keys: Vec::new(),
            autostart: false,
            startup_order: 0,
            startup_delay: 0,
            binds: Vec::new(),
            cap_drop: Vec::new(),
            tags: Vec::new(),
            comment: String::new(),
        }
    }
}

impl CtConfig {
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    pub fn hostname(&self) -> String {
        if self.hostname.trim().is_empty() {
            self.id.clone()
        } else {
            self.hostname.trim().to_string()
        }
    }

    pub fn effective_mac(&self) -> String {
        self.mac.clone().unwrap_or_else(|| derive_mac(&self.id))
    }

    pub fn memory_bytes(&self) -> u64 {
        u64::from(self.memory_mb) * 1024 * 1024
    }

    /// The cgroup v2 `cpu.max` value for the configured allowance: quota in
    /// microseconds against a 100 ms period, or `None` when unlimited.
    pub fn cpu_max(&self) -> Option<(u64, u64)> {
        if self.cores == 0 {
            None
        } else {
            Some((u64::from(self.cores) * 100_000, 100_000))
        }
    }
}

// ---------------------------------------------------------------------------
// Root document
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestDefaults {
    pub storage_dir: String,
    pub images_dir: String,
    pub bridge: String,
    pub arch: String,
    pub disk_format: DiskFormat,
    pub vnc_bind: String,
    pub vnc_port_start: u16,
}

impl Default for GuestDefaults {
    fn default() -> Self {
        Self {
            storage_dir: "/var/lib/stack-stack/guests".to_string(),
            images_dir: "/var/lib/stack-stack/images".to_string(),
            bridge: default_bridge(),
            arch: default_arch(),
            disk_format: DiskFormat::Qcow2,
            vnc_bind: default_vnc_bind(),
            vnc_port_start: 5900,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VirtualizationConfig {
    #[serde(default)]
    pub defaults: GuestDefaults,
    #[serde(default)]
    pub vms: Vec<VmConfig>,
    #[serde(default)]
    pub cts: Vec<CtConfig>,
}

/// One validation failure. `field` is a dotted path so the UI can highlight it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub field: String,
    pub message: String,
}

impl Problem {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl VirtualizationConfig {
    pub fn vm(&self, id: &str) -> Option<&VmConfig> {
        self.vms.iter().find(|vm| vm.id == id)
    }

    pub fn ct(&self, id: &str) -> Option<&CtConfig> {
        self.cts.iter().find(|ct| ct.id == id)
    }

    /// Full validation. An empty result means "safe to render and create".
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        self.validate_defaults(&mut problems);

        let mut ids: Vec<String> = Vec::new();
        let mut macs: Vec<String> = Vec::new();
        let mut vnc_ports: Vec<u16> = Vec::new();

        for (index, vm) in self.vms.iter().enumerate() {
            self.validate_vm(
                index,
                vm,
                &mut problems,
                &mut ids,
                &mut macs,
                &mut vnc_ports,
            );
        }
        for (index, ct) in self.cts.iter().enumerate() {
            self.validate_ct(index, ct, &mut problems, &mut ids, &mut macs);
        }

        problems
    }

    fn validate_defaults(&self, problems: &mut Vec<Problem>) {
        if let Err(err) = validate_ifname(&self.defaults.bridge) {
            problems.push(Problem::new("defaults.bridge", err));
        }
        if !matches!(self.defaults.arch.as_str(), "amd64" | "arm64") {
            problems.push(Problem::new(
                "defaults.arch",
                "architecture must be arm64 or amd64",
            ));
        }
        if self.defaults.vnc_port_start < 1024 {
            problems.push(Problem::new(
                "defaults.vnc_port_start",
                "VNC base port must be 1024 or above",
            ));
        }
    }

    fn validate_common(
        &self,
        field: &str,
        id: &str,
        hostname: &str,
        ids: &mut Vec<String>,
        problems: &mut Vec<Problem>,
    ) {
        if let Err(err) = sanitize_id(id) {
            problems.push(Problem::new(format!("{field}.id"), err));
        }
        if ids.iter().any(|known| known == id) {
            problems.push(Problem::new(
                format!("{field}.id"),
                format!("id {id:?} is used by more than one guest"),
            ));
        }
        ids.push(id.to_string());

        if !is_valid_hostname(hostname) {
            problems.push(Problem::new(
                format!("{field}.hostname"),
                format!("{hostname:?} is not a valid hostname"),
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_network_fields(
        &self,
        field: &str,
        bridge: &str,
        ip_mode: NetMode,
        address: &Option<String>,
        gateway: &str,
        dns: &[String],
        problems: &mut Vec<Problem>,
    ) {
        if let Err(err) = validate_ifname(bridge) {
            problems.push(Problem::new(format!("{field}.bridge"), err));
        }

        let subnet = match ip_mode {
            NetMode::Static => match address {
                Some(address) => match Ipv4Net::parse(address) {
                    Ok(net) => Some(net),
                    Err(err) => {
                        problems.push(Problem::new(format!("{field}.address"), err));
                        None
                    }
                },
                None => {
                    problems.push(Problem::new(
                        format!("{field}.address"),
                        "a static address (a.b.c.d/n) is required",
                    ));
                    None
                }
            },
            _ => None,
        };

        if ip_mode == NetMode::Static {
            if gateway.trim().is_empty() {
                problems.push(Problem::new(
                    format!("{field}.gateway"),
                    "a gateway is required for a static address",
                ));
            } else {
                match parse_ipv4(gateway) {
                    Ok(gateway_ip) => {
                        if let Some(subnet) = subnet
                            && !subnet.contains(gateway_ip)
                        {
                            problems.push(Problem::new(
                                format!("{field}.gateway"),
                                format!(
                                    "gateway {gateway_ip} is outside {}",
                                    subnet.network_cidr()
                                ),
                            ));
                        }
                    }
                    Err(err) => problems.push(Problem::new(format!("{field}.gateway"), err)),
                }
            }
        }

        for (index, server) in dns.iter().enumerate() {
            if parse_ipv4(server).is_err() {
                problems.push(Problem::new(
                    format!("{field}.dns[{index}]"),
                    format!("{server:?} is not a valid IPv4 address"),
                ));
            }
        }
    }

    fn validate_ssh_keys(&self, field: &str, keys: &[String], problems: &mut Vec<Problem>) {
        for (index, key) in keys.iter().enumerate() {
            if !is_valid_ssh_key(key) {
                problems.push(Problem::new(
                    format!("{field}.ssh_keys[{index}]"),
                    "not a valid SSH public key",
                ));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_vm(
        &self,
        index: usize,
        vm: &VmConfig,
        problems: &mut Vec<Problem>,
        ids: &mut Vec<String>,
        macs: &mut Vec<String>,
        vnc_ports: &mut Vec<u16>,
    ) {
        let field = format!("vms[{index}]");
        self.validate_common(&field, &vm.id, &vm.hostname(), ids, problems);

        if !matches!(vm.arch.as_str(), "amd64" | "arm64") {
            problems.push(Problem::new(
                format!("{field}.arch"),
                "architecture must be amd64 or arm64",
            ));
        }

        if vm.vcpus == 0 || vm.vcpus > 256 {
            problems.push(Problem::new(
                format!("{field}.vcpus"),
                "vCPUs must be between 1 and 256",
            ));
        }

        if let Some(topology) = vm.cpu_topology
            && topology.total() != u32::from(vm.vcpus)
        {
            problems.push(Problem::new(
                format!("{field}.cpu_topology"),
                format!(
                    "sockets × cores × threads = {} does not match {} vCPU(s)",
                    topology.total(),
                    vm.vcpus
                ),
            ));
        }
        if vm.cpu_topology.is_some_and(|topology| {
            topology.sockets == 0 || topology.cores == 0 || topology.threads == 0
        }) {
            problems.push(Problem::new(
                format!("{field}.cpu_topology"),
                "topology components must each be at least 1",
            ));
        }

        if vm.memory_mb < 128 {
            problems.push(Problem::new(
                format!("{field}.memory_mb"),
                "at least 128 MiB of memory is required",
            ));
        }
        if vm.hugepages && !vm.memory_mb.is_multiple_of(2) {
            problems.push(Problem::new(
                format!("{field}.memory_mb"),
                "hugepages need memory in 2 MiB steps (use an even number of MiB)",
            ));
        }
        if let Some(balloon) = vm.balloon_mb {
            if balloon < 128 {
                problems.push(Problem::new(
                    format!("{field}.balloon_mb"),
                    "the balloon floor must be at least 128 MiB",
                ));
            }
            if balloon > vm.memory_mb {
                problems.push(Problem::new(
                    format!("{field}.balloon_mb"),
                    format!(
                        "the balloon floor ({balloon} MiB) cannot exceed the memory ceiling ({} MiB)",
                        vm.memory_mb
                    ),
                ));
            }
        }

        if vm.disk_gb < 1 {
            problems.push(Problem::new(
                format!("{field}.disk_gb"),
                "the disk must be at least 1 GiB",
            ));
        }

        match MacAddr::parse(&vm.effective_mac()) {
            Ok(mac) => {
                let normalised = mac.to_string();
                if macs.contains(&normalised) {
                    problems.push(Problem::new(
                        format!("{field}.mac"),
                        format!("{normalised} is already used by another guest"),
                    ));
                }
                macs.push(normalised);
            }
            Err(err) => problems.push(Problem::new(format!("{field}.mac"), err)),
        }

        if let Some(port) = vm.vnc_port {
            if !(5900..=5999).contains(&port) {
                problems.push(Problem::new(
                    format!("{field}.vnc_port"),
                    "the VNC port must be between 5900 and 5999",
                ));
            }
            if vnc_ports.contains(&port) {
                problems.push(Problem::new(
                    format!("{field}.vnc_port"),
                    format!("VNC port {port} is already taken"),
                ));
            }
            vnc_ports.push(port);
        }

        if vm.vnc_enabled && parse_ipv4(&vm.vnc_bind).is_err() {
            problems.push(Problem::new(
                format!("{field}.vnc_bind"),
                "VNC bind address must be a valid IPv4 address",
            ));
        }

        if let Some(cpuset) = &vm.cpuset
            && parse_cpuset(cpuset).is_none()
        {
            problems.push(Problem::new(
                format!("{field}.cpuset"),
                "CPU pinning must look like 0-3,8,12-13",
            ));
        }

        for (feature_index, feature) in vm.cpu_features.iter().enumerate() {
            let trimmed = feature.trim();
            let valid = trimmed.len() >= 2
                && matches!(trimmed.as_bytes()[0], b'+' | b'-' | b'=')
                && trimmed[1..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
            if !valid {
                problems.push(Problem::new(
                    format!("{field}.cpu_features[{feature_index}]"),
                    "a CPU feature must look like +aes, -hle or =vmx",
                ));
            }
        }

        if vm.tpm && vm.arch != "amd64" {
            problems.push(Problem::new(
                format!("{field}.tpm"),
                "a TPM is only wired up for amd64 guests",
            ));
        }

        if vm.uses_cloud_init() {
            self.validate_ssh_keys(
                &format!("{field}.cloud_init"),
                &vm.cloud_init.ssh_keys,
                problems,
            );
            self.validate_network_fields(
                &format!("{field}.cloud_init"),
                &vm.bridge,
                vm.cloud_init.ip_mode,
                &vm.cloud_init.address,
                &vm.cloud_init.gateway,
                &vm.cloud_init.dns,
                problems,
            );
        }
    }

    fn validate_ct(
        &self,
        index: usize,
        ct: &CtConfig,
        problems: &mut Vec<Problem>,
        ids: &mut Vec<String>,
        macs: &mut Vec<String>,
    ) {
        let field = format!("cts[{index}]");
        self.validate_common(&field, &ct.id, &ct.hostname(), ids, problems);

        if !matches!(ct.arch.as_str(), "amd64" | "arm64") {
            problems.push(Problem::new(
                format!("{field}.arch"),
                "architecture must be amd64 or arm64",
            ));
        }
        if ct.template.trim().is_empty() {
            problems.push(Problem::new(
                format!("{field}.template"),
                "a distribution template is required",
            ));
        }
        if ct.memory_mb < 32 {
            problems.push(Problem::new(
                format!("{field}.memory_mb"),
                "at least 32 MiB of memory is required",
            ));
        }
        if ct.rootfs_gb < 1 {
            problems.push(Problem::new(
                format!("{field}.rootfs_gb"),
                "the root filesystem must be at least 1 GiB",
            ));
        }
        if ct.cores > 256 {
            problems.push(Problem::new(
                format!("{field}.cores"),
                "the CPU allowance cannot exceed 256 cores",
            ));
        }

        match MacAddr::parse(&ct.effective_mac()) {
            Ok(mac) => {
                let normalised = mac.to_string();
                if macs.contains(&normalised) {
                    problems.push(Problem::new(
                        format!("{field}.mac"),
                        format!("{normalised} is already used by another guest"),
                    ));
                }
                macs.push(normalised);
            }
            Err(err) => problems.push(Problem::new(format!("{field}.mac"), err)),
        }

        self.validate_network_fields(
            &field,
            &ct.bridge,
            ct.ip_mode,
            &ct.address,
            &ct.gateway,
            &ct.dns,
            problems,
        );
        self.validate_ssh_keys(&field, &ct.ssh_keys, problems);

        for (bind_index, bind) in ct.binds.iter().enumerate() {
            let bind_field = format!("{field}.binds[{bind_index}]");
            if !bind.source.starts_with('/') {
                problems.push(Problem::new(
                    format!("{bind_field}.source"),
                    "the host path must be absolute",
                ));
            }
            if !bind.target.starts_with('/') {
                problems.push(Problem::new(
                    format!("{bind_field}.target"),
                    "the container path must be absolute",
                ));
            }
        }

        for (cap_index, capability) in ct.cap_drop.iter().enumerate() {
            let valid = !capability.is_empty()
                && capability.len() <= 32
                && capability
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit());
            if !valid {
                problems.push(Problem::new(
                    format!("{field}.cap_drop[{cap_index}]"),
                    "a capability is an upper-case name like NET_ADMIN",
                ));
            }
        }
    }
}

/// Guest ids become a directory name and a QEMU/LXC instance name. Restricting
/// them to `[A-Za-z0-9_-]` starting with an alphanumeric makes `..`, `/` and
/// shell metacharacters structurally impossible.
pub fn sanitize_id(raw: &str) -> Result<&str, String> {
    if raw.is_empty() || raw.len() > 48 {
        return Err("guest id must be 1..=48 characters".to_string());
    }
    let first_ok = raw
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_ok = raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if first_ok && rest_ok {
        Ok(raw)
    } else {
        Err(format!("invalid guest id: {raw:?}"))
    }
}

fn one_u16() -> u16 {
    1
}

fn default_true() -> bool {
    true
}

fn default_arch() -> String {
    "amd64".to_string()
}

fn default_bridge() -> String {
    "br0".to_string()
}

fn default_vnc_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_user() -> String {
    "stack".to_string()
}

fn default_vcpus() -> u16 {
    2
}

fn default_memory() -> u32 {
    2048
}

fn default_disk() -> u32 {
    20
}

fn default_cores() -> u32 {
    2
}

fn default_ct_memory() -> u32 {
    1024
}

fn default_ct_swap() -> u32 {
    512
}

fn default_rootfs() -> u32 {
    8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_document_validates_clean() {
        let mut config = VirtualizationConfig::default();
        config.vms.push(VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        });
        config.cts.push(CtConfig {
            id: "db".to_string(),
            template: "debian".to_string(),
            ..CtConfig::default()
        });
        let problems = config.validate();
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn cpu_topology_auto_is_exact() {
        assert_eq!(
            CpuTopology::auto(1, false),
            CpuTopology {
                sockets: 1,
                cores: 1,
                threads: 1
            }
        );
        let four = CpuTopology::auto(4, false);
        assert_eq!(four.total(), 4);
        let smt = CpuTopology::auto(8, true);
        assert_eq!(smt.total(), 8);
        assert_eq!(smt.threads, 2);
        assert_eq!(smt.cores, 4);
        // An odd count cannot have two threads per core.
        assert_eq!(CpuTopology::auto(3, true).threads, 1);
        assert_eq!(CpuTopology::auto(3, true).total(), 3);
    }

    #[test]
    fn topology_mismatch_is_rejected() {
        let mut config = VirtualizationConfig::default();
        config.vms.push(VmConfig {
            id: "web".to_string(),
            vcpus: 4,
            cpu_topology: Some(CpuTopology {
                sockets: 1,
                cores: 3,
                threads: 1,
            }),
            ..VmConfig::default()
        });
        let problems = config.validate();
        assert!(
            problems.iter().any(|p| p.field.ends_with("cpu_topology")),
            "{problems:?}"
        );
    }

    #[test]
    fn duplicate_ids_and_macs_are_rejected() {
        let mut config = VirtualizationConfig::default();
        config.vms.push(VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        });
        config.vms.push(VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        });
        let problems = config.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("more than one guest"))
        );

        // Two guests that derive the same MAC: force it explicitly.
        let mut config = VirtualizationConfig::default();
        let mac = derive_mac("a");
        config.vms.push(VmConfig {
            id: "one".to_string(),
            mac: Some(mac.clone()),
            ..VmConfig::default()
        });
        config.cts.push(CtConfig {
            id: "two".to_string(),
            template: "debian".to_string(),
            mac: Some(mac),
            ..CtConfig::default()
        });
        let problems = config.validate();
        assert!(problems.iter().any(|p| p.message.contains("already used")));
    }

    #[test]
    fn static_network_must_be_consistent() {
        let mut config = VirtualizationConfig::default();
        config.cts.push(CtConfig {
            id: "db".to_string(),
            template: "debian".to_string(),
            ip_mode: NetMode::Static,
            address: Some("192.168.1.50/24".to_string()),
            gateway: "192.168.9.1".to_string(),
            ..CtConfig::default()
        });
        let problems = config.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.field.ends_with("gateway") && p.message.contains("outside")),
            "{problems:?}"
        );

        // A missing address is also a problem.
        let mut config = VirtualizationConfig::default();
        config.cts.push(CtConfig {
            id: "db".to_string(),
            template: "debian".to_string(),
            ip_mode: NetMode::Static,
            address: None,
            gateway: "192.168.1.1".to_string(),
            ..CtConfig::default()
        });
        assert!(
            config
                .validate()
                .iter()
                .any(|p| p.field.ends_with("address"))
        );
    }

    #[test]
    fn hugepages_require_even_memory() {
        let mut config = VirtualizationConfig::default();
        config.vms.push(VmConfig {
            id: "web".to_string(),
            hugepages: true,
            memory_mb: 2049,
            ..VmConfig::default()
        });
        assert!(
            config
                .validate()
                .iter()
                .any(|p| p.field.ends_with("memory_mb"))
        );
    }

    #[test]
    fn balloon_floor_cannot_exceed_the_ceiling() {
        let mut config = VirtualizationConfig::default();
        config.vms.push(VmConfig {
            id: "web".to_string(),
            memory_mb: 1024,
            balloon_mb: Some(2048),
            ..VmConfig::default()
        });
        assert!(
            config
                .validate()
                .iter()
                .any(|p| p.field.ends_with("balloon_mb"))
        );
    }

    #[test]
    fn ids_are_restricted_to_safe_names() {
        assert!(sanitize_id("web-01").is_ok());
        assert!(sanitize_id("a_b").is_ok());
        for bad in ["", "..", "a/b", "-lead", ".hidden", "a b", "a;b"] {
            assert!(sanitize_id(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(sanitize_id(&"x".repeat(49)).is_err());
    }

    #[test]
    fn mac_derivation_is_stable_and_local() {
        let first = derive_mac("web");
        assert_eq!(first, derive_mac("web"));
        assert_ne!(first, derive_mac("db"));
        assert!(first.starts_with("02:"), "{first}");
    }

    #[test]
    fn cpuset_parsing_handles_ranges_and_lists() {
        assert_eq!(
            parse_cpuset("0-3,8,12-13"),
            Some(vec![0, 1, 2, 3, 8, 12, 13])
        );
        assert_eq!(parse_cpuset(" 2 "), Some(vec![2]));
        assert_eq!(parse_cpuset("3-1"), None);
        assert_eq!(parse_cpuset(""), None);
        assert_eq!(parse_cpuset("a-b"), None);
    }

    #[test]
    fn ct_cpu_allowance_maps_to_cgroup_v2() {
        let ct = CtConfig {
            cores: 2,
            ..CtConfig::default()
        };
        assert_eq!(ct.cpu_max(), Some((200_000, 100_000)));
        let unlimited = CtConfig {
            cores: 0,
            ..CtConfig::default()
        };
        assert_eq!(unlimited.cpu_max(), None);
    }

    #[test]
    fn ssh_key_validation() {
        assert!(is_valid_ssh_key(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc user@host"
        ));
        assert!(!is_valid_ssh_key("not-a-key"));
        assert!(!is_valid_ssh_key("ssh-ed25519 short"));
    }

    #[test]
    fn hostname_validation_allows_single_labels() {
        assert!(is_valid_hostname("web"));
        assert!(is_valid_hostname("db-01"));
        assert!(is_valid_hostname("a.b.example.com"));
        for bad in ["", "-bad", "bad-", "a b", "a/b"] {
            assert!(!is_valid_hostname(bad), "{bad:?}");
        }
    }
}
