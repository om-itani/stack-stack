//! Guest lifecycle: create, start, stop, reboot, snapshot, resize, delete.
//!
//! Every function here is synchronous and drives the [`Runner`], so the HTTP
//! layer wraps them in `spawn_blocking` and the whole module is testable with a
//! scripted runner, temporary directories and no Linux host, root or KVM.
//!
//! The operations are ordered so a failure never leaves a half-built guest in
//! front of the API: a VM's directory is created, its disk and seed assembled,
//! and only then is the guest reported as created. Provisioning that needs a
//! real privileged operation (an unprivileged rootfs shift, a password) is done
//! last and reported as a warning rather than failing the whole create.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::error::ApiError;
use crate::networking::runner::Runner;
use crate::proc::Output;
use crate::virtualization::host::HostCapabilities;
use crate::virtualization::model::{CtConfig, Firmware, VirtualizationConfig, VmConfig};
use crate::virtualization::{cloudinit, host, images, lxc, qemu, qmp, sha256, status, store};

/// One image download or `lxc-create` (which fetches and unpacks a rootfs).
const CREATE_TIMEOUT: Duration = Duration::from_secs(1800);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3600);
/// Ordinary commands: `qemu-img`, `chown`, `kill`.
const SHORT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a graceful stop waits for the guest before escalating.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

/// Result of a lifecycle action, shaped for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub output: String,
    pub disk: Option<String>,
    pub seed: Option<String>,
    pub warnings: Vec<String>,
}

impl Report {
    fn note(message: impl Into<String>) -> Self {
        Self {
            output: message.into(),
            disk: None,
            seed: None,
            warnings: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_unix() -> Result<(), ApiError> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(ApiError::unavailable(
            "creating and running guests requires a unix host",
        ))
    }
}

fn invoke(
    runner: &dyn Runner,
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<Output, ApiError> {
    runner
        .run_timeout(program, args, timeout)
        .map_err(ApiError::unavailable)
}

/// Run and fail on a non-zero exit, returning combined output.
fn run_ok(
    runner: &dyn Runner,
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<String, ApiError> {
    let output = invoke(runner, program, args, timeout)?;
    if output.timed_out {
        return Err(ApiError::internal(format!(
            "`{program}` timed out after {}s: {}",
            timeout.as_secs(),
            tail(&output.combined())
        )));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "`{program}` failed: {}",
            tail(&output.combined())
        )));
    }
    Ok(output.combined())
}

/// Run and ignore the outcome; used for best-effort cleanup.
fn best_effort(runner: &dyn Runner, program: &str, args: &[String], timeout: Duration) {
    let _ = invoke(runner, program, args, timeout);
}

fn tail(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(no output)".to_string();
    }
    if trimmed.len() <= 2000 {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - 2000;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

fn paths(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn remove_dir_best_effort(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(path = %path.display(), %err, "cannot remove guest directory"),
    }
}

/// Pick the first free VNC port from `base`, avoiding the ports already taken in
/// the document. 5900-5999 is scanned once and then it wraps to the base.
pub fn allocate_vnc_port(doc: &VirtualizationConfig, base: u16) -> u16 {
    let base = base.clamp(5900, 5999);
    let used: Vec<u16> = doc.vms.iter().filter_map(|vm| vm.vnc_port).collect();
    for offset in 0..100u16 {
        let candidate = 5900 + ((u32::from(base - 5900) + u32::from(offset)) % 100) as u16;
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    base
}

// ---------------------------------------------------------------------------
// VM lifecycle
// ---------------------------------------------------------------------------

/// Create the disk and seed for a VM. The document must already contain the VM
/// (after this returns the caller persists it).
pub fn create_vm(
    cfg: &Config,
    vm: &mut VmConfig,
    caps: &HostCapabilities,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    if caps.qemu.is_none() {
        return Err(ApiError::unavailable(
            "QEMU is not installed (needed to create and run virtual machines)",
        ));
    }
    // Ids are validated before this is called, but the manager owns the paths it
    // is about to write, so re-check rather than trust the caller.
    crate::virtualization::model::sanitize_id(&vm.id).map_err(ApiError::bad_request)?;

    let dir = store::guest_dir(cfg, &vm.id);
    fs::create_dir_all(&dir)
        .map_err(|err| ApiError::internal(format!("cannot create {}: {err}", dir.display())))?;

    let disk = store::disk_path(cfg, vm);
    if disk.exists() {
        return Err(ApiError::bad_request(format!(
            "{} already has a disk; delete the guest before recreating it",
            vm.id
        )));
    }

    let mut warnings: Vec<String> = Vec::new();

    // 1. The install medium (an image to clone, an ISO to boot, or nothing).
    let medium = resolve_medium(cfg, vm, runner, &mut warnings)?;
    let iso = qemu::iso_medium(cfg, vm).map(PathBuf::from);

    // 2. The disk itself.
    create_disk(cfg, vm, medium.as_deref(), iso.is_some(), runner)?;

    // 3. The cloud-init seed.
    let seed = if vm.uses_cloud_init() {
        Some(write_seed(cfg, vm)?)
    } else {
        None
    };

    // 4. UEFI NVRAM: each guest needs its own writable copy.
    if vm.firmware == Firmware::Uefi {
        match host::detect_ovmf() {
            Some(ovmf) => {
                let vars = dir.join("vars.fd");
                fs::copy(&ovmf.vars, &vars).map_err(|err| {
                    ApiError::internal(format!("cannot copy {}: {err}", ovmf.vars.display()))
                })?;
            }
            None => warnings.push(
                "UEFI was requested but no OVMF firmware is installed on the host".to_string(),
            ),
        }
    }

    // 5. Performance sanity: warn (never fail) when the host cannot honour a
    //    request, so the guest still starts, just not with the intended tuning.
    if vm.hugepages && caps.hugepages_2m_free == 0 {
        warnings.push(
            "hugepages were requested but none are reserved; reserve them with \
             `echo N > /proc/sys/vm/nr_hugepages` or the guest will not start"
                .to_string(),
        );
    }
    if vm.cpu_model == crate::virtualization::model::CpuModel::Host && !caps.kvm {
        warnings.push(
            "KVM is unavailable, so `host` CPU passthrough will fall back to `max`".to_string(),
        );
    }

    Ok(Report {
        output: format!("created {}", vm.id),
        disk: Some(disk.display().to_string()),
        seed: seed.map(|path| path.display().to_string()),
        warnings,
    })
}

/// Start a VM: assemble the argv, honour CPU pinning and launch QEMU.
pub fn start_vm(
    cfg: &Config,
    vm: &VmConfig,
    caps: &HostCapabilities,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    if !caps.can_run_vms() {
        return Err(ApiError::unavailable(
            "QEMU is not installed or has no usable accelerator",
        ));
    }
    if status::probe_vm(cfg, vm).state == "running" {
        return Err(ApiError::bad_request(format!(
            "{} is already running",
            vm.id
        )));
    }

    let disk = store::disk_path(cfg, vm);
    if !disk.exists() {
        return Err(ApiError::bad_request(format!(
            "{} has no disk yet; create it first",
            vm.id
        )));
    }

    let mut warnings = Vec::new();
    if vm.uses_cloud_init() && !store::seed_iso_path(cfg, &vm.id).exists() {
        write_seed(cfg, vm)?;
    }

    let ovmf = if vm.firmware == Firmware::Uefi {
        host::detect_ovmf()
    } else {
        None
    };
    let plan = qemu::plan(cfg, vm, caps.kvm, ovmf.as_ref());

    // CPU pinning wraps the launch in `taskset -c <list>`.
    let (program, args) = match vm.cpuset.as_deref() {
        Some(cpuset) if !cpuset.trim().is_empty() => {
            let mut args = paths(&["-c", cpuset.trim(), &plan.program]);
            args.extend(plan.args.iter().cloned());
            ("taskset".to_string(), args)
        }
        _ => (plan.program.clone(), plan.args.clone()),
    };

    let output = invoke(runner, &program, &args, SHORT_TIMEOUT)?;
    if output.timed_out {
        return Err(ApiError::internal(format!(
            "{} timed out during start: {}",
            program,
            tail(&output.combined())
        )));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "starting {} failed: {}",
            vm.id,
            tail(&output.combined())
        )));
    }

    if status::read_pid(&store::pid_path(cfg, &vm.id)).is_none() {
        warnings.push("QEMU returned success but no pidfile appeared yet".to_string());
    }

    Ok(Report {
        output: format!("started {}", vm.id),
        disk: None,
        seed: None,
        warnings,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmControl {
    /// ACPI power button: the guest runs its own shutdown.
    Shutdown,
    /// Clean stop: ask QEMU to quit, flushing disks.
    Stop,
    /// Reset without powering off.
    Reboot,
    /// Hard power off.
    Destroy,
}

/// Stop, reboot or power off a VM. Falls back to signals when QMP is not
/// reachable, so a guest with a wedged QEMU still yields to the operator.
pub fn control_vm(
    cfg: &Config,
    vm: &VmConfig,
    control: VmControl,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    let pid_path = store::pid_path(cfg, &vm.id);
    let qmp_path = store::qmp_path(cfg, &vm.id);
    let pid = status::read_pid(&pid_path);

    if control == VmControl::Reboot {
        if qmp::reset(&qmp_path).is_err() {
            return Err(ApiError::unavailable(format!(
                "{} is not reachable over QMP; it may already be stopped",
                vm.id
            )));
        }
        return Ok(Report::note(format!("rebooted {}", vm.id)));
    }

    // Nothing to stop: tidy the stale marker so status is honest.
    let Some(pid) = pid else {
        let _ = fs::remove_file(&pid_path);
        return Ok(Report::note(format!("{} is already stopped", vm.id)));
    };

    match control {
        VmControl::Shutdown => {
            let _ = qmp::powerdown(&qmp_path);
            if !wait_for_exit(pid, SHUTDOWN_GRACE) {
                signal(runner, pid, "TERM");
                if !wait_for_exit(pid, Duration::from_secs(15)) {
                    signal(runner, pid, "KILL");
                    wait_for_exit(pid, Duration::from_secs(5));
                }
            }
        }
        VmControl::Stop => {
            // `quit` flushes and closes disks before exiting.
            if qmp::quit(&qmp_path).is_err() {
                signal(runner, pid, "TERM");
            }
            if !wait_for_exit(pid, Duration::from_secs(20)) {
                signal(runner, pid, "KILL");
                wait_for_exit(pid, Duration::from_secs(5));
            }
        }
        VmControl::Destroy => {
            signal(runner, pid, "KILL");
            wait_for_exit(pid, Duration::from_secs(5));
        }
        VmControl::Reboot => unreachable!("handled above"),
    }

    let _ = fs::remove_file(&pid_path);

    Ok(Report::note(format!(
        "{} {}",
        match control {
            VmControl::Shutdown => "shut down",
            VmControl::Stop => "stopped",
            VmControl::Destroy => "powered off",
            VmControl::Reboot => "rebooted",
        },
        vm.id
    )))
}

/// Delete a VM and everything it owns.
pub fn delete_vm(cfg: &Config, vm: &VmConfig, runner: &dyn Runner) -> Result<Report, ApiError> {
    require_unix()?;
    let _ = control_vm(cfg, vm, VmControl::Destroy, runner);
    remove_dir_best_effort(&store::guest_dir(cfg, &vm.id));
    Ok(Report::note(format!("deleted {}", vm.id)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotAction {
    Create(String),
    Apply(String),
    Delete(String),
    List,
}

/// Internal qcow2 snapshots. The guest must be stopped: `qemu-img` manipulates
/// the file directly and would race a running QEMU.
pub fn snapshot_vm(
    cfg: &Config,
    vm: &VmConfig,
    action: &SnapshotAction,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    if status::probe_vm(cfg, vm).state == "running" {
        return Err(ApiError::bad_request(
            "stop the guest before taking or applying a snapshot",
        ));
    }
    let disk = store::disk_path(cfg, vm);
    if !disk.exists() {
        return Err(ApiError::not_found(format!("{} has no disk", vm.id)));
    }

    let disk_arg = disk.display().to_string();
    let output = match action {
        SnapshotAction::List => run_ok(
            runner,
            "qemu-img",
            &paths(&["snapshot", "-l", &disk_arg]),
            SHORT_TIMEOUT,
        )?,
        SnapshotAction::Create(name) => {
            let name = sanitize_snapshot(name)?;
            run_ok(
                runner,
                "qemu-img",
                &paths(&["snapshot", "-c", &name, &disk_arg]),
                SHORT_TIMEOUT,
            )?
        }
        SnapshotAction::Apply(name) => {
            let name = sanitize_snapshot(name)?;
            run_ok(
                runner,
                "qemu-img",
                &paths(&["snapshot", "-a", &name, &disk_arg]),
                SHORT_TIMEOUT,
            )?
        }
        SnapshotAction::Delete(name) => {
            let name = sanitize_snapshot(name)?;
            run_ok(
                runner,
                "qemu-img",
                &paths(&["snapshot", "-d", &name, &disk_arg]),
                SHORT_TIMEOUT,
            )?
        }
    };

    Ok(Report::note(if output.trim().is_empty() {
        "snapshot operation complete".to_string()
    } else {
        output
    }))
}

/// Grow a VM disk. Shrinking is refused: `qemu-img resize` cannot safely reclaim
/// allocated blocks, and the guest filesystem would be corrupted.
pub fn resize_vm(
    cfg: &Config,
    vm: &mut VmConfig,
    new_gb: u32,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    if new_gb < 1 {
        return Err(ApiError::bad_request("the disk must be at least 1 GiB"));
    }
    if new_gb < vm.disk_gb {
        return Err(ApiError::bad_request(format!(
            "shrinking is not supported ({new_gb} GiB < {} GiB)",
            vm.disk_gb
        )));
    }
    let disk = store::disk_path(cfg, vm);
    if !disk.exists() {
        return Err(ApiError::not_found(format!("{} has no disk", vm.id)));
    }

    run_ok(
        runner,
        "qemu-img",
        &paths(&["resize", &disk.display().to_string(), &format!("{new_gb}G")]),
        SHORT_TIMEOUT,
    )?;
    vm.disk_gb = new_gb;

    Ok(Report::note(format!(
        "{} disk grown to {new_gb} GiB; the guest must grow its filesystem to use it",
        vm.id
    )))
}

// ---------------------------------------------------------------------------
// LXC lifecycle
// ---------------------------------------------------------------------------

/// Create an LXC container: download the template, write our config, then do the
/// offline provisioning (hostname, DNS, keys, password).
pub fn create_ct(
    cfg: &Config,
    ct: &CtConfig,
    caps: &HostCapabilities,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    if !caps.can_run_containers() {
        return Err(ApiError::unavailable(
            "LXC is not installed (needed to create containers)",
        ));
    }
    crate::virtualization::model::sanitize_id(&ct.id).map_err(ApiError::bad_request)?;

    let entry = images::find(cfg, &ct.template).ok_or_else(|| {
        ApiError::not_found(format!("unknown container template: {}", ct.template))
    })?;
    let dist = entry
        .lxc_dist
        .clone()
        .ok_or_else(|| ApiError::bad_request(format!("{} is not an LXC template", ct.template)))?;
    let release = entry
        .lxc_release
        .clone()
        .unwrap_or_else(|| "current".to_string());

    if ct_config_present(cfg, &ct.id) {
        return Err(ApiError::bad_request(format!(
            "container {} already exists; destroy it before recreating",
            ct.id
        )));
    }

    let mut warnings = Vec::new();
    if ct.unprivileged && !caps.subuid_mapped {
        return Err(ApiError::bad_request(
            "unprivileged containers need a root entry in /etc/subuid and /etc/subgid (100000:65536)",
        ));
    }

    let lxc_root = store::lxc_root(cfg);
    fs::create_dir_all(&lxc_root).map_err(|err| {
        ApiError::internal(format!("cannot create {}: {err}", lxc_root.display()))
    })?;

    let args = lxc::create_argv(cfg, ct, &dist, &release, entry.lxc_mirror.as_deref());
    run_ok(runner, "lxc-create", &args, CREATE_TIMEOUT)?;

    // Our config replaces whatever the template wrote.
    let config = lxc::render_config(cfg, ct);
    store::write_private(&store::ct_config_path(cfg, &ct.id), config.as_bytes())
        .map_err(|err| ApiError::internal(format!("cannot write the container config: {err}")))?;

    let rootfs = store::ct_dir(cfg, &ct.id).join("rootfs");

    // Unprivileged containers need the rootfs owned by the mapped host range;
    // the id map alone is not enough (the template created a root-owned tree).
    if ct.unprivileged {
        let chown = paths(&[
            "-R",
            &format!("{}:{}", lxc::IDMAP_HOST_BASE, lxc::IDMAP_HOST_BASE),
            &rootfs.display().to_string(),
        ]);
        if let Err(err) = run_ok(runner, "chown", &chown, CREATE_TIMEOUT) {
            warnings.push(format!(
                "could not shift the container root filesystem to the unprivileged id range: {err}"
            ));
        }
    }

    provision_ct_files(&rootfs, ct, &mut warnings);
    // A password failure is a warning, never a failed create: the container
    // exists and is usable, it just needs the password set later.
    let _ = set_ct_password(cfg, ct, runner, &mut warnings);

    Ok(Report {
        output: format!("created container {} from {} ({release})", ct.id, dist),
        disk: None,
        seed: None,
        warnings,
    })
}

pub fn start_ct(cfg: &Config, ct: &CtConfig, runner: &dyn Runner) -> Result<Report, ApiError> {
    require_unix()?;
    run_ok(
        runner,
        "lxc-start",
        &lxc::start_argv(cfg, &ct.id),
        SHORT_TIMEOUT,
    )?;
    Ok(Report::note(format!("started container {}", ct.id)))
}

pub fn stop_ct(
    cfg: &Config,
    ct: &CtConfig,
    kill: bool,
    runner: &dyn Runner,
) -> Result<Report, ApiError> {
    require_unix()?;
    run_ok(
        runner,
        "lxc-stop",
        &lxc::stop_argv(cfg, &ct.id, kill),
        Duration::from_secs(60),
    )?;
    Ok(Report::note(format!("stopped container {}", ct.id)))
}

pub fn delete_ct(cfg: &Config, ct: &CtConfig, runner: &dyn Runner) -> Result<Report, ApiError> {
    require_unix()?;
    // Best effort: a stopped or missing container still gets its files removed.
    best_effort(
        runner,
        "lxc-stop",
        &lxc::stop_argv(cfg, &ct.id, true),
        SHORT_TIMEOUT,
    );
    best_effort(
        runner,
        "lxc-destroy",
        &lxc::destroy_argv(cfg, &ct.id),
        SHORT_TIMEOUT,
    );
    remove_dir_best_effort(&store::ct_dir(cfg, &ct.id));
    Ok(Report::note(format!("deleted container {}", ct.id)))
}

/// Set or change the container root password.
pub fn set_ct_password(
    cfg: &Config,
    ct: &CtConfig,
    runner: &dyn Runner,
    warnings: &mut Vec<String>,
) -> Result<Report, ApiError> {
    require_unix()?;
    if ct.password.is_empty() {
        return Ok(Report::note("no password requested"));
    }
    let line = credential_line("root", &ct.password);
    let running = status::probe_ct(cfg, ct, runner).state == "running";

    let result = if running {
        runner.run_with_input(
            "lxc-attach",
            &lxc::attach_argv(cfg, &ct.id, &["chpasswd"]),
            SHORT_TIMEOUT,
            line.as_bytes(),
        )
    } else {
        let rootfs = store::ct_dir(cfg, &ct.id).join("rootfs");
        runner.run_with_input(
            "chroot",
            &paths(&[&rootfs.display().to_string(), "chpasswd"]),
            SHORT_TIMEOUT,
            line.as_bytes(),
        )
    };

    match result {
        Ok(output) if output.success() => Ok(Report::note(format!("password set for {}", ct.id))),
        Ok(output) => {
            let message = format!(
                "could not set the container password: {}",
                tail(&output.combined())
            );
            warnings.push(message.clone());
            Ok(Report::note(message))
        }
        Err(err) => {
            warnings.push(err.clone());
            Ok(Report::note(err))
        }
    }
}

// ---------------------------------------------------------------------------
// Creation internals
// ---------------------------------------------------------------------------

/// Resolve `vm.image` to a local file: a catalog entry (downloaded on demand) or
/// an operator-supplied absolute path.
fn resolve_medium(
    cfg: &Config,
    vm: &VmConfig,
    runner: &dyn Runner,
    warnings: &mut Vec<String>,
) -> Result<Option<PathBuf>, ApiError> {
    let Some(image) = vm.image.clone() else {
        return Ok(None);
    };

    if image.starts_with('/') {
        let path = PathBuf::from(&image);
        if !path.is_file() {
            return Err(ApiError::not_found(format!("image not found: {image}")));
        }
        return Ok(Some(path));
    }

    let entry = images::find(cfg, &image)
        .ok_or_else(|| ApiError::not_found(format!("unknown image: {image}")))?;
    if entry.kind != images::GuestImageKind::Vm {
        return Err(ApiError::bad_request(format!(
            "{} is an LXC template, not a VM image",
            image
        )));
    }

    let cached = images::cached_path(cfg, &entry);
    if cached.is_file() {
        return Ok(Some(cached));
    }

    let url = entry.url.clone().ok_or_else(|| {
        ApiError::bad_request(format!(
            "{} has no download URL; set `image` to a local file path instead",
            image
        ))
    })?;

    download(runner, &url, &cached, entry.sha256.as_deref())?;
    if entry.sha256.is_none() {
        warnings.push(format!(
            "{} has no checksum in the catalog; the download was not verified",
            image
        ));
    }
    Ok(Some(cached))
}

/// Fetch `url` to `dest`, verifying `expected_sha256` when supplied.
fn download(
    runner: &dyn Runner,
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ApiError::internal(format!("cannot create {}: {err}", parent.display()))
        })?;
    }
    let part = dest.with_extension("part");
    let _ = fs::remove_file(&part);

    // Prefer curl, fall back to wget. Both are idempotent when re-run.
    let curl = paths(&[
        "-fL",
        "--retry",
        "3",
        "--connect-timeout",
        "30",
        "-o",
        &part.display().to_string(),
        url,
    ]);
    let fetched = match invoke(runner, "curl", &curl, DOWNLOAD_TIMEOUT) {
        Ok(output) if output.success() => true,
        Ok(output) => {
            tracing::warn!(url, %output.stderr, "curl download failed");
            false
        }
        Err(err) => {
            tracing::debug!(%err, "curl unavailable, trying wget");
            false
        }
    };

    if !fetched {
        let wget = paths(&["-q", "-O", &part.display().to_string(), url]);
        run_ok(runner, "wget", &wget, DOWNLOAD_TIMEOUT)?;
    }

    if let Some(expected) = expected_sha256 {
        let actual = sha256::hash_file(&part)
            .map_err(|err| ApiError::internal(format!("cannot hash the download: {err}")))?;
        if !sha256::constant_time_eq(&actual, expected) {
            let _ = fs::remove_file(&part);
            return Err(ApiError::internal(format!(
                "checksum mismatch for {url}: expected {expected}, got {actual}"
            )));
        }
    }

    fs::rename(&part, dest)
        .map_err(|err| ApiError::internal(format!("cannot finish {}: {err}", dest.display())))?;
    Ok(())
}

/// Assemble the VM disk: clone a base image and grow it, or create it empty.
fn create_disk(
    cfg: &Config,
    vm: &VmConfig,
    medium: Option<&Path>,
    medium_is_iso: bool,
    runner: &dyn Runner,
) -> Result<(), ApiError> {
    let disk = store::disk_path(cfg, vm);
    let format = vm.disk_format.as_qemu_img();
    let target = format!("{}G", vm.disk_gb);

    match medium {
        Some(source) if !medium_is_iso => {
            run_ok(
                runner,
                "qemu-img",
                &paths(&[
                    "convert",
                    "-O",
                    format,
                    &source.display().to_string(),
                    &disk.display().to_string(),
                ]),
                CREATE_TIMEOUT,
            )?;

            // Never ask qemu-img to shrink: it refuses, and a silently smaller
            // disk would be wrong. Resize up only.
            let target_bytes = vm.disk_bytes();
            let base_bytes = qemu_img_virtual_size(runner, source).unwrap_or(0);
            if target_bytes <= base_bytes {
                tracing::info!(
                    guest = %vm.id,
                    "requested disk is not larger than the image; keeping the image size"
                );
            } else {
                run_ok(
                    runner,
                    "qemu-img",
                    &paths(&["resize", &disk.display().to_string(), &target]),
                    SHORT_TIMEOUT,
                )?;
            }
        }
        _ => {
            // No medium, or an installer ISO: a fresh, empty disk.
            run_ok(
                runner,
                "qemu-img",
                &paths(&["create", "-f", format, &disk.display().to_string(), &target]),
                SHORT_TIMEOUT,
            )?;
        }
    }
    Ok(())
}

/// `qemu-img info --output=json` -> `virtual-size`, best effort.
fn qemu_img_virtual_size(runner: &dyn Runner, path: &Path) -> Option<u64> {
    let output = runner
        .call(
            "qemu-img",
            &["info", "--output=json", &path.display().to_string()],
        )
        .ok()?;
    if !output.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&output.stdout).ok()?;
    value.get("virtual-size").and_then(|size| size.as_u64())
}

/// Write the three NoCloud files and build the seed ISO.
fn write_seed(cfg: &Config, vm: &VmConfig) -> Result<PathBuf, ApiError> {
    let dir = store::seed_dir(cfg, &vm.id);
    fs::create_dir_all(&dir)
        .map_err(|err| ApiError::internal(format!("cannot create {}: {err}", dir.display())))?;

    let seed = cloudinit::render(vm);
    let files = [
        ("user-data", Some(seed.user_data)),
        ("meta-data", Some(seed.meta_data)),
        ("network-config", seed.network_config),
    ];
    for (name, contents) in files {
        if let Some(contents) = contents {
            store::write_private(&dir.join(name), contents.as_bytes())
                .map_err(|err| ApiError::internal(format!("cannot write seed {name}: {err}")))?;
        }
    }

    let iso = store::seed_iso_path(cfg, &vm.id);
    let bytes = cloudinit::build_iso(vm);
    store::write_private(&iso, &bytes)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", iso.display())))?;
    Ok(iso)
}

/// Offline provisioning that only needs the root filesystem on disk.
fn provision_ct_files(rootfs: &Path, ct: &CtConfig, warnings: &mut Vec<String>) {
    // Hostname.
    let hostname = rootfs.join("etc/hostname");
    if let Err(err) = fs::write(&hostname, format!("{}\n", ct.hostname())) {
        warnings.push(format!("cannot set the container hostname: {err}"));
    }

    // /etc/hosts: add the loopback line only if it is not already there.
    let hosts = rootfs.join("etc/hosts");
    let line = lxc::hosts_line(ct);
    match fs::read_to_string(&hosts) {
        Ok(existing) => {
            if !existing.contains(&line.trim_end().to_string()) {
                let mut updated = existing;
                if !updated.ends_with('\n') {
                    updated.push('\n');
                }
                updated.push_str(&line);
                if let Err(err) = fs::write(&hosts, updated) {
                    warnings.push(format!("cannot update /etc/hosts: {err}"));
                }
            }
        }
        Err(_) => {
            let contents = format!("127.0.0.1 localhost\n{line}");
            if let Err(err) = fs::write(&hosts, contents) {
                warnings.push(format!("cannot write /etc/hosts: {err}"));
            }
        }
    }

    // DNS.
    if let Some(resolv) = lxc::resolv_conf(ct)
        && let Err(err) = fs::write(rootfs.join("etc/resolv.conf"), resolv)
    {
        warnings.push(format!("cannot write /etc/resolv.conf: {err}"));
    }

    // Authorised keys for root.
    if !ct.ssh_keys.is_empty() {
        let ssh_dir = rootfs.join("root/.ssh");
        if let Err(err) = fs::create_dir_all(&ssh_dir) {
            warnings.push(format!("cannot create /root/.ssh: {err}"));
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&ssh_dir, fs::Permissions::from_mode(0o700));
        }
        let mut body = ct.ssh_keys.join("\n");
        body.push('\n');
        let keys = ssh_dir.join("authorized_keys");
        if let Err(err) = fs::write(&keys, body) {
            warnings.push(format!("cannot write authorized_keys: {err}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&keys, fs::Permissions::from_mode(0o600));
        }
    }
}

fn ct_config_present(cfg: &Config, id: &str) -> bool {
    store::ct_config_path(cfg, id).is_file()
}

/// `user:password` for `chpasswd`, with the password stripped of characters that
/// would end the line early.
fn credential_line(user: &str, password: &str) -> String {
    let password: String = password
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect();
    format!("{user}:{password}\n")
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

fn signal(runner: &dyn Runner, pid: u32, signal: &str) {
    best_effort(
        runner,
        "kill",
        &paths(&[&format!("-{signal}"), &pid.to_string()]),
        Duration::from_secs(10),
    );
}

/// Poll until the pid disappears, or `timeout` elapses.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !status::process_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Snapshot names become a `qemu-img` argument, so keep them to a safe set.
fn sanitize_snapshot(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ApiError::bad_request(
            "a snapshot name must be 1..=64 characters",
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || trimmed.chars().all(|c| c == '.')
    {
        return Err(ApiError::bad_request(format!(
            "invalid snapshot name: {raw:?}"
        )));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::runner::fake::FakeRunner;
    use crate::virtualization::model::GuestDefaults;

    fn temp_cfg(name: &str) -> Config {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-manager-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Config {
            guests_dir: dir.join("guests").to_string_lossy().into_owned(),
            images_dir: dir.join("images").to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    #[cfg(unix)]
    fn caps(qemu: bool, kvm: bool, lxc: bool) -> HostCapabilities {
        HostCapabilities {
            qemu: qemu.then(|| crate::virtualization::host::ToolInfo {
                program: "qemu-system-x86_64".to_string(),
                version: "8.2.0".to_string(),
            }),
            kvm,
            lxc: lxc.then(|| crate::virtualization::host::ToolInfo {
                program: "lxc-create".to_string(),
                version: "5.0".to_string(),
            }),
            subuid_mapped: true,
            ..HostCapabilities::default()
        }
    }

    #[cfg(unix)]
    fn qemu_img_runner() -> FakeRunner {
        FakeRunner::new()
            .respond("qemu-img", Some(0), "", "")
            .respond_when(
                "qemu-img",
                "info",
                Some(0),
                r#"{"virtual-size": 2147483648}"#,
                "",
            )
            .respond("chown", Some(0), "", "")
            .respond("chroot", Some(0), "", "")
            .respond("kill", Some(0), "", "")
            .respond("lxc-create", Some(0), "", "")
    }

    #[cfg(unix)]
    #[test]
    fn create_vm_from_an_iso_makes_an_empty_disk_and_a_seed() {
        let cfg = temp_cfg("iso");
        let iso_dir = std::env::temp_dir().join(format!("stack-stack-iso-{}", std::process::id()));
        fs::create_dir_all(&iso_dir).expect("mkdir");
        let iso = iso_dir.join("install.iso");
        fs::write(&iso, b"iso").expect("write iso");

        let mut vm = VmConfig {
            id: "web".to_string(),
            image: Some(iso.display().to_string()),
            vnc_port: Some(5900),
            ..VmConfig::default()
        };
        let runner = qemu_img_runner();
        let report = create_vm(&cfg, &mut vm, &caps(true, true, false), &runner).expect("create");
        assert!(report.disk.is_some());
        assert!(report.seed.is_some());
        // `create`, not `convert`, because the medium is an ISO.
        let created = runner.calls().iter().any(|(program, args)| {
            program == "qemu-img" && args.first().map(String::as_str) == Some("create")
        });
        assert!(created, "{:?}", runner.calls());
        assert!(store::seed_iso_path(&cfg, "web").is_file());
        assert!(store::seed_dir(&cfg, "web").join("user-data").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn create_vm_clones_a_cloud_image_and_grows_it() {
        let cfg = temp_cfg("clone");
        // Pre-place the cached image so no download is attempted.
        fs::create_dir_all(&cfg.images_dir).expect("mkdir");
        let image = Path::new(&cfg.images_dir).join("debian-12.qcow2");
        fs::write(&image, b"base").expect("write base");

        let mut vm = VmConfig {
            id: "web".to_string(),
            image: Some("debian-12".to_string()),
            disk_gb: 30,
            vnc_port: Some(5900),
            ..VmConfig::default()
        };
        let runner = qemu_img_runner();
        create_vm(&cfg, &mut vm, &caps(true, true, false), &runner).expect("create");

        assert!(
            runner.calls().iter().any(|(program, args)| {
                program == "qemu-img" && args.first().map(String::as_str) == Some("convert")
            }),
            "{:?}",
            runner.calls()
        );
        assert!(
            runner.calls().iter().any(|(program, args)| {
                program == "qemu-img" && args.first().map(String::as_str) == Some("resize")
            }),
            "{:?}",
            runner.calls()
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_vm_refuses_a_second_disk() {
        let cfg = temp_cfg("dup");
        let mut vm = VmConfig {
            id: "web".to_string(),
            vnc_port: Some(5900),
            ..VmConfig::default()
        };
        let runner = qemu_img_runner();
        create_vm(&cfg, &mut vm, &caps(true, true, false), &runner).expect("first create");
        // Simulate qemu-img having produced the disk.
        fs::write(store::disk_path(&cfg, &vm), b"disk").expect("write disk");
        let err = create_vm(&cfg, &mut vm, &caps(true, true, false), &runner).expect_err("second");
        assert!(err.to_string().contains("already has a disk"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn create_vm_needs_qemu() {
        let cfg = temp_cfg("noqemu");
        let mut vm = VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        };
        let err = create_vm(
            &cfg,
            &mut vm,
            &caps(false, false, false),
            &FakeRunner::new(),
        )
        .expect_err("no qemu");
        assert!(err.to_string().contains("QEMU is not installed"));
    }

    #[cfg(unix)]
    #[test]
    fn resize_is_up_only() {
        let cfg = temp_cfg("resize");
        let mut vm = VmConfig {
            id: "web".to_string(),
            disk_gb: 20,
            ..VmConfig::default()
        };
        fs::create_dir_all(store::guest_dir(&cfg, "web")).expect("mkdir");
        fs::write(store::disk_path(&cfg, &vm), b"disk").expect("write");

        let err = resize_vm(&cfg, &mut vm, 10, &qemu_img_runner()).expect_err("shrink");
        assert!(err.to_string().contains("shrinking is not supported"));

        let report = resize_vm(&cfg, &mut vm, 40, &qemu_img_runner()).expect("grow");
        assert_eq!(vm.disk_gb, 40);
        assert!(report.output.contains("40 GiB"));
    }

    #[cfg(unix)]
    #[test]
    fn control_vm_on_a_stopped_guest_is_a_noop() {
        let cfg = temp_cfg("control");
        let vm = VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        };
        let report =
            control_vm(&cfg, &vm, VmControl::Shutdown, &qemu_img_runner()).expect("control");
        assert!(report.output.contains("already stopped"));
    }

    #[cfg(unix)]
    #[test]
    fn create_ct_writes_config_and_provisions_offline() {
        let cfg = temp_cfg("ct");
        let ct = CtConfig {
            id: "db".to_string(),
            template: "debian".to_string(),
            dns: vec!["1.1.1.1".to_string()],
            ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc user@host".to_string()],
            password: "hunter2".to_string(),
            ..CtConfig::default()
        };
        let runner = qemu_img_runner();

        // Simulate the rootfs lxc-create would have produced.
        let rootfs = store::ct_dir(&cfg, "db").join("rootfs");
        fs::create_dir_all(rootfs.join("etc")).expect("mkdir etc");
        fs::create_dir_all(rootfs.join("root")).expect("mkdir root");

        let report = create_ct(&cfg, &ct, &caps(false, false, true), &runner).expect("create ct");
        assert!(report.output.contains("created container db"));
        assert!(runner.ran("lxc-create"));
        assert!(runner.ran("chown"));

        let config = fs::read_to_string(store::ct_config_path(&cfg, "db")).expect("config");
        assert!(config.contains("lxc.net.0.link = br0"));
        assert!(config.contains("lxc.idmap = u 0 100000 65536"));

        assert_eq!(
            fs::read_to_string(rootfs.join("etc/hostname")).expect("hostname"),
            "db\n"
        );
        assert!(
            fs::read_to_string(rootfs.join("etc/resolv.conf"))
                .expect("resolv")
                .contains("nameserver 1.1.1.1")
        );
        assert!(rootfs.join("root/.ssh/authorized_keys").is_file());
        // The password was handed to chpasswd via stdin, never argv.
        assert!(runner.ran("chroot"));
    }

    #[cfg(unix)]
    #[test]
    fn unprivileged_ct_requires_subuid() {
        let cfg = temp_cfg("nosubuid");
        let ct = CtConfig {
            id: "db".to_string(),
            template: "debian".to_string(),
            ..CtConfig::default()
        };
        let mut host_caps = caps(false, false, true);
        host_caps.subuid_mapped = false;
        let err = create_ct(&cfg, &ct, &host_caps, &qemu_img_runner()).expect_err("subuid");
        assert!(err.to_string().contains("/etc/subuid"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn unknown_template_is_rejected() {
        let cfg = temp_cfg("unknown");
        let ct = CtConfig {
            id: "db".to_string(),
            template: "not-a-template".to_string(),
            ..CtConfig::default()
        };
        let err = create_ct(&cfg, &ct, &caps(false, false, true), &qemu_img_runner())
            .expect_err("unknown");
        assert!(err.to_string().contains("unknown container template"));
    }

    #[test]
    fn vnc_ports_avoid_the_ones_already_used() {
        let doc = VirtualizationConfig {
            defaults: GuestDefaults::default(),
            vms: vec![
                VmConfig {
                    id: "a".to_string(),
                    vnc_port: Some(5900),
                    ..VmConfig::default()
                },
                VmConfig {
                    id: "b".to_string(),
                    vnc_port: Some(5901),
                    ..VmConfig::default()
                },
            ],
            ..VirtualizationConfig::default()
        };
        assert_eq!(allocate_vnc_port(&doc, 5900), 5902);
    }

    #[test]
    fn snapshot_names_are_restricted() {
        assert_eq!(sanitize_snapshot("pre-upgrade").expect("ok"), "pre-upgrade");
        for bad in ["", "..", "a/b", "a b", "a;rm"] {
            assert!(sanitize_snapshot(bad).is_err(), "{bad:?}");
        }
        assert!(sanitize_snapshot(&"x".repeat(65)).is_err());
    }

    #[test]
    fn credential_lines_strip_control_characters() {
        assert_eq!(credential_line("root", "hunter2"), "root:hunter2\n");
        // A newline cannot smuggle in a second chpasswd entry.
        assert_eq!(credential_line("root", "a\nb"), "root:ab\n");
    }

    #[test]
    fn a_download_without_a_checksum_still_installs() {
        // Verifies the hash-free path: the file is written and renamed, no
        // verification failure. Uses a runner that stands in for curl.
        let cfg = temp_cfg("nocsum");
        let dest = Path::new(&cfg.images_dir).join("x.qcow2");
        fs::create_dir_all(&cfg.images_dir).expect("mkdir");
        // curl is faked as a success that writes nothing, so the rename fails;
        // the point of this test is that a missing checksum does not itself
        // reject the download.
        let runner = FakeRunner::new().respond("curl", Some(0), "", "");
        let result = download(&runner, "http://example/x.qcow2", &dest, None);
        assert!(result.is_err(), "a no-op curl leaves no file to rename");
        assert!(runner.ran("curl"));
    }
}
