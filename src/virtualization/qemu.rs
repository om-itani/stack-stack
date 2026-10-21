//! QEMU command-line generation.
//!
//! Every VM is launched by stack-stack directly (`qemu-system-*`, no libvirt
//! daemon) so that the exact performance flags are under our control and there
//! is no extra process to manage. The generator is pure: it turns a validated
//! [`VmConfig`] plus [`Config`] into an argv vector, which is what makes both the
//! preview and the tests meaningful.
//!
//! The tuning baked in here is what makes a modest box feel fast:
//!
//! * **KVM with a TCG fallback.** `-accel kvm` when `/dev/kvm` exists, otherwise
//!   `-accel tcg,thread=multi`, which emulates on every available host core
//!   instead of one.
//! * **virtio everywhere.** virtio-scsi disks, virtio-net, virtio-rng and the
//!   balloon: no emulated IDE or e1000.
//! * **io_uring + `cache=none`.** Direct, asynchronous I/O without host page
//!   cache double-buffering, and `discard=unmap` so guest TRIM reaches the file.
//! * **vhost-net and multiqueue.** One RX/TX queue per vCPU, handled in the
//!   kernel rather than by the QEMU event loop.
//! * **`-cpu host`.** Full CPU feature passthrough (AES-NI, AVX2), which is the
//!   single biggest win for CPU-bound guests.
//! * **`-nodefaults`.** Nothing is compiled in that the guest did not ask for.

use std::path::PathBuf;

use crate::config::Config;
use crate::virtualization::model::{CpuModel, Firmware, Machine, VmConfig};
use crate::virtualization::{images, sha256, store};

/// OVMF firmware pair for UEFI guests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvmfPaths {
    pub code: PathBuf,
    pub vars: PathBuf,
}

/// A fully-resolved launch: argv plus the derived bits the UI wants to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QemuPlan {
    pub program: String,
    pub args: Vec<String>,
    /// Human-readable VNC endpoint, when VNC is enabled.
    pub vnc: Option<String>,
    /// Stable SMBIOS UUID derived from the guest id.
    pub uuid: String,
}

/// QEMU binary for a guest architecture.
pub fn program_for(arch: &str) -> String {
    match arch {
        "arm64" => "qemu-system-aarch64".to_string(),
        _ => "qemu-system-x86_64".to_string(),
    }
}

/// Build the argv for a VM.
pub fn plan(cfg: &Config, vm: &VmConfig, kvm: bool, ovmf: Option<&OvmfPaths>) -> QemuPlan {
    let disk = store::disk_path(cfg, vm);
    let pid = store::pid_path(cfg, &vm.id);
    let qmp = store::qmp_path(cfg, &vm.id);
    let serial = store::serial_path(cfg, &vm.id);
    let agent = store::guest_dir(cfg, &vm.id).join("agent.sock");
    let seed = store::seed_iso_path(cfg, &vm.id);

    let mut args: Vec<String> = Vec::new();
    let program = program_for(&vm.arch);

    // --- identity + no implicit devices ------------------------------------
    push(&mut args, &["-name", vm.display_name()]);
    let uuid = derive_uuid(&vm.id);
    push(&mut args, &["-uuid", &uuid]);
    push(&mut args, &["-nodefaults", "-no-user-config"]);

    // --- acceleration ------------------------------------------------------
    let machine = machine_spec(vm.machine);
    push(&mut args, &["-machine", &machine]);
    if kvm {
        push(&mut args, &["-accel", "kvm"]);
    } else {
        // Multi-threaded TCG: emulate across all host cores.
        push(&mut args, &["-accel", "tcg,thread=multi"]);
    }
    push(&mut args, &["-cpu", &cpu_spec(vm, kvm)]);

    // --- SMP ---------------------------------------------------------------
    let topology = vm.topology();
    push(
        &mut args,
        &[
            "-smp",
            &format!(
                "{},sockets={},cores={},threads={},maxcpus={}",
                vm.vcpus, topology.sockets, topology.cores, topology.threads, vm.vcpus
            ),
        ],
    );

    // --- memory ------------------------------------------------------------
    push(&mut args, &["-m", &vm.memory_mb.to_string()]);
    if vm.hugepages {
        // Back the RAM with 2 MiB hugepages; the NUMA node attaches the backend.
        push(
            &mut args,
            &[
                "-object",
                &format!(
                    "memory-backend-memfd,id=mem0,size={},hugetlb=on,hugetlbsize=2M,share=on",
                    vm.memory_bytes()
                ),
            ],
        );
        push(
            &mut args,
            &[
                "-numa",
                &format!(
                    "node,nodeid=0,cpus=0-{},memdev=mem0",
                    vm.vcpus.saturating_sub(1)
                ),
            ],
        );
    }
    if vm.mem_prealloc {
        push(&mut args, &["-mem-prealloc"]);
    }

    // --- storage (virtio-scsi + dedicated IO thread) -----------------------
    push(&mut args, &["-object", "iothread,id=io0"]);
    push(
        &mut args,
        &["-device", "virtio-scsi-pci,id=scsi0,iothread=io0"],
    );

    let image_is_iso = iso_medium(cfg, vm);

    let mut drive = format!(
        "if=none,id=disk0,file={},format={},cache={},aio={}",
        disk.display(),
        vm.disk_format.as_qemu_img(),
        vm.disk_cache.as_qemu(),
        vm.aio.as_qemu()
    );
    if vm.discard {
        drive.push_str(",discard=unmap,detect-zeroes=unmap");
    }
    push(&mut args, &["-drive", &drive]);
    push(&mut args, &["-device", "scsi-hd,drive=disk0,bus=scsi0.0"]);

    if let Some(iso) = image_is_iso {
        // An ISO is attached as a read-only CD-ROM and booted from first.
        push(
            &mut args,
            &[
                "-drive",
                &format!("if=none,id=cd0,file={iso},format=raw,media=cdrom,readonly=on"),
            ],
        );
        push(&mut args, &["-device", "scsi-cd,drive=cd0,bus=scsi0.0"]);
        push(&mut args, &["-boot", "order=d,menu=on"]);
    } else {
        push(&mut args, &["-boot", "order=c"]);
    }

    // --- network -----------------------------------------------------------
    plan_network(&mut args, vm);

    // --- entropy, balloon, agent, console ----------------------------------
    if vm.rng {
        push(
            &mut args,
            &["-object", "rng-random,id=rng0,filename=/dev/urandom"],
        );
        push(&mut args, &["-device", "virtio-rng-pci,rng=rng0"]);
    }
    push(
        &mut args,
        &["-device", "virtio-balloon-pci,free-page-reporting=on"],
    );

    if vm.guest_agent {
        push(
            &mut args,
            &["-device", "virtio-serial-pci,id=virtio-serial0"],
        );
        push(
            &mut args,
            &[
                "-chardev",
                &format!(
                    "socket,id=agent0,path={},server=on,wait=off",
                    agent.display()
                ),
            ],
        );
        push(
            &mut args,
            &[
                "-device",
                "virtserialport,chardev=agent0,name=org.qemu.guest_agent.0",
            ],
        );
    }

    if vm.serial_console {
        push(
            &mut args,
            &[
                "-chardev",
                &format!(
                    "socket,id=serial0,path={},server=on,wait=off",
                    serial.display()
                ),
            ],
        );
        push(&mut args, &["-serial", "chardev:serial0"]);
    }

    // --- graphics ----------------------------------------------------------
    let vnc = if vm.vnc_enabled {
        push(&mut args, &["-vga", "virtio"]);
        let endpoint = vnc_endpoint(vm);
        push(&mut args, &["-vnc", &endpoint]);
        Some(endpoint)
    } else {
        push(&mut args, &["-display", "none"]);
        None
    };

    // --- firmware ----------------------------------------------------------
    if vm.firmware == Firmware::Uefi {
        if let Some(ovmf) = ovmf {
            push(
                &mut args,
                &[
                    "-drive",
                    &format!(
                        "if=pflash,format=raw,readonly=on,file={}",
                        ovmf.code.display()
                    ),
                ],
            );
            push(
                &mut args,
                &[
                    "-drive",
                    &format!("if=pflash,format=raw,file={}", ovmf.vars.display()),
                ],
            );
        } else {
            tracing::warn!(
                guest = %vm.id,
                "UEFI requested but no OVMF firmware was found; firmware will be ignored"
            );
        }
    }

    if vm.tpm {
        let tpm_socket = store::guest_dir(cfg, &vm.id).join("swtpm.sock");
        push(
            &mut args,
            &[
                "-chardev",
                &format!("socket,id=chrtpm,path={}", tpm_socket.display()),
            ],
        );
        push(&mut args, &["-tpmdev", "emulator,id=tpm0,chardev=chrtpm"]);
        let device = if vm.machine == Machine::Q35 {
            "tpm-crb"
        } else {
            "tpm-tis"
        };
        push(&mut args, &["-device", &format!("{device},tpmdev=tpm0")]);
    }

    // --- cloud-init seed ---------------------------------------------------
    if vm.uses_cloud_init() {
        push(
            &mut args,
            &[
                "-drive",
                &format!("file={},format=raw,if=virtio,readonly=on", seed.display()),
            ],
        );
    }

    // --- control plane -----------------------------------------------------
    push(
        &mut args,
        &[
            "-qmp",
            &format!("unix:{},server=on,wait=off", qmp.display()),
        ],
    );
    push(&mut args, &["-monitor", "none"]);
    push(&mut args, &["-pidfile", &pid.display().to_string()]);
    push(&mut args, &["-rtc", "base=utc,clock=host,driftfix=slew"]);

    // --- operator extras ---------------------------------------------------
    for extra in &vm.extra_args {
        args.push(extra.clone());
    }
    push(&mut args, &["-daemonize"]);

    QemuPlan {
        program,
        args,
        vnc,
        uuid,
    }
}

fn plan_network(args: &mut Vec<String>, vm: &VmConfig) {
    let mac = vm.effective_mac();
    if vm.bridge == "user" {
        // Slirp user networking: handy on a development box with no bridge.
        push(args, &["-netdev", "user,id=net0"]);
        push(
            args,
            &["-device", &format!("virtio-net-pci,netdev=net0,mac={mac}")],
        );
        return;
    }

    push(
        args,
        &[
            "-netdev",
            &format!("bridge,id=net0,br={},vhost=on", vm.bridge),
        ],
    );

    if vm.multiqueue && vm.vcpus > 1 {
        // One queue per vCPU, plus a control queue; the vector count must be
        // 2 × queues + 2, which is exactly what QEMU requires.
        let queues = vm.vcpus;
        let vectors = u32::from(queues) * 2 + 2;
        push(
            args,
            &[
                "-device",
                &format!(
                    "virtio-net-pci,netdev=net0,mac={mac},mq=on,vectors={vectors},queues={queues}"
                ),
            ],
        );
    } else {
        push(
            args,
            &["-device", &format!("virtio-net-pci,netdev=net0,mac={mac}")],
        );
    }
}

fn machine_spec(machine: Machine) -> String {
    match machine {
        Machine::Q35 => "q35,usb=off,vmport=off,dump-guest-core=off".to_string(),
        Machine::Pc => "pc,usb=off,dump-guest-core=off".to_string(),
    }
}

fn cpu_spec(vm: &VmConfig, kvm: bool) -> String {
    // `host` only makes sense with KVM; without it, `max` is the fastest model
    // QEMU can emulate.
    let base = match vm.cpu_model {
        CpuModel::Host if kvm => "host",
        CpuModel::Host => "max",
        CpuModel::Max => "max",
        CpuModel::Qemu64 => "qemu64",
    };
    let features: Vec<String> = vm
        .cpu_features
        .iter()
        .map(|feature| feature.trim().to_string())
        .filter(|feature| !feature.is_empty())
        .collect();
    if features.is_empty() {
        base.to_string()
    } else {
        format!("{base},{}", features.join(","))
    }
}

/// `127.0.0.1:0` — QEMU allocates the port when `vnc_port` is unset.
fn vnc_endpoint(vm: &VmConfig) -> String {
    let bind = if vm.vnc_bind.trim().is_empty() {
        "127.0.0.1"
    } else {
        vm.vnc_bind.trim()
    };
    match vm.vnc_port {
        // QEMU's `-vnc host:display` counts from 5900, so subtract the base.
        Some(port) => format!("{bind}:{}", port.saturating_sub(5900)),
        None => format!("{bind}:0"),
    }
}

/// When the configured image is an installer ISO, return the file to attach as
/// a read-only CD-ROM. Catalog entries carrying an `iso` format resolve to their
/// cache path; anything else (a qcow2/raw cloud image, or a catalog id with no
/// ISO) returns `None` and the image becomes the disk instead.
pub fn iso_medium(cfg: &Config, vm: &VmConfig) -> Option<String> {
    let image = vm.image.as_deref()?;
    if image.starts_with('/') {
        return image
            .to_ascii_lowercase()
            .ends_with(".iso")
            .then(|| image.to_string());
    }
    let entry = images::find(cfg, image)?;
    if entry.format.as_deref() == Some("iso") {
        Some(images::cached_path(cfg, &entry).display().to_string())
    } else {
        None
    }
}

fn push(args: &mut Vec<String>, parts: &[&str]) {
    for part in parts {
        args.push((*part).to_string());
    }
}

/// A stable UUID (version 4 layout) derived from the guest id: the guest keeps
/// the same machine identity across rebuilds.
pub fn derive_uuid(seed: &str) -> String {
    let digest = sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Render the plan as a single copy-pasteable shell command for the preview.
pub fn script(plan: &QemuPlan) -> String {
    let mut out = String::new();
    out.push_str("# qemu launch (copy/paste to run outside stack-stack)\n");
    out.push_str(&shell_quote(&plan.program));
    for arg in &plan.args {
        out.push(' ');
        out.push_str(&shell_quote(arg));
    }
    out.push('\n');
    out
}

/// POSIX shell quoting for a single argument.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '/' | ':' | ',' | '=' | '+' | '@')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg(name: &str) -> Config {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-qemu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Config {
            guests_dir: dir.join("guests").to_string_lossy().into_owned(),
            images_dir: dir.join("images").to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    fn vm() -> VmConfig {
        VmConfig {
            id: "web".to_string(),
            vcpus: 4,
            memory_mb: 2048,
            bridge: "br0".to_string(),
            ..VmConfig::default()
        }
    }

    fn arg_value<'a>(plan: &'a QemuPlan, flag: &str) -> Option<&'a str> {
        let index = plan.args.iter().position(|arg| arg == flag)?;
        plan.args.get(index + 1).map(String::as_str)
    }

    #[test]
    fn kvm_and_virtio_are_the_defaults() {
        let plan = plan(&temp_cfg("defaults"), &vm(), true, None);
        assert_eq!(plan.program, "qemu-system-x86_64");
        assert_eq!(arg_value(&plan, "-accel"), Some("kvm"));
        assert_eq!(arg_value(&plan, "-cpu"), Some("host"));
        assert!(
            plan.args
                .iter()
                .any(|a| a == "virtio-scsi-pci,id=scsi0,iothread=io0")
        );
        assert!(
            plan.args
                .iter()
                .any(|a| a.contains("cache=none") && a.contains("aio=io_uring"))
        );
        assert!(plan.args.iter().any(|a| a.contains("discard=unmap")));
        assert!(plan.args.iter().any(|a| a == "-nodefaults"));
        assert_eq!(plan.args.last().map(String::as_str), Some("-daemonize"));
    }

    #[test]
    fn without_kvm_it_falls_back_to_multithreaded_tcg() {
        let plan = plan(&temp_cfg("tcg"), &vm(), false, None);
        assert_eq!(arg_value(&plan, "-accel"), Some("tcg,thread=multi"));
        // `host` cannot be used without KVM, so it degrades to `max`.
        assert_eq!(arg_value(&plan, "-cpu"), Some("max"));
    }

    #[test]
    fn smp_carries_an_exact_topology() {
        let plan = plan(&temp_cfg("smp"), &vm(), true, None);
        let smp = arg_value(&plan, "-smp").expect("smp");
        assert_eq!(smp, "4,sockets=1,cores=4,threads=1,maxcpus=4");
    }

    #[test]
    fn multiqueue_vectors_match_the_queue_count() {
        let plan = plan(&temp_cfg("mq"), &vm(), true, None);
        let net = plan
            .args
            .iter()
            .find(|a| a.starts_with("virtio-net-pci"))
            .expect("nic");
        assert!(net.contains("mq=on"), "{net}");
        // 4 vCPUs -> 4 queues -> 2*4+2 = 10 vectors.
        assert!(net.contains("vectors=10"), "{net}");
        assert!(net.contains("queues=4"), "{net}");
    }

    #[test]
    fn a_single_vcpu_skips_multiqueue() {
        let mut config = vm();
        config.vcpus = 1;
        let plan = plan(&temp_cfg("single"), &config, true, None);
        let net = plan
            .args
            .iter()
            .find(|a| a.starts_with("virtio-net-pci"))
            .expect("nic");
        assert!(!net.contains("mq=on"));
    }

    #[test]
    fn user_networking_uses_slirp() {
        let mut config = vm();
        config.bridge = "user".to_string();
        let plan = plan(&temp_cfg("user"), &config, true, None);
        assert_eq!(arg_value(&plan, "-netdev"), Some("user,id=net0"));
    }

    #[test]
    fn hugepages_back_memory_with_a_memfd_object() {
        let mut config = vm();
        config.hugepages = true;
        config.memory_mb = 2048;
        let plan = plan(&temp_cfg("huge"), &config, true, None);
        let object = plan
            .args
            .iter()
            .find(|a| a.starts_with("memory-backend-memfd"))
            .expect("memfd");
        assert!(object.contains("size=2147483648"), "{object}");
        assert!(object.contains("hugetlb=on"));
        let numa = arg_value(&plan, "-numa").expect("numa");
        assert!(numa.contains("memdev=mem0"));
    }

    #[test]
    fn cloud_init_attaches_the_seed_iso() {
        let config = vm();
        let plan = plan(&temp_cfg("seed"), &config, true, None);
        assert!(
            plan.args
                .iter()
                .any(|a| a.contains("seed.iso") && a.contains("if=virtio")),
            "{:?}",
            plan.args
        );
    }

    #[test]
    fn uefi_adds_both_pflash_drives() {
        let mut config = vm();
        config.firmware = Firmware::Uefi;
        let ovmf = OvmfPaths {
            code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            vars: PathBuf::from("/var/lib/stack-stack/guests/web/vars.fd"),
        };
        let plan = plan(&temp_cfg("uefi"), &config, true, Some(&ovmf));
        assert!(
            plan.args
                .iter()
                .any(|a| a.contains("OVMF_CODE.fd") && a.contains("readonly=on"))
        );
        assert!(
            plan.args
                .iter()
                .any(|a| a.contains("vars.fd") && !a.contains("readonly=on"))
        );
    }

    #[test]
    fn iso_images_boot_from_cd() {
        let mut config = vm();
        config.image = Some("/srv/isos/install.iso".to_string());
        let plan = plan(&temp_cfg("iso"), &config, true, None);
        assert_eq!(arg_value(&plan, "-boot"), Some("order=d,menu=on"));
        assert!(plan.args.iter().any(|a| a.contains("media=cdrom")));
    }

    #[test]
    fn vnc_display_index_is_offset_from_the_port() {
        let mut config = vm();
        config.vnc_port = Some(5902);
        let plan = plan(&temp_cfg("vnc"), &config, true, None);
        assert_eq!(arg_value(&plan, "-vnc"), Some("127.0.0.1:2"));
        assert_eq!(plan.vnc.as_deref(), Some("127.0.0.1:2"));
    }

    #[test]
    fn extra_args_are_appended_before_daemonize() {
        let mut config = vm();
        config.extra_args = vec!["-overcommit".to_string(), "mem-lock=off".to_string()];
        let plan = plan(&temp_cfg("extra"), &config, true, None);
        let overcommit = plan
            .args
            .iter()
            .position(|a| a == "-overcommit")
            .expect("extra");
        let daemonize = plan
            .args
            .iter()
            .position(|a| a == "-daemonize")
            .expect("daemonize");
        assert!(overcommit < daemonize);
    }

    #[test]
    fn uuid_is_stable_and_well_formed() {
        let uuid = derive_uuid("web");
        assert_eq!(uuid, derive_uuid("web"));
        assert_ne!(uuid, derive_uuid("db"));
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        // Version 4 in the first nibble of the third group, RFC 4122 variant in
        // the first nibble of the fourth.
        assert_eq!(parts[2].as_bytes()[0], b'4', "{uuid}");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "{uuid}"
        );
    }

    #[test]
    fn script_quotes_arguments_with_spaces() {
        let mut config = vm();
        config.extra_args = vec![
            "-fw_cfg".to_string(),
            "name=opt/x,string=hello world".to_string(),
        ];
        let plan = plan(&temp_cfg("script"), &config, true, None);
        let script = script(&plan);
        assert!(
            script.contains("'name=opt/x,string=hello world'"),
            "{script}"
        );
        assert!(script.starts_with("# qemu launch"));
    }
}
