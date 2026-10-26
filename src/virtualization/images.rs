//! Guest image / LXC template catalog.
//!
//! Like the app store, this is an embedded JSON seed (so a freshly flashed box
//! has the full list with no network) optionally merged with an operator
//! override at `guest_catalog_path`. The override is what lets a user pin an
//! exact version, swap a mirror, or add their own appliance without patching the
//! binary: an entry with a matching `id` replaces the seed entry, a new `id` is
//! appended.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Baked into the binary so a fleet node never needs the catalog on disk.
const SEED_CATALOG: &str = include_str!("catalog.json");

/// Whether a catalog entry boots as a QEMU virtual machine or an LXC container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestImageKind {
    Vm,
    Lxc,
}

/// A downloadable guest image or an LXC download template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestImage {
    pub id: String,
    pub name: String,
    pub kind: GuestImageKind,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_arch")]
    pub arch: String,
    /// Download URL. `None` means the operator supplies a local path at create
    /// time (appliances whose release URLs rotate).
    #[serde(default)]
    pub url: Option<String>,
    /// Expected SHA-256 of the download, checked after fetching when present.
    #[serde(default)]
    pub sha256: Option<String>,
    /// On-disk format of a VM image: `qcow2`, `raw` or `iso`.
    #[serde(default)]
    pub format: Option<String>,
    /// Default login user baked into the cloud image.
    #[serde(default)]
    pub default_user: Option<String>,
    #[serde(default = "min_zero")]
    pub min_disk_gb: u32,
    #[serde(default = "min_memory")]
    pub min_memory_mb: u32,
    /// LXC download-template distribution name, e.g. `debian`.
    #[serde(default)]
    pub lxc_dist: Option<String>,
    #[serde(default)]
    pub lxc_release: Option<String>,
    #[serde(default)]
    pub lxc_mirror: Option<String>,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Runtime-only: the image has already been fetched into the images dir.
    #[serde(default, skip_deserializing)]
    pub cached: bool,
    /// `true` when the download URL has a pinned SHA-256 in the catalog and
    /// every fetch is checksum-verified. Rolling-release images that cannot be
    /// pinned are `false` and badged as unverified in the UI.
    #[serde(default = "default_verified")]
    pub verified: bool,
}

fn default_verified() -> bool {
    true
}

pub fn default_arch() -> String {
    "amd64".to_string()
}

fn min_zero() -> u32 {
    0
}

fn min_memory() -> u32 {
    512
}

/// Embedded catalog merged with the override file, sorted for stable output.
pub fn catalog(cfg: &Config) -> Vec<GuestImage> {
    let mut images: Vec<GuestImage> = match serde_json::from_str(SEED_CATALOG) {
        Ok(images) => images,
        Err(err) => {
            tracing::error!(%err, "embedded guest catalog is malformed");
            Vec::new()
        }
    };

    if let Ok(raw) = std::fs::read_to_string(&cfg.guest_catalog_path) {
        match serde_json::from_str::<Vec<GuestImage>>(&raw) {
            Ok(overrides) => {
                for image in overrides {
                    match images.iter_mut().find(|existing| existing.id == image.id) {
                        Some(slot) => *slot = image,
                        None => images.push(image),
                    }
                }
            }
            Err(err) => tracing::warn!(
                path = %cfg.guest_catalog_path,
                %err,
                "guest catalog override is not a valid array, ignoring"
            ),
        }
    }

    // `verified` is derived, never trusted from the document: an entry is only
    // verified when a pinned SHA-256 exists, so the UI's [Unverified] badge is
    // always truthful even for an operator override.
    for image in &mut images {
        image.verified = image.sha256.is_some();
    }

    images.sort_by(|a, b| {
        (a.kind_rank(), a.category.as_str(), a.name.as_str()).cmp(&(
            b.kind_rank(),
            b.category.as_str(),
            b.name.as_str(),
        ))
    });
    images
}

impl GuestImage {
    fn kind_rank(&self) -> u8 {
        match self.kind {
            GuestImageKind::Vm => 0,
            GuestImageKind::Lxc => 1,
        }
    }
}

/// Catalog with the runtime `cached` flag filled in.
pub fn catalog_with_state(cfg: &Config) -> Vec<GuestImage> {
    catalog(cfg)
        .into_iter()
        .map(|mut image| {
            image.cached = is_cached(cfg, &image);
            image
        })
        .collect()
}

pub fn find(cfg: &Config, id: &str) -> Option<GuestImage> {
    catalog(cfg).into_iter().find(|image| image.id == id)
}

/// Where a downloaded VM image is cached: `<images_dir>/<id>.<format>`.
pub fn cached_path(cfg: &Config, image: &GuestImage) -> PathBuf {
    let extension = image.format.clone().unwrap_or_else(|| "img".to_string());
    Path::new(&cfg.images_dir).join(format!("{}.{}", image.id, extension))
}

pub fn is_cached(cfg: &Config, image: &GuestImage) -> bool {
    match &image.url {
        Some(_) => cached_path(cfg, image).is_file(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg(name: &str) -> Config {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-guests-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Config {
            guest_catalog_path: dir.join("guests.json").to_string_lossy().into_owned(),
            images_dir: dir.join("images").to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    #[test]
    fn embedded_catalog_parses_with_unique_ids() {
        let images: Vec<GuestImage> =
            serde_json::from_str(SEED_CATALOG).expect("seed catalog must parse");
        assert!(
            images.len() >= 30,
            "the seed catalog ships the full list, got {}",
            images.len()
        );
        let mut ids: Vec<&str> = images.iter().map(|image| image.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "catalog ids must be unique");

        for image in &images {
            assert!(!image.name.is_empty(), "{} has no name", image.id);
            match image.kind {
                GuestImageKind::Vm => assert!(
                    image.format.is_some(),
                    "vm image {} must declare a format",
                    image.id
                ),
                GuestImageKind::Lxc => assert!(
                    image.lxc_dist.is_some(),
                    "lxc template {} must declare a dist",
                    image.id
                ),
            }
        }
    }

    #[test]
    fn catalog_covers_both_backends_and_appliances() {
        let cfg = temp_cfg("coverage");
        let images = catalog(&cfg);
        assert!(images.iter().any(|i| i.kind == GuestImageKind::Vm));
        assert!(images.iter().any(|i| i.kind == GuestImageKind::Lxc));
        assert!(images.iter().any(|i| i.category == "appliance"));
    }

    #[test]
    fn verification_is_derived_from_the_checksum() {
        let cfg = temp_cfg("verified");
        for image in catalog(&cfg) {
            assert_eq!(
                image.verified,
                image.sha256.is_some(),
                "{} must be verified only with a pinned checksum",
                image.id
            );
        }
    }

    #[test]
    fn override_replaces_and_appends() {
        let cfg = temp_cfg("override");
        std::fs::create_dir_all(Path::new(&cfg.guest_catalog_path).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(
            &cfg.guest_catalog_path,
            r#"[
                {"id":"debian-12","name":"Debian 12 (pinned)","kind":"vm","format":"qcow2","url":"http://mirror/debian.qcow2"},
                {"id":"mine","name":"Mine","kind":"lxc","lxc_dist":"debian"}
            ]"#,
        )
        .expect("write override");

        let images = catalog(&cfg);
        let debian = images.iter().find(|i| i.id == "debian-12").expect("debian");
        assert_eq!(debian.name, "Debian 12 (pinned)");
        assert!(images.iter().any(|i| i.id == "mine"));
        assert!(images.iter().any(|i| i.id == "ubuntu-2404"));
    }

    #[test]
    fn cached_flag_follows_the_downloaded_file() {
        let cfg = temp_cfg("cached");
        let images = catalog(&cfg);
        let debian = images
            .iter()
            .find(|i| i.url.is_some())
            .expect("a downloadable image");
        assert!(!is_cached(&cfg, debian));

        let path = cached_path(&cfg, debian);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"fake").expect("write");
        assert!(is_cached(&cfg, debian));

        let with_state = catalog_with_state(&cfg);
        assert!(
            with_state
                .iter()
                .find(|i| i.id == debian.id)
                .expect("present")
                .cached
        );
    }
}
