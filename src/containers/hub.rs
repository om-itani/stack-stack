//! Docker Hub repository search.
//!
//! Mirrors the small subset of `hub.docker.com/v2/search/repositories` the App
//! Store needs. There is deliberately no HTTP client dependency in this crate
//! (see `Cargo.toml`), so the request goes through the `curl` binary — already a
//! hard deployment dependency — exactly like every other external tool.
//!
//! Docker Hub rate-limits unauthenticated searches, so results are cached in
//! memory for [`CACHE_TTL`]; a repeat query inside the window never leaves the
//! process.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::ApiError;

/// How long a successful search result is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(300);

/// Docker Hub caps `page_size`; 25 is what the UI shows in one panel.
const PAGE_SIZE: u32 = 25;

/// Wall-clock budget for the `curl` request.
const FETCH_TIMEOUT: Duration = Duration::from_secs(12);

/// One repository as the UI needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubRepo {
    /// Repository name, e.g. `library/nginx` or `grafana/grafana`.
    pub name: String,
    pub description: String,
    pub star_count: u64,
    pub pull_count: u64,
    pub is_official: bool,
}

struct CacheEntry {
    at: Instant,
    repos: Vec<HubRepo>,
}

/// Process-wide search cache and caller.
#[derive(Clone, Default)]
pub struct HubSearch {
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

impl HubSearch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Search Docker Hub, serving a cached result when one is fresh.
    pub async fn search(&self, query: &str) -> Result<Vec<HubRepo>, ApiError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ApiError::bad_request("search query must not be empty"));
        }
        if query.len() > 128 {
            return Err(ApiError::bad_request("search query is too long"));
        }

        let key = query.to_ascii_lowercase();
        if let Some(repos) = self.cached(&key).await {
            return Ok(repos);
        }

        let repos = fetch(query).await?;
        self.store(key, repos.clone()).await;
        Ok(repos)
    }

    async fn cached(&self, key: &str) -> Option<Vec<HubRepo>> {
        let cache = self.cache.lock().await;
        let entry = cache.get(key)?;
        (entry.at.elapsed() < CACHE_TTL).then(|| entry.repos.clone())
    }

    async fn store(&self, key: String, repos: Vec<HubRepo>) {
        let mut cache = self.cache.lock().await;
        // Keep the map from growing without bound over a long uptime; a few
        // hundred distinct queries is far more than a single operator issues.
        if cache.len() > 512 {
            cache.retain(|_, entry| entry.at.elapsed() < CACHE_TTL);
        }
        cache.insert(
            key,
            CacheEntry {
                at: Instant::now(),
                repos,
            },
        );
    }
}

/// Fetch and parse a search page. Split from [`HubSearch`] so the parser can be
/// tested without a network.
async fn fetch(query: &str) -> Result<Vec<HubRepo>, ApiError> {
    let url = format!(
        "https://hub.docker.com/v2/search/repositories/?query={}&page_size={PAGE_SIZE}",
        encode_query(query)
    );
    let args = vec![
        "-fsS".to_string(),
        "--max-time".to_string(),
        FETCH_TIMEOUT.as_secs().to_string(),
        "-H".to_string(),
        "Accept: application/json".to_string(),
        url,
    ];

    let output = tokio::task::spawn_blocking(move || {
        crate::proc::run("curl", &args, None, FETCH_TIMEOUT + Duration::from_secs(2))
    })
    .await
    .map_err(|err| ApiError::internal(format!("search task failed: {err}")))?
    .map_err(|err| ApiError::unavailable(format!("curl is not available: {err}")))?;

    if !output.success() {
        return Err(ApiError::unavailable(format!(
            "Docker Hub search failed: {}",
            output.combined().trim()
        )));
    }
    parse_hub(&output.stdout)
}

/// Parse a Docker Hub search response. Field names are read tolerantly because
/// the endpoint reports `repo_name`/`short_description` where other Hub APIs use
/// `name`/`description`.
pub fn parse_hub(body: &str) -> Result<Vec<HubRepo>, ApiError> {
    let root: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| ApiError::unavailable(format!("Docker Hub returned invalid JSON: {err}")))?;

    let results = root
        .get("results")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(results.iter().filter_map(parse_repo).collect())
}

fn parse_repo(value: &serde_json::Value) -> Option<HubRepo> {
    let name = value
        .get("repo_name")
        .or_else(|| value.get("name"))
        .and_then(|value| value.as_str())?
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    let description = value
        .get("short_description")
        .or_else(|| value.get("description"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Some(HubRepo {
        name,
        description,
        star_count: value
            .get("star_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        pull_count: value
            .get("pull_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        is_official: value
            .get("is_official")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    })
}

/// Percent-encode a query-string value. Only the RFC 3986 unreserved set is left
/// as-is; everything else (spaces, `&`, `=`, `/`, …) is escaped so a pasted
/// query cannot break out of the parameter.
fn encode_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_search_shape() {
        let body = r#"{
            "count": 2,
            "results": [
                {
                    "repo_name": "library/nginx",
                    "short_description": "Official build of Nginx.",
                    "star_count": 20000,
                    "pull_count": 1000000000,
                    "is_official": true
                },
                {
                    "name": "grafana/grafana",
                    "description": "The open observability platform.",
                    "star_count": 1234,
                    "pull_count": 500000,
                    "is_official": false
                }
            ]
        }"#;
        let repos = parse_hub(body).expect("parse");
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "library/nginx");
        assert!(repos[0].is_official);
        assert_eq!(repos[0].pull_count, 1_000_000_000);
        // The tolerant fallbacks for `name` / `description` work too.
        assert_eq!(repos[1].name, "grafana/grafana");
        assert_eq!(repos[1].description, "The open observability platform.");
        assert!(!repos[1].is_official);
    }

    #[test]
    fn empty_or_malformed_bodies_degrade() {
        assert!(parse_hub("{}").expect("empty object").is_empty());
        assert!(parse_hub(r#"{"results": []}"#).expect("empty results").is_empty());
        // A row without a name is dropped, not fatal.
        let repos = parse_hub(r#"{"results": [{"star_count": 1}]}"#).expect("nameless row");
        assert!(repos.is_empty());
        assert!(parse_hub("not json").is_err());
    }

    #[test]
    fn query_encoding_escapes_reserved_characters() {
        assert_eq!(encode_query("nginx"), "nginx");
        assert_eq!(encode_query("foo bar"), "foo%20bar");
        assert_eq!(encode_query("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_query("library/nginx"), "library%2Fnginx");
    }
}
