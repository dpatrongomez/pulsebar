//! Per-collector cost: `cargo run --release --example collector_profile`
use std::time::Instant;
use pulsebar::collectors;
use pulsebar::model::MachineMetrics;

fn main() {
    let mut cols = collectors::default_collectors();
    let mut m = MachineMetrics::default();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let mut t = vec![0.0; cols.len()];
    for _ in 0..60 {
        for (i, c) in cols.iter_mut().enumerate() {
            let s = Instant::now();
            c.collect(&mut m);
            t[i] += s.elapsed().as_secs_f64();
        }
    }
    for (i, c) in cols.iter().enumerate() {
        println!("{:?}: {:.3} ms/tick", c.kind(), t[i] / 60.0 * 1e3);
    }
}
