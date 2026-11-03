//! Destructive actions, gated by explicit human confirmation.
//!
//! The only way to signal a process is `execute(Confirmed)`. A `Confirmed` can only be built
//! from a `Plan` plus a `HumanYes`, and `HumanYes` is only minted by a UI button click or an
//! interactive CLI prompt / `--yes` flag. The MCP server can *create* a pending request but
//! never mints a `HumanYes` — it has to wait for the person in the app.

use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::i18n::I18n;
use crate::model::Snapshot;
use crate::paths;
use crate::sys;
use crate::units::format_bytes;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Target {
    Pid(i32),
    /// App group key, bundle id or app name.
    App(String),
    /// An agent session (agent pid): the agent and everything it started.
    AgentSession(i32),
    /// A dev server process (stops just that process tree).
    DevServer(i32),
    Container(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// SIGTERM.
    Quit,
    /// SIGKILL.
    ForceQuit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub target: Target,
    pub mode: Mode,
    pub title: String,
    pub pids: Vec<i32>,
    pub mem_bytes: u64,
    pub ports: Vec<u16>,
}

impl Plan {
    /// What the confirmation dialog shows.
    pub fn summary(&self) -> String {
        let mut s = format!("{} ({})? Frees about {}", self.verb_title(), self.process_label(), format_bytes(self.mem_bytes as f64));
        if !self.ports.is_empty() {
            s.push_str(&format!(" and port{} {}", if self.ports.len() > 1 { "s" } else { "" }, join_ports(&self.ports)));
        }
        s.push('.');
        s
    }

    fn verb(&self) -> &'static str {
        match self.mode {
            Mode::Quit => "Quit",
            Mode::ForceQuit => "Force quit",
        }
    }

    fn verb_title(&self) -> String {
        format!("{} {}", self.verb(), self.title)
    }

    fn process_label(&self) -> String {
        if self.pids.len() > 1 { format!("{} processes", self.pids.len()) } else { "1 process".into() }
    }

    /// Dialog title: "Quit Brave Browser?" / "Force quit node (pid 42)?" (English: CLI/MCP).
    pub fn headline(&self) -> String {
        self.headline_in(&I18n::english())
    }

    /// The details a confirmation shows as separate chips (English: CLI/MCP).
    pub fn details(&self) -> Vec<String> {
        self.details_in(&I18n::english())
    }

    /// The target's display name in `i18n`'s language. Names of apps, processes and projects
    /// are kept as they are; the two composed titles ("container x", "Claude session in y")
    /// are rebuilt from their parts.
    fn title_in(&self, i18n: &I18n) -> String {
        match &self.target {
            Target::Container(_) => match self.title.strip_prefix("container ") {
                Some(name) => i18n.tf("plan.container", &[("name", name)]),
                None => self.title.clone(),
            },
            Target::AgentSession(_) => match self.title.split_once(" session in ") {
                Some((agent, project)) => i18n.tf("plan.agent_session", &[("agent", agent), ("project", project)]),
                None => self.title.clone(),
            },
            _ => self.title.clone(),
        }
    }

    /// `headline` in the UI language.
    pub fn headline_in(&self, i18n: &I18n) -> String {
        let key = match self.mode {
            Mode::Quit => "plan.quit",
            Mode::ForceQuit => "plan.force_quit",
        };
        i18n.tf(key, &[("title", &self.title_in(i18n))])
    }

    /// `details` in the UI language: process count (when there are processes), memory freed,
    /// and the ports that become free.
    pub fn details_in(&self, i18n: &I18n) -> Vec<String> {
        let mut v = vec![];
        if !self.pids.is_empty() {
            v.push(i18n.tn("n.processes", self.pids.len(), &[]));
        }
        v.push(i18n.tf("plan.frees", &[("mem", &i18n.num(format_bytes(self.mem_bytes as f64)))]));
        match self.ports.len() {
            0 => {}
            1 => v.push(i18n.tf("plan.port", &[("ports", &self.ports[0].to_string())])),
            _ => v.push(i18n.tf("plan.ports", &[("ports", &join_ports(&self.ports))])),
        }
        v
    }
}

fn join_ports(p: &[u16]) -> String {
    p.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

/// Proof that a human said yes. Construct only from real user interaction.
#[derive(Debug)]
pub struct HumanYes(());

impl HumanYes {
    /// The user clicked the confirm button in a dialog that displayed `Plan::summary()`.
    pub fn from_ui_click() -> Self {
        HumanYes(())
    }
    /// The user answered "y" to a terminal prompt or passed `--yes` explicitly.
    pub fn from_cli() -> Self {
        HumanYes(())
    }
}

pub struct Confirmed(Plan);

pub fn confirm(plan: Plan, _yes: HumanYes) -> Confirmed {
    Confirmed(plan)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub ok: bool,
    pub stopped: Vec<i32>,
    pub failed: Vec<i32>,
    pub mem_freed: u64,
    pub ports_freed: Vec<u16>,
    /// "storefront-web stopped. 458 MB and port 4322 are free."
    pub message: String,
}

fn descendants_of(s: &Snapshot, roots: &[i32]) -> Vec<i32> {
    let mut children: std::collections::HashMap<i32, Vec<i32>> = Default::default();
    for p in &s.processes {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut out: Vec<i32> = roots.to_vec();
    let mut seen: HashSet<i32> = roots.iter().copied().collect();
    let mut i = 0;
    while i < out.len() {
        for c in children.get(&out[i]).cloned().unwrap_or_default() {
            if seen.insert(c) {
                out.push(c);
            }
        }
        i += 1;
    }
    out
}

/// Resolve a target into a concrete plan. Refuses other users' processes, launchd and ourselves.
pub fn plan(s: &Snapshot, target: Target, mode: Mode) -> Result<Plan> {
    plan_as(s, target, mode, sys::current_uid())
}

/// `plan` for an explicit user id (the MCP server's data boundary).
pub fn plan_as(s: &Snapshot, target: Target, mode: Mode, me: u32) -> Result<Plan> {
    let self_pid = std::process::id() as i32;
    let (title, pids): (String, Vec<i32>) = match &target {
        Target::Pid(pid) => {
            let p = s.process(*pid).ok_or_else(|| anyhow::anyhow!("no process with pid {pid}"))?;
            (format!("{} (pid {pid})", p.name), vec![*pid])
        }
        Target::App(q) => {
            let ql = q.to_ascii_lowercase();
            let g = s
                .apps
                .iter()
                .find(|a| a.key == *q || a.app.bundle_id.as_deref() == Some(q) || a.app.name.to_ascii_lowercase() == ql)
                .ok_or_else(|| anyhow::anyhow!("no app matching '{q}'"))?;
            if g.key == crate::grouping::SYSTEM_KEY {
                bail!("refusing to quit macOS system processes");
            }
            (g.app.name.clone(), g.pids.clone())
        }
        Target::AgentSession(pid) => {
            let a = s.agents.iter().find(|a| a.pid == *pid).ok_or_else(|| anyhow::anyhow!("no agent session {pid}"))?;
            let mut v = vec![a.pid];
            v.extend(&a.spawned_pids);
            (format!("{} session in {}", a.agent_kind.label(), a.project), v)
        }
        Target::DevServer(pid) => {
            let srv = s.projects.iter().flat_map(|p| p.servers.iter()).find(|x| x.process_pid == *pid);
            let title = srv.map(|x| x.project.clone()).unwrap_or_else(|| format!("pid {pid}"));
            (title, descendants_of(s, &[*pid]))
        }
        Target::Container(id) => {
            let c = s.containers.iter().find(|c| c.id.starts_with(id.as_str()) || c.name == *id).ok_or_else(|| anyhow::anyhow!("no container '{id}'"))?;
            return Ok(Plan {
                title: format!("container {}", c.name),
                target: Target::Container(c.id.clone()),
                mode,
                pids: vec![],
                mem_bytes: c.mem_bytes,
                ports: c.published_ports.clone(),
            });
        }
    };
    let pids: Vec<i32> = pids
        .into_iter()
        .filter(|pid| *pid > 1 && *pid != self_pid)
        .filter(|pid| s.process(*pid).map(|p| p.uid == me).unwrap_or(false))
        .collect();
    if pids.is_empty() {
        bail!("nothing to stop (only your own processes can be stopped)");
    }
    let set: HashSet<i32> = pids.iter().copied().collect();
    let mem_bytes = s.processes.iter().filter(|p| set.contains(&p.pid)).map(|p| p.mem_bytes).sum();
    let mut ports: Vec<u16> = s.ports.iter().filter(|p| set.contains(&p.pid)).map(|p| p.port).collect();
    ports.sort_unstable();
    ports.dedup();
    Ok(Plan { target, mode, title, pids, mem_bytes, ports })
}

pub fn execute(c: Confirmed) -> Outcome {
    let plan = c.0;
    if let Target::Container(id) = &plan.target {
        let r = crate::docker::stop_container(id);
        let ok = r.is_ok();
        return Outcome {
            ok,
            stopped: vec![],
            failed: vec![],
            mem_freed: if ok { plan.mem_bytes } else { 0 },
            ports_freed: if ok { plan.ports.clone() } else { vec![] },
            message: match r {
                Ok(()) => freed_message(&plan.title, plan.mem_bytes, &plan.ports),
                Err(e) => format!("Could not stop {}: {e}", plan.title),
            },
        };
    }
    let sig = match plan.mode {
        Mode::Quit => libc::SIGTERM,
        Mode::ForceQuit => libc::SIGKILL,
    };
    let (mut stopped, mut failed) = (vec![], vec![]);
    for pid in &plan.pids {
        if sys::signal(*pid, sig) { stopped.push(*pid) } else { failed.push(*pid) }
    }
    let ok = failed.is_empty();
    let message = if ok {
        freed_message(&plan.title, plan.mem_bytes, &plan.ports)
    } else {
        format!("{} partially stopped: {} of {} processes did not respond.", plan.title, failed.len(), plan.pids.len())
    };
    Outcome { ok, stopped, failed, mem_freed: plan.mem_bytes, ports_freed: plan.ports, message }
}

/// The toast after an action, in the UI language (the `Outcome::message` is English for CLI/MCP).
pub fn outcome_message(i18n: &I18n, plan: &Plan, o: &Outcome) -> String {
    let title = plan.title_in(i18n);
    if o.ok {
        let mem = i18n.num(format_bytes(plan.mem_bytes as f64));
        return match plan.ports.len() {
            0 => i18n.tf("freed.none", &[("title", &title), ("mem", &mem)]),
            1 => i18n.tf("freed.port", &[("title", &title), ("mem", &mem), ("ports", &plan.ports[0].to_string())]),
            _ => i18n.tf("freed.ports", &[("title", &title), ("mem", &mem), ("ports", &join_ports(&plan.ports))]),
        };
    }
    if matches!(plan.target, Target::Container(_)) {
        return i18n.tf("freed.container_failed", &[("title", &title)]);
    }
    i18n.tf("freed.partial", &[("title", &title), ("failed", &o.failed.len().to_string()), ("total", &plan.pids.len().to_string())])
}

pub fn freed_message(title: &str, mem: u64, ports: &[u16]) -> String {
    match ports.len() {
        0 => format!("{title} stopped. {} is free.", format_bytes(mem as f64)),
        1 => format!("{title} stopped. {} and port {} are free.", format_bytes(mem as f64), ports[0]),
        _ => format!("{title} stopped. {} and ports {} are free.", format_bytes(mem as f64), join_ports(ports)),
    }
}

// ---------------------------------------------------------------------------------------------
// Pending requests (from MCP → UI)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingStatus {
    PendingConfirmation,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRequest {
    pub id: String,
    pub created_at: i64,
    pub source: String,
    pub plan: Plan,
    pub summary: String,
    pub status: PendingStatus,
    pub outcome: Option<Outcome>,
}

pub const PENDING_TTL_S: i64 = 600;

fn pending_path(id: &str) -> std::path::PathBuf {
    paths::pending_dir().join(format!("{id}.json"))
}

pub fn create_pending(plan: Plan, source: &str) -> Result<PendingRequest> {
    paths::ensure_dirs()?;
    let now = sys::now_s();
    let id = format!("q{}-{}", now, std::process::id() ^ (sys::now_ms() as u32 & 0xffff));
    let req = PendingRequest {
        id: id.clone(),
        created_at: now,
        source: source.to_string(),
        summary: plan.summary(),
        plan,
        status: PendingStatus::PendingConfirmation,
        outcome: None,
    };
    std::fs::write(pending_path(&id), serde_json::to_vec_pretty(&req)?)?;
    Ok(req)
}

pub fn read_pending(id: &str) -> Option<PendingRequest> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut r: PendingRequest = serde_json::from_slice(&std::fs::read(pending_path(id)).ok()?).ok()?;
    if r.status == PendingStatus::PendingConfirmation && sys::now_s() - r.created_at > PENDING_TTL_S {
        r.status = PendingStatus::Expired;
    }
    Some(r)
}

/// Requests still waiting for the user (oldest first).
pub fn list_pending() -> Vec<PendingRequest> {
    let mut v: Vec<PendingRequest> = std::fs::read_dir(paths::pending_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter_map(|id| read_pending(&id))
        .filter(|r| r.status == PendingStatus::PendingConfirmation)
        .collect();
    v.sort_by_key(|r| r.created_at);
    v
}

/// Called by the UI after the user answered the dialog.
pub fn resolve_pending(mut req: PendingRequest, decision: Option<HumanYes>) -> Result<PendingRequest> {
    match decision {
        Some(yes) => {
            req.status = PendingStatus::Approved;
            req.outcome = Some(execute(confirm(req.plan.clone(), yes)));
        }
        None => req.status = PendingStatus::Denied,
    }
    std::fs::write(pending_path(&req.id), serde_json::to_vec_pretty(&req)?)?;
    Ok(req)
}

/// Remove resolved/expired request files older than a day.
pub fn gc_pending() {
    let now = sys::now_s();
    for e in std::fs::read_dir(paths::pending_dir()).into_iter().flatten().flatten() {
        if let Ok(r) = serde_json::from_slice::<PendingRequest>(&std::fs::read(e.path()).unwrap_or_default())
            && now - r.created_at > 86_400 {
                let _ = std::fs::remove_file(e.path());
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freed_messages() {
        assert_eq!(freed_message("storefront-web", 458_000_000, &[4322]), "storefront-web stopped. 458 MB and port 4322 are free.");
        assert_eq!(freed_message("x", 1_370_000_000, &[3000, 8000]), "x stopped. 1.37 GB and ports 3000, 8000 are free.");
    }

    #[test]
    fn localised_headline_details_and_outcome() {
        let es = I18n::new(Some("es"));
        let mut p = Plan { target: Target::App("x".into()), mode: Mode::Quit, title: "Brave".into(), pids: vec![1, 2, 3], mem_bytes: 3_070_000_000, ports: vec![3000] };
        assert_eq!(p.headline_in(&es), "¿Salir de Brave?");
        assert_eq!(p.details_in(&es), vec!["3 procesos", "Libera 3,07 GB", "Puerto 3000"]);
        p.mode = Mode::ForceQuit;
        p.pids = vec![1];
        assert_eq!(p.headline_in(&es), "¿Forzar la salida de Brave?");
        assert_eq!(p.details_in(&es)[0], "1 proceso");
        p.target = Target::Container("abc".into());
        p.title = "container db".into();
        assert_eq!(p.headline_in(&es), "¿Forzar la salida de contenedor db?");
        // English stays what the CLI and MCP print.
        assert_eq!(p.headline(), "Force quit container db?");
        let o = Outcome { ok: true, stopped: vec![], failed: vec![], mem_freed: 0, ports_freed: vec![], message: String::new() };
        assert_eq!(outcome_message(&es, &p, &o), "contenedor db detenido. 3,07 GB y el puerto 3000 quedan libres.");
        assert_eq!(outcome_message(&I18n::english(), &p, &o), "container db stopped. 3.07 GB and port 3000 are free.");
    }

    #[test]
    fn plan_refuses_foreign_and_system() {
        use crate::model::{AppGroup, Process};
        let me = sys::current_uid();
        let s = Snapshot {
            processes: vec![
                Process { pid: 50, uid: me, name: "node".into(), mem_bytes: 10, ..Default::default() },
                Process { pid: 60, uid: me.wrapping_add(1), name: "root-thing".into(), ..Default::default() },
            ],
            apps: vec![AppGroup { key: crate::grouping::SYSTEM_KEY.into(), pids: vec![60], ..Default::default() }],
            ..Default::default()
        };
        assert!(plan(&s, Target::Pid(60), Mode::Quit).is_err());
        assert!(plan(&s, Target::App(crate::grouping::SYSTEM_KEY.into()), Mode::Quit).is_err());
        let p = plan(&s, Target::Pid(50), Mode::Quit).unwrap();
        assert_eq!(p.pids, vec![50]);
        assert!(p.summary().starts_with("Quit node (pid 50) (1 process)?"));
    }

    #[test]
    fn plan_summary_and_structured_details() {
        let mut p = Plan { target: Target::App("x".into()), mode: Mode::Quit, title: "Brave".into(), pids: vec![1, 2, 3], mem_bytes: 4_900_000, ports: vec![3000, 8000] };
        assert_eq!(p.summary(), "Quit Brave (3 processes)? Frees about 4.9 MB and ports 3000, 8000.");
        assert_eq!(p.headline(), "Quit Brave?");
        assert_eq!(p.details(), vec!["3 processes", "Frees 4.9 MB", "Ports 3000, 8000"]);
        p.mode = Mode::ForceQuit;
        p.pids = vec![7];
        p.ports = vec![5037];
        assert_eq!(p.summary(), "Force quit Brave (1 process)? Frees about 4.9 MB and port 5037.");
        assert_eq!(p.headline(), "Force quit Brave?");
        assert_eq!(p.details(), vec!["1 process", "Frees 4.9 MB", "Port 5037"]);
        p.pids.clear();
        p.ports.clear();
        assert_eq!(p.details(), vec!["Frees 4.9 MB"]);
    }
}
