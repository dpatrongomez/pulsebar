//! Times each phase of a sampling tick: `cargo run --release --example tick_profile`
use std::time::Instant;
use pulsebar::collectors::{self, process::ProcessSampler};
use pulsebar::grouping::Grouper;
use pulsebar::model::MachineMetrics;

fn main() {
    let mut cols = collectors::default_collectors();
    let mut ps = ProcessSampler::new(false);
    let mut g = Grouper::new();
    let mut m = MachineMetrics::default();
    let _ = ps.sample();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let n = 10;
    let (mut tc, mut tp, mut tg) = (0.0, 0.0, 0.0);
    for _ in 0..n {
        let t = Instant::now();
        for c in &mut cols { c.collect(&mut m); }
        tc += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut procs = ps.sample();
        tp += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let apps = g.group(&mut procs);
        tg += t.elapsed().as_secs_f64();
        std::hint::black_box(apps);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!("collectors {:.2} ms · processes {:.2} ms · grouping {:.2} ms (per tick)", tc / n as f64 * 1e3, tp / n as f64 * 1e3, tg / n as f64 * 1e3);
}
