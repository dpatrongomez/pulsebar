//! M9 acceptance: every CLI command has golden output (tables and `--json`), and every JSON
//! document validates against its schema in `schemas/`.
//!
//! Regenerate after an intentional change: `UPDATE_GOLDEN=1 cargo test --test cli_golden`.

use std::path::PathBuf;

use pulsebar::model::{RankMetric, Snapshot};
use pulsebar::units::{DataUnit, Units};
use pulsebar::{output, views};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture() -> Snapshot {
    serde_json::from_slice(&std::fs::read(root().join("tests/fixtures/snapshot.json")).unwrap()).unwrap()
}

fn golden(name: &str, actual: &str) {
    let path = root().join("tests/golden").join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(expected, actual, "golden mismatch for {name} (UPDATE_GOLDEN=1 to accept)");
}

fn validate(schema: &str, doc: &serde_json::Value) {
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(root().join("schemas").join(format!("{schema}.schema.json"))).unwrap()).unwrap();
    let v = jsonschema::validator_for(&s).expect("valid schema");
    let errors: Vec<String> = v.iter_errors(doc).map(|e| format!("{} at {}", e, e.instance_path())).collect();
    assert!(errors.is_empty(), "{schema}: {errors:#?}");
}

fn json_case(name: &str, doc: serde_json::Value) {
    validate(name, &doc);
    golden(&format!("{name}.json"), &(serde_json::to_string_pretty(&doc).unwrap() + "\n"));
}

#[test]
fn tables() {
    let s = fixture();
    let u = Units::default();
    golden("status.txt", &output::status(&s, &u));
    golden("top_memory.txt", &output::top(&s, RankMetric::Memory, 10, &u));
    golden("top_cpu.txt", &output::top(&s, RankMetric::Cpu, 5, &u));
    golden("top_net.txt", &output::top(&s, RankMetric::Network, 5, &u));
    golden("ports.txt", &output::ports(&s, &u));
    golden("projects.txt", &output::projects(&s, &u));
    golden("agents.txt", &output::agents(&s, &u));
    golden("limits.txt", &output::limits(&s));
    golden("process_702.txt", &output::process(&s, 702, &u, false).unwrap());
}

#[test]
fn json_documents_validate() {
    let s = fixture();
    json_case("status", views::status(&s));
    json_case("top", views::top(&s, RankMetric::Memory, 10));
    json_case("ports", views::ports(&s));
    json_case("what_is_on_port", views::what_is_on_port(&s, 4322));
    json_case("projects", views::projects(&s));
    json_case("agents", views::agents(&s));
    json_case("limits", views::limits(&s));
    json_case("containers", views::containers(&s));
}

#[test]
fn bits_setting_changes_rates_everywhere() {
    // M10: bytes → bits is reflected in every formatter that shows rates.
    let s = fixture();
    let bits = Units { data: DataUnit::Bits, ..Default::default() };
    let status = output::status(&s, &bits);
    assert!(status.contains("↓ 19.2 Mbps"), "{status}");
    assert!(status.contains("Memory    33.20 GB"), "memory stays in bytes");
}

#[test]
fn acceptance_examples_from_the_spec() {
    let s = fixture();
    let top = s.top_apps(RankMetric::Memory, 1)[0];
    assert_eq!((top.app.name.as_str(), top.process_count), ("Google Chrome", 96));
    // Row equals the manual sum of all its helpers (< 5 % error required; here exact).
    let manual: u64 = s.processes.iter().filter(|p| p.app_name == "Google Chrome").map(|p| p.mem_bytes).sum();
    assert_eq!(top.mem_bytes, manual);
    assert_eq!(
        pulsebar::projects::idle_banner(&s.projects).unwrap(),
        "2 dev servers are running but idle. Stopping them frees 638 MB and ports 4322, 8000."
    );
    assert_eq!(pulsebar::actions::freed_message("storefront-web", 458_000_000, &[4322]), "storefront-web stopped. 458 MB and port 4322 are free.");
    let rabbit = s.containers.iter().find(|c| c.name == "rabbitmq").unwrap();
    assert_eq!(pulsebar::docker::card_line(rabbit), "rabbitmq — 206 MB — 86 % of 240 MB — up 3 hours — port 5672");
    assert_eq!(pulsebar::limits::headline(&s.plans[0], s.timestamp / 1000), "Claude · Max 20x · This week 72% left · Resets 4 d 2 h");
}
