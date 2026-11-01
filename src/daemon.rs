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
use crate::history::{History, Sample};
use crate::model::{AlertKind, MetricKind, RankMetric, Snapshot};
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
    let shared = Arc::new(Shared {
        role,
        snapshot: RwLock::new(None),
        sparks: RwLock::new(Sparklines::default()),
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

/// Metrics written to history per tick (whole machine) and per app.
pub fn history_samples(s: &Snapshot) -> (Vec<(String, f64)>, Vec<(String, String, f64)>) {
    let machine: Vec<(String, f64)> = s.frames.iter().filter(|f| f.available).map(|f| (f.metric_kind.as_str().to_string(), f.value)).collect();
    let mut apps = Vec::new();
    for m in [RankMetric::Cpu, RankMetric::Memory, RankMetric::Energy] {
        for a in s.top_apps(m, 15) {
            let id = a.app.bundle_id.clone().unwrap_or_else(|| a.app.name.clone());
            apps.push((id, format!("app_{}", m.as_str()), a.metric(m)));
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
