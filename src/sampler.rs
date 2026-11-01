//! The sampling engine: runs every collector, samples and groups processes and derives the
//! developer views (ports, projects, agents, containers, AI plans) into one `Snapshot`.

use std::collections::HashMap;
use std::time::Duration;

use crate::collectors::{self, Background, MetricCollector, process::ProcessSampler};
use crate::grouping::Grouper;
use crate::model::*;
use crate::projects::{self, ActivityTracker, ProjectResolver};
use crate::settings::Settings;
use crate::sys::{self, BundleCache};
use crate::{agents, docker, limits};

enum Source<T> {
    Bg(Background<T>),
    Inline(Box<dyn FnMut() -> T + Send>, Option<T>),
}

impl<T: Clone + Send + 'static> Source<T> {
    fn get(&mut self) -> Option<T> {
        match self {
            Source::Bg(b) => b.latest(),
            Source::Inline(f, cache) => {
                if cache.is_none() {
                    *cache = Some(f());
                }
                cache.clone()
            }
        }
    }
}

fn source<T: Clone + Send + 'static>(daemon: bool, name: &str, every: Duration, f: impl FnMut() -> T + Send + 'static) -> Source<T> {
    if daemon {
        let mut f = f;
        Source::Bg(Background::spawn(name, every, || (), move |_| Some(f())))
    } else {
        Source::Inline(Box::new(f), None)
    }
}

pub struct Engine {
    collectors: Vec<Box<dyn MetricCollector>>,
    procs: ProcessSampler,
    grouper: Grouper<BundleCache>,
    resolver: ProjectResolver,
    pub tracker: ActivityTracker,
    machine: MachineMetrics,
    ports: Source<Vec<ListeningPort>>,
    docker: Source<(bool, Vec<Container>)>,
    plans: Source<Vec<AIPlan>>,
    idle_threshold_s: i64,
    my_uid: u32,
    hostname: String,
    ticks: u64,
    /// Refresh the process table every N ticks (machine metrics refresh every tick).
    pub process_every: u64,
    last_procs: Option<(Vec<Process>, Vec<AppGroup>)>,
}

impl Engine {
    /// `daemon`: long-running (background workers, nettop). Otherwise one-shot for the CLI.
    pub fn new(settings: &Settings, daemon: bool) -> Self {
        let plan_cfg = settings.plans.clone();
        let mut tracker = ActivityTracker::new();
        if let Some(prev) = crate::live::read_any() {
            let servers: Vec<DevServer> = prev.projects.into_iter().flat_map(|p| p.servers).collect();
            tracker.seed(&servers);
        }
        Self {
            collectors: collectors::default_collectors(),
            procs: ProcessSampler::new(daemon),
            grouper: Grouper::new(),
            resolver: ProjectResolver::new(),
            tracker,
            machine: MachineMetrics::default(),
            ports: source(daemon, "ports", Duration::from_secs(5), sys::listening_ports),
            docker: source(daemon, "docker", Duration::from_secs(10), || match docker::list_containers() {
                Ok(v) => (true, v),
                Err(_) => (false, vec![]),
            }),
            plans: source(daemon, "limits", Duration::from_secs(60), move || limits::compute(&plan_cfg, &crate::paths::home(), sys::now_s())),
            idle_threshold_s: settings.idle_threshold_min * 60,
            my_uid: sys::current_uid(),
            hostname: sysinfo::System::host_name().unwrap_or_default(),
            ticks: 0,
            process_every: 1,
            last_procs: None,
        }
    }

    pub fn tick(&mut self) -> Snapshot {
        let now_ms = sys::now_ms();
        let now = now_ms / 1000;
        for c in &mut self.collectors {
            c.collect(&mut self.machine);
        }
        let refresh = self.last_procs.is_none() || self.ticks.is_multiple_of(self.process_every.max(1));
        self.ticks += 1;
        let (processes, apps) = if refresh {
            let mut processes = self.procs.sample();
            let apps = self.grouper.group(&mut processes);
            self.last_procs = Some((processes.clone(), apps.clone()));
            (processes, apps)
        } else {
            self.last_procs.clone().unwrap_or_default()
        };
        self.machine.disk.read_bps = processes.iter().map(|p| p.disk_read_bps).sum();
        self.machine.disk.write_bps = processes.iter().map(|p| p.disk_write_bps).sum();
        self.tracker.update(&processes, now);
        fill_audio_apps(&mut self.machine.audio, &processes);

        let ports = self.ports.get().unwrap_or_default();
        let projects =
            projects::summarize(&processes, &ports, &mut self.resolver, &self.tracker, self.my_uid, now, self.idle_threshold_s);
        let agents = agents::sessions(&processes, &ports, &mut self.resolver, &self.tracker, self.my_uid, now);
        let (docker_available, containers) = self.docker.get().unwrap_or_default();
        let plans = self.plans.get().unwrap_or_default();

        Snapshot {
            timestamp: now_ms,
            hostname: self.hostname.clone(),
            frames: collectors::all_frames(&self.machine, now_ms),
            machine: self.machine.clone(),
            processes,
            apps,
            ports,
            projects,
            agents,
            plans,
            containers,
            docker_available,
        }
    }

    /// Two ticks ~1 s apart so CPU% and rates are meaningful (CLI one-shot).
    pub fn sample_once(settings: &Settings) -> Snapshot {
        let mut e = Engine::new(settings, false);
        let _ = e.tick();
        std::thread::sleep(Duration::from_millis(1000));
        e.tick()
    }
}

/// Resolve CoreAudio pids to app names; one row per app (Playing > Muted > Silent).
pub fn fill_audio_apps(audio: &mut AudioMetrics, procs: &[Process]) {
    let by_pid: HashMap<i32, &Process> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut merged: Vec<AudioApp> = Vec::new();
    for a in audio.apps.drain(..) {
        let (name, bundle) = by_pid
            .get(&a.pid)
            .map(|p| (p.app_name.clone(), p.app_bundle_id.clone()))
            .unwrap_or_else(|| (format!("pid {}", a.pid), None));
        let rank = |s: AudioState| match s {
            AudioState::Playing => 2,
            AudioState::Muted => 1,
            AudioState::Silent => 0,
        };
        if let Some(m) = merged.iter_mut().find(|m| m.app_name == name) {
            if rank(a.state) > rank(m.state) {
                m.state = a.state;
            }
            m.using_mic |= a.using_mic;
        } else {
            merged.push(AudioApp { app_name: name, bundle_id: bundle, ..a });
        }
    }
    merged.sort_by(|a, b| a.app_name.cmp(&b.app_name));
    audio.apps = merged;
}
