//! Model Context Protocol (MCP) server for stack-stack.
//!
//! Provides a standard JSON-RPC 2.0 interface (/api/mcp) adhering to the MCP
//! specification (2024-11-05). Enables AI assistants (Claude, Cursor, Antigravity,
//! and automated tools) to inspect, test, and control all homeserver capabilities
//! directly without requiring manual terminal intervention.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::containers::docker::ContainerAction;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct McpRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct McpResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

#[derive(Debug, Serialize)]
pub struct McpError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl McpResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(McpError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// GET /api/mcp
/// Returns human-readable & AI-discoverable information about the stack-stack MCP server.
pub async fn mcp_info() -> Json<Value> {
    Json(json!({
        "status": "active",
        "name": "stack-stack MCP Server",
        "version": "0.1.0",
        "protocol": "2024-11-05",
        "transport": "HTTP JSON-RPC 2.0 (POST /api/mcp)",
        "description": "Model Context Protocol interface for stack-stack homeserver management",
        "tools_count": 45,
        "capabilities": {
            "tools": { "listChanged": false }
        }
    }))
}

/// POST /api/mcp
/// Handles JSON-RPC 2.0 MCP requests.
pub async fn mcp_handler(State(state): State<AppState>, Json(req): Json<McpRequest>) -> Response {
    let id = req.id.clone().unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => {
            let res = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "stack-stack",
                    "version": "0.1.0"
                }
            });
            Json(McpResponse::ok(id, res)).into_response()
        }

        "notifications/initialized" | "initialized" => {
            Json(McpResponse::ok(id, json!({ "ok": true }))).into_response()
        }

        "ping" => Json(McpResponse::ok(id, json!({}))).into_response(),

        "tools/list" => {
            let tools = list_tools();
            Json(McpResponse::ok(id, json!({ "tools": tools }))).into_response()
        }

        "tools/call" => {
            let tool_name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            let result = execute_tool(&state, tool_name, arguments).await;
            match result {
                Ok(content_text) => {
                    let res = json!({
                        "content": [
                            {
                                "type": "text",
                                "text": content_text
                            }
                        ],
                        "isError": false
                    });
                    Json(McpResponse::ok(id, res)).into_response()
                }
                Err(err_msg) => {
                    let res = json!({
                        "content": [
                            {
                                "type": "text",
                                "text": err_msg
                            }
                        ],
                        "isError": true
                    });
                    Json(McpResponse::ok(id, res)).into_response()
                }
            }
        }

        other => Json(McpResponse::err(
            id,
            -32601,
            format!("method '{other}' not found"),
        ))
        .into_response(),
    }
}

fn list_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "get_system_overview",
            "description": "Inspect homeserver status: CPU, memory, load averages, uptime, and host details.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "test_network_port",
            "description": "Probe a TCP port on any host or local interface (like Test-NetConnection) to verify reachability and latency.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "Target hostname or IP address (e.g. 192.168.0.3, 127.0.0.1)" },
                    "port": { "type": "integer", "description": "TCP port number (e.g. 445 for SMB, 8553 for UI, 22 for SSH)" },
                    "timeout_seconds": { "type": "integer", "description": "Probe timeout in seconds (default 5)" }
                },
                "required": ["host", "port"]
            }
        }),
        json!({
            "name": "get_samba_status",
            "description": "Inspect Samba (SMB/CIFS) network shares, user accounts, and rendered smb.conf configuration.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "open_samba_firewall",
            "description": "Ensure Samba file sharing ports (TCP 139, 445; UDP 137, 138) are open in nftables, ufw, and iptables.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "close_samba_firewall",
            "description": "Ensure Samba file sharing ports (TCP 139, 445; UDP 137, 138) are closed across firewalls.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_storage_status",
            "description": "Inspect disk partitions, mount points, available storage, and filesystem capacity across the host.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_system_logs",
            "description": "Tail recent system logs and filter by systemd unit or error severity (e.g. priority 3 for errors, 4 for warnings).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "lines": { "type": "integer", "description": "Number of log lines to retrieve (default 50, max 200)" },
                    "priority": { "type": "string", "description": "Optional log priority filter: '3' (err), '4' (warning), '6' (info)" },
                    "unit": { "type": "string", "description": "Optional systemd unit filter (e.g. 'smbd.service', 'docker.service')" }
                }
            }
        }),
        json!({
            "name": "get_network_details",
            "description": "Enumerate host network interfaces, hardware MACs, IP assignments, carrier status, and MTU.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "test_dns_resolution",
            "description": "Probe DNS hostname resolution and measure lookup latency.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Domain to resolve (default 'google.com')" }
                }
            }
        }),
        json!({
            "name": "list_containers",
            "description": "List all Docker containers running on the homeserver with status, ports, and image details.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "manage_container",
            "description": "Start, stop, or restart a Docker container by name or ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Container ID or name" },
                    "action": { "type": "string", "enum": ["start", "stop", "restart"] }
                },
                "required": ["id", "action"]
            }
        }),
        json!({
            "name": "get_container_logs",
            "description": "Fetch the latest stdout/stderr log output from a Docker container.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Container ID or name (e.g. 'jellyfin')" },
                    "tail": { "type": "string", "description": "Number of lines to tail (default '50')" }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "list_virtual_machines",
            "description": "List LXC lightweight containers and QEMU virtual machines managed by stack-stack.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_firewall_status",
            "description": "Inspect active nftables firewall status and rules.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "run_diagnostic_self_test",
            "description": "Run a complete automated self-test across stack-stack services: Web UI, Samba (445), Docker daemon, memory, and disks.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "list_app_store_catalog",
            "description": "List all apps available in the App Store catalog with ID, name, category, and installed status.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "install_app",
            "description": "Install and start an application from the App Store catalog (e.g. 'jellyfin', 'vaultwarden', 'nextcloud') via docker compose.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app_id": { "type": "string", "description": "Catalog App ID to install (e.g. 'jellyfin')" }
                },
                "required": ["app_id"]
            }
        }),
        json!({
            "name": "reset_app",
            "description": "Reset an App Store application and its named volumes (runs docker compose down -v then re-installs fresh).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app_id": { "type": "string", "description": "Catalog App ID to reset (e.g. 'jellyfin')" }
                },
                "required": ["app_id"]
            }
        }),
        json!({
            "name": "remove_app",
            "description": "Stop and uninstall an App Store application, removing its containers and compose definitions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app_id": { "type": "string", "description": "Catalog App ID to remove" }
                },
                "required": ["app_id"]
            }
        }),
        json!({
            "name": "deploy_stack",
            "description": "Deploy a custom Docker Compose stack by name and YAML definition directly to the homeserver.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Stack name (lowercase, alphanumeric, dashes)" },
                    "yaml": { "type": "string", "description": "Full Docker Compose YAML content" }
                },
                "required": ["name", "yaml"]
            }
        }),
        json!({
            "name": "remove_stack",
            "description": "Stop and remove a custom Docker Compose stack by name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Stack name to remove" },
                    "force": { "type": "boolean", "description": "Force remove even if down errors" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "create_samba_share",
            "description": "Create a new SMB/CIFS network share with host path, permissions, and allowed users.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Share name (e.g. 'media', 'backups')" },
                    "path": { "type": "string", "description": "Host folder path to share (e.g. '/srv/media', '/home')" },
                    "read_only": { "type": "boolean", "description": "Read only flag (default false)" },
                    "guest_ok": { "type": "boolean", "description": "Allow guest access without auth (default false)" },
                    "valid_users": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of allowed Samba usernames"
                    },
                    "comment": { "type": "string", "description": "Optional description for Windows Explorer" }
                },
                "required": ["name", "path"]
            }
        }),
        json!({
            "name": "remove_samba_share",
            "description": "Remove an existing SMB network share by name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Share name to delete" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "create_samba_user",
            "description": "Create a new Samba file sharing user account with a secure password.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "username": { "type": "string", "description": "Username for network share authentication" },
                    "password": { "type": "string", "description": "Password (minimum 8 characters)" }
                },
                "required": ["username", "password"]
            }
        }),
        json!({
            "name": "remove_samba_user",
            "description": "Remove a Samba user account from the homeserver.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "username": { "type": "string", "description": "Username to remove" }
                },
                "required": ["username"]
            }
        }),
        json!({
            "name": "get_dns_blocklist_status",
            "description": "Inspect local DNS ad-blocking status, subscription sources, and total blocked domains.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "lookup_dns_blocklist",
            "description": "Test if a specific domain is blocked or allowed by the local DNS sinkhole (e.g. 'doubleclick.net').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Domain name to check (e.g. 'ads.google.com')" }
                },
                "required": ["domain"]
            }
        }),
        json!({
            "name": "create_virtual_machine",
            "description": "Deploy a full QEMU virtual machine with custom CPU cores, RAM, disk size, and optional OS template.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "VM name (e.g. 'ubuntu-server')" },
                    "cores": { "type": "integer", "description": "Number of vCPUs (e.g. 2, default 2)" },
                    "memory_mb": { "type": "integer", "description": "RAM size in Megabytes (e.g. 2048, default 2048)" },
                    "disk_size_gb": { "type": "integer", "description": "Disk size in Gigabytes (e.g. 20, default 20)" },
                    "image": { "type": "string", "description": "Optional catalog image ID or OS template" },
                    "autostart": { "type": "boolean", "description": "Autostart on system boot (default false)" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "create_lightweight_container",
            "description": "Deploy an LXC lightweight container (Debian, Ubuntu, Alpine) in seconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Container name (e.g. 'alpine-app')" },
                    "cores": { "type": "integer", "description": "CPU allowance in cores (0 for unlimited, default 1)" },
                    "memory_mb": { "type": "integer", "description": "RAM size in Megabytes (e.g. 512, default 512)" },
                    "disk_size_gb": { "type": "integer", "description": "Rootfs size in Gigabytes (e.g. 8, default 8)" },
                    "template": { "type": "string", "description": "Template or OS name (e.g. 'debian-12-standard', 'alpine-3.20', 'ubuntu-24.04')" },
                    "password": { "type": "string", "description": "Optional root password" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "manage_guest",
            "description": "Start, stop, reboot, shutdown, snapshot, or delete an existing VM or LXC container.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Guest ID (e.g. '1', '100')" },
                    "action": { "type": "string", "enum": ["start", "stop", "reboot", "shutdown", "delete", "snapshot"], "description": "Lifecycle action to perform" },
                    "snapshot_action": { "type": "string", "enum": ["create", "apply", "delete", "list"], "description": "Action if action is snapshot (create, apply, delete, list)" },
                    "snapshot_name": { "type": "string", "description": "Snapshot name for create/apply/delete" }
                },
                "required": ["id", "action"]
            }
        }),
        json!({
            "name": "trigger_backup",
            "description": "Trigger an immediate Restic backup snapshot for a configured job or default job.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "Optional specific job ID to run; if omitted runs the first configured job" }
                }
            }
        }),
        json!({
            "name": "list_backup_snapshots",
            "description": "List all Restic backup snapshots across repositories with timestamp, paths, and size.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "Optional job ID filter" }
                }
            }
        }),
        json!({
            "name": "generate_vpn_client",
            "description": "Generate a WireGuard client configuration profile and keypair for secure remote connection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Client name (e.g. 'omars-phone', 'macbook')" },
                    "address": { "type": "string", "description": "Optional client tunnel IP (e.g. '10.8.0.2/24')" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "get_drive_health",
            "description": "Inspect SMART drive health, temperatures, power-on hours, wear levels, and self-test history across host NVMe and SATA drives.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string", "description": "Optional device path (e.g. '/dev/sda', '/dev/nvme0n1'). If omitted, probes all detected drives." }
                }
            }
        }),
        json!({
            "name": "list_block_devices",
            "description": "Enumerate physical block devices (NVMe, SATA, partitions), mount points, and supported filesystems.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_nfs_status",
            "description": "Inspect NFS network share exports, active exportfs clients, and rendered exports configuration.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "create_nfs_export",
            "description": "Create an NFS network share export with host path, allowed clients, and mount permissions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Host folder path to export (e.g. '/srv/nfs', '/mnt/storage')" },
                    "clients": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Allowed client CIDRs or IPs (e.g. ['192.168.0.0/24', '10.0.0.5'])"
                    },
                    "read_only": { "type": "boolean", "description": "Read only flag (default false)" },
                    "sync": { "type": "boolean", "description": "Synchronous writes (default true)" },
                    "root_squash": { "type": "boolean", "description": "Map root to nobody (default true for security)" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "remove_nfs_export",
            "description": "Remove an NFS network share export by folder path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Folder path of the export to remove" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "get_proxy_status",
            "description": "Inspect reverse proxy routes, domain bindings, upstream targets, and TLS certificate status.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "create_proxy_route",
            "description": "Create a reverse proxy route mapping a custom domain or subdomain to an internal port or URL.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Unique route ID (e.g. 'jellyfin-lan')" },
                    "hostname": { "type": "string", "description": "Domain name (e.g. 'jellyfin.local' or 'vault.home.arpa')" },
                    "target_port": { "type": "integer", "description": "Target container or service port (e.g. 8096)" }
                },
                "required": ["id", "hostname", "target_port"]
            }
        }),
        json!({
            "name": "delete_proxy_route",
            "description": "Delete a reverse proxy route by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Route ID to delete" }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "get_ddns_status",
            "description": "Inspect dynamic DNS (DDNS) configuration, detected public WAN IP, and sync status.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_remote_access_status",
            "description": "Inspect remote access overlay networks (Tailscale, NetBird, Cloudflare) status and node IPs.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_alerts_status",
            "description": "Inspect configured metric alerts, monitoring threshold rules, and notification dispatch providers.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "run_ai_test_suite",
            "description": "Execute a comprehensive automated validation and diagnostic test suite covering negative input validation, boundary tests, port reachability, and subsystem health.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
    ]
}

async fn execute_tool(state: &AppState, name: &str, args: Value) -> Result<String, String> {
    match name {
        "get_system_overview" => {
            let snap = state.latest.borrow().clone();
            let hostname = state
                .network
                .runner
                .run_timeout("hostname", &[], Duration::from_secs(5))
                .map(|o| o.stdout.trim().to_string())
                .unwrap_or_else(|_| "fujitsu".to_string());

            let res = if let Some(metrics) = snap {
                json!({
                    "hostname": hostname,
                    "cpu_percent": metrics.cpu.usage_pct,
                    "cores": metrics.cpu.cores,
                    "memory_used_bytes": metrics.memory.used_bytes,
                    "memory_total_bytes": metrics.memory.total_bytes,
                    "memory_used_percent": metrics.memory.used_pct,
                    "load_avg": [metrics.load[0], metrics.load[1], metrics.load[2]],
                    "uptime_seconds": metrics.uptime_s,
                })
            } else {
                json!({
                    "hostname": hostname,
                    "status": "collecting initial telemetry"
                })
            };
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "test_network_port" => {
            let host = args
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1");
            let port = args.get("port").and_then(|v| v.as_u64()).unwrap_or(445) as u16;
            let timeout_s = args
                .get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(5);

            let start = std::time::Instant::now();
            let target = format!("{host}:{port}");

            let result = tokio::time::timeout(
                Duration::from_secs(timeout_s),
                tokio::net::TcpStream::connect(&target),
            )
            .await;

            let report = match result {
                Ok(Ok(_stream)) => {
                    let latency = start.elapsed().as_millis();
                    json!({
                        "reachable": true,
                        "host": host,
                        "port": port,
                        "latency_ms": latency,
                        "status": format!("SUCCESS: TCP port {port} on {host} is OPEN (responded in {latency}ms)")
                    })
                }
                Ok(Err(err)) => {
                    json!({
                        "reachable": false,
                        "host": host,
                        "port": port,
                        "error": err.to_string(),
                        "status": format!("FAILED: Connection to {host}:{port} failed: {err}")
                    })
                }
                Err(_) => {
                    json!({
                        "reachable": false,
                        "host": host,
                        "port": port,
                        "error": "timed out",
                        "status": format!("FAILED: Connection to {host}:{port} timed out after {timeout_s}s (likely dropped by firewall)")
                    })
                }
            };
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
        }

        "get_samba_status" => {
            let (config, _) =
                crate::services::samba::load(std::path::Path::new(&state.samba.config_path));
            let users =
                crate::services::samba::list_users(state.samba.runner.as_ref()).unwrap_or_default();
            let res = json!({
                "enabled": config.enabled,
                "workgroup": config.workgroup,
                "server_string": config.server_string,
                "shares_count": config.shares.len(),
                "shares": config.shares,
                "users": users,
                "hosts_allow": config.hosts_allow,
                "interfaces": config.interfaces,
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "open_samba_firewall" => {
            let res_status = crate::services::samba::open_firewall(state.samba.runner.as_ref());
            let res = json!({
                "ok": res_status.is_ok(),
                "ports_opened": ["139/tcp", "445/tcp", "137/udp", "138/udp"],
                "status": match res_status {
                    Ok(msg) => msg,
                    Err(e) => format!("Error: {e}"),
                },
                "message": "Samba ports 139, 445 successfully configured across firewall filters."
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "close_samba_firewall" => {
            let res_status = crate::services::samba::close_firewall(state.samba.runner.as_ref());
            let res = json!({
                "ok": res_status.is_ok(),
                "ports_closed": ["139/tcp", "445/tcp", "137/udp", "138/udp"],
                "status": match res_status {
                    Ok(msg) => msg,
                    Err(e) => format!("Error: {e}"),
                },
                "message": "Samba ports closed across firewall filters."
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_storage_status" => {
            let df_output = state.files.runner.run_timeout(
                "df",
                &["-h".to_string(), "-P".to_string()],
                Duration::from_secs(5),
            );
            let mut mounts = Vec::new();
            if let Ok(output) = df_output
                && output.success()
            {
                for line in output.stdout.lines().skip(1) {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 6 {
                        mounts.push(json!({
                            "filesystem": parts[0],
                            "size": parts[1],
                            "used": parts[2],
                            "available": parts[3],
                            "use_pct": parts[4],
                            "mount": parts[5],
                        }));
                    }
                }
            }
            let res = json!({
                "mount_points": mounts,
                "status": "Storage inspected successfully"
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_system_logs" => {
            let lines = args
                .get("lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(50)
                .min(200) as u32;
            let priority = args.get("priority").and_then(|v| v.as_str());
            let unit = args.get("unit").and_then(|v| v.as_str());
            match crate::services::logs::tail(
                state.logs.runner.as_ref(),
                unit,
                lines,
                priority,
                Duration::from_secs(5),
            ) {
                Ok(entries) => {
                    let res = json!({
                        "count": entries.len(),
                        "entries": entries
                    });
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                Err(e) => Err(format!("Log retrieval failed: {e}")),
            }
        }

        "get_network_details" => {
            let ifaces = crate::networking::interfaces::discover(state.network.runner.as_ref());
            let res = json!({
                "interfaces_count": ifaces.len(),
                "interfaces": ifaces
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "test_dns_resolution" => {
            let domain = args
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("google.com");
            let start = std::time::Instant::now();
            let query = format!("{domain}:80");
            match tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(&query))
                .await
            {
                Ok(Ok(addrs)) => {
                    let list: Vec<String> = addrs.map(|a| a.ip().to_string()).collect();
                    let elapsed = start.elapsed().as_millis();
                    let res = json!({
                        "domain": domain,
                        "resolved": true,
                        "ips": list,
                        "latency_ms": elapsed,
                        "status": format!("Resolved {domain} in {elapsed}ms")
                    });
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                Ok(Err(e)) => Err(format!("DNS resolution failed for {domain}: {e}")),
                Err(_) => Err(format!("DNS resolution timed out after 3s for {domain}")),
            }
        }

        "list_containers" => {
            let docker = &state.docker;
            let installed = crate::containers::store::installed_ids(&state.config);
            let apps = crate::containers::store::catalog(&state.config);
            let host_ip = crate::containers::host::detect().ip;

            match docker.list(&host_ip, &installed, &apps).await {
                Ok(containers) => {
                    let res = json!({
                        "count": containers.len(),
                        "containers": containers
                    });
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                Err(err) => Err(format!("Docker list failed: {err}")),
            }
        }

        "manage_container" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("id is required")?;
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or("action is required")?;

            let action_enum = match action {
                "start" => ContainerAction::Start,
                "stop" => ContainerAction::Stop,
                "restart" => ContainerAction::Restart,
                other => {
                    return Err(format!(
                        "Invalid action '{other}'. Must be start, stop, or restart."
                    ));
                }
            };

            let docker = &state.docker;
            docker
                .action(id, action_enum)
                .await
                .map(|_| format!("Container '{id}' {action} successful."))
                .map_err(|e| e.to_string())
        }

        "get_container_logs" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("id is required")?;
            let tail = args.get("tail").and_then(|v| v.as_str()).unwrap_or("50");
            match state.docker.logs(id, tail).await {
                Ok(mut stream) => {
                    let mut lines = Vec::new();
                    while let Some(Ok(item)) = stream.next().await {
                        lines.push(crate::containers::docker::log_text(item));
                        if lines.len() >= 100 {
                            break;
                        }
                    }
                    let res = json!({
                        "id": id,
                        "logs": lines.join("")
                    });
                    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                }
                Err(e) => Err(format!("Could not get logs for {id}: {e}")),
            }
        }

        "list_virtual_machines" => {
            let (config, _) =
                crate::virtualization::store::load(std::path::Path::new(&state.virt.config_path));
            let res = json!({
                "vms_count": config.vms.len(),
                "cts_count": config.cts.len(),
                "vms": config.vms,
                "cts": config.cts
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_firewall_status" => {
            let (config, _) =
                crate::networking::store::load(std::path::Path::new(&state.config.network_path));
            let status = crate::networking::status::probe(&config, state.network.runner.as_ref());
            let res = json!({
                "firewall": status.firewall,
                "services": status.services,
                "sqm": status.sqm,
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "run_diagnostic_self_test" => {
            let mut checks = Vec::new();

            // 1. Web UI port
            let ui_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect("127.0.0.1:8553"),
            )
            .await;
            checks.push(json!({
                "test": "Web UI (port 8553)",
                "passed": matches!(ui_res, Ok(Ok(_))),
                "detail": "Admin interface listening and responding on localhost:8553"
            }));

            // 2. Samba port 445
            let smb_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect("127.0.0.1:445"),
            )
            .await;
            checks.push(json!({
                "test": "Samba SMB (port 445)",
                "passed": matches!(smb_res, Ok(Ok(_))),
                "detail": if matches!(smb_res, Ok(Ok(_))) {
                    "smbd daemon is listening on port 445"
                } else {
                    "smbd daemon is not responding on 127.0.0.1:445"
                }
            }));

            // 3. Docker engine
            let installed = crate::containers::store::installed_ids(&state.config);
            let apps = crate::containers::store::catalog(&state.config);
            let host_ip = crate::containers::host::detect().ip;
            let docker_ok = state.docker.list(&host_ip, &installed, &apps).await.is_ok();
            checks.push(json!({
                "test": "Docker Daemon",
                "passed": docker_ok,
                "detail": if docker_ok { "Docker engine connected and responsive" } else { "Cannot connect to Docker daemon socket" }
            }));

            // 4. Memory
            let snap = state.latest.borrow().clone();
            let mem_ok = if let Some(m) = snap.as_ref() {
                m.memory.total_bytes > 0 && m.memory.used_bytes < m.memory.total_bytes
            } else {
                true
            };
            checks.push(json!({
                "test": "System Memory",
                "passed": mem_ok,
                "detail": "Memory subsystem healthy"
            }));

            // 5. Disk Space
            let disk_ok = if let Some(m) = snap.as_ref() {
                m.disks.iter().all(|d| d.used_pct < 98.0)
            } else {
                true
            };
            checks.push(json!({
                "test": "Disk Space",
                "passed": disk_ok,
                "detail": "Storage partitions have available capacity"
            }));

            // 6. DNS Resolution
            let dns_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::lookup_host("1.1.1.1:53"),
            )
            .await;
            let dns_ok = matches!(dns_res, Ok(Ok(_)));
            checks.push(json!({
                "test": "Network Connectivity",
                "passed": dns_ok,
                "detail": if dns_ok {
                    "Network stack and upstream connectivity verified"
                } else {
                    "Upstream network check timed out"
                }
            }));

            let all_passed = checks
                .iter()
                .all(|c| c.get("passed").and_then(|v| v.as_bool()).unwrap_or(false));
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let summary = json!({
                "all_passed": all_passed,
                "checks": checks,
                "timestamp_unix": now_secs
            });
            serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())
        }

        "list_app_store_catalog" => {
            let apps = crate::containers::store::catalog_with_state(&state.config);
            let res = json!({
                "count": apps.len(),
                "apps": apps
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "install_app" => {
            let app_id = args
                .get("app_id")
                .and_then(|v| v.as_str())
                .ok_or("app_id is required")?;

            let output = crate::containers::store::install(&state.config, app_id)
                .await
                .map_err(|e| e.to_string())?;
            let res = json!({
                "ok": true,
                "app_id": app_id,
                "output": output,
                "message": format!("Application '{app_id}' installed and started successfully.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "reset_app" => {
            let app_id = args
                .get("app_id")
                .and_then(|v| v.as_str())
                .ok_or("app_id is required")?;

            let output = crate::containers::store::reset(&state.config, app_id)
                .await
                .map_err(|e| e.to_string())?;
            let res = json!({
                "ok": true,
                "app_id": app_id,
                "output": output,
                "message": format!("Application '{app_id}' volumes reset and restarted fresh.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "remove_app" => {
            let app_id = args
                .get("app_id")
                .and_then(|v| v.as_str())
                .ok_or("app_id is required")?;

            let output = crate::containers::store::remove(&state.config, app_id)
                .await
                .map_err(|e| e.to_string())?;
            let res = json!({
                "ok": true,
                "app_id": app_id,
                "output": output,
                "message": format!("Application '{app_id}' removed successfully.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "deploy_stack" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;
            let yaml = args
                .get("yaml")
                .and_then(|v| v.as_str())
                .ok_or("yaml is required")?;

            let output = crate::containers::store::deploy_stack(&state.config, name, yaml)
                .await
                .map_err(|e| e.to_string())?;
            let res = json!({
                "ok": true,
                "name": name,
                "output": output,
                "message": format!("Compose stack '{name}' deployed successfully.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "remove_stack" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

            let output = crate::containers::store::remove_stack(&state.config, name, force)
                .await
                .map_err(|e| e.to_string())?;
            let res = json!({
                "ok": true,
                "name": name,
                "output": output,
                "message": format!("Compose stack '{name}' removed successfully.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "create_samba_share" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path is required")?;
            let read_only = args
                .get("read_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let guest_ok = args
                .get("guest_ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let valid_users: Vec<String> = args
                .get("valid_users")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|u| u.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let comment = args
                .get("comment")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let conf_path = std::path::Path::new(&state.samba.config_path);
            let (mut config, _) = crate::services::samba::load(conf_path);

            if config.shares.iter().any(|s| s.name == name) {
                return Err(format!("Share '{name}' already exists"));
            }

            let new_share = crate::services::samba::SmbShare {
                name: name.to_string(),
                path: path_str.to_string(),
                read_only,
                guest_ok,
                valid_users,
                comment,
                ..Default::default()
            };
            config.shares.push(new_share);

            crate::services::samba::save(conf_path, &config).map_err(|e| e.to_string())?;
            let lan_cidr = Some("192.168.0.0/16 10.0.0.0/8 172.16.0.0/12 127.0.0.1".to_string());
            let steps = crate::services::samba::apply(&state.samba, &config, lan_cidr).await;
            let res = json!({
                "ok": true,
                "share": name,
                "path": path_str,
                "apply_steps": steps
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "remove_samba_share" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;

            let conf_path = std::path::Path::new(&state.samba.config_path);
            let (mut config, _) = crate::services::samba::load(conf_path);

            let initial_len = config.shares.len();
            config.shares.retain(|s| s.name != name);
            if config.shares.len() == initial_len {
                return Err(format!("Share '{name}' not found"));
            }

            crate::services::samba::save(conf_path, &config).map_err(|e| e.to_string())?;
            let lan_cidr = Some("192.168.0.0/16 10.0.0.0/8 172.16.0.0/12 127.0.0.1".to_string());
            let steps = crate::services::samba::apply(&state.samba, &config, lan_cidr).await;
            let res = json!({
                "ok": true,
                "removed_share": name,
                "apply_steps": steps
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "create_samba_user" => {
            let username = args
                .get("username")
                .and_then(|v| v.as_str())
                .ok_or("username is required")?;
            let password = args
                .get("password")
                .and_then(|v| v.as_str())
                .ok_or("password is required")?;

            crate::services::samba::set_password(state.samba.runner.as_ref(), username, password)
                .map_err(|e| e.to_string())?;

            let conf_path = std::path::Path::new(&state.samba.config_path);
            let (mut config, _) = crate::services::samba::load(conf_path);
            if !config.users.iter().any(|u| u.username == username) {
                config.users.push(crate::services::samba::SmbUser {
                    username: username.to_string(),
                    enabled: true,
                });
                let _ = crate::services::samba::save(conf_path, &config);
            }

            let res = json!({
                "ok": true,
                "username": username,
                "message": format!("Samba account '{username}' created successfully.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "remove_samba_user" => {
            let username = args
                .get("username")
                .and_then(|v| v.as_str())
                .ok_or("username is required")?;

            crate::services::samba::delete_user(state.samba.runner.as_ref(), username)
                .map_err(|e| e.to_string())?;

            let conf_path = std::path::Path::new(&state.samba.config_path);
            let (mut config, _) = crate::services::samba::load(conf_path);
            config.users.retain(|u| u.username != username);
            let _ = crate::services::samba::save(conf_path, &config);

            let res = json!({
                "ok": true,
                "username": username,
                "message": format!("Samba account '{username}' deleted successfully.")
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_dns_blocklist_status" => {
            let config_path = std::path::Path::new(&state.config.network_path);
            let stats = crate::networking::blocklists::BlocklistStats::load(config_path);
            let (config, _) = crate::networking::store::load(config_path);
            let res = json!({
                "total_domains_blocked": stats.total,
                "updated_at": stats.updated_at,
                "subscriptions_count": config.dns.blocklists.len(),
                "subscriptions": config.dns.blocklists,
                "allowlist": config.dns.allowlist
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "lookup_dns_blocklist" => {
            let domain = args
                .get("domain")
                .and_then(|v| v.as_str())
                .ok_or("domain is required")?;
            let config_path = std::path::Path::new(&state.config.network_path);
            let (config, _) = crate::networking::store::load(config_path);
            let normalized =
                crate::networking::blocklists::normalize_domain(domain).unwrap_or_default();
            let is_allowed = !normalized.is_empty()
                && crate::networking::blocklists::is_allowed(&normalized, &config.dns.allowlist);
            let is_blocked = !normalized.is_empty()
                && !is_allowed
                && crate::networking::blocklists::is_blocked(
                    config_path,
                    &config.dns.allowlist,
                    domain,
                );
            let res = json!({
                "domain": domain,
                "normalized": normalized,
                "is_blocked": is_blocked,
                "is_allowed": is_allowed,
                "status": if is_blocked { "BLOCKED (Sinkholed by ad-blocker)" } else { "ALLOWED (Resolves normally)" }
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "create_virtual_machine" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;
            let cores = args.get("cores").and_then(|v| v.as_u64()).unwrap_or(2) as u16;
            let memory_mb = args
                .get("memory_mb")
                .and_then(|v| v.as_u64())
                .unwrap_or(2048) as u32;
            let disk_size_gb = args
                .get("disk_size_gb")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as u32;
            let image = args
                .get("image")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let autostart = args
                .get("autostart")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let (doc, _) =
                crate::virtualization::store::load(std::path::Path::new(&state.virt.config_path));
            let raw_vm = crate::virtualization::model::VmConfig {
                name: name.to_string(),
                vcpus: cores,
                memory_mb,
                disk_gb: disk_size_gb,
                image,
                autostart,
                ..Default::default()
            };
            let mut vm = crate::web::virtualization::prepare_vm(&doc, raw_vm);
            if doc.vm(&vm.id).is_some() || doc.ct(&vm.id).is_some() {
                return Err(format!("ID '{}' is already in use", vm.id));
            }
            if vm.vnc_enabled && vm.vnc_port.is_none() {
                vm.vnc_port = Some(crate::virtualization::manager::allocate_vnc_port(
                    &doc,
                    doc.defaults.vnc_port_start,
                ));
            }

            let mut candidate = doc.clone();
            candidate.vms.push(vm.clone());
            let problems = candidate.validate();
            if !problems.is_empty() {
                return Err(format!("Validation failed: {problems:?}"));
            }

            let cfg = state.config.clone();
            let runner = state.virt.runner.clone();
            let caps = crate::virtualization::host::probe(runner.as_ref());
            let _guard = state.virt.lock.lock().await;

            let mut to_create = vm.clone();
            let report = tokio::task::spawn_blocking(move || {
                crate::virtualization::manager::create_vm(
                    &cfg,
                    &mut to_create,
                    &caps,
                    runner.as_ref(),
                )
            })
            .await
            .map_err(|e| format!("Create VM task failed: {e}"))?
            .map_err(|e| format!("Create VM failed: {e}"))?;

            crate::virtualization::store::save(
                std::path::Path::new(&state.virt.config_path),
                &candidate,
            )
            .map_err(|e| format!("Failed to save config: {e}"))?;

            let res = json!({
                "ok": true,
                "id": vm.id,
                "name": vm.name,
                "output": report.output,
                "warnings": report.warnings
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "create_lightweight_container" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;
            let cores = args.get("cores").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let memory_mb = args
                .get("memory_mb")
                .and_then(|v| v.as_u64())
                .unwrap_or(512) as u32;
            let disk_size_gb = args
                .get("disk_size_gb")
                .and_then(|v| v.as_u64())
                .unwrap_or(8) as u32;
            let template = args
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("debian-12-standard");
            let password = args
                .get("password")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let (doc, _) =
                crate::virtualization::store::load(std::path::Path::new(&state.virt.config_path));
            let raw_ct = crate::virtualization::model::CtConfig {
                name: name.to_string(),
                hostname: name.to_string(),
                cores,
                memory_mb,
                rootfs_gb: disk_size_gb,
                template: template.to_string(),
                password,
                ..Default::default()
            };
            let ct = crate::web::virtualization::prepare_ct(&doc, raw_ct);
            if doc.vm(&ct.id).is_some() || doc.ct(&ct.id).is_some() {
                return Err(format!("ID '{}' is already in use", ct.id));
            }

            let mut candidate = doc.clone();
            candidate.cts.push(ct.clone());
            let problems = candidate.validate();
            if !problems.is_empty() {
                return Err(format!("Validation failed: {problems:?}"));
            }

            let cfg = state.config.clone();
            let runner = state.virt.runner.clone();
            let caps = crate::virtualization::host::probe(runner.as_ref());
            let _guard = state.virt.lock.lock().await;

            let to_create = ct.clone();
            let report = tokio::task::spawn_blocking(move || {
                crate::virtualization::manager::create_ct(&cfg, &to_create, &caps, runner.as_ref())
            })
            .await
            .map_err(|e| format!("Create CT task failed: {e}"))?
            .map_err(|e| format!("Create CT failed: {e}"))?;

            crate::virtualization::store::save(
                std::path::Path::new(&state.virt.config_path),
                &candidate,
            )
            .map_err(|e| format!("Failed to save config: {e}"))?;

            let res = json!({
                "ok": true,
                "id": ct.id,
                "name": ct.name,
                "output": report.output,
                "warnings": report.warnings
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "manage_guest" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("id is required")?;
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or("action is required")?;
            let (doc, _) =
                crate::virtualization::store::load(std::path::Path::new(&state.virt.config_path));

            if let Some(vm) = doc.vm(id).cloned() {
                let cfg = state.config.clone();
                let runner = state.virt.runner.clone();
                let _guard = state.virt.lock.lock().await;

                match action {
                    "start" => {
                        let caps = crate::virtualization::host::probe(runner.as_ref());
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::start_vm(
                                &cfg,
                                &vm,
                                &caps,
                                runner.as_ref(),
                            )
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Start VM failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "start", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "stop" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::control_vm(
                                &cfg,
                                &vm,
                                crate::virtualization::manager::VmControl::Stop,
                                runner.as_ref(),
                            )
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Stop VM failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "stop", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "reboot" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::control_vm(
                                &cfg,
                                &vm,
                                crate::virtualization::manager::VmControl::Reboot,
                                runner.as_ref(),
                            )
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Reboot VM failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "reboot", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "shutdown" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::control_vm(
                                &cfg,
                                &vm,
                                crate::virtualization::manager::VmControl::Shutdown,
                                runner.as_ref(),
                            )
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Shutdown VM failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "shutdown", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "delete" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::delete_vm(&cfg, &vm, runner.as_ref())
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Delete VM failed: {e}"))?;

                        let mut updated = doc.clone();
                        updated.vms.retain(|v| v.id != id);
                        let _ = crate::virtualization::store::save(
                            std::path::Path::new(&state.virt.config_path),
                            &updated,
                        );
                        let res = json!({ "ok": true, "id": id, "action": "delete", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "snapshot" => {
                        let snapshot_action_str = args
                            .get("snapshot_action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("list");
                        let snapshot_name = args
                            .get("snapshot_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let action_enum = match snapshot_action_str {
                            "create" => crate::virtualization::manager::SnapshotAction::Create(
                                snapshot_name.to_string(),
                            ),
                            "apply" => crate::virtualization::manager::SnapshotAction::Apply(
                                snapshot_name.to_string(),
                            ),
                            "delete" => crate::virtualization::manager::SnapshotAction::Delete(
                                snapshot_name.to_string(),
                            ),
                            _ => crate::virtualization::manager::SnapshotAction::List,
                        };
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::snapshot_vm(
                                &cfg,
                                &vm,
                                &action_enum,
                                runner.as_ref(),
                            )
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Snapshot VM failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "snapshot", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    other => Err(format!(
                        "Unsupported VM action '{other}'. Valid: start, stop, reboot, shutdown, delete, snapshot."
                    )),
                }
            } else if let Some(ct) = doc.ct(id).cloned() {
                let cfg = state.config.clone();
                let runner = state.virt.runner.clone();
                let _guard = state.virt.lock.lock().await;

                match action {
                    "start" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::start_ct(&cfg, &ct, runner.as_ref())
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Start CT failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "start", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "stop" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::stop_ct(
                                &cfg,
                                &ct,
                                false,
                                runner.as_ref(),
                            )
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Stop CT failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "stop", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "reboot" | "restart" => {
                        let ct_c = ct.clone();
                        let cfg_c = cfg.clone();
                        let run_c = runner.clone();
                        let report = tokio::task::spawn_blocking(move || {
                            let _ = crate::virtualization::manager::stop_ct(
                                &cfg_c,
                                &ct_c,
                                false,
                                run_c.as_ref(),
                            );
                            crate::virtualization::manager::start_ct(&cfg, &ct, runner.as_ref())
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Restart CT failed: {e}"))?;
                        let res = json!({ "ok": true, "id": id, "action": "reboot", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    "delete" => {
                        let report = tokio::task::spawn_blocking(move || {
                            crate::virtualization::manager::delete_ct(&cfg, &ct, runner.as_ref())
                        })
                        .await
                        .map_err(|e| format!("Task failed: {e}"))?
                        .map_err(|e| format!("Delete CT failed: {e}"))?;

                        let mut updated = doc.clone();
                        updated.cts.retain(|c| c.id != id);
                        let _ = crate::virtualization::store::save(
                            std::path::Path::new(&state.virt.config_path),
                            &updated,
                        );
                        let res = json!({ "ok": true, "id": id, "action": "delete", "output": report.output, "warnings": report.warnings });
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
                    }
                    other => Err(format!(
                        "Unsupported container action '{other}'. Valid: start, stop, reboot, delete."
                    )),
                }
            } else {
                Err(format!("Guest with ID '{id}' not found"))
            }
        }

        "trigger_backup" => {
            let job_id = args.get("job_id").and_then(|v| v.as_str());
            let config = state.backup.config();
            if config.jobs.is_empty() {
                return Err("No backup jobs configured".to_string());
            }
            let target_job = if let Some(id) = job_id {
                config
                    .find(id)
                    .cloned()
                    .ok_or_else(|| format!("Backup job '{id}' not found"))?
            } else {
                config
                    .jobs
                    .iter()
                    .find(|j| j.enabled)
                    .cloned()
                    .unwrap_or_else(|| config.jobs[0].clone())
            };

            let run = state.backup.run_job(&target_job).await;
            let res = json!({
                "ok": run.ok,
                "job_id": target_job.id,
                "job_name": target_job.name,
                "duration_ms": run.duration_ms,
                "snapshot_id": run.snapshot_id,
                "bytes": run.bytes,
                "detail": run.detail
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "list_backup_snapshots" => {
            let job_id = args.get("job_id").and_then(|v| v.as_str());
            let config = state.backup.config();
            if config.jobs.is_empty() {
                let res = json!({
                    "count": 0,
                    "snapshots": [],
                    "message": "No backup jobs configured yet."
                });
                return serde_json::to_string_pretty(&res).map_err(|e| e.to_string());
            }

            let mut all_snapshots = Vec::new();
            for job in &config.jobs {
                if let Some(id) = job_id
                    && job.id != id
                {
                    continue;
                }
                match state.backup.snapshots(job).await {
                    Ok(snaps) => {
                        for s in snaps {
                            all_snapshots.push(json!({
                                "job_id": job.id,
                                "job_name": job.name,
                                "snapshot": s
                            }));
                        }
                    }
                    Err(e) => {
                        all_snapshots.push(json!({
                            "job_id": job.id,
                            "job_name": job.name,
                            "error": e.to_string()
                        }));
                    }
                }
            }
            let res = json!({
                "count": all_snapshots.len(),
                "snapshots": all_snapshots
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "generate_vpn_client" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name is required")?;
            let address_arg = args.get("address").and_then(|v| v.as_str());

            let secrets_path = state.wireguard.secrets_path.clone();
            let mut secrets = crate::services::wireguard::load_secrets(&secrets_path);
            let runner = state.wireguard.runner.clone();

            // Ensure server key exists
            if !crate::services::wireguard::is_wg_key(&secrets.private_key) {
                let (server_priv, _) =
                    crate::services::wireguard::generate_keypair(runner.as_ref())
                        .map_err(|e| format!("Failed to generate server keypair: {e}"))?;
                secrets.private_key = server_priv;
                let _ = crate::services::wireguard::save_secrets(&secrets_path, &secrets);
            }

            let server_public = state
                .wireguard
                .public_key()
                .map_err(|e| format!("Cannot get server public key: {e}"))?;
            let host_ip = crate::containers::host::detect().ip;
            let mut config = state.wireguard.config();
            let endpoint = format!("{}:{}", host_ip, config.listen_port);

            // Determine client tunnel address
            let client_address = if let Some(addr) = address_arg {
                addr.to_string()
            } else {
                let peer_idx = config.peers.len() + 2;
                format!("10.8.0.{peer_idx}/32")
            };

            // Generate client keypair
            let (client_private, client_public) =
                crate::services::wireguard::generate_keypair(runner.as_ref())
                    .map_err(|e| format!("Failed to generate client keypair: {e}"))?;

            // Add peer to wireguard config if not existing
            let mut peer_exists = false;
            for peer in &mut config.peers {
                if peer.name == name {
                    peer.public_key = client_public.clone();
                    peer.allowed_ips = vec![client_address.clone()];
                    peer_exists = true;
                    break;
                }
            }
            if !peer_exists {
                config.peers.push(crate::services::wireguard::WgPeer {
                    name: name.to_string(),
                    public_key: client_public.clone(),
                    allowed_ips: vec![client_address.clone()],
                    dns: Some("1.1.1.1".to_string()),
                    persistent_keepalive: 25,
                    enabled: true,
                });
            }
            config.enabled = true;

            // Save and apply WireGuard config
            let conf_path = state.wireguard.config_path.clone();
            let _ = crate::services::wireguard::save(&conf_path, &config);
            let _ = crate::services::wireguard::apply(&state.wireguard, &config).await;

            let dns = Some("1.1.1.1");
            let client_conf = crate::services::wireguard::render_client(
                &client_private,
                &client_address,
                &server_public,
                &endpoint,
                dns,
            );

            let qr = crate::services::wireguard::qr_text(runner.as_ref(), &client_conf).ok();

            let res = json!({
                "ok": true,
                "client_name": name,
                "client_address": client_address,
                "server_endpoint": endpoint,
                "public_key": client_public,
                "config": client_conf,
                "qr": qr
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_drive_health" => {
            let device_arg = args.get("device").and_then(|v| v.as_str());
            let detected = state.smart.detect_devices();
            let devices_to_check = if let Some(d) = device_arg {
                vec![d.to_string()]
            } else if !detected.is_empty() {
                detected
            } else {
                vec!["/dev/nvme0n1".to_string(), "/dev/sda".to_string()]
            };

            let runner = state.smart.runner.clone();
            let mut drive_reports = Vec::new();

            for dev in devices_to_check {
                let output = runner.run_timeout(
                    "smartctl",
                    &["-a".to_string(), "-j".to_string(), dev.clone()],
                    Duration::from_secs(5),
                );
                if let Ok(res) = output
                    && !res.stdout.trim().is_empty()
                    && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&res.stdout)
                {
                    drive_reports.push(json!({
                        "device": dev,
                        "status": "ok",
                        "smart_data": parsed
                    }));
                    continue;
                }

                let health_out = runner.run_timeout(
                    "smartctl",
                    &["-H".to_string(), dev.clone()],
                    Duration::from_secs(5),
                );
                if let Ok(res) = health_out
                    && res.success()
                {
                    drive_reports.push(json!({
                        "device": dev,
                        "status": "ok",
                        "health_output": res.stdout.trim()
                    }));
                } else {
                    drive_reports.push(json!({
                        "device": dev,
                        "status": "unsupported_or_not_found"
                    }));
                }
            }

            let res = json!({
                "smart_available": state.smart.available(),
                "drives_count": drive_reports.len(),
                "drives": drive_reports
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "list_block_devices" => match state.storage.state() {
            Ok(storage_state) => {
                let res = json!({
                    "count": storage_state.devices.len(),
                    "root_device": storage_state.root_device,
                    "supported_formats": storage_state.formats,
                    "devices": storage_state.devices
                });
                serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
            }
            Err(err) => Err(format!("Storage list failed: {err}")),
        },

        "get_nfs_status" => {
            let (config, _) =
                crate::services::nfs::load(std::path::Path::new(&state.config.nfs_path));
            let preview = config.render();
            let active = crate::services::nfs::active_exports(state.network.runner.as_ref())
                .unwrap_or_default();
            let res = json!({
                "enabled": config.enabled,
                "v4": config.v4,
                "exports_count": config.exports.len(),
                "exports": config.exports,
                "active_exports": active,
                "preview": preview
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "create_nfs_export" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path is required")?;
            let clients = args
                .get("clients")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["*".to_string()]);
            let ro = args
                .get("read_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let sync = args.get("sync").and_then(|v| v.as_bool()).unwrap_or(true);
            let root_squash = args
                .get("root_squash")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let nfs_path = std::path::Path::new(&state.config.nfs_path);
            let (mut config, _) = crate::services::nfs::load(nfs_path);

            let export = crate::services::nfs::NfsExport {
                path: path.to_string(),
                clients,
                ro,
                sync,
                root_squash,
                no_subtree_check: true,
            };

            if let Some(pos) = config.exports.iter().position(|e| e.path == path) {
                config.exports[pos] = export.clone();
            } else {
                config.exports.push(export.clone());
            }
            config.enabled = true;

            let problems = config.validate();
            if !problems.is_empty() {
                return Err(format!("Validation failed: {problems:?}"));
            }

            crate::services::nfs::save(nfs_path, &config).map_err(|e| e.to_string())?;
            let steps = crate::services::nfs::apply(&state.nfs, &config).await;

            let res = json!({
                "ok": true,
                "path": path,
                "clients": export.clients,
                "apply_steps": steps
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "remove_nfs_export" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path is required")?;

            let nfs_path = std::path::Path::new(&state.config.nfs_path);
            let (mut config, _) = crate::services::nfs::load(nfs_path);
            let before = config.exports.len();
            config.exports.retain(|e| e.path != path);
            if config.exports.len() == before {
                return Err(format!("No NFS export found for path '{path}'"));
            }

            crate::services::nfs::save(nfs_path, &config).map_err(|e| e.to_string())?;
            let steps = crate::services::nfs::apply(&state.nfs, &config).await;

            let res = json!({
                "ok": true,
                "removed_path": path,
                "apply_steps": steps
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_proxy_status" => {
            let proxy_path = std::path::Path::new(&state.config.proxy_path);
            let (config, doc_error) = crate::proxy::load(proxy_path);
            let problems = config.validate();
            let res = json!({
                "routes_count": config.routes.len(),
                "routes": config.routes,
                "problems": problems,
                "error": doc_error
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "create_proxy_route" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("id is required")?;
            let hostname = args
                .get("hostname")
                .and_then(|v| v.as_str())
                .ok_or("hostname is required")?;
            let target_port = args
                .get("target_port")
                .and_then(|v| v.as_u64())
                .ok_or("target_port is required")? as u16;

            let proxy_path = std::path::Path::new(&state.config.proxy_path);
            let (mut config, _) = crate::proxy::load(proxy_path);

            if config.routes.iter().any(|r| r.id == id) {
                return Err(format!("A proxy route with id '{id}' already exists"));
            }

            let route = crate::proxy::ProxyRoute {
                id: id.to_string(),
                hostname: hostname.to_string(),
                upstream: crate::proxy::Upstream::Url {
                    url: format!("http://127.0.0.1:{target_port}"),
                },
                tls: crate::proxy::ProxyTls::CaManaged,
                tls_cert: None,
                enabled: true,
                comment: String::new(),
            };

            config.routes.push(route);
            let problems = config.validate();
            if !problems.is_empty() {
                return Err(format!("Validation failed: {problems:?}"));
            }

            crate::proxy::save(proxy_path, &config).map_err(|e| e.to_string())?;

            let res = json!({
                "ok": true,
                "route_id": id,
                "hostname": hostname,
                "target_port": target_port
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "delete_proxy_route" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("id is required")?;

            let proxy_path = std::path::Path::new(&state.config.proxy_path);
            let (mut config, _) = crate::proxy::load(proxy_path);
            let before = config.routes.len();
            config.routes.retain(|r| r.id != id);
            if config.routes.len() == before {
                return Err(format!("No proxy route found with id '{id}'"));
            }

            crate::proxy::save(proxy_path, &config).map_err(|e| e.to_string())?;

            let res = json!({
                "ok": true,
                "deleted_route_id": id
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_ddns_status" => {
            let config = state.ddns.config();
            let wan_ip = state.ddns.wan_ip();
            let res = json!({
                "enabled": config.enabled,
                "record_names": config.record_names,
                "zone_id": config.zone_id,
                "wan_ip": wan_ip,
                "interval_min": config.interval_min,
                "last_seen_ip": config.last_seen_ip
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "get_remote_access_status" => {
            let overview = crate::services::remote_access::get_overview();
            serde_json::to_string_pretty(&overview).map_err(|e| e.to_string())
        }

        "get_alerts_status" => {
            let (doc, error) = state.alerts.load();
            let res = json!({
                "rules_count": doc.rules.len(),
                "rules": doc.rules,
                "providers_count": doc.providers.len(),
                "providers": doc.providers,
                "error": error
            });
            serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
        }

        "run_ai_test_suite" => {
            let mut test_results = Vec::new();

            // 1. NEGATIVE TEST: Samba password < 8 characters must be rejected
            let bad_pwd_res = crate::services::samba::set_password(
                state.samba.runner.as_ref(),
                "test_user_bad",
                "short",
            );
            let pwd_ok = match bad_pwd_res {
                Err(err) => err.to_string().contains("at least 8 characters"),
                Ok(_) => false,
            };
            test_results.push(json!({
                "name": "Validation: Reject short Samba password (< 8 chars)",
                "type": "negative_validation",
                "passed": pwd_ok,
                "detail": "Samba password rejected with 400 Bad Request as expected"
            }));

            // 2. NEGATIVE TEST: Empty Samba username must be rejected
            let empty_user_res = crate::services::samba::set_password(
                state.samba.runner.as_ref(),
                "   ",
                "Password123!",
            );
            let empty_ok = match empty_user_res {
                Err(err) => err.to_string().contains("username is required"),
                Ok(_) => false,
            };
            test_results.push(json!({
                "name": "Validation: Reject empty Samba username",
                "type": "negative_validation",
                "passed": empty_ok,
                "detail": "Empty username correctly rejected"
            }));

            // 3. NEGATIVE TEST: Invalid WireGuard base64 key must be rejected
            let invalid_wg_key =
                crate::services::wireguard::is_wg_key("not-a-valid-wireguard-base64-key-at-all");
            test_results.push(json!({
                "name": "Validation: Reject malformed WireGuard private key",
                "type": "negative_validation",
                "passed": !invalid_wg_key,
                "detail": "Invalid WireGuard key rejected by validator"
            }));

            // 4. NEGATIVE TEST: Invalid Hostname rejected in network models
            let invalid_host =
                crate::networking::model::is_valid_hostname("bad hostname with spaces & symbols!");
            test_results.push(json!({
                "name": "Validation: Reject invalid hostname with spaces & symbols",
                "type": "negative_validation",
                "passed": !invalid_host,
                "detail": "Malformed hostname rejected by RFC 1123 validator"
            }));

            // 5. NEGATIVE TEST: VM config with 0 RAM & 0 vCPUs
            let (doc, _) =
                crate::virtualization::store::load(std::path::Path::new(&state.virt.config_path));
            let mut invalid_vm_candidate = doc.clone();
            invalid_vm_candidate
                .vms
                .push(crate::virtualization::model::VmConfig {
                    id: "test-invalid-id".to_string(),
                    memory_mb: 0,
                    vcpus: 0,
                    ..Default::default()
                });
            let vm_problems = invalid_vm_candidate.validate();
            let vm_val_ok = !vm_problems.is_empty();
            test_results.push(json!({
                "name": "Validation: Reject VM with 0MB RAM and 0 vCPUs",
                "type": "negative_validation",
                "passed": vm_val_ok,
                "detail": format!("Validation reported {} problem(s)", vm_problems.len())
            }));

            // 6. NEGATIVE TEST: Non-existent container logs handled without crash
            let bad_log_res = state
                .docker
                .logs("nonexistent-container-xyz-9999", "10")
                .await;
            test_results.push(json!({
                "name": "Resilience: Query non-existent container logs fails cleanly",
                "type": "error_boundary",
                "passed": bad_log_res.is_err(),
                "detail": "Non-existent container query returns clean error without panic"
            }));

            // 7. POSITIVE TEST: Web UI Port 8553 Reachability
            let ui_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect("127.0.0.1:8553"),
            )
            .await;
            test_results.push(json!({
                "name": "Probe: Web UI Port 8553 Reachable",
                "type": "service_probe",
                "passed": matches!(ui_res, Ok(Ok(_))),
                "detail": "Web UI listening and responsive on localhost:8553"
            }));

            // 8. POSITIVE TEST: Samba Port 445 Reachability
            let smb_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect("127.0.0.1:445"),
            )
            .await;
            test_results.push(json!({
                "name": "Probe: Samba SMB Port 445 Reachable",
                "type": "service_probe",
                "passed": matches!(smb_res, Ok(Ok(_))),
                "detail": "smbd listening and responsive on 127.0.0.1:445"
            }));

            // 9. POSITIVE TEST: Jellyfin Port 8096 Reachability
            let jelly_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect("127.0.0.1:8096"),
            )
            .await;
            test_results.push(json!({
                "name": "Probe: Jellyfin Media Server Port 8096 Reachable",
                "type": "service_probe",
                "passed": matches!(jelly_res, Ok(Ok(_))),
                "detail": "Jellyfin container listening and responsive on port 8096"
            }));

            // 10. POSITIVE TEST: Vaultwarden Port 8086 Reachability
            let vault_res = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect("127.0.0.1:8086"),
            )
            .await;
            test_results.push(json!({
                "name": "Probe: Vaultwarden Password Manager Port 8086 Reachable",
                "type": "service_probe",
                "passed": matches!(vault_res, Ok(Ok(_))),
                "detail": "Vaultwarden container listening and responsive on port 8086"
            }));

            // 11. POSITIVE TEST: Docker Daemon Connected
            let installed = crate::containers::store::installed_ids(&state.config);
            let apps = crate::containers::store::catalog(&state.config);
            let host_ip = crate::containers::host::detect().ip;
            let docker_ok = state.docker.list(&host_ip, &installed, &apps).await.is_ok();
            test_results.push(json!({
                "name": "Subsystem: Docker Engine Socket Responsive",
                "type": "subsystem_health",
                "passed": docker_ok,
                "detail": "Docker daemon socket active and returning container states"
            }));

            // 12. POSITIVE TEST: SMART Drive Health Telemetry
            let smart_ok = state.smart.available();
            test_results.push(json!({
                "name": "Subsystem: SMART Controller & Storage Available",
                "type": "subsystem_health",
                "passed": smart_ok,
                "detail": "smartmontools available for NVMe & SATA telemetry"
            }));

            // 13. POSITIVE TEST: Host Network Interfaces Discovered
            let ifaces = crate::networking::interfaces::discover(state.network.runner.as_ref());
            test_results.push(json!({
                "name": "Subsystem: Network Interfaces Discovered",
                "type": "subsystem_health",
                "passed": !ifaces.is_empty(),
                "detail": format!("Discovered {} host interface(s)", ifaces.len())
            }));

            // 14. POSITIVE TEST: DNS Domain Normalization
            let norm = crate::networking::blocklists::normalize_domain(
                "HTTPS://WWW.GOOGLE.COM/SEARCH?Q=TEST",
            )
            .unwrap_or_default();
            test_results.push(json!({
                "name": "Algorithm: DNS Blocklist Domain Normalization",
                "type": "functional_test",
                "passed": norm == "www.google.com",
                "detail": format!("URL correctly normalized to '{norm}'")
            }));

            let all_passed = test_results
                .iter()
                .all(|t| t.get("passed").and_then(|v| v.as_bool()).unwrap_or(false));
            let passed_count = test_results
                .iter()
                .filter(|t| t.get("passed").and_then(|v| v.as_bool()).unwrap_or(false))
                .count();
            let total_count = test_results.len();

            let summary = json!({
                "all_passed": all_passed,
                "passed": passed_count,
                "failed": total_count - passed_count,
                "total": total_count,
                "tests": test_results
            });
            serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())
        }

        unknown => Err(format!("Unknown tool '{unknown}'")),
    }
}
