//! Containers tab: Docker Engine API over the local unix socket (no TCP, no network).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::model::{ComposeStack, Container};
use crate::units::format_bytes;

pub fn socket_candidates() -> Vec<PathBuf> {
    let home = crate::paths::home();
    let mut v = Vec::new();
    if let Ok(h) = std::env::var("DOCKER_HOST")
        && let Some(p) = h.strip_prefix("unix://") {
            v.push(PathBuf::from(p));
        }
    v.extend([
        home.join(".docker/run/docker.sock"),
        PathBuf::from("/var/run/docker.sock"),
        home.join(".orbstack/run/docker.sock"),
        home.join(".colima/default/docker.sock"),
        home.join(".rd/docker.sock"),
    ]);
    v
}

pub fn find_socket() -> Option<PathBuf> {
    socket_candidates().into_iter().find(|p| p.exists())
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let Some(pos) = rest.windows(2).position(|w| w == b"\r\n") else { break };
        let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..pos]).trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
        rest = &rest[pos + 2..];
        if size == 0 || rest.len() < size {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[(size + 2).min(rest.len())..];
    }
    out
}

/// Minimal HTTP/1.1 over a unix socket (Connection: close).
pub fn request(sock: &PathBuf, method: &str, path: &str) -> Result<(u16, Vec<u8>)> {
    let mut s = UnixStream::connect(sock).with_context(|| format!("connect {}", sock.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(s, "{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").context("bad HTTP response")?;
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = &buf[split + 4..];
    let chunked = head.to_ascii_lowercase().contains("transfer-encoding: chunked");
    Ok((status, if chunked { dechunk(body) } else { body.to_vec() }))
}

fn get_json(sock: &PathBuf, path: &str) -> Result<Value> {
    let (status, body) = request(sock, "GET", path)?;
    if status != 200 {
        bail!("docker API {path}: HTTP {status}");
    }
    Ok(serde_json::from_slice(&body)?)
}

/// Build a container from `/containers/json` + optional stats + inspect.
pub fn parse_container(list_item: &Value, stats: Option<&Value>, inspect: Option<&Value>) -> Container {
    let labels = &list_item["Labels"];
    let mut ports: Vec<u16> = list_item["Ports"]
        .as_array()
        .map(|a| a.iter().filter_map(|p| p["PublicPort"].as_u64().map(|x| x as u16)).collect())
        .unwrap_or_default();
    ports.sort_unstable();
    ports.dedup();
    let mut c = Container {
        id: list_item["Id"].as_str().unwrap_or("").chars().take(12).collect(),
        name: list_item["Names"][0].as_str().unwrap_or("").trim_start_matches('/').to_string(),
        image: list_item["Image"].as_str().unwrap_or("").to_string(),
        stack: labels["com.docker.compose.project"].as_str().map(str::to_string),
        project_hint: labels["com.docker.compose.project.working_dir"].as_str().map(str::to_string),
        state: list_item["State"].as_str().unwrap_or("").to_string(),
        status: list_item["Status"].as_str().unwrap_or("").to_string(),
        published_ports: ports,
        ..Default::default()
    };
    if let Some(svc) = labels["com.docker.compose.service"].as_str()
        && c.stack.is_some() {
            c.name = svc.to_string();
        }
    if let Some(s) = stats {
        let ms = &s["memory_stats"];
        let usage = ms["usage"].as_u64().unwrap_or(0);
        let cache = ms["stats"]["inactive_file"].as_u64().or_else(|| ms["stats"]["cache"].as_u64()).unwrap_or(0);
        c.mem_bytes = usage.saturating_sub(cache);
        c.mem_limit = ms["limit"].as_u64().unwrap_or(0);
        let cpu = s["cpu_stats"]["cpu_usage"]["total_usage"].as_f64().unwrap_or(0.0);
        let pre = s["precpu_stats"]["cpu_usage"]["total_usage"].as_f64().unwrap_or(0.0);
        let sys = s["cpu_stats"]["system_cpu_usage"].as_f64().unwrap_or(0.0);
        let presys = s["precpu_stats"]["system_cpu_usage"].as_f64().unwrap_or(0.0);
        if sys > presys && cpu >= pre {
            // % of the Docker VM (all its CPUs = 100 %), consistent with "% of Mac" elsewhere.
            c.cpu_pct = (cpu - pre) / (sys - presys) * 100.0;
        }
        if let Some(nets) = s["networks"].as_object() {
            c.net_rx_bytes = nets.values().map(|n| n["rx_bytes"].as_u64().unwrap_or(0)).sum();
            c.net_tx_bytes = nets.values().map(|n| n["tx_bytes"].as_u64().unwrap_or(0)).sum();
        }
    }
    // Only treat the limit as real when one was explicitly configured.
    if let Some(i) = inspect {
        let explicit = i["HostConfig"]["Memory"].as_u64().unwrap_or(0);
        c.mem_limit = explicit;
    }
    c
}

pub fn list_containers() -> Result<Vec<Container>> {
    let sock = find_socket().context("no Docker socket found")?;
    let list = get_json(&sock, "/containers/json?all=1")?;
    let items = list.as_array().cloned().unwrap_or_default();
    let handles: Vec<_> = items
        .into_iter()
        .map(|item| {
            let sock = sock.clone();
            std::thread::spawn(move || {
                let id = item["Id"].as_str().unwrap_or("").to_string();
                let running = item["State"].as_str() == Some("running");
                let stats = running.then(|| get_json(&sock, &format!("/containers/{id}/stats?stream=false")).ok()).flatten();
                let inspect = get_json(&sock, &format!("/containers/{id}/json")).ok();
                parse_container(&item, stats.as_ref(), inspect.as_ref())
            })
        })
        .collect();
    let mut v: Vec<Container> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    v.sort_by(|a, b| a.stack.cmp(&b.stack).then(a.name.cmp(&b.name)));
    Ok(v)
}

/// Stop a container. Only called after explicit user confirmation (see `actions`).
pub(crate) fn stop_container(id: &str) -> Result<()> {
    let sock = find_socket().context("no Docker socket found")?;
    let (status, _) = request(&sock, "POST", &format!("/containers/{id}/stop"))?;
    if status != 204 && status != 304 {
        bail!("docker stop {id}: HTTP {status}");
    }
    Ok(())
}

pub fn stacks(containers: &[Container]) -> Vec<(ComposeStack, Vec<&Container>)> {
    let mut map: std::collections::BTreeMap<String, (ComposeStack, Vec<&Container>)> = Default::default();
    for c in containers {
        let name = c.stack.clone().unwrap_or_else(|| "standalone".into());
        let e = map.entry(name.clone()).or_insert_with(|| (ComposeStack { name, project_hint: c.project_hint.clone() }, vec![]));
        e.1.push(c);
    }
    map.into_values().collect()
}

/// Containers at ≥ 80 % of an explicit memory limit.
pub fn near_limit(containers: &[Container]) -> Vec<&Container> {
    containers.iter().filter(|c| c.up() && c.mem_pct_of_limit().is_some_and(|p| p >= 80.0)).collect()
}

/// "rabbitmq — 206 MB — 86 % of 240 MB — up 3 hours — port 5672"
pub fn card_line(c: &Container) -> String {
    let mut parts = vec![c.name.clone(), format_bytes(c.mem_bytes as f64)];
    if let Some(p) = c.mem_pct_of_limit() {
        parts.push(format!("{p:.0} % of {}", format_bytes(c.mem_limit as f64)));
    }
    parts.push(c.status.to_lowercase());
    if !c.published_ports.is_empty() {
        let ps: Vec<String> = c.published_ports.iter().map(u16::to_string).collect();
        parts.push(format!("port{} {}", if ps.len() > 1 { "s" } else { "" }, ps.join(", ")));
    }
    parts.join(" — ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rabbitmq_card() {
        let item = json!({"Id":"abcdef1234567890","Names":["/shop-rabbitmq-1"],"Image":"rabbitmq:3","State":"running","Status":"Up 3 hours",
            "Labels":{"com.docker.compose.project":"shop","com.docker.compose.service":"rabbitmq"},
            "Ports":[{"PrivatePort":5672,"PublicPort":5672},{"PrivatePort":5672,"PublicPort":5672}]});
        let stats = json!({"memory_stats":{"usage":216_000_000u64,"limit":8_000_000_000u64,"stats":{"inactive_file":10_000_000u64}},
            "cpu_stats":{"cpu_usage":{"total_usage":200.0},"system_cpu_usage":10000.0},
            "precpu_stats":{"cpu_usage":{"total_usage":100.0},"system_cpu_usage":9000.0},
            "networks":{"eth0":{"rx_bytes":10,"tx_bytes":20}}});
        let inspect = json!({"HostConfig":{"Memory":240_000_000u64}});
        let c = parse_container(&item, Some(&stats), Some(&inspect));
        assert_eq!(c.name, "rabbitmq");
        assert_eq!(c.stack.as_deref(), Some("shop"));
        assert_eq!(c.mem_bytes, 206_000_000);
        assert!((c.cpu_pct - 10.0).abs() < 1e-9);
        assert_eq!(card_line(&c), "rabbitmq — 206 MB — 86 % of 240 MB — up 3 hours — port 5672");
        assert_eq!(near_limit(std::slice::from_ref(&c)).len(), 1);
    }

    #[test]
    fn chunked() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), b"Wikipedia");
    }
}
