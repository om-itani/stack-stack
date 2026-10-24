//! Physical disks and the format/mount operations behind the Storage page.
//!
//! Nothing here talks to a device directly: everything shells out to the tools
//! Debian already ships (`lsblk`, `mkfs.*`, `mount`, `umount`, `blkid`). That
//! keeps the crate free of a filesystem or partition crate, and it means the
//! same guards can be exercised in tests with a fake runner.
//!
//! Every operation that can destroy data refuses unless the device is an
//! unmounted whole disk or partition that does not back the root filesystem.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::runner::Runner;

/// Filesystems offered for formatting. Kept short on purpose: each extra choice
/// is another way to lose a disk to a mis-click, and these two cover a data disk.
pub const FORMATS: [&str; 2] = ["ext4", "xfs"];

/// Mount points the UI may create. A path outside these is refused so a stray
/// one cannot shadow `/etc`, `/var` or the root filesystem.
pub const ALLOWED_MOUNT_PREFIXES: [&str; 3] = ["/mnt/", "/srv/", "/media/"];

/// Mount points that must never be unmounted from this page.
const PROTECTED_MOUNTS: [&str; 9] = [
    "/", "/boot", "/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/run",
];

/// One node of the block-device tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDevice {
    /// Kernel name, e.g. `sda`, `sda1`, `nvme0n1p2`.
    pub name: String,
    /// Full path, e.g. `/dev/sda1`.
    pub path: String,
    /// `disk`, `part`, `lvm`, `crypt`, `rom`, …
    pub kind: String,
    pub size_bytes: u64,
    pub model: Option<String>,
    pub fstype: Option<String>,
    pub mountpoint: Option<String>,
    pub label: Option<String>,
    pub children: Vec<BlockDevice>,
}

impl BlockDevice {
    /// True when this node or any descendant is mounted.
    pub fn has_mounted_descendant(&self) -> bool {
        if self.mountpoint.as_deref().is_some_and(|m| !m.is_empty()) {
            return true;
        }
        self.children
            .iter()
            .any(BlockDevice::has_mounted_descendant)
    }

    fn walk<'a>(&'a self, out: &mut Vec<&'a BlockDevice>) {
        out.push(self);
        for child in &self.children {
            child.walk(out);
        }
    }
}

/// Disk operations, over an injected runner so tests need no Linux host.
#[derive(Clone)]
pub struct StorageContext {
    runner: Arc<dyn Runner>,
    fstab: PathBuf,
}

impl StorageContext {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            fstab: PathBuf::from("/etc/fstab"),
        }
    }

    /// Point the fstab writer at another file. Test-only: production always uses
    /// the real `/etc/fstab`.
    #[cfg(test)]
    pub fn with_fstab(runner: Arc<dyn Runner>, fstab: PathBuf) -> Self {
        Self { runner, fstab }
    }

    /// The full block-device tree, root filesystem check included.
    pub fn devices(&self) -> Result<Vec<BlockDevice>, ApiError> {
        let output = self
            .runner
            .call(
                "lsblk",
                &[
                    "-J",
                    "-b",
                    "-o",
                    "NAME,PATH,TYPE,SIZE,MODEL,FSTYPE,MOUNTPOINT,LABEL",
                ],
            )
            .map_err(|err| ApiError::unavailable(format!("cannot run lsblk: {err}")))?;
        if !output.success() {
            return Err(ApiError::unavailable(format!(
                "lsblk failed: {}",
                output.combined().trim()
            )));
        }
        parse_lsblk(&output.stdout)
    }

    /// Everything the UI needs in one call, plus which device backs `/` so the
    /// page can mark it as untouchable.
    pub fn state(&self) -> Result<StorageState, ApiError> {
        Ok(StorageState {
            devices: self.devices()?,
            root_device: self.root_source(),
            formats: FORMATS.iter().map(|fs| (*fs).to_string()).collect(),
        })
    }

    /// The device backing `/`, resolved to its whole-disk name where possible.
    fn root_source(&self) -> Option<String> {
        let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
        let source = mounts
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some("/"))?
            .split_whitespace()
            .next()?
            .to_string();
        Some(source)
    }

    /// Format `device` with `fs`. Refuses anything that is not an unmounted
    /// whole disk or partition, or that backs the root filesystem.
    pub fn format(&self, device: &str, fs: &str) -> Result<String, ApiError> {
        validate_device_path(device)?;
        if !FORMATS.contains(&fs) {
            return Err(ApiError::bad_request(format!(
                "unsupported filesystem {fs:?}; choose one of {}",
                FORMATS.join(", ")
            )));
        }

        let devices = self.devices()?;
        let mut flat = Vec::new();
        for node in &devices {
            node.walk(&mut flat);
        }
        let node = flat
            .iter()
            .find(|node| node.path == device)
            .copied()
            .ok_or_else(|| ApiError::not_found(format!("{device} is not a block device")))?;

        if node.kind != "part" && node.kind != "disk" {
            return Err(ApiError::bad_request(format!(
                "{device} is a {} device, not a disk or partition",
                node.kind
            )));
        }
        if node.has_mounted_descendant() {
            return Err(ApiError::bad_request(format!(
                "{device} is mounted (or has a mounted partition); unmount it first"
            )));
        }
        if let Some(root) = self.root_source()
            && (root == device || node.children.iter().any(|child| child.path == root))
        {
            return Err(ApiError::bad_request(format!(
                "{device} backs the root filesystem and cannot be formatted"
            )));
        }

        // `-F`/`-f` are the "this really is a fresh filesystem" switches, which is
        // exactly what the confirmation dialog in the UI obtained.
        let args: Vec<String> = match fs {
            "ext4" => vec!["-F".to_string(), device.to_string()],
            "xfs" => vec!["-f".to_string(), device.to_string()],
            _ => unreachable!("validated above"),
        };
        let program = format!("mkfs.{fs}");
        let output = self
            .runner
            .run(&program, &args)
            .map_err(|err| ApiError::unavailable(format!("cannot run {program}: {err}")))?;
        if !output.success() {
            return Err(ApiError::internal(format!(
                "{program} failed: {}",
                output.combined().trim()
            )));
        }
        Ok(format!("{device} formatted as {fs}"))
    }

    /// Mount `device` at `path`, optionally adding it to `/etc/fstab`.
    pub fn mount(&self, device: &str, path: &str, persist: bool) -> Result<String, ApiError> {
        validate_device_path(device)?;
        validate_mount_path(path)?;

        let devices = self.devices()?;
        let mut flat = Vec::new();
        for node in &devices {
            node.walk(&mut flat);
        }
        let node = flat
            .iter()
            .find(|node| node.path == device)
            .copied()
            .ok_or_else(|| ApiError::not_found(format!("{device} is not a block device")))?;
        if node.mountpoint.as_deref().is_some_and(|m| !m.is_empty()) {
            return Err(ApiError::bad_request(format!(
                "{device} is already mounted at {}",
                node.mountpoint.as_deref().unwrap_or("?")
            )));
        }
        let fstype = node.fstype.clone().filter(|fs| !fs.is_empty());

        std::fs::create_dir_all(path)
            .map_err(|err| ApiError::internal(format!("cannot create {path}: {err}")))?;

        let output = self
            .runner
            .call("mount", &[device, path])
            .map_err(|err| ApiError::unavailable(format!("cannot run mount: {err}")))?;
        if !output.success() {
            return Err(ApiError::internal(format!(
                "mount {device} {path} failed: {}",
                output.combined().trim()
            )));
        }

        if persist {
            // A persisted entry needs the filesystem type; without one the boot
            // would have to guess, which is how a missing fstab entry turns into
            // an emergency shell.
            let fstype = fstype.ok_or_else(|| {
                ApiError::bad_request(
                    "the device has no filesystem to record in /etc/fstab; mount without persist",
                )
            })?;
            self.append_fstab(device, path, &fstype)?;
        }
        Ok(format!("{device} mounted at {path}"))
    }

    /// Unmount whatever is at `target` (a mount point or a device path).
    pub fn unmount(&self, target: &str) -> Result<String, ApiError> {
        if target.trim().is_empty() {
            return Err(ApiError::bad_request("mount point is required"));
        }
        if is_protected_mountpoint(target) {
            return Err(ApiError::bad_request(format!(
                "{target} is a system mount point and cannot be unmounted here"
            )));
        }

        let output = self
            .runner
            .call("umount", &[target])
            .map_err(|err| ApiError::unavailable(format!("cannot run umount: {err}")))?;
        if !output.success() {
            return Err(ApiError::internal(format!(
                "umount {target} failed: {}",
                output.combined().trim()
            )));
        }
        Ok(format!("{target} unmounted"))
    }

    /// Append a boot-persistent entry for `device` if one is not already there.
    fn append_fstab(&self, device: &str, path: &str, fstype: &str) -> Result<(), ApiError> {
        let existing = std::fs::read_to_string(&self.fstab).unwrap_or_default();
        if existing
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(path))
        {
            // The mount point already has an entry; touching it again would
            // either duplicate it or change its options behind the operator.
            return Ok(());
        }

        // Prefer a UUID: device names are not stable across reboots, and an
        // fstab that points at the wrong disk is worse than none.
        let uuid = self
            .runner
            .call("blkid", &["-s", "UUID", "-o", "value", device])
            .ok()
            .filter(|output| output.success())
            .map(|output| output.stdout.trim().to_string())
            .filter(|value| !value.is_empty());
        let source = match uuid {
            Some(uuid) => format!("UUID={uuid}"),
            None => device.to_string(),
        };

        // Keep one backup of the original, then append with a marker comment.
        let backup = self.fstab.with_extension("stack-stack.bak");
        if !backup.exists()
            && let Ok(original) = std::fs::read(&self.fstab)
        {
            let _ = std::fs::write(&backup, original);
        }

        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&format!(
            "# added by stack-stack\n{source} {path} {fstype} defaults,nofail 0 2\n"
        ));
        std::fs::write(&self.fstab, next).map_err(|err| {
            ApiError::internal(format!("cannot update {}: {err}", self.fstab.display()))
        })
    }
}

/// Devices plus the context the UI needs to mark them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageState {
    pub devices: Vec<BlockDevice>,
    /// Device backing `/`, if it could be resolved.
    pub root_device: Option<String>,
    pub formats: Vec<String>,
}

/// `/dev/name`, name being a plain device component. This is the only shape
/// accepted anywhere in this module, so no path can reach a different BDF/device
/// node or a shell metacharacter.
pub fn validate_device_path(device: &str) -> Result<(), ApiError> {
    let Some(name) = device.strip_prefix("/dev/") else {
        return Err(ApiError::bad_request(format!(
            "{device:?} is not a /dev path"
        )));
    };
    let valid = !name.is_empty()
        && !name.contains('/')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(ApiError::bad_request(format!(
            "{device:?} is not a valid device path"
        )));
    }
    Ok(())
}

/// Absolute, no traversal, and under an allowed prefix.
pub fn validate_mount_path(path: &str) -> Result<(), ApiError> {
    if !path.starts_with('/') {
        return Err(ApiError::bad_request("mount point must be absolute"));
    }
    if path.contains("..") {
        return Err(ApiError::bad_request("mount point must not contain '..'"));
    }
    let allowed = ALLOWED_MOUNT_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix));
    if !allowed {
        return Err(ApiError::bad_request(format!(
            "mount point must be under one of {}",
            ALLOWED_MOUNT_PREFIXES.join(", ")
        )));
    }
    Ok(())
}

/// True for the mounts this page must never take down.
pub fn is_protected_mountpoint(path: &str) -> bool {
    let trimmed = path.trim_end_matches('/');
    let normalized = if trimmed.is_empty() { "/" } else { trimmed };
    PROTECTED_MOUNTS
        .iter()
        .any(|protected| normalized == *protected || normalized == protected.trim_end_matches('/'))
}

/// Parse `lsblk -J -b` output into the device tree.
pub fn parse_lsblk(json: &str) -> Result<Vec<BlockDevice>, ApiError> {
    let root: LsblkRoot = serde_json::from_str(json)
        .map_err(|err| ApiError::unavailable(format!("cannot read lsblk output: {err}")))?;
    Ok(root
        .blockdevices
        .iter()
        .map(node_to_device)
        .filter(|device| !device.name.is_empty())
        .collect())
}

fn node_to_device(node: &LsblkNode) -> BlockDevice {
    let name = node.name.clone();
    let path = node
        .path
        .clone()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| format!("/dev/{name}"));
    BlockDevice {
        name,
        path,
        kind: node.kind.clone().unwrap_or_default(),
        size_bytes: node.size.as_ref().map(size_bytes).unwrap_or(0),
        model: clean(node.model.clone()),
        fstype: clean(node.fstype.clone()),
        mountpoint: clean(node.mountpoint.clone()),
        label: clean(node.label.clone()),
        children: node.children.iter().map(node_to_device).collect(),
    }
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `lsblk -b` reports a number, but a string is accepted too.
fn size_bytes(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::Number(number) => number.as_u64().unwrap_or(0),
        serde_json::Value::String(text) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

#[derive(Deserialize)]
struct LsblkRoot {
    #[serde(default)]
    blockdevices: Vec<LsblkNode>,
}

#[derive(Deserialize)]
struct LsblkNode {
    name: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    size: Option<serde_json::Value>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fstype: Option<String>,
    #[serde(default)]
    mountpoint: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    children: Vec<LsblkNode>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A runner that answers with canned output per program.
    struct FakeRunner {
        replies: Mutex<Vec<(String, crate::proc::Output)>>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn new(replies: Vec<(&str, crate::proc::Output)>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(
                    replies
                        .into_iter()
                        .map(|(program, output)| (program.to_string(), output))
                        .collect(),
                ),
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    fn ok(stdout: &str) -> crate::proc::Output {
        crate::proc::Output {
            code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
            timed_out: false,
        }
    }

    fn fail(stderr: &str) -> crate::proc::Output {
        crate::proc::Output {
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.to_string(),
            timed_out: false,
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<crate::proc::Output, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((program.to_string(), args.to_vec()));
            let replies = self.replies.lock().expect("replies");
            // Replies are not consumed: `format` and `mount` each re-read the
            // device tree, so `lsblk` has to answer every call the same way.
            if let Some((_, output)) = replies.iter().find(|(name, _)| name == program) {
                return Ok(output.clone());
            }
            // `mount`, `umount`, `mkfs.*` and `blkid` succeed by default.
            Ok(ok(""))
        }
    }

    /// A disk with one mounted root partition and one free data disk.
    fn lsblk_sample() -> &'static str {
        r#"{
          "blockdevices": [
            {
              "name": "sda", "path": "/dev/sda", "type": "disk", "size": 1000204886016,
              "model": "WDC WD10EZEX", "fstype": null, "mountpoint": null, "label": null,
              "children": [
                {"name":"sda1","path":"/dev/sda1","type":"part","size":536870912000,
                 "fstype":"ext4","mountpoint":"/","label":"root"}
              ]
            },
            {
              "name": "sdb", "path": "/dev/sdb", "type": "disk", "size": 2000398934016,
              "model": "SAMSUNG HD204UI", "fstype": null, "mountpoint": null, "label": null,
              "children": []
            }
          ]
        }"#
    }

    fn context(replies: Vec<(&str, crate::proc::Output)>) -> (StorageContext, Arc<FakeRunner>) {
        let runner = FakeRunner::new(replies);
        let dir = std::env::temp_dir().join(format!(
            "ss-storage-{}-{}",
            std::process::id(),
            runner.calls.lock().expect("calls").len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = StorageContext::with_fstab(runner.clone(), dir.join("fstab"));
        (ctx, runner)
    }

    #[test]
    fn parses_the_device_tree() {
        let devices = parse_lsblk(lsblk_sample()).expect("parse");
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "sda");
        assert_eq!(devices[0].size_bytes, 1_000_204_886_016);
        assert_eq!(devices[0].model.as_deref(), Some("WDC WD10EZEX"));
        assert_eq!(devices[0].children[0].path, "/dev/sda1");
        assert_eq!(devices[0].children[0].mountpoint.as_deref(), Some("/"));
        assert!(devices[0].has_mounted_descendant());
        assert!(!devices[1].has_mounted_descendant());
    }

    #[test]
    fn device_paths_are_strictly_shaped() {
        for ok in ["/dev/sda", "/dev/sda1", "/dev/nvme0n1p2", "/dev/disk-x_1"] {
            assert!(validate_device_path(ok).is_ok(), "{ok} should pass");
        }
        for bad in [
            "",
            "/etc/passwd",
            "/dev/",
            "/dev/sda/../sdb",
            "/dev/../etc/passwd",
            "/dev/sd a",
            "/dev/sd;rm",
        ] {
            assert!(validate_device_path(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn mount_paths_stay_under_the_allowed_prefixes() {
        assert!(validate_mount_path("/mnt/disk1").is_ok());
        assert!(validate_mount_path("/srv/data").is_ok());
        assert!(validate_mount_path("/media/usb").is_ok());
        assert!(validate_mount_path("relative").is_err());
        assert!(validate_mount_path("/etc/evil").is_err());
        assert!(validate_mount_path("/mnt/../etc").is_err());
        assert!(validate_mount_path("/mnt").is_err());
    }

    #[test]
    fn system_mounts_are_protected() {
        for protected in ["/", "/boot", "/etc", "/var", "/usr"] {
            assert!(
                is_protected_mountpoint(protected),
                "{protected} should be protected"
            );
        }
        for allowed in ["/mnt/disk1", "/srv/data", "/media/usb"] {
            assert!(
                !is_protected_mountpoint(allowed),
                "{allowed} should be allowed"
            );
        }
    }

    #[test]
    fn format_refuses_a_mounted_device() {
        let (ctx, _) = context(vec![("lsblk", ok(lsblk_sample()))]);
        // sda1 is the root partition.
        let err = ctx.format("/dev/sda1", "ext4").expect_err("must refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
        // sda as a whole disk has a mounted child.
        let err = ctx.format("/dev/sda", "ext4").expect_err("must refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn format_refuses_an_unknown_filesystem() {
        let (ctx, _) = context(vec![("lsblk", ok(lsblk_sample()))]);
        let err = ctx.format("/dev/sdb", "ntfs").expect_err("must refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn format_runs_mkfs_for_a_free_disk() {
        let (ctx, runner) = context(vec![("lsblk", ok(lsblk_sample()))]);
        let message = ctx.format("/dev/sdb", "ext4").expect("format");
        assert!(message.contains("/dev/sdb"), "{message}");
        let calls = runner.calls.lock().expect("calls");
        assert!(
            calls.iter().any(|(program, args)| program == "mkfs.ext4"
                && args.first().map(String::as_str) == Some("-F")),
            "{calls:?}"
        );
    }

    #[test]
    fn mount_refuses_a_path_outside_the_prefixes() {
        let (ctx, _) = context(vec![("lsblk", ok(lsblk_sample()))]);
        let err = ctx
            .mount("/dev/sdb", "/etc/evil", false)
            .expect_err("refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn mount_rejects_a_device_that_is_already_mounted() {
        let (ctx, _) = context(vec![("lsblk", ok(lsblk_sample()))]);
        let err = ctx.mount("/dev/sda1", "/mnt/x", false).expect_err("refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn unmount_refuses_a_system_mount() {
        let (ctx, _) = context(vec![]);
        let err = ctx.unmount("/etc").expect_err("refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn mount_failure_surfaces_the_tool_error() {
        let (ctx, _) = context(vec![
            ("lsblk", ok(lsblk_sample())),
            ("mount", fail("mount: /mnt/x: unknown filesystem type")),
        ]);
        let err = ctx.mount("/dev/sdb", "/mnt/x", false).expect_err("fail");
        match err {
            ApiError::Internal(message) => assert!(message.contains("unknown filesystem")),
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}
