//! Search over the snapshot (⌘F in the window): which apps and which of their processes match a
//! free-text query. Pure and allocation-light so the GUI can run it once per frame.
//!
//! A query matches, case-insensitively:
//! * an **app** by name or bundle id;
//! * a **process** by name, executable path or command line substring, by exact PID, or by exact listening port (`:3000` or `3000`).
//!
//! `:3000` matches ports only. A bare number matches a PID or port when one exists with that exact
//! value (so typing a PID shows exactly that process); otherwise it falls back to plain text.

use std::collections::{HashMap, HashSet};

use crate::model::{AppGroup, Process, Snapshot};

/// What a query matched inside one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppHit {
    /// The app itself matched (name / bundle id): show it with all its processes.
    App,
    /// Only these processes matched: show the app expanded with just them.
    Processes(Vec<i32>),
}

#[derive(Debug, Clone)]
pub struct Matcher {
    text: String,
    /// Exact PID / port to look for.
    number: Option<u32>,
    /// `:3000` — match listening ports only.
    port_only: bool,
    /// Bare number that equals a real PID or port: text matching is skipped.
    number_is_exact: bool,
    /// pid → listening ports (only pids that listen).
    ports: HashMap<i32, Vec<u16>>,
}

impl Matcher {
    /// `None` for an empty / whitespace-only query (no filtering).
    pub fn new(query: &str, snap: &Snapshot) -> Option<Self> {
        let q = query.trim();
        if q.is_empty() {
            return None;
        }
        let (port_only, body) = match q.strip_prefix(':') {
            Some(rest) => (true, rest.trim()),
            None => (false, q),
        };
        let number = (!body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())).then(|| body.parse::<u32>().ok()).flatten();
        let mut ports: HashMap<i32, Vec<u16>> = HashMap::new();
        for p in &snap.ports {
            ports.entry(p.pid).or_default().push(p.port);
        }
        let number_is_exact = !port_only
            && number.is_some_and(|n| snap.processes.iter().any(|p| p.pid as i64 == n as i64) || snap.ports.iter().any(|p| p.port as u32 == n));
        Some(Self { text: body.to_lowercase(), number, port_only, number_is_exact, ports })
    }

    fn lists_port(&self, pid: i32, n: u32) -> bool {
        self.ports.get(&pid).is_some_and(|v| v.iter().any(|p| *p as u32 == n))
    }

    fn contains(&self, hay: &str) -> bool {
        !self.text.is_empty() && hay.to_lowercase().contains(&self.text)
    }

    /// Does the app itself (name / bundle id) match?
    pub fn matches_app(&self, a: &AppGroup) -> bool {
        if self.port_only || self.number_is_exact {
            return false;
        }
        self.contains(&a.app.name) || a.app.bundle_id.as_deref().is_some_and(|b| self.contains(b))
    }

    /// Does this process match on its own fields?
    pub fn matches_process(&self, p: &Process) -> bool {
        if let Some(n) = self.number {
            if p.pid as i64 == n as i64 && !self.port_only {
                return true;
            }
            if self.lists_port(p.pid, n) {
                return true;
            }
        }
        if self.port_only || self.number_is_exact {
            return false;
        }
        self.contains(&p.name)
            || self.contains(&p.executable_path)
            || p.argv.iter().any(|a| self.contains(a))
    }
}

/// Result of filtering a snapshot: matching apps (by key) and, for apps that matched through
/// their processes only, the matching PIDs. One pass over the snapshot's processes.
#[derive(Debug, Default)]
pub struct Matches {
    pub apps: HashMap<String, AppHit>,
    /// Every matching process, whichever way its app matched; used for row highlighting.
    pub pids: HashSet<i32>,
}

pub fn filter(snap: &Snapshot, m: &Matcher) -> Matches {
    let mut out = Matches::default();
    for a in &snap.apps {
        if m.matches_app(a) {
            // The app matched by itself; still record process-level hits to highlight them.
            for pid in &a.pids {
                if snap.process(*pid).is_some_and(|p| m.matches_process(p)) {
                    out.pids.insert(*pid);
                }
            }
            out.apps.insert(a.key.clone(), AppHit::App);
            continue;
        }
        let hits: Vec<i32> = a.pids.iter().copied().filter(|pid| snap.process(*pid).is_some_and(|p| m.matches_process(p))).collect();
        if !hits.is_empty() {
            out.pids.extend(hits.iter().copied());
            out.apps.insert(a.key.clone(), AppHit::Processes(hits));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{App, ListeningPort};

    fn proc(pid: i32, name: &str, argv: &[&str]) -> Process {
        Process { pid, name: name.into(), executable_path: format!("/usr/local/bin/{name}"), argv: argv.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn snap() -> Snapshot {
        let procs = vec![
            proc(100, "Slack", &[]),
            proc(101, "Slack Helper", &["--type=renderer"]),
            proc(412, "node", &["node", "server.js", "--port", "3000"]),
            proc(3000, "weird", &[]),
            proc(500, "postgres", &[]),
        ];
        let app = |key: &str, name: &str, bundle: Option<&str>, pids: &[i32]| AppGroup {
            key: key.into(),
            app: App { name: name.into(), bundle_id: bundle.map(Into::into), ..Default::default() },
            pids: pids.to_vec(),
            ..Default::default()
        };
        Snapshot {
            apps: vec![
                app("bundle:com.tinyspeck.slackmacgap", "Slack", Some("com.tinyspeck.slackmacgap"), &[100, 101]),
                app("name:node", "node", None, &[412, 3000]),
                app("name:postgres", "postgres", None, &[500]),
            ],
            processes: procs,
            ports: vec![
                ListeningPort { pid: 412, port: 3000, address: "*".into(), protocol: "tcp".into(), command: "node".into() },
                ListeningPort { pid: 500, port: 5432, address: "*".into(), protocol: "tcp".into(), command: "postgres".into() },
            ],
            ..Default::default()
        }
    }

    fn run(q: &str) -> Matches {
        let s = snap();
        filter(&s, &Matcher::new(q, &s).unwrap())
    }

    #[test]
    fn empty_query_means_no_filter() {
        let s = snap();
        assert!(Matcher::new("", &s).is_none());
        assert!(Matcher::new("   ", &s).is_none());
    }

    #[test]
    fn app_name_and_bundle_id() {
        assert_eq!(run("slack").apps.get("bundle:com.tinyspeck.slackmacgap"), Some(&AppHit::App));
        assert!(run("SLACK").apps.contains_key("bundle:com.tinyspeck.slackmacgap"));
        assert!(run("tinyspeck").apps.contains_key("bundle:com.tinyspeck.slackmacgap"));
        assert!(!run("slack").apps.contains_key("name:node"));
    }

    #[test]
    fn process_name_and_command_line() {
        let m = run("renderer");
        assert_eq!(m.apps.get("bundle:com.tinyspeck.slackmacgap"), Some(&AppHit::Processes(vec![101])));
        assert_eq!(m.apps.len(), 1);
        assert_eq!(run("server.js").apps.get("name:node"), Some(&AppHit::Processes(vec![412])));
    }

    #[test]
    fn port_with_and_without_colon() {
        for q in ["3000", ":3000"] {
            let m = run(q);
            let hits = &m.apps["name:node"];
            match hits {
                AppHit::Processes(v) => assert!(v.contains(&412), "{q}: {v:?}"),
                AppHit::App => panic!("{q}: unexpected app-level hit"),
            }
        }
        assert!(run(":5432").pids.contains(&500));
    }

    #[test]
    fn colon_matches_ports_only_not_pids_or_text() {
        // pid 3000 exists but ":3000" is a port query.
        let m = run(":3000");
        assert_eq!(m.pids, HashSet::from([412]));
        assert!(run(":slack").apps.is_empty());
    }

    #[test]
    fn bare_number_is_an_exact_pid_not_a_substring() {
        // 412 appears nowhere else, 3000 is both a pid and a port.
        let m = run("412");
        assert_eq!(m.pids, HashSet::from([412]));
        assert_eq!(m.apps.get("name:node"), Some(&AppHit::Processes(vec![412])));
        assert_eq!(m.apps.len(), 1);
        // 41 is not an exact pid/port: falls back to text, which matches nothing here.
        assert!(run("41").apps.is_empty());
        // "10" is no pid, but substring-matches nothing; "100" is exactly Slack's pid.
        let m = run("100");
        assert_eq!(m.pids, HashSet::from([100]));
        assert_eq!(m.apps.len(), 1);
    }

    #[test]
    fn pid_and_port_collision_shows_both() {
        let m = run("3000");
        assert_eq!(m.pids, HashSet::from([412, 3000]));
    }

    #[test]
    fn unmatched_query_returns_nothing() {
        let m = run("zzzz");
        assert!(m.apps.is_empty() && m.pids.is_empty());
    }
}
