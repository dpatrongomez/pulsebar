//! GPU, Neural Engine / power (IOReport), temperatures + fans (SMC), battery, audio, Bluetooth.
//! All of these use slower data sources and run in background workers. Linux has its own source
//! for each (see `crate::linux`) and no Neural Engine.

use std::time::Duration;

use super::{Background, MetricCollector};
use crate::model::*;
use crate::sys;

fn plist_from_cmd(cmd: &str, args: &[&str], timeout: Duration) -> Option<plist::Value> {
    let out = sys::run(cmd, args, timeout)?;
    plist::Value::from_reader_xml(out.as_bytes()).ok()
}

fn int(d: &plist::Dictionary, k: &str) -> Option<i64> {
    match d.get(k)? {
        plist::Value::Integer(i) => i.as_signed().or_else(|| i.as_unsigned().map(|u| u as i64)),
        plist::Value::Boolean(b) => Some(*b as i64),
        plist::Value::Real(r) => Some(*r as i64),
        _ => None,
    }
}

fn boolean(d: &plist::Dictionary, k: &str) -> Option<bool> {
    match d.get(k)? {
        plist::Value::Boolean(b) => Some(*b),
        plist::Value::Integer(i) => i.as_signed().map(|v| v != 0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------------------------

/// Parse `ioreg -r -d 1 -w 0 -c IOAccelerator -a`.
pub fn parse_ioaccelerator(v: &plist::Value) -> GpuMetrics {
    let mut g = GpuMetrics::default();
    let Some(arr) = v.as_array() else { return g };
    for item in arr {
        let Some(d) = item.as_dictionary() else { continue };
        let Some(perf) = d.get("PerformanceStatistics").and_then(|p| p.as_dictionary()) else { continue };
        g.available = true;
        if g.name.is_empty() {
            g.name = d.get("model").and_then(|m| m.as_string()).or_else(|| d.get("IOClass").and_then(|m| m.as_string())).unwrap_or("GPU").to_string();
        }
        let util = int(perf, "Device Utilization %").or_else(|| int(perf, "GPU Activity(%)")).unwrap_or(0) as f64;
        g.utilization_pct = g.utilization_pct.max(util);
        let used = int(perf, "In use system memory").or_else(|| int(perf, "vramUsedBytes")).unwrap_or(0).max(0) as u64;
        g.vram_used += used;
        if let Some(total_mb) = int(d, "VRAM,totalMB") {
            g.vram_total += total_mb.max(0) as u64 * 1_000_000;
        }
    }
    g
}

pub fn parse_gpu_name(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v["SPDisplaysDataType"][0]["sppci_model"].as_str().map(str::to_string)
}

pub struct GpuCollector {
    worker: Background<GpuMetrics>,
}

impl GpuCollector {
    #[cfg(not(target_os = "macos"))]
    pub fn new() -> Self {
        Self { worker: Background::spawn("gpu", Duration::from_secs(2), || (), |_| crate::linux::read_gpu()) }
    }

    #[cfg(target_os = "macos")]
    pub fn new() -> Self {
        let worker = Background::spawn(
            "gpu",
            Duration::from_secs(2),
            || {
                sys::run("/usr/sbin/system_profiler", &["SPDisplaysDataType", "-json"], Duration::from_secs(10))
                    .and_then(|j| parse_gpu_name(&j))
            },
            |name: &mut Option<String>| {
                let v = plist_from_cmd("/usr/sbin/ioreg", &["-r", "-d", "1", "-w", "0", "-c", "IOAccelerator", "-a"], Duration::from_secs(3))?;
                let mut g = parse_ioaccelerator(&v);
                if let Some(n) = name {
                    g.name = n.clone();
                }
                Some(g)
            },
        );
        Self { worker }
    }
}

impl Default for GpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricCollector for GpuCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Gpu
    }
    fn collect(&mut self, out: &mut MachineMetrics) {
        if let Some(mut g) = self.worker.latest() {
            g.power_w = out.gpu.power_w; // filled by EnergyCollector
            if g.vram_total == 0 {
                g.vram_total = out.memory.total; // unified memory
            }
            out.gpu = g;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Energy: CPU/GPU/ANE power via IOReport → Neural tab + Power
// ---------------------------------------------------------------------------------------------

/// ANE power at which we call it "100 % busy" (rough; Apple publishes no utilisation API).
pub const ANE_REFERENCE_W: f64 = 8.0;

#[derive(Clone, Copy, Default, Debug)]
pub struct EnergySample {
    pub cpu: f64,
    pub gpu: f64,
    pub ane: f64,
    pub dram: f64,
}

pub struct EnergyCollector {
    worker: Option<Background<EnergySample>>,
}

impl EnergyCollector {
    pub fn new() -> Self {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let worker = Background::spawn("energy", Duration::from_secs(3), super::ffi::EnergyReporter::new, |r| {
                let w = r.as_mut()?.sample()?;
                Some(EnergySample { cpu: w.cpu, gpu: w.gpu, ane: w.ane, dram: w.dram })
            });
            Self { worker: Some(worker) }
        }
        #[cfg(target_os = "linux")]
        {
            // RAPL counts microjoules for the CPU package; most distros restrict it to root, and
            // then this simply never reports. Neural and GPU power stay at 0: nothing measures them.
            let worker = Background::spawn("energy", Duration::from_secs(3), || None, |prev: &mut Option<(std::time::Instant, u64)>| {
                let now = std::time::Instant::now();
                let uj = crate::linux::rapl_energy_uj()?;
                let last = prev.replace((now, uj));
                let (t0, uj0) = last?;
                let cpu = uj.saturating_sub(uj0) as f64 / 1e6 / now.duration_since(t0).as_secs_f64().max(0.1);
                Some(EnergySample { cpu, ..Default::default() })
            });
            Self { worker: Some(worker) }
        }
        #[cfg(not(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64"))))]
        {
            Self { worker: None }
        }
    }
}

impl Default for EnergyCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricCollector for EnergyCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Neural
    }
    fn collect(&mut self, out: &mut MachineMetrics) {
        let Some(e) = self.worker.as_ref().and_then(|w| w.latest()) else {
            out.neural.available = false;
            return;
        };
        if cfg!(target_os = "linux") {
            out.power.cpu_w = Some(e.cpu);
            if out.power.system_w.is_none() {
                out.power.system_w = Some(e.cpu);
            }
            return;
        }
        out.neural = NeuralMetrics {
            available: true,
            power_w: e.ane,
            utilization_pct: (e.ane / ANE_REFERENCE_W * 100.0).clamp(0.0, 100.0),
        };
        out.power.cpu_w = Some(e.cpu);
        out.power.gpu_w = Some(e.gpu);
        out.power.ane_w = Some(e.ane);
        out.gpu.power_w = Some(e.gpu);
        if out.power.system_w.is_none() {
            out.power.system_w = Some(e.cpu + e.gpu + e.ane + e.dram);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Temperatures + fans
// ---------------------------------------------------------------------------------------------

/// Pick CPU/GPU temperatures out of a sensor list (labels differ between Intel and Apple silicon).
pub fn classify_temps(all: &[(String, f64)]) -> (Option<f64>, Option<f64>) {
    let avg = |pred: &dyn Fn(&str) -> bool| {
        let v: Vec<f64> = all.iter().filter(|(l, t)| pred(l) && *t > 0.0 && *t < 130.0).map(|(_, t)| *t).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    let cpu = avg(&|l: &str| {
        let l = l.to_ascii_lowercase();
        // The last four are Linux's Intel and AMD sensors.
        l.contains("cpu") || l.contains("tdie") || l.contains("pacc") || l.contains("eacc") || l.contains("coretemp") || l.contains("k10temp") || l.contains("zenpower") || l.contains("tctl")
    });
    let gpu = avg(&|l: &str| {
        let l = l.to_ascii_lowercase();
        l.contains("gpu") || l.contains("nouveau") || l.contains("radeon")
    });
    (cpu, gpu)
}

pub struct SensorsCollector {
    worker: Background<SensorMetrics>,
}

struct SensorState {
    comps: sysinfo::Components,
    #[cfg(target_os = "macos")]
    smc: Option<super::ffi::Smc>,
}

impl SensorsCollector {
    pub fn new() -> Self {
        let worker = Background::spawn(
            "sensors",
            Duration::from_secs(10),
            || SensorState {
                comps: sysinfo::Components::new_with_refreshed_list(),
                #[cfg(target_os = "macos")]
                smc: super::ffi::Smc::open(),
            },
            |st: &mut SensorState| {
                st.comps.refresh(false);
                let all: Vec<(String, f64)> = st
                    .comps
                    .list()
                    .iter()
                    .filter_map(|c| c.temperature().map(|t| (c.label().to_string(), t as f64)))
                    .collect();
                let (cpu, gpu) = classify_temps(&all);
                #[allow(unused_mut)]
                let mut fans = if cfg!(target_os = "linux") { crate::linux::read_fans() } else { Vec::new() };
                #[cfg(target_os = "macos")]
                if let Some(smc) = &st.smc {
                    let n = smc.read("FNum").unwrap_or(0.0) as usize;
                    for i in 0..n.min(4) {
                        if let Some(rpm) = smc.read(&format!("F{i}Ac")) {
                            fans.push(Fan {
                                index: i,
                                rpm: rpm.max(0.0),
                                min_rpm: smc.read(&format!("F{i}Mn")).unwrap_or(0.0),
                                max_rpm: smc.read(&format!("F{i}Mx")).unwrap_or(0.0),
                            });
                        }
                    }
                }
                Some(SensorMetrics {
                    cpu_temp_c: cpu,
                    gpu_temp_c: gpu,
                    fans,
                    all: all.into_iter().map(|(l, t)| Breakdown::new(l, t)).collect(),
                })
            },
        );
        Self { worker }
    }
}

impl Default for SensorsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricCollector for SensorsCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Temperature
    }
    fn collect(&mut self, out: &mut MachineMetrics) {
        if let Some(s) = self.worker.latest() {
            out.sensors = s;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Battery
// ---------------------------------------------------------------------------------------------

/// Returns battery metrics and whole-system power (W) when the SMC telemetry exposes it.
pub fn parse_smart_battery(v: &plist::Value) -> (BatteryMetrics, Option<f64>) {
    let mut b = BatteryMetrics::default();
    let Some(d) = v.as_array().and_then(|a| a.first()).and_then(|x| x.as_dictionary()) else {
        return (b, None);
    };
    b.present = boolean(d, "BatteryInstalled").unwrap_or(true);
    let cur = int(d, "CurrentCapacity").unwrap_or(0) as f64;
    let max = int(d, "MaxCapacity").unwrap_or(100).max(1) as f64;
    b.percent = (cur * 100.0 / max).clamp(0.0, 100.0);
    b.charging = boolean(d, "IsCharging").unwrap_or(false);
    b.on_ac = boolean(d, "ExternalConnected").unwrap_or(false);
    b.cycle_count = int(d, "CycleCount");
    let design = int(d, "DesignCapacity").unwrap_or(0) as f64;
    let raw_max = int(d, "AppleRawMaxCapacity").or_else(|| int(d, "NominalChargeCapacity")).unwrap_or(0) as f64;
    if design > 0.0 && raw_max > 0.0 {
        b.health_pct = Some((raw_max * 100.0 / design).min(100.0));
    }
    b.temperature_c = int(d, "Temperature").map(|t| t as f64 / 100.0);
    let mins = int(d, "TimeRemaining").or_else(|| int(d, "AvgTimeToEmpty"));
    b.time_remaining_min = mins.filter(|m| *m > 0 && *m < 65_535);
    if let (Some(mv), Some(ma)) = (int(d, "Voltage"), int(d, "InstantAmperage").or_else(|| int(d, "Amperage"))) {
        // Amperage is reported as a signed 64-bit value: negative while discharging.
        let ma = ma as i32 as f64;
        b.power_w = Some(-(mv as f64 / 1000.0) * (ma / 1000.0));
    }
    let system_w = d
        .get("PowerTelemetryData")
        .and_then(|p| p.as_dictionary())
        .and_then(|p| int(p, "SystemLoad").or_else(|| int(p, "SystemPowerIn")))
        .filter(|mw| *mw > 0)
        .map(|mw| mw as f64 / 1000.0);
    (b, system_w)
}

pub struct BatteryCollector {
    worker: Background<(BatteryMetrics, Option<f64>)>,
}

impl BatteryCollector {
    pub fn new() -> Self {
        let worker = Background::spawn("battery", Duration::from_secs(10), || (), |_| {
            if cfg!(target_os = "linux") {
                return crate::linux::read_battery().map(|b| (b, None));
            }
            let v = plist_from_cmd("/usr/sbin/ioreg", &["-r", "-n", "AppleSmartBattery", "-a"], Duration::from_secs(3))?;
            Some(parse_smart_battery(&v))
        });
        Self { worker }
    }
}

impl Default for BatteryCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricCollector for BatteryCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Battery
    }
    fn collect(&mut self, out: &mut MachineMetrics) {
        if let Some((b, sys_w)) = self.worker.latest() {
            if sys_w.is_some() {
                out.power.system_w = sys_w;
            } else if b.present && !b.on_ac {
                out.power.system_w = b.power_w.filter(|w| *w > 0.0);
            }
            out.battery = b;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Audio (CoreAudio HAL)
// ---------------------------------------------------------------------------------------------

pub struct AudioCollector {
    worker: Background<AudioMetrics>,
}

impl AudioCollector {
    pub fn new() -> Self {
        let worker = Background::spawn("audio", Duration::from_secs(2), || (), |_| Some(read_audio()));
        Self { worker }
    }
}

impl Default for AudioCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
pub fn read_audio() -> AudioMetrics {
    use super::ffi;
    let mut a = AudioMetrics::default();
    if let Some(dev) = ffi::default_device(false) {
        a.output_device = ffi::device_name(dev);
        a.output_volume = ffi::device_volume(dev, false);
        a.output_muted = ffi::device_muted(dev);
    }
    if let Some(dev) = ffi::default_device(true) {
        a.input_volume = ffi::device_volume(dev, true);
    }
    a.apps = ffi::audio_processes()
        .into_iter()
        .filter(|(_, out, inp)| *out || *inp)
        .map(|(pid, out, inp)| AudioApp {
            pid,
            app_name: String::new(),
            bundle_id: None,
            state: if !out {
                AudioState::Silent
            } else if a.output_muted {
                AudioState::Muted
            } else {
                AudioState::Playing
            },
            using_mic: inp,
        })
        .collect();
    a
}

#[cfg(not(target_os = "macos"))]
pub fn read_audio() -> AudioMetrics {
    crate::linux::read_audio()
}

/// Set output (or input) volume, 0..1. Returns false when the device has no volume control.
pub fn set_volume(input: bool, v: f64) -> bool {
    #[cfg(target_os = "macos")]
    {
        super::ffi::default_device(input).map(|d| super::ffi::set_device_volume(d, input, v)).unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::linux::set_volume(input, v)
    }
}

pub fn set_muted(muted: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        super::ffi::default_device(false).map(|d| super::ffi::set_device_muted(d, muted)).unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::linux::set_muted(muted)
    }
}

impl MetricCollector for AudioCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Sound
    }
    fn collect(&mut self, out: &mut MachineMetrics) {
        if let Some(a) = self.worker.latest() {
            out.audio = a;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Bluetooth (system_profiler; slow → every 60 s)
// ---------------------------------------------------------------------------------------------

fn pct(v: &serde_json::Value) -> Option<f64> {
    v.as_str().and_then(|s| s.trim_end_matches('%').trim().parse().ok()).or_else(|| v.as_f64())
}

pub fn parse_bluetooth(json: &str) -> BluetoothMetrics {
    let mut m = BluetoothMetrics::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return m };
    let root = &v["SPBluetoothDataType"][0];
    m.powered = root["controller_properties"]["controller_state"].as_str().map(|s| s.contains("on")).unwrap_or(false);
    for (key, connected) in [("device_connected", true), ("device_not_connected", false)] {
        let Some(list) = root[key].as_array() else { continue };
        for entry in list {
            let Some(obj) = entry.as_object() else { continue };
            for (name, d) in obj {
                m.devices.push(BluetoothDevice {
                    name: name.clone(),
                    address: d["device_address"].as_str().unwrap_or("").to_string(),
                    kind: d["device_minorType"].as_str().unwrap_or("").to_string(),
                    connected,
                    battery_main: pct(&d["device_batteryLevelMain"]).or_else(|| pct(&d["device_batteryLevel"])),
                    battery_left: pct(&d["device_batteryLevelLeft"]),
                    battery_right: pct(&d["device_batteryLevelRight"]),
                    battery_case: pct(&d["device_batteryLevelCase"]),
                });
            }
        }
    }
    m
}

pub struct BluetoothCollector {
    worker: Background<BluetoothMetrics>,
}

impl BluetoothCollector {
    pub fn new() -> Self {
        let worker = Background::spawn("bluetooth", Duration::from_secs(60), || (), |_| {
            if cfg!(target_os = "linux") {
                return crate::linux::read_bluetooth();
            }
            sys::run("/usr/sbin/system_profiler", &["SPBluetoothDataType", "-json"], Duration::from_secs(20)).map(|j| parse_bluetooth(&j))
        });
        Self { worker }
    }
}

impl Default for BluetoothCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricCollector for BluetoothCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Bluetooth
    }
    fn collect(&mut self, out: &mut MachineMetrics) {
        if let Some(b) = self.worker.latest() {
            out.bluetooth = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_fixture() {
        let j = r#"{"SPBluetoothDataType":[{"controller_properties":{"controller_state":"attrib_on"},
          "device_connected":[{"AirPods Pro":{"device_address":"AA:BB","device_minorType":"Headphones",
             "device_batteryLevelLeft":"85%","device_batteryLevelRight":"90%","device_batteryLevelCase":"60%"}},
             {"Magic Trackpad":{"device_address":"CC","device_batteryLevelMain":"41%"}}],
          "device_not_connected":[{"Old Keyboard":{"device_address":"DD"}}]}]}"#;
        let m = parse_bluetooth(j);
        assert!(m.powered);
        assert_eq!(m.devices.len(), 3);
        assert_eq!(m.devices[0].battery_left, Some(85.0));
        assert_eq!(m.devices[0].battery_case, Some(60.0));
        assert_eq!(m.devices[1].battery_main, Some(41.0));
        assert!(!m.devices[2].connected);
    }

    #[test]
    fn temps() {
        let all = vec![("PMU tdie1".to_string(), 50.0), ("PMU tdie2".into(), 60.0), ("GPU MTR".into(), 45.0), ("NAND".into(), 30.0)];
        let (c, g) = classify_temps(&all);
        assert_eq!(c, Some(55.0));
        assert_eq!(g, Some(45.0));
    }

    #[test]
    fn linux_sensor_labels() {
        let all = vec![("coretemp Package id 0".to_string(), 60.0), ("k10temp Tctl".into(), 70.0), ("amdgpu edge".into(), 50.0), ("nvme Composite".into(), 35.0)];
        let (c, g) = classify_temps(&all);
        assert_eq!(c, Some(65.0));
        assert_eq!(g, Some(50.0));
    }

    #[test]
    fn battery_plist() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><array><dict>
          <key>CurrentCapacity</key><integer>80</integer><key>MaxCapacity</key><integer>100</integer>
          <key>IsCharging</key><false/><key>ExternalConnected</key><false/>
          <key>CycleCount</key><integer>123</integer><key>DesignCapacity</key><integer>5000</integer>
          <key>AppleRawMaxCapacity</key><integer>4500</integer><key>Temperature</key><integer>3050</integer>
          <key>Voltage</key><integer>12000</integer><key>InstantAmperage</key><integer>18446744073709550616</integer>
          <key>TimeRemaining</key><integer>300</integer></dict></array></plist>"#;
        let v = plist::Value::from_reader_xml(xml.as_bytes()).unwrap();
        let (b, _) = parse_smart_battery(&v);
        assert_eq!(b.percent, 80.0);
        assert_eq!(b.health_pct, Some(90.0));
        assert_eq!(b.temperature_c, Some(30.5));
        assert_eq!(b.time_remaining_min, Some(300));
        let w = b.power_w.unwrap();
        assert!((w - 12.0).abs() < 0.01, "discharging 1A at 12V = 12W, got {w}");
    }

    #[test]
    fn gpu_plist() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><array><dict>
          <key>IOClass</key><string>AGXAcceleratorG14X</string>
          <key>PerformanceStatistics</key><dict><key>Device Utilization %</key><integer>27</integer>
          <key>In use system memory</key><integer>1500000000</integer></dict></dict></array></plist>"#;
        let v = plist::Value::from_reader_xml(xml.as_bytes()).unwrap();
        let g = parse_ioaccelerator(&v);
        assert!(g.available);
        assert_eq!(g.utilization_pct, 27.0);
        assert_eq!(g.vram_used, 1_500_000_000);
    }
}
