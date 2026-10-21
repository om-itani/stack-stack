//! Virtualization: KVM/QEMU virtual machines and LXC containers.
//!
//! Design rules for this whole module, mirroring the router:
//!
//! * **Nothing runs that has not been validated.** [`model::VirtualizationConfig::validate`]
//!   runs before a document is persisted, and every generator reads only
//!   validated values.
//! * **Generators are pure.** [`qemu`], [`lxc`] and [`cloudinit`] turn a config
//!   into argv or text with no I/O, so they are unit-testable on any host.
//! * **Self-contained.** A guest seed is an ISO built by [`iso9660`], a download
//!   is verified by [`sha256`], and control goes over QMP ([`qmp`]) — no
//!   `genisoimage`, `libvirt`, `openssl` or other host tooling is required to
//!   provision and run a guest.
//! * **Everything degrades.** Without `/dev/kvm`, QEMU falls back to
//!   multithreaded TCG; without QEMU or LXC the API still loads and reports why.
//!
//! The performance budget lives in [`qemu`]: KVM with a TCG fallback, virtio
//! everywhere, io_uring with `cache=none`, vhost-net with multiqueue, `-cpu host`
//! passthrough and memory ballooning. On the same hardware that is the
//! difference between a usable guest and an unusable one.

pub mod cloudinit;
pub mod host;
pub mod images;
pub mod iso9660;
pub mod lxc;
pub mod manager;
pub mod model;
pub mod qemu;
pub mod qmp;
pub mod sha256;
pub mod status;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::networking::runner::{Runner, SystemRunner};
use crate::virtualization::model::{CtConfig, VmConfig};

/// Preview of a VM launch: the QEMU command and the cloud-init files.
#[derive(Debug, Clone, Serialize)]
pub struct VmPreview {
    pub qemu: String,
    pub uuid: String,
    pub vnc: Option<String>,
    pub user_data: Option<String>,
    pub meta_data: Option<String>,
    pub network_config: Option<String>,
}

/// Preview of a container: the generated LXC config.
#[derive(Debug, Clone, Serialize)]
pub struct CtPreview {
    pub lxc: String,
    pub resolv_conf: Option<String>,
}

/// Render every artefact for a VM without touching the system.
pub fn preview_vm(cfg: &Config, vm: &VmConfig, kvm: bool) -> VmPreview {
    let ovmf = host::detect_ovmf();
    let plan = qemu::plan(cfg, vm, kvm, ovmf.as_ref());
    let seed = cloudinit::render(vm);
    VmPreview {
        qemu: qemu::script(&plan),
        uuid: plan.uuid,
        vnc: plan.vnc,
        user_data: vm.uses_cloud_init().then_some(seed.user_data),
        meta_data: vm.uses_cloud_init().then_some(seed.meta_data),
        network_config: vm
            .uses_cloud_init()
            .then_some(seed.network_config)
            .flatten(),
    }
}

/// Render every artefact for a container without touching the system.
pub fn preview_ct(cfg: &Config, ct: &CtConfig) -> CtPreview {
    CtPreview {
        lxc: lxc::render_config(cfg, ct),
        resolv_conf: lxc::resolv_conf(ct),
    }
}

/// Shared runtime handles for the virtualization API.
#[derive(Clone)]
pub struct VirtContext {
    /// Where the configuration document lives.
    pub config_path: PathBuf,
    /// Command executor for probes, downloads and lifecycle steps.
    pub runner: Arc<dyn Runner>,
    /// Serialises lifecycle operations: two `create`/`start` calls for the same
    /// guest would otherwise race on the same disk and pidfile.
    pub lock: Arc<Mutex<()>>,
}

impl VirtContext {
    pub fn new(config_path: PathBuf) -> Self {
        // Guest commands (a template download, a disk convert) can be long; the
        // callers that need longer still pass an explicit deadline.
        let runner: Arc<dyn Runner> = Arc::new(SystemRunner::new(Duration::from_secs(60)));
        Self {
            config_path,
            runner,
            lock: Arc::new(Mutex::new(())),
        }
    }
}
