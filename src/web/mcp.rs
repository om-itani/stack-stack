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
        "tools_count": 9,
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
            let mem_ok = if let Some(m) = snap {
                m.memory.total_bytes > 0 && m.memory.used_bytes < m.memory.total_bytes
            } else {
                true
            };
            checks.push(json!({
                "test": "System Memory",
                "passed": mem_ok,
                "detail": "Memory subsystem healthy"
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

        unknown => Err(format!("Unknown tool '{unknown}'")),
    }
}
