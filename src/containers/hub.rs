//! Docker Hub repository search.
//!
//! Mirrors the small subset of `hub.docker.com/v2/search/repositories` the App
//! Store needs. There is deliberately no HTTP client dependency in this crate
//! (see `Cargo.toml`), so the request goes through the `curl` binary — already a
//! hard deployment dependency — exactly like every other external tool. The
//! transport sits behind [`HubHttp`], so tests never touch the network and a
//! missing `curl` is a reported, non-fatal state rather than a silent failure.
//!
//! Docker Hub rate-limits unauthenticated searches. Results are cached in memory
//! for [`CACHE_TTL`] and the `Retry-After` / `X-RateLimit-*` headers are honoured
//! so a burst of keystrokes cannot burn the quota.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::ApiError;
use crate::proc;

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

/// A single HTTP response, reduced to what the search needs.
#[derive(Debug, Clone, Default)]
pub struct HttpReply {
    pub status: u16,
    pub body: String,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The transport the search runs over. Implemented by [`CurlHttp`] in
/// production and by a stub in tests.
pub trait HubHttp: Send + Sync {
    /// False when the underlying tool is unavailable, so the UI can disable the
    /// search rather than showing an empty result list.
    fn available(&self) -> bool;
    fn get(&self, url: &str) -> Result<HttpReply, String>;
}

/// `curl`-backed transport.
pub struct CurlHttp {
    available: bool,
}

impl CurlHttp {
    pub fn new() -> Self {
        Self {
            available: command_exists("curl"),
        }
    }
}

impl Default for CurlHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl HubHttp for CurlHttp {
    fn available(&self) -> bool {
        self.available
    }

    fn get(&self, url: &str) -> Result<HttpReply, String> {
        let args = vec![
            "-sS".to_string(),
            // Include response headers so the rate-limit fields can be read.
            "-i".to_string(),
            // Follow redirects; the final response's headers are the ones parsed.
            "-L".to_string(),
            "--max-time".to_string(),
            FETCH_TIMEOUT.as_secs().to_string(),
            "-A".to_string(),
            "stack-stack/0.1".to_string(),
            "-H".to_string(),
            "Accept: application/json".to_string(),
            url.to_string(),
        ];
        let output = proc::run("curl", &args, None, FETCH_TIMEOUT + Duration::from_secs(2))
            .map_err(|err| format!("cannot run curl: {err}"))?;
        if output.timed_out {
            return Err("curl timed out".to_string());
        }
        // curl exits non-zero for a transport error but still for HTTP >= 400
        // unless `--fail`; we do not pass `--fail`, so a non-zero code here is a
        // real transport failure.
        if !output.success() && output.stdout.is_empty() {
            return Err(format!("curl failed: {}", output.stderr.trim()));
        }
        Ok(parse_http_dump(&output.stdout))
    }
}

/// `true` when `program` is on `PATH`. Used to report a missing `curl` up front.
pub fn command_exists(program: &str) -> bool {
    proc::run(
        program,
        &["--version".to_string()],
        None,
        Duration::from_secs(5),
    )
    .map(|output| !output.timed_out)
    .unwrap_or(false)
}

/// Parse `curl -i` output into status, headers and body. A redirect adds extra
/// header blocks; the last one (and the text after it) wins.
pub fn parse_http_dump(raw: &str) -> HttpReply {
    let normalized = raw.replace("\r\n", "\n");
    let mut reply = HttpReply::default();
    let mut rest = normalized.as_str();

    while let Some(header_end) = rest.find('\n') {
        let status_line = &rest[..header_end];
        if !status_line.starts_with("HTTP/") {
            break;
        }
        reply.status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);

        let Some(block_end) = rest.find("\n\n") else {
            // Headers only, no body.
            reply.headers = parse_header_block(&rest[header_end + 1..]);
            break;
        };
        reply.headers = parse_header_block(&rest[header_end + 1..block_end]);
        rest = &rest[block_end + 2..];

        // Another `HTTP/` line means this block was an intermediate redirect.
        if !rest.starts_with("HTTP/") {
            reply.body = rest.to_string();
            break;
        }
    }
    reply
}

fn parse_header_block(block: &str) -> Vec<(String, String)> {
    block
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect()
}

/// Parsed rate-limit state from the response headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimit {
    pub remaining: Option<u64>,
    pub reset_epoch_s: Option<u64>,
    pub retry_after_s: Option<u64>,
}

impl RateLimit {
    fn from_headers(reply: &HttpReply) -> Self {
        let parse = |name: &str| {
            reply
                .header(name)
                .and_then(|value| value.trim().parse::<u64>().ok())
        };
        Self {
            remaining: parse("x-ratelimit-remaining"),
            reset_epoch_s: parse("x-ratelimit-reset"),
            retry_after_s: parse("retry-after"),
        }
    }
}

struct CacheEntry {
    at: Instant,
    repos: Vec<HubRepo>,
}

/// Process-wide search cache, caller and rate-limit gate.
#[derive(Clone)]
pub struct HubSearch {
    http: Arc<dyn HubHttp>,
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
    /// Wall-clock instant before which no upstream call is attempted, set from
    /// `Retry-After` / `X-RateLimit-Reset` after a 429.
    retry_until: Arc<Mutex<Option<Instant>>>,
}

impl HubSearch {
    pub fn new(http: Arc<dyn HubHttp>) -> Self {
        Self {
            http,
            cache: Arc::new(Mutex::new(HashMap::new())),
            retry_until: Arc::new(Mutex::new(None)),
        }
    }

    /// False when the transport is unavailable (no `curl`), so the UI can say so.
    pub fn available(&self) -> bool {
        self.http.available()
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
        // A fresh cache hit is served even while rate-limited: it costs nothing.
        if let Some(repos) = self.cached(&key).await {
            return Ok(repos);
        }

        if !self.http.available() {
            return Err(ApiError::unavailable(
                "Docker Hub search is unavailable: `curl` was not found on this host",
            ));
        }

        // Respect an active backoff before spending another request.
        if let Some(wait) = self.backoff_remaining().await {
            return Err(ApiError::too_many_requests(format!(
                "Docker Hub rate-limited. Retry in {}s",
                wait.max(1)
            )));
        }

        let url = format!(
            "https://hub.docker.com/v2/search/repositories/?query={}&page_size={PAGE_SIZE}",
            encode_query(query)
        );
        let reply = {
            let http = self.http.clone();
            tokio::task::spawn_blocking(move || http.get(&url))
                .await
                .map_err(|err| ApiError::internal(format!("search task failed: {err}")))?
                .map_err(ApiError::unavailable)?
        };

        let rate = RateLimit::from_headers(&reply);

        if reply.status == 429 {
            self.set_backoff(&rate).await;
            let wait = rate
                .retry_after_s
                .or_else(|| rate.reset_epoch_s.map(seconds_until_epoch))
                .unwrap_or(CACHE_TTL.as_secs());
            tracing::warn!(
                remaining = rate.remaining,
                retry_after_s = rate.retry_after_s,
                "docker hub rate-limited the search"
            );
            return Err(ApiError::too_many_requests(format!(
                "Docker Hub rate-limited. Retry in {}s",
                wait.max(1)
            )));
        }

        if reply.status == 404 {
            // No results is a 200 with an empty array on this endpoint; a 404 is
            // something else, so surface it rather than pretending "no results".
            return Err(ApiError::unavailable(
                "Docker Hub returned 404 for the search endpoint",
            ));
        }
        if !(200..300).contains(&reply.status) {
            return Err(ApiError::unavailable(format!(
                "Docker Hub search failed with HTTP {}",
                reply.status
            )));
        }

        if let Some(remaining) = rate.remaining {
            tracing::debug!(remaining, "docker hub search succeeded");
        }
        let repos = parse_hub(&reply.body)?;
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

    async fn set_backoff(&self, rate: &RateLimit) {
        let seconds = rate
            .retry_after_s
            .or_else(|| rate.reset_epoch_s.map(seconds_until_epoch))
            .unwrap_or(CACHE_TTL.as_secs());
        let mut guard = self.retry_until.lock().await;
        *guard = Some(Instant::now() + Duration::from_secs(seconds.max(1)));
    }

    async fn backoff_remaining(&self) -> Option<u64> {
        let guard = self.retry_until.lock().await;
        let until = (*guard)?;
        let now = Instant::now();
        if until <= now {
            return None;
        }
        Some(until.duration_since(now).as_secs().max(1))
    }
}

/// Seconds from now until a Unix epoch second, floored at one.
fn seconds_until_epoch(epoch_s: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    epoch_s.saturating_sub(now).max(1)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stub transport that returns canned replies and counts calls.
    struct FakeHttp {
        replies: std::sync::Mutex<Vec<HttpReply>>,
        calls: AtomicUsize,
        available: bool,
    }

    impl FakeHttp {
        fn new(replies: Vec<HttpReply>) -> Arc<Self> {
            Arc::new(Self {
                replies: std::sync::Mutex::new(replies),
                calls: AtomicUsize::new(0),
                available: true,
            })
        }

        fn unavailable() -> Arc<Self> {
            Arc::new(Self {
                replies: std::sync::Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
                available: false,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl HubHttp for FakeHttp {
        fn available(&self) -> bool {
            self.available
        }

        fn get(&self, _url: &str) -> Result<HttpReply, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut replies = self.replies.lock().expect("lock");
            if replies.is_empty() {
                Ok(HttpReply {
                    status: 200,
                    body: r#"{"results":[]}"#.to_string(),
                    headers: Vec::new(),
                })
            } else {
                Ok(replies.remove(0))
            }
        }
    }

    fn ok_body() -> String {
        r#"{"results":[
            {"repo_name":"library/nginx","short_description":"Nginx","star_count":10,"pull_count":20,"is_official":true}
        ]}"#
        .to_string()
    }

    #[tokio::test]
    async fn search_returns_repos_and_caches() {
        let fake = FakeHttp::new(vec![HttpReply {
            status: 200,
            body: ok_body(),
            headers: vec![("x-ratelimit-remaining".to_string(), "42".to_string())],
        }]);
        let hub = HubSearch::new(fake.clone());

        let first = hub.search("nginx").await.expect("search");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "library/nginx");

        // A repeat query is a cache hit: no second upstream call.
        let second = hub.search("NGINX").await.expect("search cached");
        assert_eq!(second.len(), 1);
        assert_eq!(fake.calls(), 1, "second search must be served from cache");
    }

    #[tokio::test]
    async fn rate_limit_is_reported_and_respected() {
        let fake = FakeHttp::new(vec![HttpReply {
            status: 429,
            body: String::new(),
            headers: vec![("retry-after".to_string(), "30".to_string())],
        }]);
        let hub = HubSearch::new(fake.clone());

        let err = hub.search("redis").await.expect_err("rate limited");
        match err {
            ApiError::TooManyRequests(message) => {
                assert!(message.contains("Retry in"), "message was {message:?}");
            }
            other => panic!("expected TooManyRequests, got {other:?}"),
        }

        // The next distinct query must not hit the network during backoff.
        let err = hub.search("postgres").await.expect_err("still limited");
        assert!(matches!(err, ApiError::TooManyRequests(_)));
        assert_eq!(fake.calls(), 1, "backoff must suppress the second request");
    }

    #[tokio::test]
    async fn reset_header_sets_backoff_when_retry_after_missing() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
            + 45;
        let fake = FakeHttp::new(vec![HttpReply {
            status: 429,
            body: String::new(),
            headers: vec![("x-ratelimit-reset".to_string(), future.to_string())],
        }]);
        let hub = HubSearch::new(fake.clone());
        assert!(hub.search("alpine").await.is_err());
        assert!(hub.backoff_remaining().await.is_some());
    }

    #[tokio::test]
    async fn missing_transport_is_reported_clearly() {
        let hub = HubSearch::new(FakeHttp::unavailable());
        assert!(!hub.available());
        let err = hub.search("nginx").await.expect_err("unavailable");
        match err {
            ApiError::Unavailable(message) => assert!(message.contains("curl")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_search_shape() {
        let repos = parse_hub(&ok_body()).expect("parse");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "library/nginx");
        assert!(repos[0].is_official);
    }

    #[test]
    fn empty_or_malformed_bodies_degrade() {
        assert!(parse_hub("{}").expect("empty object").is_empty());
        assert!(
            parse_hub(r#"{"results": []}"#)
                .expect("empty results")
                .is_empty()
        );
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

    #[test]
    fn http_dump_parsing_reads_status_headers_and_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-RateLimit-Remaining: 7\r\n\r\n{\"results\":[]}";
        let reply = parse_http_dump(raw);
        assert_eq!(reply.status, 200);
        assert_eq!(reply.header("x-ratelimit-remaining"), Some("7"));
        assert_eq!(reply.body, "{\"results\":[]}");
    }

    #[test]
    fn http_dump_parsing_uses_the_final_redirect_block() {
        let raw = "HTTP/1.1 301 Moved\r\nLocation: https://x/\r\n\r\nHTTP/1.1 429 Too Many\r\nRetry-After: 12\r\n\r\n";
        let reply = parse_http_dump(raw);
        assert_eq!(reply.status, 429);
        assert_eq!(reply.header("retry-after"), Some("12"));
        assert!(reply.body.is_empty());
    }
}
