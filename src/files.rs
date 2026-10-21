//! File manager: guarded filesystem browsing and editing.
//!
//! The whole module is organised around one rule: **a user path is never used
//! until it has been proven to live inside a configured root**. `.`/`..` are
//! collapsed lexically first, then the path (for reads) or its parent (for
//! writes) is canonicalised so symlinks cannot be used to escape. Roots
//! themselves are only editable through the authenticated API.
//!
//! Everything here is synchronous and blocking; the HTTP layer wraps each call
//! in `spawn_blocking`. Archives shell out to `tar`/`unzip` (the same choice the
//! router made) with a reject-on-traversal filter over the entry names.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_LISTING_CAP: usize = 5000;
pub const DEFAULT_UPLOAD_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Largest text file returned inline by `read`.
pub const READ_CAP_BYTES: u64 = 2 * 1024 * 1024;
/// Largest body accepted by `save`.
pub const SAVE_CAP_BYTES: usize = 16 * 1024 * 1024;
/// Chunk size for streaming a download.
pub const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// The file-manager document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    /// Directories that may be written to. Reads are allowed anywhere reachable
    /// *inside* a root; `/etc` and other system paths are read-only because they
    /// are simply not listed here.
    pub roots: Vec<String>,
    pub show_hidden: bool,
    pub upload_max_bytes: u64,
    pub listing_cap: usize,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            roots: vec![
                "/var/lib/stack-stack".to_string(),
                "/srv".to_string(),
                "/home".to_string(),
            ],
            show_hidden: false,
            upload_max_bytes: DEFAULT_UPLOAD_MAX_BYTES,
            listing_cap: DEFAULT_LISTING_CAP,
        }
    }
}

impl FilesConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        if self.roots.is_empty() {
            problems.push(Problem {
                field: "roots".to_string(),
                message: "at least one writable root is required".to_string(),
            });
        }
        let mut seen: Vec<&str> = Vec::new();
        for (index, root) in self.roots.iter().enumerate() {
            let field = format!("roots[{index}]");
            let trimmed = root.trim();
            if trimmed.is_empty() || !trimmed.starts_with('/') {
                problems.push(Problem {
                    field,
                    message: "root must be an absolute path".to_string(),
                });
                continue;
            }
            if seen.contains(&trimmed) {
                problems.push(Problem {
                    field,
                    message: format!("{trimmed:?} is listed twice"),
                });
            }
            seen.push(trimmed);
        }
        if self.listing_cap == 0 || self.listing_cap > 100_000 {
            problems.push(Problem {
                field: "listing_cap".to_string(),
                message: "listing cap must be between 1 and 100000".to_string(),
            });
        }
        if self.upload_max_bytes == 0 {
            problems.push(Problem {
                field: "upload_max_bytes".to_string(),
                message: "upload limit must be greater than zero".to_string(),
            });
        }
        problems
    }
}

/// Shared runtime handles for the file API.
#[derive(Clone)]
pub struct FilesContext {
    pub config_path: PathBuf,
    pub runner: Arc<dyn Runner>,
}

impl FilesContext {
    pub fn new(config_path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self {
            config_path,
            runner,
        }
    }
}

pub fn load(path: &Path) -> (FilesConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<FilesConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                FilesConfig::default(),
                Some(format!("{} is not valid files JSON: {err}", path.display())),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (FilesConfig::default(), None),
        Err(err) => (
            FilesConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &Path, config: &FilesConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise files config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

// ---------------------------------------------------------------------------
// Path guard
// ---------------------------------------------------------------------------

/// Collapse `.` and `..` lexically, without touching the filesystem. `..` at the
/// root of an absolute path is dropped rather than escaping.
pub fn lexical(path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // Relative `..` above the start: keep it so the result is not
                    // silently reinterpreted as an absolute path.
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

struct Root {
    /// The value the operator typed, for messages.
    display: PathBuf,
    /// Its resolved real path, for containment checks.
    canonical: PathBuf,
}

/// The set of writable roots, canonicalised once per request.
pub struct Guard {
    roots: Vec<Root>,
}

impl Guard {
    pub fn new(roots: &[String]) -> Self {
        let roots = roots
            .iter()
            .filter_map(|root| {
                let display = lexical(root.trim());
                let canonical = std::fs::canonicalize(&display).ok()?;
                Some(Root { display, canonical })
            })
            .collect();
        Self { roots }
    }

    pub fn roots(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|root| root.display.to_string_lossy().to_string())
            .collect()
    }

    fn contains(&self, candidate: &Path) -> Option<&Root> {
        self.roots
            .iter()
            .find(|root| candidate.starts_with(&root.canonical))
    }

    /// Resolve an existing path for reading. Canonicalises the path itself so a
    /// symlink pointing outside the roots is refused.
    pub fn read(&self, raw: &str) -> Result<PathBuf, ApiError> {
        let wanted = lexical(raw);
        let canonical = std::fs::canonicalize(&wanted)
            .map_err(|err| ApiError::not_found(format!("{}: {err}", wanted.display())))?;
        if self.contains(&canonical).is_none() {
            return Err(outside(&canonical));
        }
        Ok(canonical)
    }

    /// True when `path` is one of the configured roots exactly.
    pub fn is_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| root.canonical == path)
    }

    /// Resolve a path for writing where the target may or may not exist. The
    /// parent is canonicalised; the target's final name is then reattached.
    pub fn write(&self, raw: &str) -> Result<PathBuf, ApiError> {
        let wanted = lexical(raw);
        let parent = wanted
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| ApiError::bad_request("a path with a parent directory is required"))?;
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|err| ApiError::not_found(format!("{}: {err}", parent.display())))?;
        if self.contains(&canonical_parent).is_none() {
            return Err(outside(&canonical_parent));
        }
        // `..` was already collapsed; the final component must be a plain name.
        let name = wanted
            .file_name()
            .ok_or_else(|| ApiError::bad_request("a file name is required"))?;
        Ok(canonical_parent.join(name))
    }

    /// Resolve a path that must not exist yet (new file, new directory, rename
    /// target).
    pub fn create(&self, raw: &str) -> Result<PathBuf, ApiError> {
        let target = self.write(raw)?;
        if target.exists() {
            return Err(ApiError::bad_request(format!(
                "{} already exists",
                target.display()
            )));
        }
        Ok(target)
    }
}

fn outside(path: &Path) -> ApiError {
    ApiError::bad_request(format!(
        "{} is outside the configured file roots",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    /// Path as the client should pass it back.
    pub path: String,
    /// `file`, `dir`, `symlink` or `other`.
    pub kind: String,
    pub size: u64,
    pub modified_ms: Option<u64>,
    /// `rwxr-xr-x` style.
    pub mode: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

pub fn list(config: &FilesConfig, guard: &Guard, raw: &str) -> Result<Listing, ApiError> {
    let dir = guard.read(raw)?;
    let meta = std::fs::metadata(&dir)
        .map_err(|err| ApiError::internal(format!("{}: {err}", dir.display())))?;
    if !meta.is_dir() {
        return Err(ApiError::bad_request(format!(
            "{} is not a directory",
            dir.display()
        )));
    }

    let owners = AccountTable::load();
    let read_dir = std::fs::read_dir(&dir)
        .map_err(|err| ApiError::internal(format!("cannot list {}: {err}", dir.display())))?;

    let mut entries: Vec<Entry> = Vec::new();
    let mut truncated = false;
    for item in read_dir.flatten() {
        let name = item.file_name().to_string_lossy().to_string();
        if !config.show_hidden && name.starts_with('.') {
            continue;
        }
        if entries.len() >= config.listing_cap {
            truncated = true;
            break;
        }
        entries.push(entry(&item.path(), &name, &owners));
    }

    entries.sort_by(|a, b| {
        let a_dir = a.kind == "dir";
        let b_dir = b.kind == "dir";
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let parent = dir
        .parent()
        .filter(|parent| guard.contains(parent).is_some())
        .map(|parent| parent.to_string_lossy().to_string());

    Ok(Listing {
        path: dir.to_string_lossy().to_string(),
        parent,
        entries,
        truncated,
    })
}

fn entry(path: &Path, name: &str, owners: &AccountTable) -> Entry {
    // `symlink_metadata` never follows the link, so a dangling symlink still
    // shows up (and reports its target) instead of vanishing.
    let link = std::fs::symlink_metadata(path).ok();
    let is_link = link
        .as_ref()
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    let meta = std::fs::metadata(path).ok().or(link);

    let kind = match &meta {
        Some(meta) if is_link => "symlink",
        Some(meta) if meta.is_dir() => "dir",
        Some(meta) if meta.is_file() => "file",
        _ => "other",
    };

    let symlink_target = if is_link {
        std::fs::read_link(path)
            .ok()
            .map(|target| target.to_string_lossy().to_string())
    } else {
        None
    };

    Entry {
        name: name.to_string(),
        path: path.to_string_lossy().to_string(),
        kind: kind.to_string(),
        size: meta.as_ref().map(|meta| meta.len()).unwrap_or(0),
        modified_ms: meta
            .as_ref()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64),
        mode: meta.as_ref().map(mode_string),
        owner: meta
            .as_ref()
            .and_then(uid_of)
            .and_then(|uid| owners.user(uid)),
        group: meta
            .as_ref()
            .and_then(gid_of)
            .and_then(|gid| owners.group(gid)),
        symlink_target,
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileText {
    pub path: String,
    pub size: u64,
    pub truncated: bool,
    pub mime: String,
    pub content: String,
}

pub fn read_text(guard: &Guard, raw: &str) -> Result<FileText, ApiError> {
    let path = guard.read(raw)?;
    let meta = std::fs::metadata(&path)
        .map_err(|err| ApiError::internal(format!("{}: {err}", path.display())))?;
    if !meta.is_file() {
        return Err(ApiError::bad_request(format!(
            "{} is not a regular file",
            path.display()
        )));
    }

    let bytes = std::fs::read(&path)
        .map_err(|err| ApiError::internal(format!("cannot read {}: {err}", path.display())))?;
    let truncated = bytes.len() as u64 > READ_CAP_BYTES;
    let slice = &bytes[..bytes.len().min(READ_CAP_BYTES as usize)];

    if slice.iter().take(8192).any(|byte| *byte == 0) {
        return Err(ApiError::bad_request(
            "that file looks binary; download it instead of editing",
        ));
    }

    Ok(FileText {
        path: path.to_string_lossy().to_string(),
        size: meta.len(),
        truncated,
        mime: mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string(),
        content: String::from_utf8_lossy(slice).into_owned(),
    })
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

pub fn mkdir(guard: &Guard, raw: &str) -> Result<String, ApiError> {
    let target = guard.create(raw)?;
    std::fs::create_dir(&target)
        .map_err(|err| ApiError::internal(format!("cannot create {}: {err}", target.display())))?;
    Ok(target.to_string_lossy().to_string())
}

pub fn create_file(guard: &Guard, raw: &str) -> Result<String, ApiError> {
    let target = guard.create(raw)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|err| ApiError::internal(format!("cannot create {}: {err}", target.display())))?;
    Ok(target.to_string_lossy().to_string())
}

pub fn save_file(guard: &Guard, raw: &str, content: &str) -> Result<u64, ApiError> {
    if content.len() > SAVE_CAP_BYTES {
        return Err(ApiError::bad_request(format!(
            "content exceeds the {} byte save limit",
            SAVE_CAP_BYTES
        )));
    }
    let target = guard.write(raw)?;
    write_atomic(&target, content.as_bytes())
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", target.display())))?;
    Ok(content.len() as u64)
}

pub fn rename(guard: &Guard, from: &str, to: &str) -> Result<String, ApiError> {
    let source = guard.read(from)?;
    let target = guard.create(to)?;
    std::fs::rename(&source, &target)
        .map_err(|err| ApiError::internal(format!("cannot rename: {err}")))?;
    Ok(target.to_string_lossy().to_string())
}

pub fn delete(guard: &Guard, raw: &str, recursive: bool) -> Result<(), ApiError> {
    let target = guard.read(raw)?;
    // Refuse to delete the root itself: that is never what the operator meant.
    if guard.is_root(&target) {
        return Err(ApiError::bad_request("refusing to delete a file root"));
    }
    let meta = std::fs::symlink_metadata(&target)
        .map_err(|err| ApiError::internal(format!("{}: {err}", target.display())))?;
    if meta.is_dir() {
        if !recursive {
            let empty = std::fs::read_dir(&target)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                return Err(ApiError::bad_request(
                    "the directory is not empty; confirm a recursive delete",
                ));
            }
            std::fs::remove_dir(&target)
                .map_err(|err| ApiError::internal(format!("cannot remove: {err}")))?;
        } else {
            std::fs::remove_dir_all(&target)
                .map_err(|err| ApiError::internal(format!("cannot remove: {err}")))?;
        }
    } else {
        std::fs::remove_file(&target)
            .map_err(|err| ApiError::internal(format!("cannot remove: {err}")))?;
    }
    Ok(())
}

/// Parse and apply an octal mode such as `644` or `0755`.
pub fn chmod(guard: &Guard, raw: &str, mode: &str) -> Result<(), ApiError> {
    let target = guard.read(raw)?;
    let value = parse_mode(mode)?;
    apply_mode(&target, value)
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) -> Result<(), ApiError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|err| ApiError::internal(format!("cannot chmod: {err}")))
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) -> Result<(), ApiError> {
    Err(ApiError::unavailable("chmod requires a unix host"))
}

pub fn parse_mode(mode: &str) -> Result<u32, ApiError> {
    let trimmed = mode.trim().trim_start_matches("0o");
    if trimmed.is_empty() || trimmed.len() > 4 || !trimmed.chars().all(|c| ('0'..='7').contains(&c))
    {
        return Err(ApiError::bad_request(format!(
            "{mode:?} is not an octal mode (e.g. 644 or 0755)"
        )));
    }
    u32::from_str_radix(trimmed, 8)
        .map_err(|_| ApiError::bad_request(format!("{mode:?} is not an octal mode")))
}

/// Change ownership by name; `None` leaves that half unchanged.
pub fn chown(
    guard: &Guard,
    raw: &str,
    user: Option<&str>,
    group: Option<&str>,
) -> Result<(), ApiError> {
    let target = guard.read(raw)?;
    let accounts = AccountTable::load();
    let uid = match user.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => Some(
            accounts
                .uid(name)
                .ok_or_else(|| ApiError::bad_request(format!("no such user: {name}")))?,
        ),
        None => None,
    };
    let gid = match group.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => Some(
            accounts
                .gid(name)
                .ok_or_else(|| ApiError::bad_request(format!("no such group: {name}")))?,
        ),
        None => None,
    };
    if uid.is_none() && gid.is_none() {
        return Err(ApiError::bad_request("a user or a group is required"));
    }
    apply_owner(&target, uid, gid)
}

#[cfg(unix)]
fn apply_owner(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<(), ApiError> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ApiError::bad_request("path contains a NUL byte"))?;
    // -1 means "leave unchanged".
    let rc = unsafe {
        libc::chown(
            c_path.as_ptr(),
            uid.map(|value| value as libc::uid_t).unwrap_or(u32::MAX),
            gid.map(|value| value as libc::gid_t).unwrap_or(u32::MAX),
        )
    };
    if rc != 0 {
        return Err(ApiError::internal(format!(
            "cannot chown: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_owner(_path: &Path, _uid: Option<u32>, _gid: Option<u32>) -> Result<(), ApiError> {
    Err(ApiError::unavailable("chown requires a unix host"))
}

// ---------------------------------------------------------------------------
// Accounts (owner/group display and pickers)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct AccountTable {
    users: Vec<(u32, String)>,
    groups: Vec<(u32, String)>,
}

impl AccountTable {
    pub fn load() -> Self {
        let mut table = AccountTable::default();
        for line in std::fs::read_to_string("/etc/passwd")
            .unwrap_or_default()
            .lines()
        {
            let mut fields = line.split(':');
            if let (Some(name), Some(_), Some(uid)) = (fields.next(), fields.next(), fields.next())
                && let Ok(uid) = uid.parse::<u32>()
            {
                table.users.push((uid, name.to_string()));
            }
        }
        for line in std::fs::read_to_string("/etc/group")
            .unwrap_or_default()
            .lines()
        {
            let mut fields = line.split(':');
            if let (Some(name), Some(_), Some(gid)) = (fields.next(), fields.next(), fields.next())
                && let Ok(gid) = gid.parse::<u32>()
            {
                table.groups.push((gid, name.to_string()));
            }
        }
        table
    }

    pub fn user(&self, uid: u32) -> Option<String> {
        self.users
            .iter()
            .find(|(id, _)| *id == uid)
            .map(|(_, name)| name.clone())
    }

    pub fn group(&self, gid: u32) -> Option<String> {
        self.groups
            .iter()
            .find(|(id, _)| *id == gid)
            .map(|(_, name)| name.clone())
    }

    pub fn uid(&self, name: &str) -> Option<u32> {
        self.users
            .iter()
            .find(|(_, entry)| entry == name)
            .map(|(id, _)| *id)
    }

    pub fn gid(&self, name: &str) -> Option<u32> {
        self.groups
            .iter()
            .find(|(_, entry)| entry == name)
            .map(|(id, _)| *id)
    }

    pub fn usernames(&self) -> Vec<String> {
        self.users.iter().map(|(_, name)| name.clone()).collect()
    }

    pub fn groupnames(&self) -> Vec<String> {
        self.groups.iter().map(|(_, name)| name.clone()).collect()
    }
}

// ---------------------------------------------------------------------------
// Archives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    TarZst,
    TarXz,
    TarBz2,
}

impl ArchiveKind {
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".zip") {
            Some(Self::Zip)
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
            Some(Self::TarZst)
        } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
            Some(Self::TarXz)
        } else if lower.ends_with(".tar.bz2") {
            Some(Self::TarBz2)
        } else if lower.ends_with(".tar") {
            Some(Self::Tar)
        } else {
            None
        }
    }
}

/// True when an archive entry name is safe to extract: relative, no `..`, no
/// absolute prefix and no Windows-style drive or backslash trick.
pub fn safe_entry_name(name: &str) -> bool {
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains(':') {
        return false;
    }
    Path::new(&normalized)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Extract an archive into a new sibling directory named after it.
pub fn extract(guard: &Guard, runner: &dyn Runner, raw: &str) -> Result<String, ApiError> {
    let archive = guard.read(raw)?;
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let kind = ArchiveKind::from_name(&name)
        .ok_or_else(|| ApiError::bad_request("unsupported archive type"))?;

    let stem = name
        .split(".tar")
        .next()
        .unwrap_or(&name)
        .trim_end_matches(".zip")
        .trim_end_matches(".tgz")
        .trim_end_matches(".targz")
        .to_string();
    let stem = if stem.is_empty() {
        "extracted".to_string()
    } else {
        stem
    };

    let dest = guard.create(&format!(
        "{}/{}",
        archive
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string()),
        stem
    ))?;
    std::fs::create_dir(&dest)
        .map_err(|err| ApiError::internal(format!("cannot create {}: {err}", dest.display())))?;

    // List the entries first and refuse outright on a single traversal attempt,
    // so nothing untrusted is ever written even partially.
    let listing = list_archive(runner, &archive, kind)?;
    if let Some(bad) = listing.iter().find(|entry| !safe_entry_name(entry)) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(ApiError::bad_request(format!(
            "archive refuses to extract: unsafe entry {bad:?}"
        )));
    }

    let archive_s = archive.to_string_lossy().to_string();
    let dest_s = dest.to_string_lossy().to_string();
    let args: Vec<String> = match kind {
        ArchiveKind::Zip => vec![
            "unzip".to_string(),
            "-o".to_string(),
            "-q".to_string(),
            archive_s.clone(),
            "-d".to_string(),
            dest_s.clone(),
        ],
        ArchiveKind::Tar => tar_args("", &archive_s, &dest_s),
        ArchiveKind::TarGz => tar_args("z", &archive_s, &dest_s),
        ArchiveKind::TarZst => tar_args("--zstd", &archive_s, &dest_s),
        ArchiveKind::TarXz => tar_args("J", &archive_s, &dest_s),
        ArchiveKind::TarBz2 => tar_args("j", &archive_s, &dest_s),
    };

    let output = if kind == ArchiveKind::Zip {
        runner.run_timeout("unzip", &args, Duration::from_secs(1800))
    } else {
        runner.run_timeout("tar", &args, Duration::from_secs(1800))
    }
    .map_err(|err| {
        let _ = std::fs::remove_dir_all(&dest);
        ApiError::unavailable(format!("archive tool is not available: {err}"))
    })?;

    if !output.success() {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(ApiError::internal(format!(
            "extraction failed: {}",
            output.combined().trim()
        )));
    }
    Ok(dest_s)
}

fn tar_args(compression: &str, archive: &str, dest: &str) -> Vec<String> {
    let mut args = vec!["-x".to_string()];
    if !compression.is_empty() {
        args.push(format!("-{compression}"));
    }
    args.push("-f".to_string());
    args.push(archive.to_string());
    args.push("-C".to_string());
    args.push(dest.to_string());
    args
}

/// Entry names inside an archive, via the external tool.
pub fn list_archive(
    runner: &dyn Runner,
    archive: &Path,
    kind: ArchiveKind,
) -> Result<Vec<String>, ApiError> {
    let archive_s = archive.to_string_lossy().to_string();
    let (program, args) = match kind {
        ArchiveKind::Zip => ("unzip", vec!["-Z1".to_string(), archive_s]),
        _ => ("tar", vec!["-tf".to_string(), archive_s]),
    };
    let output = runner
        .run_timeout(program, &args, Duration::from_secs(300))
        .map_err(|err| ApiError::unavailable(format!("{program} is not available: {err}")))?;
    if !output.success() {
        return Err(ApiError::internal(format!(
            "cannot list the archive: {}",
            output.combined().trim()
        )));
    }
    Ok(output
        .stdout
        .lines()
        .map(|line| line.trim().trim_end_matches('/').to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

/// Build the `tar` argv that creates a `.tar.zst` from `sources` in `base`.
pub fn create_tar_args(base: &str, sources: &[String], target: &str) -> Vec<String> {
    let mut args = vec![
        "--zstd".to_string(),
        "-cf".to_string(),
        target.to_string(),
        "-C".to_string(),
        base.to_string(),
    ];
    args.extend(sources.iter().cloned());
    args
}

// ---------------------------------------------------------------------------
// Metadata helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn uid_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.uid())
}

#[cfg(not(unix))]
fn uid_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn gid_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.gid())
}

#[cfg(not(unix))]
fn gid_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn mode_string(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    let mode = meta.mode();
    let mut out = String::with_capacity(10);
    out.push(if meta.is_dir() { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        out.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

#[cfg(not(unix))]
fn mode_string(_meta: &std::fs::Metadata) -> String {
    "-".to_string()
}

/// Write an uploaded body to `dest_dir/name` via a `.part` file, then rename.
///
/// `reader` is consumed by the caller's multipart stream; this helper stays
/// blocking and is called from `spawn_blocking`.
pub fn finish_upload(dest_dir: &Path, name: &str, part_path: &Path) -> Result<String, ApiError> {
    let final_path = dest_dir.join(name);
    if final_path.exists() {
        return Err(ApiError::bad_request(format!(
            "{} already exists",
            final_path.display()
        )));
    }
    std::fs::rename(part_path, &final_path).map_err(|err| {
        let _ = std::fs::remove_file(part_path);
        ApiError::internal(format!("cannot finish upload: {err}"))
    })?;
    Ok(final_path.to_string_lossy().to_string())
}

/// Sanitise a client-supplied upload file name to a single path component.
pub fn sanitize_filename(raw: &str) -> Result<String, ApiError> {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('.');
    if name.is_empty() || name == "." || name == ".." || name.contains('\u{0}') {
        return Err(ApiError::bad_request("invalid file name"));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ss-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn guard_for(root: &Path) -> Guard {
        Guard::new(&[root.to_string_lossy().to_string()])
    }

    #[test]
    fn lexical_collapses_dots_and_cannot_escape_the_root() {
        assert_eq!(lexical("/a/b/../c"), PathBuf::from("/a/c"));
        assert_eq!(lexical("/a/./b/"), PathBuf::from("/a/b"));
        // `..` above the root is dropped, never escapes.
        assert_eq!(lexical("/../../etc/passwd"), PathBuf::from("/etc/passwd"));
    }

    #[test]
    fn guard_refuses_paths_outside_a_root() {
        let root = temp_root("guard");
        let guard = guard_for(&root);
        let outside = temp_root("guard-outside");

        assert!(guard.write(&format!("{}/ok.txt", root.display())).is_ok());
        assert!(
            guard
                .write(&format!("{}/x.txt", outside.display()))
                .is_err(),
            "a sibling directory must be rejected"
        );
        // Traversal back to a parent is rejected.
        assert!(
            guard
                .write(&format!("{}/../escape.txt", root.display()))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn guard_refuses_a_symlink_that_escapes() {
        let root = temp_root("symlink-root");
        let outside = temp_root("symlink-target");
        std::fs::write(outside.join("secret.txt"), "top secret").expect("write");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link")).expect("symlink");

        let guard = guard_for(&root);
        assert!(
            guard.read(&format!("{}/link", root.display())).is_err(),
            "a symlink out of the root must be refused"
        );
    }

    #[test]
    fn listing_sorts_directories_first_and_hides_dotfiles() {
        let root = temp_root("listing");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("b.txt"), "b").expect("write");
        std::fs::write(root.join("a.txt"), "a").expect("write");
        std::fs::write(root.join(".hidden"), "h").expect("write");

        let config = FilesConfig {
            roots: vec![root.to_string_lossy().to_string()],
            ..FilesConfig::default()
        };
        let guard = guard_for(&root);
        let listing = list(&config, &guard, &root.to_string_lossy()).expect("list");

        let names: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["sub", "a.txt", "b.txt"]);
        assert!(listing.parent.is_none(), "the root has no reachable parent");

        let show = FilesConfig {
            show_hidden: true,
            ..config
        };
        let listing = list(&show, &guard, &root.to_string_lossy()).expect("list");
        assert!(listing.entries.iter().any(|entry| entry.name == ".hidden"));
    }

    #[test]
    fn read_text_refuses_binary() {
        let root = temp_root("binary");
        std::fs::write(root.join("bin"), [0u8, 1, 2, 3]).expect("write");
        let guard = guard_for(&root);
        assert!(
            read_text(&guard, &format!("{}/bin", root.display())).is_err(),
            "binary content must be refused for the editor"
        );
    }

    #[test]
    fn save_then_read_round_trips() {
        let root = temp_root("save");
        let guard = guard_for(&root);
        let path = format!("{}/note.txt", root.display());
        save_file(&guard, &path, "hello").expect("save");
        let text = read_text(&guard, &path).expect("read");
        assert_eq!(text.content, "hello");
        assert_eq!(text.size, 5);
    }

    #[test]
    fn delete_requires_recursion_for_non_empty_directories() {
        let root = temp_root("delete");
        std::fs::create_dir_all(root.join("dir")).expect("mkdir");
        std::fs::write(root.join("dir/file"), "x").expect("write");
        let guard = guard_for(&root);
        let dir = format!("{}/dir", root.display());
        assert!(
            delete(&guard, &dir, false).is_err(),
            "non-empty needs recursion"
        );
        delete(&guard, &dir, true).expect("recursive delete");
        assert!(!Path::new(&dir).exists());
    }

    #[test]
    fn delete_refuses_a_root() {
        let root = temp_root("root-delete");
        let guard = guard_for(&root);
        assert!(delete(&guard, &root.to_string_lossy(), true).is_err());
    }

    #[test]
    fn mode_parsing_is_strict_octal() {
        assert_eq!(parse_mode("644").expect("mode"), 0o644);
        assert_eq!(parse_mode("0755").expect("mode"), 0o755);
        for bad in ["", "999", "rwx", "12345"] {
            assert!(parse_mode(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn archive_kinds_are_recognised_by_extension() {
        assert_eq!(ArchiveKind::from_name("a.zip"), Some(ArchiveKind::Zip));
        assert_eq!(ArchiveKind::from_name("a.tar.gz"), Some(ArchiveKind::TarGz));
        assert_eq!(
            ArchiveKind::from_name("a.tar.zst"),
            Some(ArchiveKind::TarZst)
        );
        assert_eq!(ArchiveKind::from_name("a.tar.xz"), Some(ArchiveKind::TarXz));
        assert_eq!(ArchiveKind::from_name("a.tgz"), Some(ArchiveKind::TarGz));
        assert_eq!(ArchiveKind::from_name("a.txt"), None);
    }

    #[test]
    fn entry_name_filter_blocks_traversal() {
        assert!(safe_entry_name("a/b/c.txt"));
        assert!(safe_entry_name("./a.txt"));
        for bad in [
            "../etc/passwd",
            "/etc/passwd",
            "..\\windows",
            "a/../../b",
            "C:/x",
        ] {
            assert!(!safe_entry_name(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn extract_refuses_an_archive_with_a_traversal_entry() {
        let root = temp_root("extract-bad");
        std::fs::write(root.join("evil.tar"), "not really a tar").expect("write");
        let guard = guard_for(&root);
        let runner = FakeRunner::new().respond("tar", Some(0), "../escape\nok.txt\n", "");
        let err = extract(&guard, &runner, &format!("{}/evil.tar", root.display()))
            .expect_err("traversal must be refused");
        assert!(err.to_string().contains("unsafe entry"), "{err}");
    }

    #[test]
    fn tar_args_select_the_right_compression() {
        let args = create_tar_args("/srv", &["docs".to_string()], "/srv/docs.tar.zst");
        assert_eq!(args[0], "--zstd");
        assert!(args.contains(&"-C".to_string()));
        assert!(args.contains(&"/srv".to_string()));
    }

    #[test]
    fn filename_sanitising_strips_paths() {
        assert_eq!(
            sanitize_filename("../../etc/passwd").expect("name"),
            "passwd"
        );
        assert_eq!(sanitize_filename("a/b\\c.txt").expect("name"), "c.txt");
        assert!(sanitize_filename(".").is_err());
        assert!(sanitize_filename("").is_err());
    }

    #[test]
    fn default_config_is_valid() {
        assert!(FilesConfig::default().validate().is_empty());
    }
}
