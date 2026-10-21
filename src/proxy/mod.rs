//! Reverse-proxy routes: the document, its validator and pure helpers.
//!
//! A route maps one hostname to one upstream (a container's published port or a
//! literal `host:port`). Routing is exact-host only — no path rewriting — which
//! keeps the forwarder a byte pump rather than a rewrite engine, and is why
//! WebSocket upgrades need no special case.
//!
//! As everywhere else in this crate: nothing is applied that has not been
//! validated, the helpers here are pure, and store reads are forgiving while a
//! save is atomic.

pub mod forward;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::ipmath::Port;
use crate::networking::model::{Problem, is_valid_hostname};
use crate::networking::store::write_atomic;

pub const DEFAULT_HOSTNAME_SUFFIX: &str = "lan";

/// How a route's certificate is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyTls {
    /// A leaf issued by the internal CA, renewed automatically.
    #[default]
    CaManaged,
    /// A named certificate from the router's certificate directory.
    Cert,
    /// Plain HTTP: no TLS termination for this hostname.
    Off,
}

/// Where a route forwards to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Upstream {
    /// A container's published port. `port` picks a specific private port when
    /// the container publishes several; otherwise the web port is preferred.
    Container {
        id: String,
        #[serde(default)]
        port: Option<u16>,
    },
    /// A literal `http://host:port` (or `https://`) target.
    Url { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRoute {
    pub id: String,
    pub hostname: String,
    pub upstream: Upstream,
    #[serde(default)]
    pub tls: ProxyTls,
    /// Certificate name when `tls == Cert`.
    #[serde(default)]
    pub tls_cert: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default)]
    pub routes: Vec<ProxyRoute>,
}

impl ProxyConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut ids: Vec<&str> = Vec::new();
        let mut hosts: Vec<String> = Vec::new();

        for (index, route) in self.routes.iter().enumerate() {
            let field = format!("routes[{index}]");
            if route.id.trim().is_empty() {
                problems.push(Problem {
                    field: format!("{field}.id"),
                    message: "route id must not be empty".to_string(),
                });
            }
            if ids.contains(&route.id.as_str()) {
                problems.push(Problem {
                    field: format!("{field}.id"),
                    message: format!("route id {:?} is used twice", route.id),
                });
            }
            ids.push(route.id.as_str());

            if !is_valid_hostname(&route.hostname) {
                problems.push(Problem {
                    field: format!("{field}.hostname"),
                    message: format!("{:?} is not a valid hostname", route.hostname),
                });
            }
            let normalised = normalise_host(&route.hostname);
            if !normalised.is_empty() && hosts.contains(&normalised) {
                problems.push(Problem {
                    field: format!("{field}.hostname"),
                    message: format!("{:?} is routed twice", route.hostname),
                });
            }
            hosts.push(normalised);

            match &route.upstream {
                Upstream::Container { id, port } => {
                    if id.trim().is_empty() {
                        problems.push(Problem {
                            field: format!("{field}.upstream.id"),
                            message: "container id must not be empty".to_string(),
                        });
                    }
                    if let Some(port) = port
                        && Port::new(*port).is_err()
                    {
                        problems.push(Problem {
                            field: format!("{field}.upstream.port"),
                            message: "container port must be 1-65535".to_string(),
                        });
                    }
                }
                Upstream::Url { url } => {
                    if parse_upstream_url(url).is_none() {
                        problems.push(Problem {
                            field: format!("{field}.upstream.url"),
                            message:
                                "upstream URL must look like http://host:port or https://host:port"
                                    .to_string(),
                        });
                    }
                }
            }

            if route.tls == ProxyTls::Cert
                && route
                    .tls_cert
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
            {
                problems.push(Problem {
                    field: format!("{field}.tls_cert"),
                    message: "choose a certificate name when TLS mode is \"cert\"".to_string(),
                });
            }
        }

        problems
    }

    /// The enabled route serving `host`, matched case-insensitively with any
    /// port stripped. Returns the first match; validation forbids duplicates.
    pub fn find(&self, host: &str) -> Option<&ProxyRoute> {
        let wanted = normalise_host(host);
        if wanted.is_empty() {
            return None;
        }
        self.routes
            .iter()
            .find(|route| route.enabled && normalise_host(&route.hostname) == wanted)
    }

    pub fn find_by_id(&self, id: &str) -> Option<&ProxyRoute> {
        self.routes.iter().find(|route| route.id == id)
    }
}

/// Strip a `:port` and lowercase, for comparisons.
pub fn normalise_host(host: &str) -> String {
    let host = host.trim();
    // IPv6 literals arrive as `[::1]:443`.
    let host = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, _)) => inner,
            None => rest,
        }
    } else {
        match host.rsplit_once(':') {
            // Only strip a port when the remainder is a single-colon host: an
            // unbracketed IPv6 address (`::1`) must survive intact.
            Some((head, tail))
                if !head.is_empty()
                    && !head.contains(':')
                    && tail.chars().all(|c| c.is_ascii_digit()) =>
            {
                head
            }
            _ => host,
        }
    };
    host.to_ascii_lowercase()
}

/// A parsed upstream target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamAddr {
    pub host: String,
    pub port: u16,
    /// True when the URL used `https`, which matters only for the log line.
    pub tls: bool,
}

/// Parse `http(s)://host[:port][/path]`. `None` for anything else.
pub fn parse_upstream_url(url: &str) -> Option<UpstreamAddr> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    let (tls, default_port) = match scheme {
        "http" => (false, 80u16),
        "https" => (true, 443u16),
        _ => return None,
    };
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            let value = port.parse::<u16>().ok()?;
            (host.to_string(), Port::new(value).ok()?.value())
        }
        _ => (authority.to_string(), default_port),
    };
    if host.is_empty() {
        return None;
    }
    Some(UpstreamAddr { host, port, tls })
}

/// Default hostname for a new route from an app or container name.
pub fn default_hostname(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        format!("app.{DEFAULT_HOSTNAME_SUFFIX}")
    } else {
        format!("{trimmed}.{DEFAULT_HOSTNAME_SUFFIX}")
    }
}

/// Load the document. Forgiving reads; a malformed file is reported.
pub fn load(path: &Path) -> (ProxyConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<ProxyConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                ProxyConfig::default(),
                Some(format!("{} is not valid proxy JSON: {err}", path.display())),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (ProxyConfig::default(), None),
        Err(err) => (
            ProxyConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &Path, config: &ProxyConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise proxy config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(id: &str, host: &str) -> ProxyRoute {
        ProxyRoute {
            id: id.to_string(),
            hostname: host.to_string(),
            upstream: Upstream::Url {
                url: "http://127.0.0.1:8080".to_string(),
            },
            tls: ProxyTls::CaManaged,
            tls_cert: None,
            enabled: true,
            comment: String::new(),
        }
    }

    #[test]
    fn default_config_validates_clean() {
        assert!(ProxyConfig::default().validate().is_empty());
    }

    #[test]
    fn duplicate_hostnames_and_bad_upstreams_are_rejected() {
        let config = ProxyConfig {
            routes: vec![
                route("a", "app.lan"),
                route("b", "APP.lan"),
                ProxyRoute {
                    id: "c".to_string(),
                    hostname: "bad host".to_string(),
                    upstream: Upstream::Url {
                        url: "ftp://nope".to_string(),
                    },
                    tls: ProxyTls::Cert,
                    tls_cert: None,
                    enabled: true,
                    comment: String::new(),
                },
            ],
        };
        let problems = config.validate();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.iter().any(|f| f.ends_with("hostname")));
        assert!(fields.iter().any(|f| f.ends_with("upstream.url")));
        assert!(fields.iter().any(|f| f.ends_with("tls_cert")));
    }

    #[test]
    fn find_matches_ignoring_case_and_port_and_disabled() {
        let mut config = ProxyConfig {
            routes: vec![route("a", "App.Lan")],
        };
        assert!(config.find("app.lan:8443").is_some());
        assert!(config.find("other.lan").is_none());
        config.routes[0].enabled = false;
        assert!(config.find("app.lan").is_none());
    }

    #[test]
    fn normalise_host_strips_ports_but_keeps_ipv6() {
        assert_eq!(normalise_host("App.Lan:443"), "app.lan");
        assert_eq!(normalise_host("app.lan"), "app.lan");
        assert_eq!(normalise_host("[::1]:443"), "::1");
        assert_eq!(normalise_host("::1"), "::1");
    }

    #[test]
    fn url_parsing_covers_defaults_and_ports() {
        assert_eq!(
            parse_upstream_url("http://127.0.0.1:8080/api"),
            Some(UpstreamAddr {
                host: "127.0.0.1".to_string(),
                port: 8080,
                tls: false
            })
        );
        assert_eq!(
            parse_upstream_url("https://box.lan"),
            Some(UpstreamAddr {
                host: "box.lan".to_string(),
                port: 443,
                tls: true
            })
        );
        for bad in ["", "ftp://x", "http://", "http://a b", "box.lan:80"] {
            assert!(parse_upstream_url(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn default_hostname_slugs_the_name() {
        assert_eq!(default_hostname("Jellyfin"), "jellyfin.lan");
        assert_eq!(default_hostname("My App_2"), "my-app-2.lan");
        assert_eq!(default_hostname("///"), "app.lan");
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ss-proxy-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("proxy.json");
        let config = ProxyConfig {
            routes: vec![route("a", "app.lan")],
        };
        save(&path, &config).expect("save");
        let (loaded, error) = load(&path);
        assert!(error.is_none());
        assert_eq!(loaded, config);
    }
}
