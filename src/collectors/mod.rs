//! M1 — Metric collectors.
//!
//! Every collector implements [`MetricCollector`]: on each sampling tick it updates its part of
//! [`MachineMetrics`]; [`frame_for`] turns that into a timestamped [`MetricFrame`].
//! Collectors whose data source is slow (ioreg, system_profiler, SMC, IOReport) run in a
//! [`Background`] worker at their own cadence and the tick only copies the latest value, so the
//! 1 s tick never blocks.

pub mod devices;
#[cfg(target_os = "macos")]
pub mod ffi;
pub mod process;
pub mod system;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::model::{Breakdown, MachineMetrics, MetricFrame, MetricKind};

pub trait MetricCollector: Send {
    fn kind(&self) -> MetricKind;
    /// Update this collector's part of `out`.
    fn collect(&mut self, out: &mut MachineMetrics);
}

/// A worker thread that refreshes a value at its own interval.
pub struct Background<T> {
    latest: Arc<Mutex<Option<T>>>,
    stop: Arc<AtomicBool>,
}

impl<T: Clone + Send + 'static> Background<T> {
    pub fn spawn<S, I, F>(name: &str, interval: Duration, init: I, mut f: F) -> Self
    where
        I: FnOnce() -> S + Send + 'static,
        F: FnMut(&mut S) -> Option<T> + Send + 'static,
    {
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        let _ = std::thread::Builder::new().name(format!("pulsebar-{name}")).spawn(move || {
            let mut state = init();
            while !s.load(Ordering::Relaxed) {
                if let Some(v) = f(&mut state) {
                    *l.lock().unwrap() = Some(v);
                }
                // Sleep in small steps so drop() stops the worker promptly.
                let mut slept = Duration::ZERO;
                while slept < interval && !s.load(Ordering::Relaxed) {
                    let step = Duration::from_millis(200).min(interval - slept);
                    std::thread::sleep(step);
                    slept += step;
                }
            }
        });
        Self { latest, stop }
    }

    pub fn latest(&self) -> Option<T> {
        self.latest.lock().unwrap().clone()
    }
}

impl<T> Drop for Background<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Build the standard collector set.
pub fn default_collectors() -> Vec<Box<dyn MetricCollector>> {
    vec![
        Box::new(system::CpuCollector::new()),
        Box::new(system::MemoryCollector::new()),
        Box::new(system::DiskCollector::new()),
        Box::new(system::NetworkCollector::new()),
        Box::new(devices::GpuCollector::new()),
        Box::new(devices::EnergyCollector::new()),
        Box::new(devices::SensorsCollector::new()),
        Box::new(devices::BatteryCollector::new()),
        Box::new(devices::AudioCollector::new()),
        Box::new(devices::BluetoothCollector::new()),
    ]
}

/// The headline frame for each metric kind.
pub fn frame_for(kind: MetricKind, m: &MachineMetrics, timestamp: i64) -> MetricFrame {
    fn b(label: impl Into<String>, value: f64) -> Breakdown {
        Breakdown::new(label, value)
    }
    let (value, unit, available, breakdown) = match kind {
        MetricKind::Cpu => (
            m.cpu.total_pct,
            "%",
            true,
            vec![b("user", m.cpu.user_pct), b("system", m.cpu.system_pct), b("idle", m.cpu.idle_pct)],
        ),
        MetricKind::Memory => (
            m.memory.used as f64,
            "bytes",
            m.memory.total > 0,
            vec![
                b("app", m.memory.app as f64),
                b("wired", m.memory.wired as f64),
                b("compressed", m.memory.compressed as f64),
                b("cached", m.memory.cached as f64),
                b("free", m.memory.free as f64),
                b("swap", m.memory.swap_used as f64),
            ],
        ),
        MetricKind::Disk => (
            m.disk.read_bps + m.disk.write_bps,
            "bytes/s",
            true,
            vec![
                b("read", m.disk.read_bps),
                b("write", m.disk.write_bps),
                b("boot_free", m.disk.boot_free().map(|v| v.free as f64).unwrap_or(0.0)),
            ],
        ),
        MetricKind::Network => (
            m.network.rx_bps + m.network.tx_bps,
            "bytes/s",
            true,
            vec![b("down", m.network.rx_bps), b("up", m.network.tx_bps)],
        ),
        MetricKind::Gpu => (
            m.gpu.utilization_pct,
            "%",
            m.gpu.available,
            vec![b("vram_used", m.gpu.vram_used as f64), b("power_w", m.gpu.power_w.unwrap_or(0.0))],
        ),
        MetricKind::Neural => (m.neural.utilization_pct, "%", m.neural.available, vec![b("power_w", m.neural.power_w)]),
        MetricKind::Temperature => (
            m.sensors.cpu_temp_c.unwrap_or(0.0),
            "°C",
            m.sensors.cpu_temp_c.is_some(),
            vec![b("cpu", m.sensors.cpu_temp_c.unwrap_or(0.0)), b("gpu", m.sensors.gpu_temp_c.unwrap_or(0.0))],
        ),
        MetricKind::Fans => {
            let n = m.sensors.fans.len().max(1) as f64;
            (
                m.sensors.fans.iter().map(|f| f.rpm).sum::<f64>() / n,
                "rpm",
                !m.sensors.fans.is_empty(),
                m.sensors.fans.iter().map(|f| b(format!("fan{}", f.index), f.rpm)).collect(),
            )
        }
        MetricKind::Battery => (
            m.battery.percent,
            "%",
            m.battery.present,
            vec![b("power_w", m.battery.power_w.unwrap_or(0.0).abs())],
        ),
        MetricKind::Power => (
            m.power.system_w.unwrap_or(0.0),
            "W",
            m.power.system_w.is_some(),
            vec![
                b("cpu", m.power.cpu_w.unwrap_or(0.0)),
                b("gpu", m.power.gpu_w.unwrap_or(0.0)),
                b("ane", m.power.ane_w.unwrap_or(0.0)),
            ],
        ),
        MetricKind::Sound => (
            m.audio.output_volume.unwrap_or(0.0) * 100.0,
            "%",
            m.audio.output_volume.is_some(),
            vec![
                b("playing_apps", m.audio.apps.iter().filter(|a| a.state == crate::model::AudioState::Playing).count() as f64),
                b("input_volume", m.audio.input_volume.unwrap_or(0.0) * 100.0),
            ],
        ),
        MetricKind::Bluetooth => (
            m.bluetooth.devices.iter().filter(|d| d.connected).count() as f64,
            "devices",
            m.bluetooth.powered,
            m.bluetooth
                .devices
                .iter()
                .filter_map(|d| d.battery_main.or(d.battery_left).map(|v| b(d.name.clone(), v)))
                .collect(),
        ),
    };
    MetricFrame {
        timestamp,
        metric_kind: kind,
        value: if value.is_finite() { value.max(0.0) } else { 0.0 },
        unit: unit.to_string(),
        available,
        breakdown: breakdown
            .into_iter()
            .map(|mut x| {
                x.value = if x.value.is_finite() { x.value.max(0.0) } else { 0.0 };
                x
            })
            .collect(),
    }
}

pub fn all_frames(m: &MachineMetrics, timestamp: i64) -> Vec<MetricFrame> {
    MetricKind::ALL.into_iter().map(|k| frame_for(k, m, timestamp)).collect()
}
