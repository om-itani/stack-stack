//! Custom guest images: fetch from a URL, import from a host path, and manage
//! the downloaded cache.
//!
//! This is the escape hatch for anything the shipped catalog does not cover.
//! Both paths end in the same place, the images directory, and both validate the
//! result with `qemu-img info` before it is offered to the VM form, so a
//! truncated download or a wrong file is rejected here rather than at boot.
//!
//! Nothing is downloaded with a new dependency: `curl` (or `wget`) on the shared
//! runner does the transfer, exactly like the cloud-image path.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::runner::Runner;

/// Extensions accepted as a guest image.
const IMAGE_EXTENSIONS: [&str; 5] = ["iso", "img", "qcow2", "qcow", "raw"];

/// A generous ceiling for one download. Cloud images and ISOs are large; this is
/// only here so a wedged transfer cannot hang forever.
const DOWNLOAD_TIMEOUT_S: u64 = 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedImage {
    pub name: String,
    pub size_bytes: u64,
    /// Modification time, Unix seconds.
    pub modified: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportResult {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// Download `url` into the images directory under `name` (or a name derived from
/// the URL). Returns once the file exists and `qemu-img` recognises it.
pub fn fetch(
    images_dir: &Path,
    runner: &dyn Runner,
    url: &str,
    name: Option<&str>,
) -> Result<ImportResult, ApiError> {
    validate_url(url)?;
    let name = match name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => sanitize_name(name).ok_or_else(|| {
            ApiError::bad_request(format!(
                "{name:?} is not a usable file name (expected something like kali.iso)"
            ))
        })?,
        None => name_from_url(url).ok_or_else(|| {
            ApiError::bad_request(
                "cannot derive a file name from that URL; pass one ending in .iso or .qcow2",
            )
        })?,
    };

    std::fs::create_dir_all(images_dir).map_err(|err| {
        ApiError::internal(format!("cannot create {}: {err}", images_dir.display()))
    })?;
    let target = images_dir.join(&name);

    // Write to a `.part` file and rename, so a half-downloaded image is never
    // visible to the create path.
    let partial = images_dir.join(format!(".{name}.part"));
    let args = vec![
        "-fL".to_string(),
        "--max-time".to_string(),
        DOWNLOAD_TIMEOUT_S.to_string(),
        "-o".to_string(),
        partial.to_string_lossy().into_owned(),
        url.to_string(),
    ];
    let output = runner
        .run_timeout(
            "curl",
            &args,
            std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_S + 60),
        )
        .map_err(|err| ApiError::unavailable(format!("cannot run curl: {err}")))?;
    if !output.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(ApiError::internal(format!(
            "download failed: {}",
            output.combined().trim()
        )));
    }

    // The transfer succeeded; a file that qemu-img cannot read is not an image.
    if let Err(err) = recognize(runner, &partial) {
        let _ = std::fs::remove_file(&partial);
        return Err(err);
    }

    std::fs::rename(&partial, &target)
        .map_err(|err| ApiError::internal(format!("cannot move the download into place: {err}")))?;
    Ok(result_for(&target, name))
}

/// Copy an image that already exists on this host into the cache.
pub fn import(
    images_dir: &Path,
    runner: &dyn Runner,
    source: &str,
) -> Result<ImportResult, ApiError> {
    let source = source.trim();
    if !source.starts_with('/') {
        return Err(ApiError::bad_request(
            "the host path must be absolute, for example /home/you/kali.iso",
        ));
    }
    let source_path = Path::new(source);
    let metadata = std::fs::metadata(source_path)
        .map_err(|err| ApiError::bad_request(format!("cannot read {source}: {err}")))?;
    if !metadata.is_file() {
        return Err(ApiError::bad_request(format!("{source} is not a file")));
    }

    let name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(sanitize_name)
        .ok_or_else(|| ApiError::bad_request(format!("{source} has no usable file name")))?;

    std::fs::create_dir_all(images_dir).map_err(|err| {
        ApiError::internal(format!("cannot create {}: {err}", images_dir.display()))
    })?;
    let target = images_dir.join(&name);

    // Copying the cache onto itself would truncate the file.
    if let (Ok(a), Ok(b)) = (source_path.canonicalize(), images_dir.canonicalize())
        && a.parent() == Some(b.as_path())
    {
        return Err(ApiError::bad_request(
            "that file is already in the images directory",
        ));
    }
    if target.exists() {
        return Err(ApiError::bad_request(format!(
            "{name} is already in the images directory; delete it first or rename the source"
        )));
    }

    recognize(runner, source_path)?;

    let partial = images_dir.join(format!(".{name}.part"));
    std::fs::copy(source_path, &partial)
        .map_err(|err| ApiError::internal(format!("cannot copy {source}: {err}")))?;
    std::fs::rename(&partial, &target)
        .map_err(|err| ApiError::internal(format!("cannot move the copy into place: {err}")))?;
    Ok(result_for(&target, name))
}

/// Every image file in the cache, newest first.
pub fn list(images_dir: &Path) -> Vec<CachedImage> {
    let Ok(entries) = std::fs::read_dir(images_dir) else {
        return Vec::new();
    };
    let mut images: Vec<CachedImage> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !is_image_name(&name) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some(CachedImage {
                name,
                size_bytes: metadata.len(),
                modified: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0),
            })
        })
        .collect();
    images.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.name.cmp(&b.name)));
    images
}

/// Delete one cached image. The name is validated as a plain file name, so it
/// cannot reach outside the images directory.
pub fn remove(images_dir: &Path, name: &str) -> Result<(), ApiError> {
    let name = sanitize_name(name).ok_or_else(|| ApiError::bad_request("invalid image name"))?;
    let path = images_dir.join(&name);
    if !path.is_file() {
        return Err(ApiError::not_found(format!(
            "{name} is not in the images directory"
        )));
    }
    std::fs::remove_file(&path)
        .map_err(|err| ApiError::internal(format!("cannot delete {name}: {err}")))
}

/// Run `qemu-img info` and insist that it recognised the file.
fn recognize(runner: &dyn Runner, path: &Path) -> Result<(), ApiError> {
    let output = runner
        .call(
            "qemu-img",
            &["info", "--output=json", &path.display().to_string()],
        )
        .map_err(|err| ApiError::unavailable(format!("cannot run qemu-img: {err}")))?;
    if !output.success() {
        return Err(ApiError::bad_request(format!(
            "{} is not a disk image qemu-img can read: {}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("the file"),
            output.combined().trim()
        )));
    }
    Ok(())
}

fn result_for(path: &Path, name: String) -> ImportResult {
    ImportResult {
        name,
        path: path.to_string_lossy().into_owned(),
        size_bytes: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
    }
}

/// Only http(s), so a stored URL cannot become a `file://` read.
pub fn validate_url(url: &str) -> Result<(), ApiError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(ApiError::bad_request("url is required"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ApiError::bad_request(
            "url must start with http:// or https://",
        ));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::bad_request("url must not contain whitespace"));
    }
    Ok(())
}

/// The file name to store a URL under: its last path segment, without the query.
pub fn name_from_url(url: &str) -> Option<String> {
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let last = path.rsplit('/').find(|segment| !segment.is_empty())?;
    sanitize_name(last)
}

/// A plain `name.ext` with an image extension, or `None`.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() || name.len() > 128 || name.contains("..") {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return None;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    {
        return None;
    }
    is_image_name(name).then(|| name.to_string())
}

/// True when the name carries an extension this feature accepts.
pub fn is_image_name(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    IMAGE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
}

/// Best-effort extension for the VM form's disk format field.
#[cfg(test)]
pub fn format_hint(name: &str) -> &'static str {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
    {
        Some(ext) if ext == "iso" => "raw",
        _ => "qcow2",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_must_be_plain_and_have_an_image_extension() {
        assert_eq!(sanitize_name("kali.iso"), Some("kali.iso".to_string()));
        assert_eq!(
            sanitize_name("debian-13-genericcloud-amd64.qcow2"),
            Some("debian-13-genericcloud-amd64.qcow2".to_string())
        );
        for bad in [
            "",
            "../etc/passwd",
            "/absolute/kali.iso",
            "sub/dir.iso",
            ".hidden.iso",
            "notes.txt",
            "noextension",
            "bad name.iso",
        ] {
            assert!(sanitize_name(bad).is_none(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn derives_a_name_from_a_url() {
        assert_eq!(
            name_from_url("https://example.com/isos/kali-linux-2024.iso"),
            Some("kali-linux-2024.iso".to_string())
        );
        assert_eq!(
            name_from_url("https://example.com/download/debian.qcow2?token=abc#frag"),
            Some("debian.qcow2".to_string())
        );
        assert_eq!(name_from_url("https://example.com/"), None);
        assert_eq!(name_from_url("https://example.com/notanimage.txt"), None);
    }

    #[test]
    fn url_validation_only_allows_http() {
        assert!(validate_url("https://example.com/x.iso").is_ok());
        assert!(validate_url("http://example.com/x.iso").is_ok());
        for bad in [
            "",
            "file:///etc/passwd",
            "/local/path.iso",
            "https://a b.iso",
        ] {
            assert!(validate_url(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn format_hint_maps_isos_to_raw() {
        assert_eq!(format_hint("kali.iso"), "raw");
        assert_eq!(format_hint("debian.qcow2"), "qcow2");
        assert_eq!(format_hint("disk.img"), "qcow2");
    }

    #[test]
    fn list_is_empty_for_a_missing_directory() {
        let missing = std::env::temp_dir().join("ss-images-definitely-missing");
        assert!(list(&missing).is_empty());
    }
}
