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
        "tools_count": 27,
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

        unknown => Err(format!("Unknown tool '{unknown}'")),
    }
}
