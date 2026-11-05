//! M8 — MCP server over stdio (JSON-RPC 2.0, newline-delimited). OFF by default: `pulsebar mcp`
//! refuses to start unless enabled in Settings. Data is scoped to the current user, and
//! `pulsebar_request_quit` only creates a pending request that a human must approve in the app.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::actions::{self, Mode, Target};
use crate::model::{RankMetric, Snapshot};
use crate::settings::Settings;
use crate::{live, sampler, sys, views};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub trait SnapshotSource {
    fn snapshot(&mut self) -> Snapshot;
}

/// Prefer the running app's live snapshot; otherwise sample (cached for 3 s).
pub struct LiveSource {
    settings: Settings,
    cache: Option<(Instant, Snapshot)>,
}

impl LiveSource {
    pub fn new(settings: Settings) -> Self {
        Self { settings, cache: None }
    }
}

impl SnapshotSource for LiveSource {
    fn snapshot(&mut self) -> Snapshot {
        if let Some(s) = live::read_fresh() {
            return s;
        }
        if let Some((t, s)) = &self.cache
            && t.elapsed() < Duration::from_secs(3) {
                return s.clone();
            }
        let s = sampler::Engine::sample_once(&self.settings);
        self.cache = Some((Instant::now(), s.clone()));
        s
    }
}

pub struct Server<S: SnapshotSource> {
    source: S,
    uid: u32,
    /// Where pending quit requests go (overridable in tests).
    pub create_pending: Box<dyn Fn(actions::Plan) -> anyhow::Result<actions::PendingRequest>>,
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": schema})
}

pub fn tools() -> Vec<Value> {
    let empty = json!({"type": "object", "properties": {}, "additionalProperties": false});
    vec![
        tool("pulsebar_status", "Machine summary: CPU, memory, disk, network, GPU, Neural Engine, battery, temperatures, power and the 3 busiest apps.", empty.clone()),
        tool(
            "pulsebar_top_apps",
            "Rank applications (all their helper processes aggregated) by a metric.",
            json!({"type": "object", "properties": {
                "metric": {"type": "string", "enum": ["cpu", "memory", "energy", "disk", "network"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 10}
            }, "required": ["metric"], "additionalProperties": false}),
        ),
        tool(
            "pulsebar_what_is_on_port",
            "Which process/app/project is listening on a TCP port, its memory and how long it has been idle.",
            json!({"type": "object", "properties": {"port": {"type": "integer", "minimum": 1, "maximum": 65535}}, "required": ["port"], "additionalProperties": false}),
        ),
        tool("pulsebar_list_projects", "Projects (by folder) with their processes, dev servers, ports and idle state.", empty.clone()),
        tool("pulsebar_list_agents", "Active AI coding-agent sessions and the processes each one started.", empty.clone()),
        tool("pulsebar_list_containers", "Docker containers grouped by compose stack, with memory vs limit.", empty.clone()),
        tool("pulsebar_limits", "AI plan usage computed from local agent logs (no network).", empty.clone()),
        tool("pulsebar_diagnose", "What is slow right now and why: top CPU/memory apps plus notable conditions.", empty.clone()),
        tool(
            "pulsebar_request_quit",
            "Ask the user to stop a process, an app, an agent session or a dev server. NEVER stops anything by itself: it creates a request the user must approve in the PulseBar window. Poll pulsebar_quit_status for the result.",
            json!({"type": "object", "properties": {
                "pid": {"type": "integer"},
                "app": {"type": "string", "description": "App name or bundle id"},
                "agent_session": {"type": "integer", "description": "Agent pid (stops the agent and everything it started)"},
                "dev_server": {"type": "integer", "description": "Dev server pid"},
                "force": {"type": "boolean", "default": false}
            }, "additionalProperties": false}),
        ),
        tool(
            "pulsebar_quit_status",
            "Status of a quit request: pending_confirmation, approved (with what was freed), denied or expired.",
            json!({"type": "object", "properties": {"request_id": {"type": "string"}}, "required": ["request_id"], "additionalProperties": false}),
        ),
    ]
}

impl<S: SnapshotSource> Server<S> {
    pub fn new(source: S) -> Self {
        Self { source, uid: sys::current_uid(), create_pending: Box::new(|p| actions::create_pending(p, "mcp")) }
    }

    pub fn with_uid(mut self, uid: u32) -> Self {
        self.uid = uid;
        self
    }

    fn scoped(&mut self) -> Snapshot {
        views::scope_to_user(&self.source.snapshot(), self.uid)
    }

    pub fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "pulsebar_status" => Ok(views::status(&self.scoped())),
            "pulsebar_top_apps" => {
                let m = args["metric"].as_str().and_then(RankMetric::parse).ok_or("metric must be cpu|memory|energy|disk|network")?;
                let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 100) as usize;
                Ok(views::top(&self.scoped(), m, limit))
            }
            "pulsebar_what_is_on_port" => {
                let port = args["port"].as_u64().filter(|p| (1..=65535).contains(p)).ok_or("port must be 1-65535")? as u16;
                Ok(views::what_is_on_port(&self.scoped(), port))
            }
            "pulsebar_list_projects" => Ok(views::projects(&self.scoped())),
            "pulsebar_list_agents" => Ok(views::agents(&self.scoped())),
            "pulsebar_list_containers" => Ok(views::containers(&self.scoped())),
            "pulsebar_limits" => Ok(views::limits(&self.scoped())),
            "pulsebar_diagnose" => {
                let uid = self.uid;
                Ok(views::diagnosis(&self.scoped(), uid))
            }
            "pulsebar_request_quit" => {
                let target = if let Some(p) = args["pid"].as_i64() {
                    Target::Pid(p as i32)
                } else if let Some(a) = args["app"].as_str() {
                    Target::App(a.to_string())
                } else if let Some(p) = args["agent_session"].as_i64() {
                    Target::AgentSession(p as i32)
                } else if let Some(p) = args["dev_server"].as_i64() {
                    Target::DevServer(p as i32)
                } else {
                    return Err("provide one of: pid, app, agent_session, dev_server".into());
                };
                let mode = if args["force"].as_bool().unwrap_or(false) { Mode::ForceQuit } else { Mode::Quit };
                let snap = self.scoped();
                let plan = actions::plan_as(&snap, target, mode, self.uid).map_err(|e| e.to_string())?;
                let req = (self.create_pending)(plan).map_err(|e| e.to_string())?;
                let app_running = live::read_fresh().is_some();
                Ok(json!({
                    "status": "pending_confirmation",
                    "request_id": req.id,
                    "summary": req.summary,
                    "message": if app_running {
                        "Waiting for the user to confirm in the PulseBar window. Nothing has been stopped."
                    } else {
                        "Nothing has been stopped. The PulseBar app is not running; the user must open it to confirm this request."
                    },
                }))
            }
            "pulsebar_quit_status" => {
                let id = args["request_id"].as_str().ok_or("request_id required")?;
                let r = actions::read_pending(id).ok_or("unknown request_id")?;
                Ok(json!({"request_id": r.id, "status": r.status, "summary": r.summary, "outcome": r.outcome}))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// Handle one JSON-RPC message; returns the response (None for notifications).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or("");
        let id = id?; // notifications (no id) get no response
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let requested = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION).to_string();
                Ok(json!({
                    "protocolVersion": requested,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "pulsebar", "title": "PulseBar", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Local machine activity data grouped by app. Read-only, except pulsebar_request_quit which always requires the user's approval in the PulseBar window."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.call_tool(name, &args) {
                    Ok(v) => json!({
                        "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}],
                        "structuredContent": v,
                        "isError": false
                    }),
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                })
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        })
    }
}

pub fn serve_stdio(settings: Settings) -> anyhow::Result<()> {
    if !settings.mcp_enabled {
        eprintln!("PulseBar MCP server is disabled (default). Enable it in PulseBar → Settings → MCP, or run `pulsebar config mcp on`.");
        std::process::exit(2);
    }
    let mut server = Server::new(LiveSource::new(settings));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        if let Some(r) = resp {
            writeln!(stdout, "{}", serde_json::to_string(&r)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}
