//! GUI-side wording for facts the core modules phrase in English for the CLI and MCP
//! (idle-server banner, AI plan headline and pace, error messages). The core text is unchanged;
//! these rebuild the same sentences from the same data through `i18n`.

use crate::i18n::I18n;
use crate::model::{AIPlan, AgentState, PaceState, ProjectSummary};
use crate::projects;
use crate::units::{Units, format_duration};

/// "4 dev servers are running but idle. Stopping them frees 1.37 GB and ports 3000, 4321."
pub fn idle_banner(i18n: &I18n, u: &Units, projects: &[ProjectSummary]) -> Option<String> {
    let idle = projects::idle_servers(projects);
    if idle.is_empty() {
        return None;
    }
    let mem: u64 = idle.iter().map(|s| s.mem_bytes).sum();
    let pids: std::collections::HashSet<i32> = idle.iter().map(|s| s.process_pid).collect();
    let mut ports: Vec<u16> = projects.iter().flat_map(|p| p.servers.iter()).filter(|s| pids.contains(&s.process_pid)).map(|s| s.port).collect();
    ports.sort_unstable();
    ports.dedup();
    let list = ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ");
    let ports_s = i18n.tn("n.port_list", ports.len(), &[("ports", &list)]);
    let n = idle.len();
    Some(format!("{} {}", i18n.tn("idle.running", n, &[]), i18n.tn("idle.frees", n, &[("mem", &u.bytes(mem)), ("ports", &ports_s)])))
}

/// "This week" / "5h window" / other labels as they are.
pub fn window_label(i18n: &I18n, w: &str) -> String {
    match w {
        "week" | "7d" => i18n.t("window.week"),
        "" => String::new(),
        other => i18n.tf("window.generic", &[("w", other)]),
    }
}

/// The pace sentence from `limits::pace`, in the UI language. Unknown text is shown as is.
pub fn pace_text(i18n: &I18n, text: &str) -> String {
    let after = |p: &str| text.strip_prefix(p).map(str::to_string);
    if let Some(t) = after("runs out in ") {
        return i18n.tf("pace.runs_out", &[("t", &t)]);
    }
    if let Some(t) = after("limit reached · resets in ") {
        return i18n.tf("pace.limit_reached", &[("t", &t)]);
    }
    if let Some(v) = text.strip_suffix("% left") {
        return i18n.tf("pct_left", &[("v", v)]);
    }
    match text {
        "on pace" => i18n.t("pace.on_pace"),
        "well under limit" => i18n.t("pace.under"),
        "set a token budget in Settings" => i18n.t("pace.set_budget"),
        "enter usage in Settings" => i18n.t("pace.enter_usage"),
        other => other.to_string(),
    }
}

/// `limits::headline` in the UI language: "Claude · Max 20x · This week 72% left · Resets 4 d 2 h".
pub fn plan_headline(i18n: &I18n, p: &AIPlan, now: i64) -> String {
    let win = window_label(i18n, &p.window);
    let base = [("provider", p.provider.as_str()), ("plan", p.plan.as_str()), ("win", win.as_str())];
    if p.pace_state == PaceState::Unknown && p.used_pct == 0.0 && p.source != "manual" {
        let mut a = base.to_vec();
        let pace = pace_text(i18n, &p.pace_text);
        a.push(("pace", &pace));
        return i18n.tf("plan.unknown", &a);
    }
    let left = format!("{:.0}", (100.0 - p.used_pct).max(0.0));
    let mut a = base.to_vec();
    a.push(("left", &left));
    let mut s = i18n.tf("plan.left", &a);
    if let Some(r) = p.resets_at.filter(|r| *r > now) {
        s.push_str(&i18n.tf("plan.resets", &[("t", &format_duration(r - now))]));
    }
    s
}

pub fn agent_state(i18n: &I18n, s: AgentState) -> String {
    i18n.t(match s {
        AgentState::Working => "agent.working",
        AgentState::Waiting => "agent.waiting",
        AgentState::Idle => "agent.idle",
    })
}

/// Project state as `projects::summarise` labels it.
pub fn project_state(i18n: &I18n, s: &str) -> String {
    match s {
        "running" | "idle" | "serving" => i18n.t(&format!("project.{s}")),
        other => other.to_string(),
    }
}

/// Error from planning an action, in the UI language. Unrecognised errors are shown as they are.
pub fn error_text(i18n: &I18n, e: &anyhow::Error) -> String {
    let m = e.to_string();
    if m.starts_with("nothing to stop") {
        i18n.t("err.nothing_to_stop")
    } else if m.starts_with("refusing to quit") {
        i18n.t("err.system")
    } else if m.starts_with("no process") || m.starts_with("no app") || m.starts_with("no agent") || m.starts_with("no container") {
        i18n.t("err.gone")
    } else {
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DevServer;

    fn server(pid: i32, port: u16, mem: u64) -> DevServer {
        DevServer { project: "p".into(), process_pid: pid, command: "node".into(), port, started_at: 0, last_active_at: 0, mem_bytes: mem, idle_secs: 9999, idle: true }
    }

    /// The English wording must stay identical to the core text the CLI prints.
    #[test]
    fn english_matches_core_wording() {
        let en = I18n::english();
        let u = Units::default();
        let one = vec![ProjectSummary { servers: vec![server(1, 3000, 458_000_000)], ..Default::default() }];
        let two = vec![ProjectSummary { servers: vec![server(1, 4322, 400_000_000), server(2, 8000, 238_000_000)], ..Default::default() }];
        for p in [&one, &two] {
            assert_eq!(idle_banner(&en, &u, p), projects::idle_banner(p));
        }
        assert_eq!(idle_banner(&en, &u, &[]), None);
    }

    #[test]
    fn spanish_banner_and_plans() {
        let es = I18n::new(Some("es"));
        let u = Units { decimal_comma: true, ..Default::default() };
        let two = vec![ProjectSummary { servers: vec![server(1, 4322, 400_000_000), server(2, 8000, 1_370_000_000)], ..Default::default() }];
        assert_eq!(
            idle_banner(&es, &u, &two).unwrap(),
            "2 servidores de desarrollo están en marcha pero inactivos. Detenerlos libera 1,77 GB y los puertos 4322, 8000."
        );
        assert_eq!(pace_text(&es, "runs out in 3 h 2 min"), "se agota en 3 h 2 min");
        assert_eq!(pace_text(&es, "on pace"), "al ritmo previsto");
        assert_eq!(window_label(&es, "week"), "Esta semana");
        let plan = AIPlan {
            provider: "Claude".into(),
            plan: "Max 20x".into(),
            window: "week".into(),
            limit_kind: "tokens".into(),
            used_pct: 28.0,
            resets_at: Some(1000 + 4 * 86_400 + 2 * 3600),
            pace_state: PaceState::OnPace,
            pace_text: "on pace".into(),
            source: "x".into(),
        };
        assert_eq!(plan_headline(&es, &plan, 1000), "Claude · Max 20x · Esta semana 72% restante · Se renueva en 4 d 2 h");
        assert_eq!(plan_headline(&I18n::english(), &plan, 1000), crate::limits::headline(&plan, 1000));
    }
}
