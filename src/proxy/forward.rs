//! The forwarder: a header-aware byte pump.
//!
//! It reads only the request *head*, rewrites the hop-relevant bits, then hands
//! the connection to `copy_bidirectional`. Everything after the head — bodies,
//! chunked encoding, and every byte of a WebSocket session — is moved verbatim,
//! which is why an upgrade works with no special case: the `Upgrade` and
//! `Connection` headers are preserved, the upstream completes its own handshake,
//! and the pump then carries frames like any other traffic.
//!
//! Deliberately no HTTP client crate is involved: the alternative would be
//! re-framing every message, which is slower and cannot carry an upgrade.

use std::io;
use std::net::{IpAddr, SocketAddr};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest request head accepted before the connection is dropped. A head this
/// large is a broken client, not a legitimate request.
pub const MAX_HEAD: usize = 64 * 1024;

/// Headers that are meaningful only to the immediate hop and must not be
/// forwarded. `Connection` and `Upgrade` are intentionally *not* here: they are
/// what makes a WebSocket handshake work through the proxy.
const HOP_BY_HOP: [&str; 3] = [
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
];

/// A parsed HTTP/1.x request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub target: String,
    pub version: String,
    /// Header names keep their original case; lookups are case-insensitive.
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `Host` header, port included if present.
    pub fn host(&self) -> Option<String> {
        self.header("host").map(|host| host.trim().to_string())
    }

    pub fn is_upgrade(&self) -> bool {
        self.header("upgrade").is_some()
    }
}

/// Read one request head from `stream`.
///
/// Returns the bytes read so far, the offset just past the head terminator, and
/// the parsed head. The bytes after `head_offset` are early body/upgrade data
/// that must be forwarded too; nothing is thrown away.
pub async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> io::Result<Option<(Vec<u8>, usize, Head)>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            // Peer closed before sending a full head.
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);

        if let Some(end) = head_end(&buffer) {
            let head = parse_head(&buffer[..end]).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "malformed request head")
            })?;
            return Ok(Some((buffer, end, head)));
        }
        if buffer.len() > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head exceeds the maximum size",
            ));
        }
    }
}

/// Byte offset just past the head terminator (`\r\n\r\n` or `\n\n`).
fn head_end(buffer: &[u8]) -> Option<usize> {
    if let Some(index) = find(buffer, b"\r\n\r\n") {
        return Some(index + 4);
    }
    find(buffer, b"\n\n").map(|index| index + 2)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parse a request head (without the terminator).
pub fn parse_head(bytes: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() || target.is_empty() {
        return None;
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }

    Some(Head {
        method,
        target,
        version,
        headers,
    })
}

/// Build the outbound head: strip hop-by-hop proxy headers, point `Host` at the
/// upstream, and annotate the forwarding chain.
pub fn rewrite_head(
    head: &Head,
    upstream: SocketAddr,
    original_host: &str,
    proto: &str,
    client_ip: Option<IpAddr>,
) -> Vec<u8> {
    let mut out = String::with_capacity(512);
    out.push_str(&head.method);
    out.push(' ');
    out.push_str(&head.target);
    out.push(' ');
    out.push_str(if head.version.is_empty() {
        "HTTP/1.1"
    } else {
        &head.version
    });
    out.push_str("\r\n");

    let upstream_host = upstream.to_string();
    let forwarded_for = match (head.header("x-forwarded-for"), client_ip) {
        (Some(existing), Some(ip)) => format!("{existing}, {ip}"),
        (Some(existing), None) => existing.to_string(),
        (None, Some(ip)) => ip.to_string(),
        (None, None) => String::new(),
    };

    for (name, value) in &head.headers {
        let lower = name.to_ascii_lowercase();
        // Replaced below.
        if lower == "host" || lower == "x-forwarded-for" || lower == "x-forwarded-proto" {
            continue;
        }
        if HOP_BY_HOP.contains(&lower.as_str()) {
            continue;
        }
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }

    out.push_str("Host: ");
    out.push_str(&upstream_host);
    out.push_str("\r\n");
    out.push_str("X-Forwarded-Host: ");
    out.push_str(original_host);
    out.push_str("\r\n");
    out.push_str("X-Forwarded-Proto: ");
    out.push_str(proto);
    out.push_str("\r\n");
    if !forwarded_for.is_empty() {
        out.push_str("X-Forwarded-For: ");
        out.push_str(&forwarded_for);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out.into_bytes()
}

/// Everything the forwarder needs besides the buffers, bundled so the argument
/// list stays readable.
pub struct ForwardContext<'a> {
    pub upstream: SocketAddr,
    /// The client's original `Host`, forwarded as `X-Forwarded-Host`.
    pub original_host: &'a str,
    /// `https` for the TLS front door, `http` otherwise.
    pub proto: &'a str,
    pub client_ip: Option<IpAddr>,
}

/// Raw-forward a connection whose head has already been read.
///
/// `raw` is everything read from the client and `head_offset` is where the head
/// ends, so the outbound head is replaced while any early body bytes are kept.
pub async fn forward<S>(
    mut client: S,
    raw: &[u8],
    head_offset: usize,
    head: &Head,
    ctx: &ForwardContext<'_>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let upstream = ctx.upstream;
    let mut server = match TcpStream::connect(upstream).await {
        Ok(server) => server,
        Err(err) => {
            let body = serde_json::json!({
                "error": format!("cannot reach upstream {upstream}"),
                "detail": err.to_string(),
            })
            .to_string();
            write_error(&mut client, 502, "Bad Gateway", &body).await;
            return Ok(());
        }
    };
    let _ = server.set_nodelay(true);

    let rewritten = rewrite_head(head, upstream, ctx.original_host, ctx.proto, ctx.client_ip);
    if let Err(err) = server.write_all(&rewritten).await {
        tracing::debug!(%err, "upstream closed before the head was sent");
        return Ok(());
    }
    if head_offset < raw.len()
        && let Err(err) = server.write_all(&raw[head_offset..]).await
    {
        tracing::debug!(%err, "upstream closed before the body was sent");
        return Ok(());
    }

    match tokio::io::copy_bidirectional(&mut client, &mut server).await {
        Ok((to_upstream, to_client)) => {
            tracing::debug!(to_upstream, to_client, "proxy connection finished");
            Ok(())
        }
        Err(err) => {
            // A reset is the normal end of many WebSocket sessions; not an error.
            tracing::debug!(%err, "proxy connection ended");
            Ok(())
        }
    }
}

/// Write a small HTTP/1.1 error response and close.
pub async fn write_error<S: AsyncWrite + Unpin>(
    client: &mut S,
    status: u16,
    reason: &str,
    body: &str,
) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n{body}",
        body.len()
    );
    let _ = client.write_all(response.as_bytes()).await;
    let _ = client.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn upstream() -> SocketAddr {
        "192.168.1.50:8096".parse().expect("addr")
    }

    #[tokio::test]
    async fn read_head_splits_head_from_early_body() {
        let raw = b"POST /api/x?y=1 HTTP/1.1\r\nHost: app.lan\r\nContent-Length: 3\r\n\r\nabc";
        let mut cursor: &[u8] = raw;
        let (buffer, offset, head) = read_head(&mut cursor).await.expect("read").expect("head");
        assert_eq!(&buffer[offset..], b"abc");
        assert_eq!(head.method, "POST");
        assert_eq!(head.target, "/api/x?y=1");
        assert_eq!(head.host().as_deref(), Some("app.lan"));
    }

    #[tokio::test]
    async fn read_head_handles_a_connection_closed_without_a_full_head() {
        let mut cursor: &[u8] = b"GET / HTTP/1.1\r\nHost: app.lan\r\n";
        let result = read_head(&mut cursor).await.expect("io");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_head_rejects_an_oversized_head() {
        let mut big = vec![b'a'; MAX_HEAD + 16];
        big.extend_from_slice(b"\r\n\r\n");
        let mut cursor: &[u8] = &big;
        let err = read_head(&mut cursor).await.expect_err("too large");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rewrite_points_host_at_the_upstream_and_keeps_upgrade_headers() {
        let head = parse_head(
            b"GET /ws HTTP/1.1\r\nHost: app.lan\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\nX-Forwarded-For: 10.0.0.9\r\nProxy-Authorization: secret\r\n",
        )
        .expect("parse");

        let out = String::from_utf8(rewrite_head(
            &head,
            upstream(),
            "app.lan",
            "https",
            Some("192.168.1.20".parse().expect("ip")),
        ))
        .expect("utf8");

        assert!(out.starts_with("GET /ws HTTP/1.1\r\n"));
        assert!(out.contains("Host: 192.168.1.50:8096\r\n"));
        assert!(out.contains("X-Forwarded-Host: app.lan\r\n"));
        assert!(out.contains("X-Forwarded-Proto: https\r\n"));
        // The existing chain is extended, not replaced.
        assert!(out.contains("X-Forwarded-For: 10.0.0.9, 192.168.1.20\r\n"));
        // Upgrade survives; the proxy-only header does not.
        assert!(out.contains("Upgrade: websocket\r\n"));
        assert!(out.contains("Connection: Upgrade\r\n"));
        assert!(!out.contains("Proxy-Authorization"));
        // Exactly one Host header pointing at the upstream.
        assert_eq!(out.matches("Host: 192.168.1.50:8096").count(), 1);
    }

    #[test]
    fn rewrite_without_a_client_ip_omits_the_forwarded_for() {
        let head = parse_head(b"GET / HTTP/1.1\r\nHost: app.lan\r\n").expect("parse");
        let out = String::from_utf8(rewrite_head(&head, upstream(), "app.lan", "http", None))
            .expect("utf8");
        assert!(!out.contains("X-Forwarded-For"));
        assert!(out.contains("X-Forwarded-Proto: http\r\n"));
    }

    #[test]
    fn parse_head_rejects_garbage() {
        assert!(parse_head(b"").is_none());
        assert!(parse_head(b"GET\r\n").is_none());
        assert!(parse_head(b"GET / HTTP/1.1\r\nBrokenHeader\r\n").is_none());
    }
}
