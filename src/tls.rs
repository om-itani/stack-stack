//! TLS front door and host dispatch.
//!
//! Two listeners sit next to the plain `:8553` admin port:
//!
//! * `:443` terminates TLS with a rustls acceptor whose certificate is chosen by
//!   SNI from an in-memory map ([`CertResolver`], refreshed whenever a route or
//!   certificate changes). After the handshake the request head decides where it
//!   goes: the admin hostname is served the embedded SPA and `/api/*` (through
//!   the same login-gated router as `:8553`), every other hostname is raw-proxied
//!   to its upstream.
//! * `:80` only ever redirects to `https://`.
//!
//! Reading the head before dispatching (rather than trusting SNI alone) is what
//! lets a bare-IP request still reach the admin UI while a proxied hostname gets
//! its bytes pumped verbatim. The head is replayed into hyper for the admin
//! path, so nothing is lost.
//!
//! Everything that touches `openssl` goes through [`crate::ca`]; this module only
//! assembles rustls structures from PEM on disk.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpListener;

use crate::ca::CaContext;
use crate::error::ApiError;
use crate::proxy::forward;
use crate::proxy::{self, ProxyConfig, ProxyRoute, ProxyTls, Upstream};
use crate::state::AppState;

/// How long a client has to send its request head before we give up.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Backoff after an accept error so a transient failure cannot spin the CPU.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(200);

/// The certificate map, consulted by rustls for every handshake.
#[derive(Clone, Default)]
pub struct CertResolver {
    by_name: Arc<RwLock<HashMap<String, Arc<CertifiedKey>>>>,
    fallback: Arc<RwLock<Option<Arc<CertifiedKey>>>>,
}

impl fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.by_name.read().map(|map| map.len()).unwrap_or(0);
        f.debug_struct("CertResolver")
            .field("names", &count)
            .finish()
    }
}

impl CertResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the map wholesale. Called after any route or certificate change.
    pub fn set(
        &self,
        by_name: HashMap<String, Arc<CertifiedKey>>,
        fallback: Option<Arc<CertifiedKey>>,
    ) {
        *self.by_name.write().expect("cert lock") = by_name;
        *self.fallback.write().expect("cert lock") = fallback;
    }

    pub fn len(&self) -> usize {
        self.by_name.read().map(|map| map.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lookup(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        self.by_name
            .read()
            .expect("cert lock")
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if let Some(name) = hello.server_name()
            && let Some(cert) = self.lookup(name)
        {
            return Some(cert);
        }
        self.fallback.read().expect("cert lock").clone()
    }
}

/// Runtime state for the TLS front door, shared through [`AppState`].
#[derive(Clone)]
pub struct TlsRuntime {
    pub admin_hostname: String,
    pub http_port: u16,
    pub https_port: u16,
    /// Explicit default certificate name, if any.
    pub default_cert: Option<String>,
    pub ca: CaContext,
    pub resolver: CertResolver,
    pub supervisor: TlsSupervisor,
}

impl TlsRuntime {
    pub fn new(config: &crate::config::Config, ca: CaContext) -> Self {
        Self {
            admin_hostname: config.tls_admin_hostname.to_ascii_lowercase(),
            http_port: config.tls_http_port,
            https_port: config.tls_https_port,
            default_cert: config.tls_default_cert.clone(),
            ca,
            resolver: CertResolver::new(),
            supervisor: TlsSupervisor::default(),
        }
    }
}

/// One port's preflight result, shown before the listener is switched on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortCheck {
    pub port: u16,
    /// `https` or `http`.
    pub role: String,
    pub available: bool,
    pub detail: String,
}

/// True for the admin hostname, a bare IP literal, or a missing host.
pub fn is_admin_host(host: &str, admin_hostname: &str) -> bool {
    let host = proxy::normalise_host(host);
    if host.is_empty() {
        return true;
    }
    if host.eq_ignore_ascii_case(admin_hostname.trim()) {
        return true;
    }
    host.parse::<IpAddr>().is_ok()
}

// ---------------------------------------------------------------------------
// Certificate loading
// ---------------------------------------------------------------------------

/// Load a PEM certificate/key pair into a rustls signing key.
pub fn load_certified(cert_path: &Path, key_path: &Path) -> Option<Arc<CertifiedKey>> {
    let cert_pem = std::fs::read(cert_path).ok()?;
    let key_pem = std::fs::read(key_path).ok()?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<_, _>>()
        .ok()?;
    if certs.is_empty() {
        return None;
    }
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &key_pem[..]).ok()??;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key).ok()?;
    Some(Arc::new(CertifiedKey::new(certs, signing)))
}

/// The leaf name `ca::issue_leaf` uses for a hostname.
fn leaf_name(hostname: &str) -> String {
    hostname.trim().to_ascii_lowercase()
}

/// Rebuild the resolver map from proxy routes, the CA directory and the router's
/// certificate directory. Cheap enough to run on every mutation.
pub fn refresh(state: &AppState) -> Result<(), ApiError> {
    let ca_dir = PathBuf::from(&state.config.ca_dir);
    let certs_dir = network_certs_dir(state);
    let (config, _) = proxy::load(std::path::Path::new(&state.config.proxy_path));

    let mut map: HashMap<String, Arc<CertifiedKey>> = HashMap::new();
    let mut fallback: Option<Arc<CertifiedKey>> = None;

    // The admin hostname's own CA leaf is the natural fallback.
    let admin_cert = load_certified(
        &ca_dir.join(format!("{}.crt", leaf_name(&state.tls.admin_hostname))),
        &ca_dir.join(format!("{}.key", leaf_name(&state.tls.admin_hostname))),
    );
    if let Some(cert) = &admin_cert {
        map.insert(state.tls.admin_hostname.clone(), Arc::clone(cert));
        fallback = Some(Arc::clone(cert));
    }

    for route in &config.routes {
        if !route.enabled {
            continue;
        }
        if let Some(cert) = route_cert(&ca_dir, &certs_dir, route) {
            map.insert(proxy::normalise_host(&route.hostname), cert);
        }
    }

    // An explicit default certificate wins over the admin leaf.
    if let Some(name) = state
        .tls
        .default_cert
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        && let Some(cert) = load_certified(
            &certs_dir.join(format!("{name}.crt")),
            &certs_dir.join(format!("{name}.key")),
        )
    {
        fallback = Some(cert);
    }

    state.tls.resolver.set(map, fallback);
    tracing::debug!(
        hosts = state.tls.resolver.len(),
        "TLS certificate map refreshed"
    );
    Ok(())
}

fn route_cert(ca_dir: &Path, certs_dir: &Path, route: &ProxyRoute) -> Option<Arc<CertifiedKey>> {
    match route.tls {
        ProxyTls::Off => None,
        ProxyTls::CaManaged => {
            let name = leaf_name(&route.hostname);
            load_certified(
                &ca_dir.join(format!("{name}.crt")),
                &ca_dir.join(format!("{name}.key")),
            )
        }
        ProxyTls::Cert => {
            let name = route.tls_cert.as_deref()?.trim();
            if name.is_empty() {
                return None;
            }
            load_certified(
                &certs_dir.join(format!("{name}.crt")),
                &certs_dir.join(format!("{name}.key")),
            )
        }
    }
}

/// The router document's certificate directory (Phase 3's certs API).
fn network_certs_dir(state: &AppState) -> PathBuf {
    let path = PathBuf::from(&state.config.network_path);
    let (config, _) = crate::networking::store::load(&path);
    PathBuf::from(config.certs.directory)
}

/// Issue the admin hostname's CA leaf if it is missing or close to expiry.
pub fn ensure_admin_cert(state: &AppState) -> Result<(), ApiError> {
    let dir = PathBuf::from(&state.config.ca_dir);
    let name = leaf_name(&state.tls.admin_hostname);
    let cert_path = dir.join(format!("{name}.crt"));
    let key_path = dir.join(format!("{name}.key"));

    if cert_path.is_file() && key_path.is_file() {
        let existing = state
            .tls
            .ca
            .list()
            .into_iter()
            .find(|leaf| leaf.name == name);
        if existing.as_ref().is_some_and(|leaf| !leaf.needs_renewal) {
            return Ok(());
        }
    }

    let ip = crate::containers::host::detect().ip;
    let ip = if ip == "0.0.0.0" { None } else { Some(ip) };
    state
        .tls
        .ca
        .issue_leaf(&state.tls.admin_hostname, &[], ip.as_deref())?;
    Ok(())
}

/// Scan every CA-managed route (and the admin hostname) and re-issue leaves
/// inside the renewal window. Returns the hostnames that were re-issued.
///
/// Called on boot and after any route mutation, so a certificate can never sit
/// expired while the box is running.
pub fn renew_due_routes(state: &AppState) -> Vec<String> {
    let mut renewed = Vec::new();
    if !state.tls.ca.runner.available("openssl", &["version"]) {
        return renewed;
    }

    let (config, _) = proxy::load(std::path::Path::new(&state.config.proxy_path));
    let mut targets: Vec<String> = config
        .routes
        .iter()
        .filter(|route| route.enabled && route.tls == ProxyTls::CaManaged)
        .map(|route| route.hostname.clone())
        .collect();
    targets.push(state.tls.admin_hostname.clone());

    let leaves = state.tls.ca.list();
    let ip = crate::containers::host::detect().ip;
    let ip = (ip != "0.0.0.0").then_some(ip);

    for hostname in targets {
        let leaf = leaves
            .iter()
            .find(|leaf| leaf.hostname.eq_ignore_ascii_case(&hostname));
        let due = leaf.is_none_or(|leaf| leaf.needs_renewal);
        if !due {
            continue;
        }
        if let Some(leaf) = leaf
            && let Some(days) = leaf.days_remaining
            && days >= 0
        {
            // Warning *before* the re-issue, so the operator sees why.
            state
                .notify
                .emit(crate::notify::NotificationRequest::warning(
                    crate::notify::event::CERT_EXPIRING,
                    format!("Certificate for {hostname} expires in {days} days"),
                    format!(
                        "{hostname} was inside the {} day renewal window and has been re-issued.",
                        crate::ca::RENEW_DAYS
                    ),
                ));
        }
        match state.tls.ca.issue_leaf(&hostname, &[], ip.as_deref()) {
            Ok(_) => {
                renewed.push(hostname.clone());
                tracing::info!(%hostname, "CA leaf re-issued");
            }
            Err(err) => {
                tracing::warn!(%hostname, %err, "could not renew the CA leaf");
            }
        }
    }

    if !renewed.is_empty()
        && let Err(err) = refresh(state)
    {
        tracing::warn!(%err, "could not refresh the TLS map after renewal");
    }
    renewed
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

/// Check that the front-door ports can be bound, naming what is in the way.
pub fn preflight(state: &AppState) -> Vec<PortCheck> {
    vec![
        check_port(state.tls.https_port, "https"),
        check_port(state.tls.http_port, "http"),
    ]
}

fn check_port(port: u16, role: &str) -> PortCheck {
    match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(listener) => {
            drop(listener);
            PortCheck {
                port,
                role: role.to_string(),
                available: true,
                detail: format!("port {port} is free"),
            }
        }
        Err(err) => PortCheck {
            port,
            role: role.to_string(),
            available: false,
            detail: format!(
                "port {port} is already in use ({err}); stop the conflicting service first{}",
                conflicting_unit(port)
                    .map(|unit| format!(" (looks like {unit})"))
                    .unwrap_or_default()
            ),
        },
    }
}

/// Best-effort: name the unit holding a port, so the operator knows what to stop.
fn conflicting_unit(port: u16) -> Option<String> {
    let output = std::process::Command::new("ss")
        .args(["-ltnpH", &format!("sport = :{port}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // `users:(("nginx",pid=1,fd=6))`
    let start = text.find("((\"")?;
    let rest = &text[start + 3..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// Listeners
// ---------------------------------------------------------------------------

/// Owns the two listener tasks so the front door can be started and stopped at
/// runtime (the Settings toggle) without a service restart.
#[derive(Clone, Default)]
pub struct TlsSupervisor {
    inner: Arc<tokio::sync::Mutex<Option<Supervisor>>>,
}

struct Supervisor {
    http: tokio::task::JoinHandle<()>,
    https: tokio::task::JoinHandle<()>,
}

impl TlsSupervisor {
    pub async fn running(&self) -> bool {
        let guard = self.inner.lock().await;
        guard
            .as_ref()
            .is_some_and(|s| !s.http.is_finished() && !s.https.is_finished())
    }

    /// Bind and serve both ports. Idempotent while already running.
    pub async fn start(&self, state: AppState) -> Result<(), ApiError> {
        {
            let guard = self.inner.lock().await;
            if let Some(supervisor) = guard.as_ref()
                && !supervisor.http.is_finished()
                && !supervisor.https.is_finished()
            {
                return Ok(());
            }
        }

        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|err| ApiError::internal(format!("cannot configure TLS: {err}")))?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(state.tls.resolver.clone()));
        // HTTP/1.1 only: the admin app and the raw proxy are both HTTP/1.1.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

        // Bind both before spawning so a failure on either leaves no half-open
        // front door.
        let https = TcpListener::bind(("0.0.0.0", state.tls.https_port))
            .await
            .map_err(|err| {
                ApiError::unavailable(format!(
                    "cannot bind :{} for HTTPS: {err}",
                    state.tls.https_port
                ))
            })?;
        let http = TcpListener::bind(("0.0.0.0", state.tls.http_port))
            .await
            .map_err(|err| {
                ApiError::unavailable(format!(
                    "cannot bind :{} for the HTTP redirect: {err}",
                    state.tls.http_port
                ))
            })?;

        let router = crate::web::router(state.clone());
        let http_task = tokio::spawn(accept_http_redirect(state.clone(), http));
        let https_task = tokio::spawn(accept_https(state.clone(), https, acceptor, router));

        let mut guard = self.inner.lock().await;
        *guard = Some(Supervisor {
            http: http_task,
            https: https_task,
        });
        Ok(())
    }

    /// Stop both listeners and release the ports.
    pub async fn stop(&self) {
        let taken = self.inner.lock().await.take();
        if let Some(supervisor) = taken {
            supervisor.http.abort();
            supervisor.https.abort();
            let _ = supervisor.http.await;
            let _ = supervisor.https.await;
        }
    }
}

async fn accept_https(
    state: AppState,
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    router: axum::Router,
) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(%err, "https accept failed");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let _ = tcp.set_nodelay(true);
        let state = state.clone();
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            match acceptor.accept(tcp).await {
                Ok(tls) => dispatch(state, tls, peer, router).await,
                Err(err) => tracing::debug!(%err, peer = %peer, "TLS handshake failed"),
            }
        });
    }
}

async fn dispatch<S>(state: AppState, mut tls: S, peer: SocketAddr, router: axum::Router)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let head = match tokio::time::timeout(HEAD_TIMEOUT, forward::read_head(&mut tls)).await {
        Ok(Ok(Some(head))) => head,
        Ok(Ok(None)) => return,
        Ok(Err(err)) => {
            tracing::debug!(%err, peer = %peer, "cannot read request head");
            return;
        }
        Err(_) => {
            let _ = write_plain(&mut tls, 408, "Request Timeout", "no request was sent").await;
            return;
        }
    };
    let (raw, offset, parsed) = head;
    let host = parsed.host().unwrap_or_default();

    if is_admin_host(&host, &state.tls.admin_hostname) {
        serve_admin(router, PrefixedIo::new(raw, tls)).await;
        return;
    }

    let (config, _) = proxy::load(std::path::Path::new(&state.config.proxy_path));
    let Some(route) = config.find(&host) else {
        let _ = write_plain(
            &mut tls,
            421,
            "Misdirected Request",
            &format!("no proxy route is configured for {host:?}"),
        )
        .await;
        return;
    };

    match resolve_upstream(&state, route, &config).await {
        Ok(upstream) => {
            tracing::debug!(
                %host,
                %upstream,
                upgrade = parsed.is_upgrade(),
                "proxy connection"
            );
            let context = forward::ForwardContext {
                upstream,
                original_host: &host,
                proto: "https",
                client_ip: Some(peer.ip()),
            };
            if let Err(err) = forward::forward(tls, &raw, offset, &parsed, &context).await {
                tracing::debug!(%err, %host, "proxy forward failed");
            }
        }
        Err(detail) => {
            let body = serde_json::json!({ "error": detail }).to_string();
            let _ = forward::write_error(&mut tls, 502, "Bad Gateway", &body).await;
        }
    }
}

/// Resolve a route's upstream to a concrete socket address.
pub async fn resolve_upstream(
    state: &AppState,
    route: &ProxyRoute,
    _config: &ProxyConfig,
) -> Result<SocketAddr, String> {
    match &route.upstream {
        Upstream::Url { url } => {
            let parsed = proxy::parse_upstream_url(url)
                .ok_or_else(|| format!("upstream URL {url:?} is not usable"))?;
            let addr = format!("{}:{}", parsed.host, parsed.port);
            tokio::net::lookup_host(&addr)
                .await
                .map_err(|err| format!("cannot resolve {addr}: {err}"))?
                .next()
                .ok_or_else(|| format!("{addr} did not resolve to an address"))
        }
        Upstream::Container { id, port } => {
            let containers = state
                .docker
                .list("", &[], &[])
                .await
                .map_err(|err| format!("cannot ask Docker about {id:?}: {err}"))?;
            let container = containers
                .iter()
                .find(|c| c.id == *id || c.name == *id)
                .ok_or_else(|| format!("container {id:?} was not found"))?;
            let public = choose_public_port(container, *port)
                .ok_or_else(|| format!("container {id:?} has no published port to forward to"))?;
            let ip = crate::containers::host::detect().ip;
            let addr = format!("{ip}:{public}");
            addr.parse::<SocketAddr>()
                .map_err(|err| format!("{addr} is not a usable upstream address: {err}"))
        }
    }
}

/// Pick the host port to forward to: the requested private port if given,
/// otherwise the container's web port, otherwise the first published TCP port.
fn choose_public_port(
    container: &crate::containers::model::Container,
    requested: Option<u16>,
) -> Option<u16> {
    let published: Vec<&crate::containers::model::ContainerPort> = container
        .ports
        .iter()
        .filter(|port| port.public.is_some())
        .filter(|port| port.protocol.eq_ignore_ascii_case("tcp"))
        .collect();
    if published.is_empty() {
        return None;
    }

    if let Some(requested) = requested {
        return published
            .iter()
            .find(|port| port.private == requested)
            .and_then(|port| port.public);
    }
    published.first().and_then(|port| port.public)
}

async fn serve_admin<S>(router: axum::Router, io: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use tower::ServiceExt;

    let service = service_fn(move |request: hyper::Request<Incoming>| {
        let router = router.clone();
        async move {
            let (mut parts, body) = request.into_parts();
            parts.headers.insert(
                axum::http::header::HeaderName::from_static("x-forwarded-proto"),
                axum::http::header::HeaderValue::from_static("https"),
            );
            let request = hyper::Request::from_parts(parts, body);
            let response = router
                .oneshot(request)
                .await
                .unwrap_or_else(|err| match err {});
            Ok::<_, std::convert::Infallible>(response)
        }
    });

    let connection = hyper::server::conn::http1::Builder::new()
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .with_upgrades();
    if let Err(err) = connection.await {
        tracing::debug!(%err, "admin TLS connection ended");
    }
}

async fn accept_http_redirect(state: AppState, listener: TcpListener) {
    let https_port = state.tls.https_port;
    loop {
        let (mut tcp, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(%err, "http accept failed");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            let read = match tokio::time::timeout(
                Duration::from_secs(5),
                tokio::io::AsyncReadExt::read(&mut tcp, &mut buffer),
            )
            .await
            {
                Ok(Ok(read)) if read > 0 => read,
                _ => return,
            };
            let request = String::from_utf8_lossy(&buffer[..read]);
            let response = redirect_response(&request, https_port);
            let _ = tcp.write_all(response.as_bytes()).await;
            let _ = tcp.flush().await;
        });
    }
}

/// Build the `301` that sends a plain-HTTP request to HTTPS.
pub fn redirect_response(request: &str, https_port: u16) -> String {
    let mut lines = request.lines();
    let target = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let host = lines
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .starts_with("host:")
                .then(|| line[5..].trim().to_string())
        })
        .unwrap_or_default();

    // Strip any `:80` the client sent; keep the authority otherwise.
    let host = proxy::normalise_host(&host);
    let authority = if https_port == 443 {
        host.clone()
    } else {
        format!("{host}:{https_port}")
    };
    let target = if target.starts_with('/') { target } else { "/" };
    let location = format!("https://{authority}{target}");

    format!(
        "HTTP/1.1 301 Moved Permanently\r\n\
         location: {location}\r\n\
         content-length: 0\r\n\
         connection: close\r\n\
         strict-transport-security: max-age=31536000\r\n\
         \r\n"
    )
}

/// Minimal plain HTTP error for the pre-hyper paths.
async fn write_plain<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    reason: &str,
    message: &str,
) -> io::Result<()> {
    let body = serde_json::json!({ "error": message }).to_string();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

// ---------------------------------------------------------------------------
// PrefixedIo: replay buffered bytes, then delegate
// ---------------------------------------------------------------------------

/// An `AsyncRead`/`AsyncWrite` that yields a buffered prefix before the
/// underlying stream. Used to hand hyper a head we already consumed while
/// dispatching by `Host`.
struct PrefixedIo<S> {
    prefix: Vec<u8>,
    position: usize,
    inner: S,
}

impl<S> PrefixedIo<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            position: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.position < this.prefix.len() {
            let remaining = &this.prefix[this.position..];
            let take = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..take]);
            this.position += take;
            if this.position >= this.prefix.len() {
                this.prefix = Vec::new();
                this.position = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn admin_host_matching_covers_hostname_ips_and_empty() {
        assert!(is_admin_host("stack-stack.lan", "stack-stack.lan"));
        assert!(is_admin_host("STACK-STACK.LAN:443", "stack-stack.lan"));
        assert!(is_admin_host("192.168.1.1", "stack-stack.lan"));
        assert!(is_admin_host("", "stack-stack.lan"));
        assert!(!is_admin_host("app.lan", "stack-stack.lan"));
    }

    #[test]
    fn redirect_rewrites_to_https_and_drops_the_plain_port() {
        let request = "GET /files?x=1 HTTP/1.1\r\nHost: stack-stack.lan\r\n\r\n";
        let response = redirect_response(request, 443);
        assert!(response.starts_with("HTTP/1.1 301"));
        assert!(response.contains("location: https://stack-stack.lan/files?x=1\r\n"));

        let request = "GET / HTTP/1.1\r\nHost: 192.168.1.1:80\r\n\r\n";
        let response = redirect_response(request, 8443);
        assert!(response.contains("location: https://192.168.1.1:8443/\r\n"));
    }

    #[test]
    fn redirect_without_a_host_still_produces_a_location() {
        let response = redirect_response("GET /x HTTP/1.1\r\n", 443);
        assert!(response.contains("location: https:///x\r\n"));
    }

    #[tokio::test]
    async fn prefixed_io_replays_then_delegates() {
        let mut io = PrefixedIo::new(b"HEAD".to_vec(), &b"tail"[..]);
        let mut out = Vec::new();
        io.read_to_end(&mut out).await.expect("read");
        assert_eq!(out, b"HEADtail");
    }

    #[test]
    fn leaf_names_are_the_hostname() {
        assert_eq!(leaf_name("App.Lan "), "app.lan");
    }

    #[test]
    fn resolver_falls_back_when_sni_is_unknown() {
        let resolver = CertResolver::new();
        assert!(resolver.is_empty());
        resolver.set(HashMap::new(), None);
        assert!(resolver.lookup("nope").is_none());
    }
}
