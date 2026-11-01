//! Agents tab: detect AI coding-agent sessions (Claude Code, Codex, Gemini CLI, Cursor Agent,
//! GitHub Copilot CLI) and everything each session started.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::model::{AgentKind, AgentSession, AgentState, ListeningPort, Process, SpawnedProcess};
use crate::projects::{ActivityTracker, ProjectResolver};

fn base(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// Classify a process as an agent entry point.
pub fn detect_kind(p: &Process) -> Option<AgentKind> {
    let name = p.name.to_ascii_lowercase();
    let exe = base(&p.executable_path).to_ascii_lowercase();
    let argv0 = p.argv.first().map(|a| base(a).to_ascii_lowercase()).unwrap_or_default();
    let joined = p.argv.iter().take(3).map(|a| a.to_ascii_lowercase()).collect::<Vec<_>>().join(" ");
    let is = |n: &str| name == n || exe == n || argv0 == n;
    // Never classify editors/helpers (e.g. "Cursor Helper") as agents.
    if p.executable_path.contains(".app/") && !p.executable_path.contains("/Resources/") {
        return None;
    }
    if is("claude") || joined.contains("@anthropic-ai/claude-code") || joined.contains("claude-code/cli.js") {
        Some(AgentKind::ClaudeCode)
    } else if is("codex") || joined.contains("@openai/codex") {
        Some(AgentKind::Codex)
    } else if is("gemini") || joined.contains("@google/gemini-cli") {
        Some(AgentKind::GeminiCli)
    } else if is("cursor-agent") || joined.contains("cursor-agent") {
        Some(AgentKind::CursorAgent)
    } else if is("copilot") || joined.contains("@github/copilot") {
        Some(AgentKind::GithubCopilot)
    } else {
        None
    }
}

const SHELLS: [&str; 6] = ["zsh", "bash", "sh", "fish", "dash", "login"];

pub fn classify_role(p: &Process, ports: &[u16]) -> String {
    let n = p.name.to_ascii_lowercase();
    let args = p.argv.join(" ").to_ascii_lowercase();
    if n.contains("chrome for testing") || args.contains("chrome-for-testing") || n.contains("headless") {
        "Chrome for Testing".into()
    } else if args.contains("mcp") {
        "MCP server".into()
    } else if !ports.is_empty() {
        format!("dev server :{}", ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", :"))
    } else if SHELLS.contains(&n.as_str()) {
        "shell".into()
    } else {
        "tool".into()
    }
}

pub fn git_branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).ok()?;
    let h = head.trim();
    Some(h.strip_prefix("ref: refs/heads/").map(str::to_string).unwrap_or_else(|| h.chars().take(8).collect()))
}

pub fn descendants(root: i32, children: &HashMap<i32, Vec<i32>>) -> Vec<i32> {
    let mut out = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut q = VecDeque::from([root]);
    while let Some(p) = q.pop_front() {
        for c in children.get(&p).into_iter().flatten() {
            if seen.insert(*c) {
                out.push(*c);
                q.push_back(*c);
            }
        }
    }
    out.sort_unstable();
    out
}

pub fn sessions(
    procs: &[Process],
    ports: &[ListeningPort],
    resolver: &mut ProjectResolver,
    tracker: &ActivityTracker,
    my_uid: u32,
    now: i64,
) -> Vec<AgentSession> {
    let by_pid: HashMap<i32, &Process> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let kinds: HashMap<i32, AgentKind> =
        procs.iter().filter(|p| p.uid == my_uid).filter_map(|p| detect_kind(p).map(|k| (p.pid, k))).collect();

    let mut out = Vec::new();
    for (&pid, &kind) in &kinds {
        // Skip if an ancestor is already an agent session (wrapper → native binary).
        let mut cur = by_pid.get(&pid).map(|p| p.ppid).unwrap_or(0);
        let mut nested = false;
        for _ in 0..64 {
            if cur <= 1 {
                break;
            }
            if kinds.contains_key(&cur) {
                nested = true;
                break;
            }
            cur = by_pid.get(&cur).map(|p| p.ppid).unwrap_or(0);
        }
        if nested {
            continue;
        }
        let p = by_pid[&pid];
        let desc = descendants(pid, &children);
        let ports_of = |pid: i32| {
            let mut v: Vec<u16> = ports.iter().filter(|l| l.pid == pid).map(|l| l.port).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let started: Vec<SpawnedProcess> = desc
            .iter()
            .filter_map(|d| by_pid.get(d))
            .filter(|d| detect_kind(d).is_none())
            .map(|d| {
                let pp = ports_of(d.pid);
                SpawnedProcess { pid: d.pid, name: d.name.clone(), role: classify_role(d, &pp), mem_bytes: d.mem_bytes, ports: pp }
            })
            .filter(|s| s.role != "shell")
            .collect();
        let all: Vec<&Process> = std::iter::once(p).chain(desc.iter().filter_map(|d| by_pid.get(d).copied())).collect();
        let cpu: f64 = all.iter().map(|x| x.cpu_pct).sum();
        let mem: u64 = all.iter().map(|x| x.mem_bytes).sum();
        let last_active = all.iter().filter_map(|x| tracker.last_active(x.pid)).max().unwrap_or(now);
        let quiet = now - last_active;
        let state = if cpu > 3.0 || quiet < 10 {
            AgentState::Working
        } else if quiet > 300 {
            AgentState::Idle
        } else {
            AgentState::Waiting
        };
        let root = resolver.resolve(&p.cwd);
        let project = root
            .as_ref()
            .and_then(|r| r.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| Path::new(&p.cwd).file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "—".into());
        out.push(AgentSession {
            agent_kind: kind,
            pid,
            project,
            state,
            started_at: p.start_time,
            mem_bytes: mem,
            cpu_percent: cpu,
            spawned_pids: desc,
            started,
            branch_or_task: root.as_deref().and_then(git_branch),
        });
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(a.pid.cmp(&b.pid)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(pid: i32, ppid: i32, name: &str, argv: &[&str]) -> Process {
        Process {
            pid,
            ppid,
            uid: 501,
            name: name.into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            executable_path: format!("/opt/homebrew/bin/{name}"),
            cwd: "/Users/me/code/shop".into(),
            mem_bytes: 100,
            ..Default::default()
        }
    }

    #[test]
    fn claude_session_with_children() {
        let procs = vec![
            p(10, 5, "node", &["node", "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js"]),
            p(11, 10, "zsh", &["/bin/zsh", "-c", "npm run dev"]),
            p(12, 11, "node", &["node", "next", "dev", "--port", "3000"]),
            p(13, 10, "Google Chrome for Testing", &["Google Chrome for Testing", "--headless"]),
            p(14, 10, "node", &["node", "playwright-mcp"]),
            p(20, 5, "codex", &["codex"]),
            p(21, 5, "vim", &["vim"]),
        ];
        let ports = vec![ListeningPort { pid: 12, port: 3000, address: "*".into(), protocol: "TCP".into(), command: "node".into() }];
        let mut r = ProjectResolver::with(PathBuf::from("/Users/me"), Box::new(|d: &Path, m: &str| d.ends_with("shop") && m == ".git"));
        let s = sessions(&procs, &ports, &mut r, &ActivityTracker::new(), 501, 0);
        assert_eq!(s.len(), 2);
        let c = s.iter().find(|x| x.agent_kind == AgentKind::ClaudeCode).unwrap();
        assert_eq!(c.spawned_pids, vec![11, 12, 13, 14]);
        assert_eq!(c.project, "shop");
        assert_eq!(c.mem_bytes, 500);
        let roles: Vec<&str> = c.started.iter().map(|x| x.role.as_str()).collect();
        assert_eq!(roles, vec!["dev server :3000", "Chrome for Testing", "MCP server"]);
    }
}
