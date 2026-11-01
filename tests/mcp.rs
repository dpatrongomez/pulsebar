//! M8 acceptance against the fixture: well-formed JSON-RPC, data scoped to the current user,
//! and `pulsebar_request_quit` only ever creates a pending request.

use std::cell::RefCell;
use std::rc::Rc;

use pulsebar::actions::{PendingRequest, PendingStatus};
use pulsebar::mcp::{Server, SnapshotSource};
use pulsebar::model::Snapshot;
use serde_json::{Value, json};

struct Fixture(Snapshot);
impl SnapshotSource for Fixture {
    fn snapshot(&mut self) -> Snapshot {
        self.0.clone()
    }
}

fn server() -> (Server<Fixture>, Rc<RefCell<Vec<PendingRequest>>>) {
    let snap: Snapshot =
        serde_json::from_slice(&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/snapshot.json")).unwrap()).unwrap();
    let mut s = Server::new(Fixture(snap)).with_uid(501);
    let created = Rc::new(RefCell::new(Vec::new()));
    let c = created.clone();
    s.create_pending = Box::new(move |plan| {
        let r = PendingRequest {
            id: "q-test".into(),
            created_at: 0,
            source: "mcp".into(),
            summary: plan.summary(),
            plan,
            status: PendingStatus::PendingConfirmation,
            outcome: None,
        };
        c.borrow_mut().push(r.clone());
        Ok(r)
    });
    (s, created)
}

fn call(s: &mut Server<Fixture>, name: &str, args: Value) -> Value {
    let r = s.handle(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], 7);
    r["result"].clone()
}

#[test]
fn handshake_and_tool_list() {
    let (mut s, _) = server();
    let init = s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none(), "notifications get no reply");
    let list = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["pulsebar_status", "pulsebar_top_apps", "pulsebar_what_is_on_port", "pulsebar_list_projects", "pulsebar_list_agents", "pulsebar_request_quit"] {
        assert!(names.contains(&n), "missing {n}");
    }
    let bad = s.handle(&json!({"jsonrpc":"2.0","id":3,"method":"nope"})).unwrap();
    assert_eq!(bad["error"]["code"], -32601);
}

#[test]
fn whats_slow_returns_structured_json_scoped_to_user() {
    let (mut s, _) = server();
    let r = call(&mut s, "pulsebar_top_apps", json!({"metric":"memory","limit":10}));
    assert_eq!(r["isError"], false);
    let apps = r["structuredContent"]["apps"].as_array().unwrap();
    assert_eq!(apps[0]["app"], "Google Chrome");
    // macOS row consists only of root / _windowserver processes → excluded.
    assert!(apps.iter().all(|a| a["app"] != "macOS"));
    // Text content is the same JSON, pretty-printed.
    let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, r["structuredContent"]);

    let port = call(&mut s, "pulsebar_what_is_on_port", json!({"port":4322}));
    let l = &port["structuredContent"]["listeners"][0];
    assert_eq!((l["process"].as_str(), l["project"].as_str(), l["idle_secs"].as_i64()), (Some("node"), Some("storefront-web"), Some(7200)));

    let d = call(&mut s, "pulsebar_diagnose", json!({}));
    assert!(d["structuredContent"]["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("dev servers are running but idle")));
}

#[test]
fn request_quit_never_kills_only_queues() {
    let (mut s, created) = server();
    let r = call(&mut s, "pulsebar_request_quit", json!({"dev_server": 702}));
    assert_eq!(r["structuredContent"]["status"], "pending_confirmation");
    assert!(r["structuredContent"]["message"].as_str().unwrap().contains("Nothing has been stopped"));
    let reqs = created.borrow();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].plan.pids, vec![702]);
    assert_eq!(reqs[0].plan.ports, vec![4322]);
    // Other users' processes can't even be requested.
    drop(reqs);
    let e = call(&mut s, "pulsebar_request_quit", json!({"pid": 90}));
    assert_eq!(e["isError"], true);
}
