//! App catalog and on-disk compose store.
//!
//! The catalog is an embedded JSON seed, optionally merged with an operator
//! override file. Installing an app writes its compose file atomically and then
//! shells out to `docker compose up -d`; removing it runs `down` and deletes the
//! directory. Compose files live at `<apps_dir>/<app-id>/docker-compose.yml`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::Config;
use crate::containers::model::App;
use crate::error::ApiError;

/// Baked into the binary so a fleet node never needs the catalog on disk.
const SEED_CATALOG: &str = include_str!("catalog.json");

const COMPOSE_FILE: &str = "docker-compose.yml";

/// Upper bound on how much compose output is echoed back in an error body.
const MAX_ERROR_TAIL: usize = 2000;

// ---------------------------------------------------------------------------
// Id validation
// ---------------------------------------------------------------------------

/// App ids become a single path component under `apps_dir`. Accepting only
/// `[A-Za-z0-9_-]` starting with an alphanumeric makes `..`, `/` and friends
/// structurally impossible rather than something we have to escape.
pub fn sanitize_app_id(raw: &str) -> Result<&str, ApiError> {
    if raw.is_empty() || raw.len() > 64 {
        return Err(ApiError::bad_request("app id must be 1..=64 characters"));
    }
    let mut chars = raw.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_ok = raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if first_ok && rest_ok {
        Ok(raw)
    } else {
        Err(ApiError::bad_request(format!("invalid app id: {raw:?}")))
    }
}

/// Container identifiers double as names in Docker's API, so allow the name
/// charset too (`.`, `_`, `-`), but never a path separator.
pub fn sanitize_container_id(raw: &str) -> Result<&str, ApiError> {
    if raw.is_empty() || raw.len() > 128 {
        return Err(ApiError::bad_request(
            "container id must be 1..=128 characters",
        ));
    }
    let first_ok = raw
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_ok = raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if first_ok && rest_ok {
        Ok(raw)
    } else {
        Err(ApiError::bad_request(format!(
            "invalid container id: {raw:?}"
        )))
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Embedded catalog merged with the override file, sorted for stable output.
///
/// A malformed override is logged and ignored; a malformed *embedded* catalog
/// is a build bug, but we degrade to an empty list rather than panic in prod.
pub fn catalog(cfg: &Config) -> Vec<App> {
    let mut apps: Vec<App> = match serde_json::from_str(SEED_CATALOG) {
        Ok(apps) => apps,
        Err(err) => {
            tracing::error!(%err, "embedded app catalog is malformed");
            Vec::new()
        }
    };

    if let Ok(raw) = std::fs::read_to_string(&cfg.catalog_path) {
        match serde_json::from_str::<Vec<App>>(&raw) {
            Ok(overrides) => {
                for app in overrides {
                    match apps.iter_mut().find(|existing| existing.id == app.id) {
                        Some(slot) => *slot = app,
                        None => apps.push(app),
                    }
                }
            }
            Err(err) => tracing::warn!(
                path = %cfg.catalog_path,
                %err,
                "catalog override is not a valid app array, ignoring"
            ),
        }
    }

    apps.sort_by(|a, b| {
        (a.category.as_str(), a.name.as_str()).cmp(&(b.category.as_str(), b.name.as_str()))
    });
    apps
}

/// Catalog with the runtime `installed` flag filled in, ready to serialise.
pub fn catalog_with_state(cfg: &Config) -> Vec<App> {
    let installed = installed_ids(cfg);
    catalog(cfg)
        .into_iter()
        .map(|mut app| {
            app.installed = installed.iter().any(|id| id == &app.id);
            app
        })
        .collect()
}

// ---------------------------------------------------------------------------
// On-disk layout
// ---------------------------------------------------------------------------

pub fn app_dir(cfg: &Config, id: &str) -> PathBuf {
    Path::new(&cfg.apps_dir).join(id)
}

pub fn compose_path(cfg: &Config, id: &str) -> PathBuf {
    app_dir(cfg, id).join(COMPOSE_FILE)
}

/// Directory names under `apps_dir` that hold a compose file.
pub fn installed_ids(cfg: &Config) -> Vec<String> {
    let dir = Path::new(&cfg.apps_dir);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().join(COMPOSE_FILE).is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------
// Install / remove
// ---------------------------------------------------------------------------

/// Write the app's compose file and bring it up. Returns compose output.
pub async fn install(cfg: &Config, raw_id: &str) -> Result<String, ApiError> {
    let id = sanitize_app_id(raw_id)?.to_string();
    let app = catalog(cfg)
        .into_iter()
        .find(|app| app.id == id)
        .ok_or_else(|| ApiError::not_found(format!("unknown app: {id}")))?;

    if app.compose.trim().is_empty() {
        return Err(ApiError::internal(format!(
            "app {id} has no compose definition"
        )));
    }

    let dir = app_dir(cfg, &id);
    let compose = app.compose;
    let dir_for_write = dir.clone();
    tokio::task::spawn_blocking(move || write_compose(&dir_for_write, compose.as_bytes()))
        .await
        .map_err(|err| ApiError::internal(format!("install task failed: {err}")))??;

    let output = run_compose(&dir, &["up", "-d"], cfg.compose_timeout_s).await?;
    tracing::info!(app = %id, "app-store install finished");
    Ok(output)
}

/// `docker compose down` then delete the app directory.
pub async fn remove(cfg: &Config, raw_id: &str) -> Result<String, ApiError> {
    let id = sanitize_app_id(raw_id)?.to_string();
    let dir = app_dir(cfg, &id);

    if !compose_path(cfg, &id).is_file() {
        return Err(ApiError::not_found(format!("app {id} is not installed")));
    }

    let output = run_compose(&dir, &["down"], cfg.compose_timeout_s).await?;

    let dir_for_remove = dir.clone();
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir_for_remove))
        .await
        .map_err(|err| ApiError::internal(format!("remove task failed: {err}")))?
        .map_err(|err| ApiError::internal(format!("cannot remove {}: {err}", dir.display())))?;

    tracing::info!(app = %id, "app-store app removed");
    Ok(output)
}

// ---------------------------------------------------------------------------
// Operator-authored stacks
// ---------------------------------------------------------------------------

/// Compose project names are lower-case and become both a directory name and a
/// `docker compose -p` value, so the accepted set is deliberately narrow:
/// `[a-z0-9]` first, then `[a-z0-9_-]`. Upper case is rejected because Compose
/// itself refuses it, and `/`, `.` and `..` are structurally impossible.
pub fn sanitize_stack_name(raw: &str) -> Result<&str, ApiError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 63 {
        return Err(ApiError::bad_request(
            "stack name must be 1..=63 characters",
        ));
    }
    let mut chars = raw.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = raw
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if first_ok && rest_ok {
        Ok(raw)
    } else {
        Err(ApiError::bad_request(format!(
            "invalid stack name {raw:?}: use lower-case letters, digits, '-' and '_', starting with a letter or digit"
        )))
    }
}

pub fn stack_dir(cfg: &Config, name: &str) -> PathBuf {
    Path::new(&cfg.stacks_dir).join(name)
}

pub fn stack_compose_path(cfg: &Config, name: &str) -> PathBuf {
    stack_dir(cfg, name).join(COMPOSE_FILE)
}

/// Stack names that have a compose file on disk, sorted.
pub fn installed_stacks(cfg: &Config) -> Vec<String> {
    let dir = Path::new(&cfg.stacks_dir);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().join(COMPOSE_FILE).is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Every stack with its compose text, for the UI. A stack whose file cannot be
/// read is skipped rather than failing the whole listing.
pub fn stack_infos(cfg: &Config) -> Vec<crate::containers::model::StackInfo> {
    installed_stacks(cfg)
        .into_iter()
        .filter_map(|name| {
            let compose = std::fs::read_to_string(stack_compose_path(cfg, &name)).ok()?;
            Some(crate::containers::model::StackInfo { name, compose })
        })
        .collect()
}

/// Write the stack's compose file and bring it up with an explicit project name.
/// Returns the compose output.
pub async fn deploy_stack(cfg: &Config, raw_name: &str, yaml: &str) -> Result<String, ApiError> {
    let name = sanitize_stack_name(raw_name)?.to_string();
    if yaml.trim().is_empty() {
        return Err(ApiError::bad_request("compose document must not be empty"));
    }
    if !looks_like_compose(yaml) {
        return Err(ApiError::bad_request(
            "compose document does not look like a YAML `services:` document",
        ));
    }

    let dir = stack_dir(cfg, &name);
    let dir_for_write = dir.clone();
    let body = yaml.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || write_compose(&dir_for_write, &body))
        .await
        .map_err(|err| ApiError::internal(format!("stack task failed: {err}")))??;

    let compose_file = stack_compose_path(cfg, &name);
    let args = vec![
        "-p".to_string(),
        name.clone(),
        "-f".to_string(),
        compose_file.to_string_lossy().into_owned(),
        "up".to_string(),
        "-d".to_string(),
        "--remove-orphans".to_string(),
    ];
    let output = run_compose_owned(&dir, args, cfg.compose_timeout_s).await?;
    tracing::info!(stack = %name, "compose stack deployed");
    Ok(output)
}

/// `docker compose down` for the stack, then delete its directory.
pub async fn remove_stack(cfg: &Config, raw_name: &str) -> Result<String, ApiError> {
    let name = sanitize_stack_name(raw_name)?.to_string();
    if !stack_compose_path(cfg, &name).is_file() {
        return Err(ApiError::not_found(format!("stack {name} does not exist")));
    }

    let dir = stack_dir(cfg, &name);
    let compose_file = stack_compose_path(cfg, &name);
    let args = vec![
        "-p".to_string(),
        name.clone(),
        "-f".to_string(),
        compose_file.to_string_lossy().into_owned(),
        "down".to_string(),
        "--remove-orphans".to_string(),
    ];
    let output = run_compose_owned(&dir, args, cfg.compose_timeout_s).await?;

    let dir_for_remove = dir.clone();
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir_for_remove))
        .await
        .map_err(|err| ApiError::internal(format!("remove task failed: {err}")))?
        .map_err(|err| ApiError::internal(format!("cannot remove {}: {err}", dir.display())))?;

    tracing::info!(stack = %name, "compose stack removed");
    Ok(output)
}

/// Cheap sanity check that the pasted document is compose-shaped. It only looks
/// for a top-level `services:` key so it cannot reject a valid-but-unusual file,
/// but it catches the common "pasted the wrong thing" mistake before we shell
/// out to Docker and surface a nine-line YAML parse error.
fn looks_like_compose(yaml: &str) -> bool {
    // `services:` is always a top-level key, so it must sit at column zero.
    yaml.lines().any(|line| line.starts_with("services:"))
}

/// Temp file in the same directory, then rename: a half-written compose file is
/// never visible to a concurrent `up`.
fn write_compose(dir: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    std::fs::create_dir_all(dir)
        .map_err(|err| ApiError::internal(format!("cannot create {}: {err}", dir.display())))?;

    let target = dir.join(COMPOSE_FILE);
    let tmp = dir.join(format!("{COMPOSE_FILE}.tmp"));

    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(err) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(ApiError::internal(format!(
            "cannot write {}: {err}",
            tmp.display()
        )));
    }

    std::fs::rename(&tmp, &target)
        .map_err(|err| ApiError::internal(format!("cannot install {}: {err}", target.display())))
}

// ---------------------------------------------------------------------------
// Compose CLI
// ---------------------------------------------------------------------------

struct ComposeCli {
    program: String,
    prefix: Vec<String>,
}

/// `docker compose` is the supported form; `docker-compose` is the fallback for
/// hosts with only the v1 binary. Detected once per process.
fn compose_cli() -> &'static ComposeCli {
    static CLI: OnceLock<ComposeCli> = OnceLock::new();
    CLI.get_or_init(|| {
        if command_succeeds("docker", &["compose", "version"]) {
            ComposeCli {
                program: "docker".to_string(),
                prefix: vec!["compose".to_string()],
            }
        } else if command_succeeds("docker-compose", &["version"]) {
            ComposeCli {
                program: "docker-compose".to_string(),
                prefix: Vec::new(),
            }
        } else {
            tracing::warn!("neither `docker compose` nor `docker-compose` is available");
            // Optimistic default so the eventual error names the real tool.
            ComposeCli {
                program: "docker".to_string(),
                prefix: vec!["compose".to_string()],
            }
        }
    })
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn run_compose(dir: &Path, args: &[&str], timeout_s: u64) -> Result<String, ApiError> {
    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    run_compose_owned(dir, owned, timeout_s).await
}

/// Like [`run_compose`] but with pre-built owned arguments, needed when an
/// argument (a stack name or compose path) is computed at runtime.
async fn run_compose_owned(
    dir: &Path,
    args: Vec<String>,
    timeout_s: u64,
) -> Result<String, ApiError> {
    let dir = dir.to_path_buf();
    let timeout = Duration::from_secs(timeout_s.clamp(30, 1800));

    tokio::task::spawn_blocking(move || {
        let cli = compose_cli();
        let mut full = cli.prefix.clone();
        full.extend(args);
        run_blocking(&cli.program, &full, &dir, timeout)
    })
    .await
    .map_err(|err| ApiError::internal(format!("compose task failed: {err}")))?
}

/// Run a compose subcommand through the shared process helper: separate pipe
/// drains (so a full buffer can never deadlock) plus a wall-clock deadline.
fn run_blocking(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<String, ApiError> {
    let output = crate::proc::run(program, args, Some(cwd), timeout)
        .map_err(|err| ApiError::unavailable(format!("cannot run `{program}`: {err}")))?;

    let rendered_args = args.join(" ");
    if output.timed_out {
        return Err(ApiError::internal(format!(
            "`{program} {rendered_args}` timed out after {}s: {}",
            timeout.as_secs(),
            tail(&output.combined())
        )));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "`{program} {rendered_args}` failed: {}",
            tail(&output.combined())
        )));
    }
    Ok(output.combined())
}

/// Last `MAX_ERROR_TAIL` bytes of compose output, trimmed for a JSON body.
fn tail(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(no output)".to_string();
    }
    if trimmed.len() <= MAX_ERROR_TAIL {
        return trimmed.to_string();
    }
    // The cut can land mid-character; advance to the next UTF-8 boundary.
    let mut start = trimmed.len() - MAX_ERROR_TAIL;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg(name: &str) -> Config {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Config {
            apps_dir: dir.join("apps").to_string_lossy().into_owned(),
            catalog_path: dir.join("catalog.json").to_string_lossy().into_owned(),
            stacks_dir: dir.join("stacks").to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    #[test]
    fn stack_names_are_lowercase_path_components() {
        for ok in ["web", "my-stack", "stack_2", "a1"] {
            assert!(sanitize_stack_name(ok).is_ok(), "{ok:?} must be accepted");
        }
        for bad in ["", "..", "a/b", ".hidden", "-lead", "Upper", "a b", "a.b"] {
            assert!(sanitize_stack_name(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(sanitize_stack_name(&"x".repeat(64)).is_err());
    }

    #[test]
    fn compose_shape_check_requires_top_level_services() {
        assert!(looks_like_compose("services:\n  web:\n    image: nginx\n"));
        assert!(looks_like_compose("# a comment\nservices:\n  a:\n"));
        // An indented `services:` is not a top-level key.
        assert!(!looks_like_compose("  services:\n    x:\n"));
        assert!(!looks_like_compose("version: '3'\nweb:\n  image: nginx\n"));
    }

    #[tokio::test]
    async fn deploy_stack_rejects_bad_input_before_reaching_docker() {
        let cfg = temp_cfg("stacks-validate");

        // Bad name.
        assert!(matches!(
            deploy_stack(&cfg, "Bad Name", "services:\n  a:\n").await,
            Err(ApiError::BadRequest(_))
        ));
        // Empty document.
        assert!(matches!(
            deploy_stack(&cfg, "web", "   \n").await,
            Err(ApiError::BadRequest(_))
        ));
        // Not a compose document.
        assert!(matches!(
            deploy_stack(&cfg, "web", "version: '3'\n").await,
            Err(ApiError::BadRequest(_))
        ));
        // None of the rejected calls may have written anything.
        assert!(!stack_compose_path(&cfg, "web").exists());
    }

    #[tokio::test]
    async fn remove_unknown_stack_is_not_found() {
        let cfg = temp_cfg("stacks-remove-missing");
        assert!(matches!(
            remove_stack(&cfg, "ghost").await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn installed_stacks_lists_directories_with_a_compose_file() {
        let cfg = temp_cfg("stacks-list");
        let dir = stack_dir(&cfg, "web");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("docker-compose.yml"), "services:\n  web:\n").expect("write");
        // An empty directory with no compose file must be ignored.
        std::fs::create_dir_all(stack_dir(&cfg, "empty")).expect("mkdir empty");

        let names = installed_stacks(&cfg);
        assert_eq!(names, vec!["web".to_string()]);

        let infos = stack_infos(&cfg);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "web");
        assert!(infos[0].compose.contains("services:"));
    }

    #[test]
    fn embedded_catalog_parses_with_unique_ids() {
        let apps: Vec<App> = serde_json::from_str(SEED_CATALOG).expect("seed catalog must parse");
        assert!(
            apps.len() >= 20,
            "seed catalog ships the full app store, got {}",
            apps.len()
        );
        let mut ids: Vec<&str> = apps.iter().map(|app| app.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "seed catalog ids must be unique");
        for app in &apps {
            assert!(
                sanitize_app_id(&app.id).is_ok(),
                "seed id {} must be a valid path component",
                app.id
            );
            assert!(!app.compose.trim().is_empty(), "{} has no compose", app.id);
            assert!(
                app.compose.contains(&format!("stack-stack.app={}", app.id)),
                "{} compose must carry the app label",
                app.id
            );
            // A pinned tag is what makes an install reproducible and lets a
            // checksum be added later; `:latest` would silently drift.
            let image = app
                .compose
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("image:")
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                })
                .unwrap_or_else(|| panic!("{} has no image", app.id));
            assert!(
                !image.ends_with(":latest") && image.contains(':'),
                "{} image {image:?} must be pinned to a tag",
                app.id
            );
        }
    }

    #[test]
    fn override_replaces_and_appends() {
        let cfg = temp_cfg("override");
        std::fs::create_dir_all(Path::new(&cfg.catalog_path).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(
            &cfg.catalog_path,
            r#"[
                {"id":"jellyfin","name":"Jellyfin Custom","category":"media"},
                {"id":"mine","name":"Mine","category":"utilities"}
            ]"#,
        )
        .expect("write override");

        let apps = catalog(&cfg);
        let jellyfin = apps.iter().find(|a| a.id == "jellyfin").expect("jellyfin");
        assert_eq!(jellyfin.name, "Jellyfin Custom");
        assert!(apps.iter().any(|a| a.id == "mine"));
        // Seed apps not overridden survive.
        assert!(apps.iter().any(|a| a.id == "sonarr"));
    }

    #[test]
    fn malformed_override_is_ignored() {
        let cfg = temp_cfg("bad-override");
        std::fs::create_dir_all(Path::new(&cfg.catalog_path).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(&cfg.catalog_path, "{ not json").expect("write");
        let apps = catalog(&cfg);
        assert!(
            apps.len() >= 20,
            "falls back to the seed catalog, got {}",
            apps.len()
        );
    }

    #[test]
    fn sanitize_rejects_traversal_and_odd_ids() {
        assert!(sanitize_app_id("jellyfin").is_ok());
        assert!(sanitize_app_id("my-app_2").is_ok());
        for bad in ["", "..", "../etc", "a/b", "-lead", ".hidden", "a b"] {
            assert!(sanitize_app_id(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(sanitize_app_id(&"x".repeat(65)).is_err());
    }

    #[test]
    fn container_id_allows_docker_names() {
        assert!(sanitize_container_id("b1c2d3").is_ok());
        assert!(sanitize_container_id("paperless-db").is_ok());
        assert!(sanitize_container_id("a.b_c").is_ok());
        for bad in ["", "../x", "a/b", "a b", ".."] {
            assert!(
                sanitize_container_id(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(sanitize_container_id(&"x".repeat(129)).is_err());
    }

    #[test]
    fn install_writes_compose_and_reports_state() {
        let cfg = temp_cfg("install");
        let dir = app_dir(&cfg, "jellyfin");
        let compose = catalog(&cfg)
            .into_iter()
            .find(|a| a.id == "jellyfin")
            .expect("jellyfin")
            .compose;
        write_compose(&dir, compose.as_bytes()).expect("write compose");

        assert!(compose_path(&cfg, "jellyfin").is_file());
        assert_eq!(installed_ids(&cfg), vec!["jellyfin".to_string()]);
        assert!(
            catalog_with_state(&cfg)
                .iter()
                .find(|a| a.id == "jellyfin")
                .expect("jellyfin")
                .installed
        );
        // No temp file is left behind.
        assert!(!dir.join(format!("{COMPOSE_FILE}.tmp")).exists());
    }

    #[test]
    fn tail_truncates_from_the_front() {
        let long = "a".repeat(MAX_ERROR_TAIL + 500);
        assert_eq!(tail(&long).len(), MAX_ERROR_TAIL);
        assert_eq!(tail("   "), "(no output)");
    }
}
