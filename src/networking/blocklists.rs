//! Blocklist subscriptions, compiled into an unbound local-zone include.
//!
//! Unbound is the only resolver in the stack, and it can answer names with
//! NXDOMAIN from a `local-zone` directive. This module turns subscription lists
//! (hosts-format files or plain domain lists) into one such file, so ad and
//! tracker blocking needs no second process and no second web UI.
//!
//! Fetching goes through the `curl` binary on the shared runner, exactly like
//! the Docker Hub search: no HTTP client crate is added for a daily download
//! that `curl` already does.
//!
//! The generated file lives beside the network document rather than in
//! `/etc/unbound/unbound.conf.d`, because the systemd unit only makes
//! `/etc/stack-stack` writable; `render` pulls it in with `include:`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::NetworkConfig;
use crate::networking::store;
use crate::runner::Runner;

/// Generated unbound include, relative to the directory holding `network.json`.
pub const COMPILED_FILE: &str = "unbound-blocklist.conf";
/// Stats written beside it for the UI.
pub const STATS_FILE: &str = "blocklist-stats.json";

/// Ceiling on compiled domains. A list this large is already pathological; the
/// cap keeps one bad subscription from exhausting memory.
pub const MAX_DOMAINS: usize = 500_000;

/// Names hosts files legitimately point at loopback. Turning these into NXDOMAIN
/// breaks the machine that asked.
const NEVER_BLOCK: [&str; 10] = [
    "localhost",
    "localhost.localdomain",
    "local",
    "broadcasthost",
    "ip6-localhost",
    "ip6-loopback",
    "ip6-localnet",
    "ip6-mcastprefix",
    "ip6-allnodes",
    "ip6-allrouters",
];

/// How long after the last successful fetch the daily refresh becomes due.
const REFRESH_AFTER_S: u64 = 20 * 3600;
/// Local hour the daily refresh waits for.
const REFRESH_HOUR: i32 = 3;

pub fn compiled_path(config_path: &Path) -> PathBuf {
    config_dir(config_path).join(COMPILED_FILE)
}

pub fn stats_path(config_path: &Path) -> PathBuf {
    config_dir(config_path).join(STATS_FILE)
}

fn config_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/etc/stack-stack"))
}

/// Per-source outcome, for the UI table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStats {
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub builtin: bool,
    pub domains: usize,
    /// `ok`, `failed` or `skipped`.
    pub status: String,
    pub error: Option<String>,
    pub fetched_at: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlocklistStats {
    pub updated_at: Option<u64>,
    pub total: usize,
    pub sources: Vec<SourceStats>,
}

impl BlocklistStats {
    pub fn load(config_path: &Path) -> Self {
        std::fs::read_to_string(stats_path(config_path))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self, config_path: &Path) -> Result<(), ApiError> {
        let path = stats_path(config_path);
        let mut body = serde_json::to_vec_pretty(self).map_err(|err| {
            ApiError::internal(format!("cannot serialise blocklist stats: {err}"))
        })?;
        body.push(b'\n');
        store::write_atomic(&path, &body)
            .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
    }
}

/// Fetch every enabled subscription, compile the result and reload unbound.
///
/// `config` is mutated so `dns.blocklist_file` points at the generated include;
/// the caller is responsible for persisting it.
pub fn update(
    config: &mut NetworkConfig,
    config_path: &Path,
    runner: &dyn Runner,
) -> Result<BlocklistStats, ApiError> {
    let now = now_secs();
    let mut fetched: BTreeSet<String> = BTreeSet::new();
    let mut sources: Vec<SourceStats> = Vec::new();

    for source in &config.dns.blocklists {
        let mut stat = SourceStats {
            name: source.name.clone(),
            url: source.url.clone(),
            enabled: source.enabled,
            builtin: source.builtin,
            status: "skipped".to_string(),
            ..Default::default()
        };

        if !source.enabled {
            sources.push(stat);
            continue;
        }
        if let Err(err) = validate_url(&source.url) {
            stat.status = "failed".to_string();
            stat.error = Some(err);
            sources.push(stat);
            continue;
        }

        match fetch(runner, &source.url) {
            Ok(body) => {
                let mut domains = BTreeSet::new();
                parse_domains(&body, &mut domains);
                stat.domains = domains.len();
                stat.status = "ok".to_string();
                stat.fetched_at = Some(now);
                fetched.extend(domains);
            }
            Err(err) => {
                stat.status = "failed".to_string();
                stat.error = Some(err);
            }
        }
        sources.push(stat);
    }

    let compiled = compile(&config.dns.allowlist, &config.dns.blocked_domains, &fetched);

    // Write the include even when empty: an empty (header-only) file is what
    // makes the previous blocklist disappear on the reload.
    let include = compiled_path(config_path);
    let rendered = render_local_zones(&compiled);
    store::write_atomic(&include, rendered.as_bytes())
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", include.display())))?;

    config.dns.blocklist_file = if compiled.is_empty() {
        None
    } else {
        Some(include.to_string_lossy().into_owned())
    };

    let stats = BlocklistStats {
        updated_at: Some(now),
        total: compiled.len(),
        sources,
    };
    stats.save(config_path)?;

    // Reload only when unbound is actually the resolver here.
    if config.dns.enabled
        && let Err(err) = reload(runner)
    {
        tracing::warn!(%err, "blocklist written but unbound did not reload");
    }
    Ok(stats)
}

/// Ask unbound to re-read its configuration. `systemctl reload` is the
/// distribution's own path (Debian's unit defines ExecReload); the control
/// binary is the fallback for a hand-installed unbound.
fn reload(runner: &dyn Runner) -> Result<(), String> {
    let systemctl = runner.call("systemctl", &["reload", "unbound"]);
    if matches!(&systemctl, Ok(output) if output.success()) {
        return Ok(());
    }
    let control = runner.call("unbound-control", &["reload"]);
    match (&systemctl, &control) {
        (_, Ok(output)) if output.success() => Ok(()),
        (Ok(output), _) => Err(format!(
            "systemctl reload unbound: {}",
            output.combined().trim()
        )),
        (Err(err), _) => Err(format!("could not reload unbound: {err}")),
    }
}

fn fetch(runner: &dyn Runner, url: &str) -> Result<String, String> {
    let output = runner
        .call("curl", &["-fsSL", "--max-time", "90", url])
        .map_err(|err| format!("cannot run curl: {err}"))?;
    if output.timed_out {
        return Err("fetch timed out".to_string());
    }
    if !output.success() {
        return Err(format!("curl failed: {}", output.combined().trim()));
    }
    Ok(output.stdout)
}

/// Domains that should end up in the compiled set: everything fetched, plus the
/// hand-managed entries, minus the allowlist.
pub fn compile(allowlist: &[String], manual: &[String], fetched: &BTreeSet<String>) -> Vec<String> {
    let mut all: BTreeSet<String> = fetched.clone();
    for entry in manual {
        if let Some(domain) = normalize_domain(entry) {
            all.insert(domain);
        }
    }
    all.into_iter()
        .filter(|domain| !is_allowed(domain, allowlist))
        .take(MAX_DOMAINS)
        .collect()
}

/// True when `domain` is on the allowlist, directly or via a parent entry.
pub fn is_allowed(domain: &str, allowlist: &[String]) -> bool {
    allowlist.iter().any(|entry| {
        let allowed = entry.trim().trim_end_matches('.').to_ascii_lowercase();
        if allowed.is_empty() {
            return false;
        }
        domain == allowed || domain.ends_with(&format!(".{allowed}"))
    })
}

/// The unbound include: one `always_nxdomain` local zone per domain.
pub fn render_local_zones(domains: &[String]) -> String {
    let mut out = String::with_capacity(domains.len() * 48 + 128);
    out.push_str("# Generated by stack-stack from the configured blocklists.\n");
    out.push_str("# Manual edits are overwritten on the next refresh.\n");
    for domain in domains {
        out.push_str("local-zone: \"");
        out.push_str(domain);
        out.push_str(".\" always_nxdomain\n");
    }
    out
}

/// `true` when `domain` is in the compiled include.
pub fn is_blocked(config_path: &Path, allowlist: &[String], domain: &str) -> bool {
    let Some(domain) = normalize_domain(domain) else {
        return false;
    };
    if is_allowed(&domain, allowlist) {
        return false;
    }
    let needle = format!("\"{domain}.\"");
    std::fs::read_to_string(compiled_path(config_path))
        .map(|body| body.lines().any(|line| line.contains(&needle)))
        .unwrap_or(false)
}

/// Pull blocking domains out of a hosts file or a plain domain list.
pub fn parse_domains(text: &str, out: &mut BTreeSet<String>) {
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('!') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };

        // Hosts format is `<address> <name> [name…]`; a plain list is just the
        // name on its own line.
        let candidates: Vec<&str> = if looks_like_address(first) {
            fields.collect()
        } else {
            vec![first]
        };
        for candidate in candidates {
            if let Some(domain) = normalize_domain(candidate) {
                out.insert(domain);
            }
        }
    }
}

/// A hosts-file address, as opposed to a domain: an IPv4 quad or anything with a
/// colon (IPv6). A bare token like `dead.beef` must not be mistaken for one.
fn looks_like_address(token: &str) -> bool {
    if token.contains(':') {
        return true;
    }
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 3 && part.chars().all(|c| c.is_ascii_digit())
        })
}

/// Case-fold, drop the root dot, and reject anything that is not a plausible
/// DNS name. This is the only place a list entry becomes a config line, so it is
/// also the guard against an entry that would break the unbound file.
pub fn normalize_domain(raw: &str) -> Option<String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.len() > 253
        || domain.starts_with('.')
        || domain.contains("..")
        || !domain.contains('.')
        || domain.contains('/')
        || domain.contains(':')
        || domain.contains('*')
        || domain.contains('"')
        || domain.contains('\\')
        || domain.contains(' ')
        || looks_like_address(&domain)
        || NEVER_BLOCK.contains(&domain.as_str())
    {
        return None;
    }
    // Labels: letters, digits and hyphens, not starting or ending with a hyphen.
    let labels_ok = domain.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    labels_ok.then_some(domain)
}

/// Reject anything that is not a plain http(s) URL, so a stored source cannot
/// turn into a `file://` read or an option passed to curl.
pub fn validate_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("url is required".to_string());
    }
    if trimmed.len() > 2048 {
        return Err("url is too long".to_string());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("url must start with http:// or https://".to_string());
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("url must not contain whitespace or control characters".to_string());
    }
    let host = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or("")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if host.is_empty() {
        return Err("url has no host".to_string());
    }
    Ok(())
}

/// True when the daily refresh should run: after the configured local hour and
/// at least [`REFRESH_AFTER_S`] since the last success.
pub fn refresh_due(last: Option<u64>, now: u64, local_hour: i32) -> bool {
    if local_hour < REFRESH_HOUR {
        return false;
    }
    last.map(|last| now.saturating_sub(last) >= REFRESH_AFTER_S)
        .unwrap_or(true)
}

/// Local wall-clock hour, so "03:00" means the operator's 03:00.
#[cfg(unix)]
pub fn local_hour() -> i32 {
    // SAFETY: `time` and `localtime_r` are called with valid pointers; the `tm`
    // is zeroed before use and only read after a non-null return.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        tm.tm_hour
    }
}

#[cfg(not(unix))]
pub fn local_hour() -> i32 {
    0
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

    fn set(entries: &[&str]) -> BTreeSet<String> {
        entries.iter().map(|entry| (*entry).to_string()).collect()
    }

    #[test]
    fn parses_hosts_and_plain_lists() {
        let text = "\
# comment
! adblock-style comment
0.0.0.0 ads.example.com
127.0.0.1 tracker.example.com # inline comment
::1 ipv6.example.com
0.0.0.0 localhost
plain.example.com
another.example.net
";
        let mut out = BTreeSet::new();
        parse_domains(text, &mut out);
        assert!(out.contains("ads.example.com"));
        assert!(out.contains("tracker.example.com"));
        assert!(out.contains("ipv6.example.com"));
        assert!(out.contains("plain.example.com"));
        assert!(out.contains("another.example.net"));
        // Loopback names from the hosts preamble must never be blocked.
        assert!(!out.contains("localhost"));
    }

    #[test]
    fn normalization_rejects_unusable_entries() {
        assert_eq!(
            normalize_domain("Ads.Example.COM."),
            Some("ads.example.com".to_string())
        );
        for bad in [
            "",
            "nodot",
            ".leading",
            "double..dot",
            "has space.com",
            "path/thing.com",
            "1.2.3.4",
            "wild*card.com",
            "\"quote\".com",
            "localhost",
        ] {
            assert!(
                normalize_domain(bad).is_none(),
                "{bad:?} should be rejected"
            );
        }
        // A hyphen inside a label is fine; at the edge it is not.
        assert_eq!(
            normalize_domain("a-b.example.com"),
            Some("a-b.example.com".to_string())
        );
        assert!(normalize_domain("-bad.example.com").is_none());
        assert!(normalize_domain("bad-.example.com").is_none());
    }

    #[test]
    fn allowlist_matches_subdomains_too() {
        let allow = vec!["example.com".to_string(), "other.net".to_string()];
        assert!(is_allowed("example.com", &allow));
        assert!(is_allowed("ads.example.com", &allow));
        assert!(!is_allowed("notexample.com", &allow));
        assert!(!is_allowed("example.org", &allow));
    }

    #[test]
    fn compile_dedupes_and_subtracts_the_allowlist() {
        let fetched = set(&["ads.example.com", "tracker.example.com"]);
        let manual = vec![
            "manual.example.net".to_string(),
            "ads.example.com".to_string(),
        ];
        let allow = vec!["tracker.example.com".to_string()];

        let compiled = compile(&allow, &manual, &fetched);
        assert!(compiled.contains(&"ads.example.com".to_string()));
        assert!(compiled.contains(&"manual.example.net".to_string()));
        assert!(!compiled.contains(&"tracker.example.com".to_string()));
        // Sorted and duplicate-free.
        let mut sorted = compiled.clone();
        sorted.sort();
        assert_eq!(compiled, sorted);
    }

    #[test]
    fn rendered_zones_are_unbound_local_zones() {
        let rendered = render_local_zones(&["ads.example.com".to_string()]);
        assert!(rendered.contains("local-zone: \"ads.example.com.\" always_nxdomain"));
        // Header only when there is nothing to block.
        let empty = render_local_zones(&[]);
        assert!(!empty.contains("local-zone"));
    }

    #[test]
    fn url_validation_only_allows_http() {
        assert!(validate_url("https://example.com/hosts").is_ok());
        assert!(validate_url("http://example.com").is_ok());
        for bad in [
            "",
            "file:///etc/passwd",
            "ftp://x/y",
            "https://",
            "https://a b.com",
        ] {
            assert!(validate_url(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn refresh_waits_for_the_configured_hour_and_interval() {
        let now = 1_000_000_000;
        // Before 03:00 local: never due.
        assert!(!refresh_due(None, now, 2));
        // After 03:00 with no previous run: due.
        assert!(refresh_due(None, now, 3));
        // Ran recently: not due yet.
        assert!(!refresh_due(Some(now - 60), now, 4));
        // Ran more than 20 hours ago: due.
        assert!(refresh_due(Some(now - 21 * 3600), now, 4));
    }

    #[test]
    fn looks_like_address_distinguishes_hosts_from_domains() {
        assert!(looks_like_address("0.0.0.0"));
        assert!(looks_like_address("127.0.0.1"));
        assert!(looks_like_address("::1"));
        assert!(!looks_like_address("example.com"));
        assert!(!looks_like_address("dead.beef"));
        assert!(!looks_like_address("1.2.3"));
    }
}
