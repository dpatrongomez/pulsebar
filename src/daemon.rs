//! Long-running sampling loop.
//!
//! PulseBar runs as two processes so the always-on part stays small:
//! * **Agent** (`pulsebar app`): menu bar items, history, alerts, `live.json`. No GPU window.
//! * **Window** (`pulsebar window`): the egui window. It samples for itself while open (for
//!   1 s freshness) but never writes history or evaluates alerts, and exits when closed.
//!
//! The two talk through small files in Application Support (alerts, snoozes, navigation,
//! settings), which also keeps the CLI and MCP server on the same data.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::alerts::{self, Alert, AlertEngine};
use crate::history::{self, AppEvent, History, RestartDetector, Sample};
use crate::model::{AlertKind, AppGroup, MetricKind, RankMetric, Snapshot};
use crate::sampler::Engine;
use crate::settings::Settings;
use crate::{live, paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Agent,
    Window,
}

/// Short in-memory series for sparklines (last ~2 min per metric).
#[derive(Default, Clone)]
pub struct Sparklines {
    pub series: std::collections::HashMap<MetricKind, VecDeque<f64>>,
}

impl Sparklines {
    pub const LEN: usize = 120;
    pub fn push(&mut self, s: &Snapshot) {
        for f in &s.frames {
            let q = self.series.entry(f.metric_kind).or_default();
            q.push_back(f.value);
            while q.len() > Self::LEN {
                q.pop_front();
            }
        }
    }
    /// Fill from the last 10 minutes of 1-second history buckets.
    pub fn seed(&mut self, h: &History, now: i64) {
        for k in MetricKind::ALL {
            let Ok(rows) = h.series(k.as_str(), None, now - 600, now, now) else { continue };
            let q = self.series.entry(k).or_default();
            q.extend(rows.iter().map(|b| b.value_avg));
            while q.len() > Self::LEN {
                q.pop_front();
            }
        }
    }

    pub fn get(&self, k: MetricKind) -> Vec<f64> {
        self.series.get(&k).map(|q| q.iter().copied().collect()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snooze {
    pub kind: AlertKind,
    pub app_key: String,
    pub until: i64,
}

pub struct Shared {
    pub role: Role,
    pub snapshot: RwLock<Option<Arc<Snapshot>>>,
    pub sparks: RwLock<Sparklines>,
    pub alerts: Mutex<Vec<Alert>>,
    pub settings: RwLock<Settings>,
    pub foreground: AtomicBool,
    pub history: Mutex<Option<History>>,
    /// Called after each tick (the agent uses it to refresh menu bar items).
    pub on_tick: Mutex<Option<Box<dyn Fn() + Send>>>,
}

// ---- small file channels ----------------------------------------------------------------------

fn read_json<T: for<'de> Deserialize<'de> + Default>(p: &std::path::Path) -> T {
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_json<T: Serialize>(p: &std::path::Path, v: &T) {
    let _ = paths::ensure_dirs();
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_vec(v).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

pub fn read_alerts() -> Vec<Alert> {
    read_json(&paths::alerts_file())
}

pub fn dismiss_alert(a: &Alert) {
    let mut v = read_alerts();
    v.retain(|x| !(x.kind == a.kind && x.app_key == a.app_key && x.at == a.at));
    write_json(&paths::alerts_file(), &v);
}

pub fn snooze_alert(a: &Alert, until: i64) {
    let mut v: Vec<Snooze> = read_json(&paths::snoozes_file());
    v.retain(|s| s.until > crate::sys::now_s());
    v.push(Snooze { kind: a.kind, app_key: a.app_key.clone(), until });
    write_json(&paths::snoozes_file(), &v);
    dismiss_alert(a);
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

// ---- loop ---------------------------------------------------------------------------------------

pub fn spawn(settings: Settings, role: Role) -> Arc<Shared> {
    let history = History::open(&paths::database_file()).ok();
    // A freshly opened window starts with the agent's recent history so the Overview bars
    // aren't empty for the first two minutes.
    let mut sparks = Sparklines::default();
    if role == Role::Window
        && let Some(h) = history.as_ref() {
            sparks.seed(h, crate::sys::now_s());
        }
    let shared = Arc::new(Shared {
        role,
        snapshot: RwLock::new(None),
        sparks: RwLock::new(sparks),
        alerts: Mutex::new(read_alerts()),
        settings: RwLock::new(settings.clone()),
        foreground: AtomicBool::new(role == Role::Window),
        history: Mutex::new(history),
        on_tick: Mutex::new(None),
    });
    let sh = shared.clone();
    std::thread::Builder::new().name("pulsebar-sampler".into()).spawn(move || run(sh, settings)).expect("spawn sampler");
    shared
}

/// Apps kept in history: the top N by each of CPU, memory, energy, disk and network.
pub const TRACKED_PER_METRIC: usize = 15;

/// Metrics written to history per tick (whole machine) and per app.
pub fn history_samples(s: &Snapshot) -> (Vec<(String, f64)>, Vec<(String, String, f64)>) {
    let mut machine: Vec<(String, f64)> = s.frames.iter().filter(|f| f.available).map(|f| (f.metric_kind.as_str().to_string(), f.value)).collect();
    // Separate series for the Overview's "Written today" / "Downloaded" totals.
    machine.push(("disk_write".into(), s.machine.disk.write_bps));
    machine.push(("net_down".into(), s.machine.network.rx_bps));
    // Per-app series for every app that is among the busiest by any metric, so the app detail
    // view has all five charts. Keys of the set are history ids (bundle id, else name).
    let mut tracked: Vec<&AppGroup> = Vec::new();
    for m in RankMetric::ALL {
        // Idle apps (value 0) don't earn a place: they'd only fill the ranking by name.
        for a in s.top_apps(m, TRACKED_PER_METRIC).into_iter().filter(|a| a.metric(m) > 0.0) {
            if !tracked.iter().any(|t| t.key == a.key) {
                tracked.push(a);
            }
        }
    }
    let mut apps = Vec::new();
    for a in tracked {
        let id = history::app_id(a);
        for m in RankMetric::ALL {
            apps.push((id.clone(), format!("app_{}", m.as_str()), a.metric(m)));
        }
    }
    (machine, apps)
}

fn run(sh: Arc<Shared>, settings: Settings) {
    let agent = sh.role == Role::Agent;
    let mut engine = Engine::new(&settings, true);
    let mut alert_engine = agent.then(|| {
        let mut rules = alerts::default_rules(settings.alert_disk_write_bps, settings.alert_network_bps);
        if let Some(h) = sh.history.lock().unwrap().as_ref() {
            for r in h.load_rules().unwrap_or_default() {
                if let Some(x) = rules.iter_mut().find(|x| x.kind == r.kind) {
                    *x = r;
                }
            }
        }
        AlertEngine::new(rules)
    });
    let mut restarts = RestartDetector::default();
    let mut last_live = Instant::now() - Duration::from_secs(60);
    let mut last_gc = Instant::now();
    let mut settings_mtime = mtime(&paths::settings_file());
    let mut snooze_mtime = None;
    let mut alerts_mtime = None;
    loop {
        let start = Instant::now();

        // Settings edited by the window (or the CLI) → pick them up.
        let m = mtime(&paths::settings_file());
        if m != settings_mtime {
            settings_mtime = m;
            *sh.settings.write().unwrap() = Settings::load();
        }
        let st = sh.settings.read().unwrap().clone();

        // In the background (menu bar only) the process table refreshes every third tick.
        engine.process_every = if sh.foreground.load(Ordering::Relaxed) { 1 } else { 3 };
        let snap = engine.tick();
        let now = snap.timestamp / 1000;

        if agent {
            if let Some(h) = sh.history.lock().unwrap().as_mut() {
                let (machine, apps) = history_samples(&snap);
                let mut samples: Vec<Sample> = machine.iter().map(|(m, v)| Sample { app: None, metric: m, value: *v }).collect();
                samples.extend(apps.iter().map(|(a, m, v)| Sample { app: Some(a), metric: m, value: *v }));
                h.record(now, &samples);
                let _ = h.maintain(now, st.retention.seconds(), 10);
                for (id, old, new) in restarts.feed(&snap) {
                    let name = snap.apps.iter().find(|g| history::app_id(g) == id).map(|g| g.app.name.clone()).unwrap_or_else(|| id.clone());
                    let _ = h.record_event(&AppEvent { ts: now, app: id, kind: "restart".into(), message: format!("{name} restarted (main pid {old} → {new})") });
                }
            }
            let ae = alert_engine.as_mut().unwrap();
            let sm = mtime(&paths::snoozes_file());
            if sm != snooze_mtime {
                snooze_mtime = sm;
                for s in read_json::<Vec<Snooze>>(&paths::snoozes_file()) {
                    ae.snooze(s.kind, &s.app_key, s.until);
                }
            }
            let fired = ae.feed_snapshot(&snap);
            if !fired.is_empty() {
                if let Some(h) = sh.history.lock().unwrap().as_ref() {
                    for a in fired.iter().filter(|a| !a.app_key.is_empty()) {
                        if let Some(g) = snap.apps.iter().find(|g| g.key == a.app_key) {
                            let _ = h.record_event(&AppEvent { ts: a.at, app: history::app_id(g), kind: "alert".into(), message: a.message.clone() });
                        }
                    }
                }
                if st.notifications {
                    for a in &fired {
                        alerts::notify(if a.app_name.is_empty() { "Alert" } else { &a.app_name }, &a.message);
                    }
                }
                let mut all = read_alerts();
                all.extend(fired);
                let len = all.len();
                if len > 50 {
                    all.drain(..len - 50);
                }
                write_json(&paths::alerts_file(), &all);
            }
            if last_live.elapsed() >= Duration::from_secs(5) {
                let _ = live::write(&snap);
                last_live = Instant::now();
            }
            if last_gc.elapsed() >= Duration::from_secs(3600) {
                crate::actions::gc_pending();
                last_gc = Instant::now();
            }
        }
        // Both roles mirror the alerts file (the window shows/dismisses them).
        let am = mtime(&paths::alerts_file());
        if am != alerts_mtime {
            alerts_mtime = am;
            *sh.alerts.lock().unwrap() = read_alerts();
        }

        sh.sparks.write().unwrap().push(&snap);
        *sh.snapshot.write().unwrap() = Some(Arc::new(snap));
        if let Some(cb) = sh.on_tick.lock().unwrap().as_ref() {
            cb();
        }

        let interval =
            if sh.foreground.load(Ordering::Relaxed) { st.sample_interval_ms.max(500) } else { st.background_interval_ms.max(1000) };
        std::thread::sleep(Duration::from_millis(interval).saturating_sub(start.elapsed()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{App, AppGroup};

    fn app(name: &str, cpu: f64, mem: u64, net: f64) -> AppGroup {
        AppGroup {
            key: format!("name:{name}"),
            app: App { name: name.into(), ..Default::default() },
            cpu_pct: cpu,
            mem_bytes: mem,
            net_rx_bps: net,
            ..Default::default()
        }
    }

    #[test]
    fn tracks_union_of_top_apps_with_all_metrics() {
        // 20 apps; "Chatty" is only busy on the network, "Hog" only on CPU/memory.
        let mut apps: Vec<AppGroup> = (0..20).map(|i| app(&format!("a{i}"), 1.0 + i as f64, 1000 * (i + 1) as u64, 0.0)).collect();
        apps.push(app("Chatty", 0.0, 1, 9e9));
        let s = Snapshot { apps, ..Default::default() };
        let (_, rows) = history_samples(&s);
        let ids: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert!(ids.contains("Chatty"), "top network app must be tracked");
        assert!(ids.contains("a19") && !ids.contains("a0"));
        // Every tracked app gets all five series.
        for id in &ids {
            let metrics: Vec<&str> = rows.iter().filter(|r| r.0 == *id).map(|r| r.1.as_str()).collect();
            assert_eq!(metrics.len(), 5, "{id}: {metrics:?}");
            assert!(metrics.contains(&"app_disk") && metrics.contains(&"app_network"));
        }
        let net = rows.iter().find(|r| r.0 == "Chatty" && r.1 == "app_network").unwrap();
        assert_eq!(net.2, 9e9);
    }
}
