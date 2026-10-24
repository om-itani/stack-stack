//! Container and stack auto-update, with a rollback path.
//!
//! Everything goes through the `docker` / `docker compose` CLI on the shared
//! runner, so there is no second Docker client and no new dependency. The
//! document records, per stack, whether updates are allowed, how often to check,
//! and the digest currently deployed plus the one it replaced, which is what
//! rollback pins back to.
//!
//! Scope, deliberately: a **stack** can be checked, redeployed and rolled back,
//! because its compose file is the record of how it runs and a digest can be
//! pinned into it. A standalone container can be checked, but not rolled back:
//! recreating one means reconstructing its run arguments, and guessing those is
//! how a rollback turns into an outage.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::runner::Runner;

/// How often a check may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Schedule {
    #[default]
    Manual,
    Daily,
    Weekly,
}

/// One tracked stack or container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEntry {
    /// Stack project name, or container id/name.
    pub id: String,
    /// `stack` or `container`; only a stack can be redeployed and rolled back.
    pub kind: String,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default)]
    pub schedule: Schedule,
    /// Digest currently deployed, as `sha256:…`.
    #[serde(default)]
    pub pinned_digest: Option<String>,
    /// Digest the last update replaced, for rollback.
    #[serde(default)]
    pub previous_digest: Option<String>,
    #[serde(default)]
    pub last_checked: Option<u64>,
    #[serde(default)]
    pub last_result: Option<String>,
    /// Set by a check that found a different digest upstream.
    #[serde(default)]
    pub update_available: bool,
}

impl UpdateEntry {
    fn new(id: &str, kind: &str) -> Self {
        Self {
            id: id.to_string(),
            kind: kind.to_string(),
            auto_update: false,
            schedule: Schedule::Manual,
            pinned_digest: None,
            previous_digest: None,
            last_checked: None,
            last_result: None,
            update_available: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatesDoc {
    #[serde(default)]
    pub entries: Vec<UpdateEntry>,
}

impl UpdatesDoc {
    pub fn find(&self, id: &str) -> Option<&UpdateEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut UpdateEntry> {
        self.entries.iter_mut().find(|entry| entry.id == id)
    }

    fn entry_mut(&mut self, id: &str, kind: &str) -> &mut UpdateEntry {
        if self.find(id).is_none() {
            self.entries.push(UpdateEntry::new(id, kind));
        }
        self.find_mut(id).expect("just inserted")
    }
}

/// What a check found.
#[derive(Debug, Clone, Serialize)]
pub struct CheckOutcome {
    pub id: String,
    pub changed: bool,
    pub digest: Option<String>,
    pub previous: Option<String>,
    pub message: String,
}

#[derive(Clone)]
pub struct UpdateContext {
    doc_path: PathBuf,
    stacks_dir: PathBuf,
    timeout_s: u64,
    runner: Arc<dyn Runner>,
}

impl UpdateContext {
    pub fn new(
        doc_path: PathBuf,
        stacks_dir: PathBuf,
        timeout_s: u64,
        runner: Arc<dyn Runner>,
    ) -> Self {
        Self {
            doc_path,
            stacks_dir,
            timeout_s,
            runner,
        }
    }

    /// Forgiving read: a missing file is an empty document, a malformed one is
    /// reported but does not stop the service.
    pub fn load(&self) -> (UpdatesDoc, Option<String>) {
        match std::fs::read_to_string(&self.doc_path) {
            Ok(raw) => match serde_json::from_str::<UpdatesDoc>(&raw) {
                Ok(doc) => (doc, None),
                Err(err) => (
                    UpdatesDoc::default(),
                    Some(format!(
                        "{} is not valid JSON: {err}",
                        self.doc_path.display()
                    )),
                ),
            },
            Err(_) => (UpdatesDoc::default(), None),
        }
    }

    pub fn save(&self, doc: &UpdatesDoc) -> Result<(), ApiError> {
        let mut body = serde_json::to_vec_pretty(doc)
            .map_err(|err| ApiError::internal(format!("cannot serialise updates: {err}")))?;
        body.push(b'\n');
        crate::networking::store::write_atomic(&self.doc_path, &body).map_err(|err| {
            ApiError::internal(format!("cannot write {}: {err}", self.doc_path.display()))
        })
    }

    /// Set auto-update and the schedule for one stack, creating the entry if it
    /// does not exist. Defaults are off/manual, so nothing is ever opted in by
    /// accident.
    pub fn configure(
        &self,
        id: &str,
        kind: &str,
        auto_update: bool,
        schedule: Schedule,
    ) -> Result<UpdatesDoc, ApiError> {
        crate::containers::store::sanitize_stack_name(id)?;
        let (mut doc, _) = self.load();
        let entry = doc.entry_mut(id, kind);
        entry.auto_update = auto_update;
        entry.schedule = schedule;
        self.save(&doc)?;
        Ok(doc)
    }

    /// Pull the image and report whether the digest moved.
    pub fn check(&self, id: &str) -> Result<CheckOutcome, ApiError> {
        let (mut doc, _) = self.load();
        let compose = self.stack_compose_path(id)?;
        let text = read(&compose)?;
        let image = compose_image(&text).ok_or_else(|| {
            ApiError::bad_request(format!("{} has no image: line", compose.display()))
        })?;

        let before = self.image_digest(&image)?;
        self.compose(id, &compose, &["pull"])?;
        let after = self.image_digest(&image)?;

        let changed = match (&before, &after) {
            (Some(before), Some(after)) => before != after,
            // Nothing cached before and something cached now: this is the first
            // pull, which is a change worth surfacing.
            (None, Some(_)) => true,
            _ => false,
        };

        let now = now_secs();
        let entry = doc.entry_mut(id, "stack");
        entry.last_checked = Some(now);
        entry.update_available = changed;
        if changed {
            if before.is_some() {
                entry.previous_digest = before.clone();
            }
            entry.pinned_digest = after.clone();
            entry.last_result = Some("update available".to_string());
        } else {
            entry.last_result = Some("up to date".to_string());
        }
        self.save(&doc)?;

        let message = if changed {
            format!("{id}: a newer image is available")
        } else {
            format!("{id}: already up to date")
        };
        Ok(CheckOutcome {
            id: id.to_string(),
            changed,
            digest: after,
            previous: before,
            message,
        })
    }

    /// Bring the stack up again on whatever the compose file now points at.
    pub fn apply(&self, id: &str) -> Result<String, ApiError> {
        let compose = self.stack_compose_path(id)?;
        let output = self.compose(id, &compose, &["up", "-d", "--remove-orphans"])?;

        let (mut doc, _) = self.load();
        if let Some(entry) = doc.find_mut(id) {
            entry.update_available = false;
            entry.last_result = Some("redeployed".to_string());
        }
        self.save(&doc)?;
        Ok(output)
    }

    /// Pin the previous digest into the compose file and redeploy.
    pub fn rollback(&self, id: &str) -> Result<String, ApiError> {
        let (mut doc, _) = self.load();
        let previous = doc
            .find(id)
            .and_then(|entry| entry.previous_digest.clone())
            .ok_or_else(|| {
                ApiError::bad_request(format!("{id} has no previous digest to roll back to"))
            })?;

        let compose = self.stack_compose_path(id)?;
        let text = read(&compose)?;
        let rewritten = rewrite_image_line(&text, &previous).map_err(ApiError::bad_request)?;
        crate::networking::store::write_atomic(&compose, rewritten.as_bytes()).map_err(|err| {
            ApiError::internal(format!("cannot write {}: {err}", compose.display()))
        })?;

        let output = self.compose(id, &compose, &["up", "-d", "--remove-orphans"])?;

        if let Some(entry) = doc.find_mut(id) {
            let replaced = entry.pinned_digest.take();
            entry.pinned_digest = Some(previous);
            entry.previous_digest = replaced;
            entry.update_available = false;
            entry.last_result = Some("rolled back".to_string());
        }
        self.save(&doc)?;
        Ok(output)
    }

    /// Project name and compose path for a stack that exists on disk.
    fn stack_compose_path(&self, id: &str) -> Result<PathBuf, ApiError> {
        let name = crate::containers::store::sanitize_stack_name(id)?;
        let path = self.stacks_dir.join(name).join("docker-compose.yml");
        if !path.is_file() {
            return Err(ApiError::not_found(format!(
                "stack {name} has no compose file"
            )));
        }
        Ok(path)
    }

    fn compose(&self, id: &str, compose: &Path, args: &[&str]) -> Result<String, ApiError> {
        let name = crate::containers::store::sanitize_stack_name(id)?.to_string();
        let mut full: Vec<String> = vec![
            "compose".to_string(),
            "-p".to_string(),
            name,
            "-f".to_string(),
            compose.to_string_lossy().into_owned(),
        ];
        full.extend(args.iter().map(|arg| (*arg).to_string()));

        let timeout = Duration::from_secs(self.timeout_s.clamp(30, 1800));
        let output = self
            .runner
            .run_timeout("docker", &full, timeout)
            .map_err(|err| ApiError::unavailable(format!("cannot run docker compose: {err}")))?;
        if !output.success() {
            return Err(ApiError::internal(format!(
                "docker compose {} failed: {}",
                args.first().copied().unwrap_or(""),
                output.combined().trim()
            )));
        }
        Ok(output.combined().trim().to_string())
    }

    fn image_digest(&self, image: &str) -> Result<Option<String>, ApiError> {
        let output = self
            .runner
            .call(
                "docker",
                &[
                    "image",
                    "inspect",
                    "--format",
                    "{{index .RepoDigests 0}}",
                    image,
                ],
            )
            .map_err(|err| ApiError::unavailable(format!("cannot run docker: {err}")))?;
        if !output.success() {
            // Not cached locally, or no RepoDigests entry: not an error.
            return Ok(None);
        }
        Ok(parse_digest(&output.stdout))
    }
}

/// True when a scheduled check is due. Manual never runs on its own.
pub fn schedule_due(schedule: Schedule, last_checked: Option<u64>, now: u64) -> bool {
    let interval = match schedule {
        Schedule::Manual => return false,
        Schedule::Daily => 24 * 3600,
        Schedule::Weekly => 7 * 24 * 3600,
    };
    last_checked
        .map(|last| now.saturating_sub(last) >= interval)
        .unwrap_or(true)
}

/// The first `image:` value in a compose document.
pub fn compose_image(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || !trimmed.starts_with("image:") {
            continue;
        }
        let value = trimmed
            .trim_start_matches("image:")
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// The repository part of an image reference: any tag removed, any registry port
/// kept.
pub fn image_repo(value: &str) -> String {
    let value = value.trim();
    let value = value.split('@').next().unwrap_or(value);
    match value.rfind(':') {
        // `nginx:1.27` -> `nginx`; `localhost:5000/nginx` keeps its port.
        Some(index) if !value[index + 1..].contains('/') => value[..index].to_string(),
        _ => value.to_string(),
    }
}

/// Pin the first `image:` line to `digest`.
///
/// Refuses an image that interpolates a variable: the real name is only known
/// when compose runs, so pinning a digest there would produce a nonsense
/// reference rather than a rollback.
pub fn rewrite_image_line(text: &str, digest: &str) -> Result<String, String> {
    let digest = digest.trim();
    if digest.is_empty() {
        return Err("no digest to pin".to_string());
    }
    let mut out = String::with_capacity(text.len() + digest.len() + 16);
    let mut replaced = false;

    for segment in text.split_inclusive('\n') {
        let line = segment.trim_end_matches('\n').trim_end_matches('\r');
        let trimmed = line.trim_start();
        if !replaced && !trimmed.starts_with('#') && trimmed.starts_with("image:") {
            let indent = &line[..line.len() - trimmed.len()];
            let value = trimmed
                .trim_start_matches("image:")
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if value.contains("${") {
                return Err(format!(
                    "image {value:?} is set from a variable; rollback cannot pin a digest here"
                ));
            }
            out.push_str(indent);
            out.push_str("image: ");
            out.push_str(&image_repo(value));
            out.push('@');
            out.push_str(digest);
            if segment.ends_with('\n') {
                out.push('\n');
            }
            replaced = true;
            continue;
        }
        out.push_str(segment);
    }

    if !replaced {
        return Err("the compose file has no image: line to pin".to_string());
    }
    Ok(out)
}

/// Digest out of a `RepoDigests` entry, which reads `repo@sha256:…`.
pub fn parse_digest(value: &str) -> Option<String> {
    let value = value.trim();
    let (_, digest) = value.split_once('@')?;
    let digest = digest.trim();
    (!digest.is_empty()).then(|| digest.to_string())
}

/// Stacks whose scheduled check is due on this tick.
///
/// Only stacks qualify: a standalone container can be checked, but there is no
/// unattended way to recreate one on a new image.
pub fn due_ids(doc: &UpdatesDoc, now: u64) -> Vec<String> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for entry in &doc.entries {
        if entry.kind == "stack"
            && entry.auto_update
            && schedule_due(entry.schedule, entry.last_checked, now)
        {
            ids.insert(entry.id.clone());
        }
    }
    ids.into_iter().collect()
}

fn read(path: &Path) -> Result<String, ApiError> {
    std::fs::read_to_string(path)
        .map_err(|err| ApiError::internal(format!("cannot read {}: {err}", path.display())))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_first_image_line() {
        let text = "# a comment\nservices:\n  web:\n    image: nginx:1.27\n  db:\n    image: postgres:16\n";
        assert_eq!(compose_image(text), Some("nginx:1.27".to_string()));
        assert_eq!(compose_image("services:\n  web:\n    build: .\n"), None);
        // A commented-out image is not the image.
        assert_eq!(compose_image("# image: skipme\n"), None);
    }

    #[test]
    fn image_repo_strips_tags_but_keeps_registry_ports() {
        assert_eq!(image_repo("nginx:1.27"), "nginx");
        assert_eq!(image_repo("nginx"), "nginx");
        assert_eq!(image_repo("library/nginx:latest"), "library/nginx");
        assert_eq!(image_repo("localhost:5000/nginx"), "localhost:5000/nginx");
        assert_eq!(
            image_repo("localhost:5000/nginx:1.27"),
            "localhost:5000/nginx"
        );
        assert_eq!(image_repo("nginx@sha256:abc"), "nginx");
    }

    #[test]
    fn pins_a_digest_into_the_image_line() {
        let text = "services:\n  web:\n    image: nginx:1.27\n    restart: unless-stopped\n";
        let out = rewrite_image_line(text, "sha256:deadbeef").expect("rewrite");
        assert!(out.contains("image: nginx@sha256:deadbeef"), "{out}");
        assert!(
            out.contains("restart: unless-stopped"),
            "other lines are preserved"
        );
        assert!(!out.contains("nginx:1.27"));
    }

    #[test]
    fn refuses_to_pin_an_interpolated_image() {
        let text = "services:\n  web:\n    image: ${IMAGE}\n";
        assert!(rewrite_image_line(text, "sha256:x").is_err());
        assert!(rewrite_image_line("services: {}\n", "sha256:x").is_err());
        assert!(rewrite_image_line("image: nginx\n", "").is_err());
    }

    #[test]
    fn schedule_due_respects_the_interval_and_manual_never_runs() {
        let now = 2_000_000_000;
        assert!(!schedule_due(Schedule::Manual, None, now));
        assert!(schedule_due(Schedule::Daily, None, now));
        assert!(!schedule_due(Schedule::Daily, Some(now - 3600), now));
        assert!(schedule_due(Schedule::Daily, Some(now - 25 * 3600), now));
        assert!(!schedule_due(
            Schedule::Weekly,
            Some(now - 3 * 24 * 3600),
            now
        ));
        assert!(schedule_due(
            Schedule::Weekly,
            Some(now - 8 * 24 * 3600),
            now
        ));
    }

    #[test]
    fn parses_repo_digests() {
        assert_eq!(
            parse_digest("nginx@sha256:abc123"),
            Some("sha256:abc123".to_string())
        );
        assert_eq!(parse_digest(""), None);
        assert_eq!(parse_digest("nginx"), None);
    }

    #[test]
    fn due_ids_only_includes_enabled_and_due_entries() {
        let now = 2_000_000_000;
        let doc = UpdatesDoc {
            entries: vec![
                UpdateEntry {
                    auto_update: true,
                    schedule: Schedule::Daily,
                    last_checked: Some(now - 30 * 3600),
                    ..UpdateEntry::new("old", "stack")
                },
                UpdateEntry {
                    auto_update: true,
                    schedule: Schedule::Daily,
                    last_checked: Some(now - 60),
                    ..UpdateEntry::new("fresh", "stack")
                },
                UpdateEntry {
                    auto_update: false,
                    schedule: Schedule::Daily,
                    ..UpdateEntry::new("off", "stack")
                },
            ],
        };
        assert_eq!(due_ids(&doc, now), vec!["old".to_string()]);
    }

    #[test]
    fn entries_default_to_off_and_manual() {
        let entry = UpdateEntry::new("web", "stack");
        assert!(!entry.auto_update);
        assert_eq!(entry.schedule, Schedule::Manual);
        assert!(entry.pinned_digest.is_none());
    }
}
