//! Wire types for the container / app-store surface.
//!
//! The `Serialize` shapes are mirrored field-for-field in
//! `frontend/src/api.ts`; keep the two in sync when a field changes.

use serde::{Deserialize, Serialize};

/// Where a running container came from: a stack-stack app template, or a
/// compose file somebody deployed by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerSource {
    AppStore,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPort {
    /// Port as exposed inside the container.
    pub private: u16,
    /// Host port, when the port is actually published. `None` for an exposed
    /// but unpublished port.
    pub public: Option<u16>,
    pub protocol: String,
    /// Host IP the binding is restricted to, if any.
    pub ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    /// Container name without the leading slash Docker reports.
    pub name: String,
    pub image: String,
    /// Machine state: `running`, `exited`, `paused`, …
    pub state: String,
    /// Human status line from Docker, e.g. `Up 2 hours`.
    pub status: String,
    /// Creation time, Unix seconds.
    pub created: i64,
    pub ports: Vec<ContainerPort>,
    pub source: ContainerSource,
    /// App id when `source == AppStore`.
    pub app_id: Option<String>,
    /// Ready-to-click URLs for published TCP ports, already host-qualified.
    pub web_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPort {
    pub container: u16,
    pub host: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

/// A catalog template. Parsed from the embedded seed JSON (and an optional
/// override file), then serialised to the browser with `installed` filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Short glyph (usually an emoji) shown on the card.
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub ports: Vec<AppPort>,
    /// Host port that serves the web UI, used for the "Open Web UI" button.
    #[serde(default)]
    pub web_port: Option<u16>,
    /// Compose file written verbatim on install.
    #[serde(default)]
    pub compose: String,
    /// Runtime-only flag; never read from a catalog file.
    #[serde(default, skip_deserializing)]
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIp {
    pub ip: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Terminal (exec) WebSocket protocol
// ---------------------------------------------------------------------------

/// Browser -> server. Terminal input is JSON so resize frames can share the
/// channel without a second binary encoding.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecClientMessage {
    Data { data: String },
    Resize { cols: u16, rows: u16 },
}

/// Server -> browser.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecServerMessage {
    Data { data: String },
    Exit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_source_uses_kebab_case() {
        let json = serde_json::to_string(&ContainerSource::AppStore).expect("serialize");
        assert_eq!(json, r#""app-store""#);
        let back: ContainerSource = serde_json::from_str(r#""manual""#).expect("deserialize");
        assert_eq!(back, ContainerSource::Manual);
    }

    #[test]
    fn app_installed_flag_is_not_read_from_catalog_json() {
        let app: App =
            serde_json::from_str(r#"{"id":"x","name":"X","installed":true}"#).expect("parse");
        assert!(
            !app.installed,
            "catalog files must not be able to fake runtime state"
        );
        assert_eq!(app.ports.len(), 0);
    }

    #[test]
    fn app_port_defaults_to_tcp() {
        let port: AppPort = serde_json::from_str(r#"{"container":80,"host":8080}"#).expect("parse");
        assert_eq!(port.protocol, "tcp");
    }

    #[test]
    fn exec_protocol_round_trips() {
        let msg: ExecClientMessage =
            serde_json::from_str(r#"{"type":"resize","cols":80,"rows":24}"#).expect("parse");
        match msg {
            ExecClientMessage::Resize { cols, rows } => {
                assert_eq!((cols, rows), (80, 24));
            }
            ExecClientMessage::Data { .. } => panic!("expected resize"),
        }

        let out = serde_json::to_string(&ExecServerMessage::Exit).expect("serialize");
        assert_eq!(out, r#"{"type":"exit"}"#);
    }
}
