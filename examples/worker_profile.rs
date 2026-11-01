//! Cost of the background workers' data sources.
use std::time::Instant;

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("{label}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    r
}

fn main() {
    let s = pulsebar::settings::Settings::default();
    let home = pulsebar::paths::home();
    time("limits::compute", || pulsebar::limits::compute(&s.plans, &home, pulsebar::sys::now_s()));
    time("listening_ports (lsof)", pulsebar::sys::listening_ports);
    let mut comps = sysinfo::Components::new_with_refreshed_list();
    time("components refresh", || comps.refresh(false));
    time("audio", pulsebar::collectors::devices::read_audio);
    time("docker", || pulsebar::docker::list_containers().ok());
    for _ in 0..3 {
        time("audio (warm)", pulsebar::collectors::devices::read_audio);
        time("components (warm)", || comps.refresh(false));
        time("limits (warm)", || pulsebar::limits::compute(&s.plans, &home, pulsebar::sys::now_s()));
    }
}

#[allow(dead_code)]
fn unused() {}
