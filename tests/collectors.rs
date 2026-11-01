//! M1: every collector's frame is timestamped, non-negative, and its breakdown is coherent.

use pulsebar::collectors::{all_frames, frame_for};
use pulsebar::model::{MetricKind, Snapshot};

fn fixture() -> Snapshot {
    serde_json::from_slice(&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/snapshot.json")).unwrap()).unwrap()
}

fn part(f: &pulsebar::model::MetricFrame, label: &str) -> f64 {
    f.breakdown.iter().find(|b| b.label == label).map(|b| b.value).unwrap_or_else(|| panic!("no {label}"))
}

#[test]
fn frames_are_coherent() {
    let s = fixture();
    let frames = all_frames(&s.machine, s.timestamp);
    assert_eq!(frames.len(), MetricKind::ALL.len());
    for f in &frames {
        assert_eq!(f.timestamp, s.timestamp);
        assert!(f.value >= 0.0 && f.value.is_finite(), "{:?}", f.metric_kind);
        assert!(f.breakdown.iter().all(|b| b.value >= 0.0), "{:?}", f.metric_kind);
    }
    let cpu = frame_for(MetricKind::Cpu, &s.machine, 0);
    assert!((part(&cpu, "user") + part(&cpu, "system") + part(&cpu, "idle") - 100.0).abs() < 0.5);
    assert!((part(&cpu, "user") + part(&cpu, "system") - cpu.value).abs() < 0.5);
    let mem = frame_for(MetricKind::Memory, &s.machine, 0);
    assert_eq!(part(&mem, "app") + part(&mem, "wired") + part(&mem, "compressed"), mem.value);
    assert!(mem.value <= s.machine.memory.total as f64);
    let net = frame_for(MetricKind::Network, &s.machine, 0);
    assert_eq!(part(&net, "down") + part(&net, "up"), net.value);
    let disk = frame_for(MetricKind::Disk, &s.machine, 0);
    assert_eq!(part(&disk, "read") + part(&disk, "write"), disk.value);
}

#[test]
fn unavailable_sensors_are_flagged_not_faked() {
    let m = pulsebar::model::MachineMetrics::default();
    for k in [MetricKind::Gpu, MetricKind::Neural, MetricKind::Temperature, MetricKind::Fans, MetricKind::Battery, MetricKind::Power] {
        let f = frame_for(k, &m, 0);
        assert!(!f.available, "{k:?} should be unavailable on an empty machine");
        assert_eq!(f.value, 0.0);
    }
}

#[test]
fn app_rows_match_process_sums() {
    // "Apps, not processes": every app row equals the sum of its processes.
    let s = fixture();
    for a in &s.apps {
        let sum: u64 = a.pids.iter().filter_map(|p| s.process(*p)).map(|p| p.mem_bytes).sum();
        assert_eq!(a.mem_bytes, sum, "{}", a.app.name);
        assert_eq!(a.process_count, a.pids.len());
    }
}
