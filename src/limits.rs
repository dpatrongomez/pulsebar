//! AI plan limits, computed **locally** from the agents' own log files. No provider API is ever
//! called (privacy rule: the only network calls are update check + licence).
//!
//! * Codex writes `rate_limits` (used %, window, reset) into `~/.codex/sessions/**/*.jsonl`.
//! * Claude Code writes per-message token usage into `~/.claude/projects/**/*.jsonl`; we sum
//!   the window and compare against the budget configured in settings.
//! * Copilot / Cursor: manual values from settings.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::model::{AIPlan, PaceState};
use crate::settings::PlanConfig;
use crate::units::format_duration;

/// Pace given usage, the window length and time to reset.
pub fn pace(used_pct: f64, window_s: i64, resets_in_s: Option<i64>) -> (PaceState, String) {
    let left = (100.0 - used_pct).max(0.0);
    let Some(resets_in) = resets_in_s.filter(|_| window_s > 0) else {
        return (PaceState::Unknown, format!("{left:.0}% left"));
    };
    if used_pct >= 100.0 {
        return (PaceState::RunsOut, format!("limit reached · resets in {}", format_duration(resets_in)));
    }
    let elapsed = (window_s - resets_in).clamp(1, window_s) as f64;
    let rate = used_pct / elapsed; // % per second
    if rate <= 0.0 {
        return (PaceState::OnPace, "on pace".into());
    }
    let to_full = left / rate;
    if to_full >= resets_in as f64 {
        let projected = used_pct + rate * resets_in as f64;
        if projected < 60.0 { (PaceState::Ahead, "well under limit".into()) } else { (PaceState::OnPace, "on pace".into()) }
    } else {
        (PaceState::RunsOut, format!("runs out in {}", format_duration(to_full as i64)))
    }
}

fn recent_jsonl(root: &Path, since: SystemTime, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            // Only descend into dirs touched within the window.
            if md.modified().map(|m| m >= since).unwrap_or(true) || depth < 3 {
                recent_jsonl(&p, since, out, depth + 1);
            }
        } else if p.extension().is_some_and(|x| x == "jsonl") && md.modified().map(|m| m >= since).unwrap_or(false) {
            out.push(p);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexWindow {
    pub used_pct: f64,
    pub window_minutes: i64,
    pub resets_at: i64,
}

/// Extract the latest `rate_limits` from Codex session log lines.
pub fn parse_codex_rate_limits(text: &str) -> Vec<(String, CodexWindow)> {
    let mut found = Vec::new();
    for line in text.lines().rev() {
        if !line.contains("rate_limits") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let ts = v["timestamp"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp()).unwrap_or(0);
        let rl = if v["payload"]["rate_limits"].is_object() { &v["payload"]["rate_limits"] } else { &v["rate_limits"] };
        for key in ["primary", "secondary"] {
            let w = &rl[key];
            let Some(used) = w["used_percent"].as_f64() else { continue };
            let window_minutes = w["window_minutes"].as_i64().unwrap_or(0);
            let resets_at = w["resets_at"].as_i64().or_else(|| w["resets_in_seconds"].as_i64().map(|s| ts + s)).unwrap_or(0);
            found.push((key.to_string(), CodexWindow { used_pct: used, window_minutes, resets_at }));
        }
        if !found.is_empty() {
            break;
        }
    }
    found
}

/// Sum Claude token usage (input + output + cache creation) for messages at/after `since`.
/// Returns (tokens, earliest message timestamp in window).
pub fn sum_claude_tokens(text: &str, since: i64) -> (f64, Option<i64>) {
    let mut total = 0.0;
    let mut first = None::<i64>;
    let mut seen = std::collections::HashSet::new();
    for line in text.lines() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(ts) = v["timestamp"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp()) else {
            continue;
        };
        if ts < since {
            continue;
        }
        // Streaming writes the same message id several times; count once.
        if let Some(id) = v["message"]["id"].as_str()
            && !seen.insert(id.to_string()) {
                continue;
            }
        let u = &v["message"]["usage"];
        let t = u["input_tokens"].as_f64().unwrap_or(0.0)
            + u["output_tokens"].as_f64().unwrap_or(0.0)
            + u["cache_creation_input_tokens"].as_f64().unwrap_or(0.0);
        total += t;
        first = Some(first.map_or(ts, |f: i64| f.min(ts)));
    }
    (total, first)
}

/// Stream matching lines of many files without loading them whole (logs can be huge).
fn for_each_line(files: &[PathBuf], needle: &str, mut f: impl FnMut(&str)) {
    use std::io::BufRead;
    for path in files {
        let Ok(file) = std::fs::File::open(path) else { continue };
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line.len() < 256 * 1024 && line.contains(needle) {
                        f(&line);
                    }
                }
            }
        }
    }
}

/// Per-file cache of (timestamp, tokens) entries keyed by (mtime, size): unchanged logs are
/// never re-read, appended ones are re-parsed.
type FileEntries = std::sync::Arc<Vec<(i64, f64)>>;

fn claude_file_entries(path: &Path) -> FileEntries {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (SystemTime, u64, FileEntries)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let md = std::fs::metadata(path).ok();
    let key = md.as_ref().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
    if let (Some((mt, len)), Some((cmt, clen, v))) = (key, cache.lock().unwrap().get(path))
        && mt == *cmt && len == *clen {
            return v.clone();
        }
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for_each_line(&[path.to_path_buf()], "\"usage\"", |line| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let Some(ts) = v["timestamp"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp()) else {
            return;
        };
        if let Some(id) = v["message"]["id"].as_str()
            && !seen.insert(id.to_string()) {
                return;
            }
        let u = &v["message"]["usage"];
        let t = u["input_tokens"].as_f64().unwrap_or(0.0)
            + u["output_tokens"].as_f64().unwrap_or(0.0)
            + u["cache_creation_input_tokens"].as_f64().unwrap_or(0.0);
        entries.push((ts, t));
    });
    let v = std::sync::Arc::new(entries);
    if let Some((mt, len)) = key {
        let mut c = cache.lock().unwrap();
        if c.len() > 2000 {
            c.clear();
        }
        c.insert(path.to_path_buf(), (mt, len, v.clone()));
    }
    v
}

fn claude_tokens_streaming(files: &[PathBuf], since: i64) -> (f64, Option<i64>) {
    let mut total = 0.0;
    let mut first: Option<i64> = None;
    for f in files {
        for (ts, t) in claude_file_entries(f).iter() {
            if *ts >= since {
                total += t;
                first = Some(first.map_or(*ts, |x| x.min(*ts)));
            }
        }
    }
    (total, first)
}

fn window_label(hours: f64) -> String {
    if hours >= 160.0 { "week".into() } else { format!("{hours:.0}h") }
}

pub fn compute(plans: &[PlanConfig], home: &Path, now: i64) -> Vec<AIPlan> {
    let mut out = Vec::new();
    for cfg in plans {
        let window_s = (cfg.window_hours * 3600.0) as i64;
        let since_sys = SystemTime::now() - Duration::from_secs(window_s.max(0) as u64);
        match cfg.provider.as_str() {
            "codex" => {
                let mut files = Vec::new();
                recent_jsonl(&home.join(".codex/sessions"), SystemTime::now() - Duration::from_secs(7 * 86_400), &mut files, 0);
                files.sort_by_key(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok());
                // Newest files first; stop at the first that has rate limits.
                let mut windows = Vec::new();
                for f in files.iter().rev().take(20) {
                    let mut last = String::new();
                    for_each_line(std::slice::from_ref(f), "rate_limits", |l| {
                        last.clear();
                        last.push_str(l);
                    });
                    windows = parse_codex_rate_limits(&last);
                    if !windows.is_empty() {
                        break;
                    }
                }
                for (_key, w) in windows {
                    let resets_in = (w.resets_at > now).then(|| w.resets_at - now);
                    let (state, text) = pace(w.used_pct, w.window_minutes * 60, resets_in);
                    out.push(AIPlan {
                        provider: "Codex".into(),
                        plan: cfg.plan.clone(),
                        window: window_label(w.window_minutes as f64 / 60.0),
                        limit_kind: "usage".into(),
                        used_pct: w.used_pct,
                        resets_at: (w.resets_at > 0).then_some(w.resets_at),
                        pace_state: state,
                        pace_text: text,
                        source: "~/.codex/sessions".into(),
                    });
                }
            }
            "claude" => {
                let (used_pct, resets_at, source) = if let Some(m) = cfg.manual_used_pct {
                    (Some(m), cfg.manual_resets_at, "manual".to_string())
                } else {
                    let mut files = Vec::new();
                    recent_jsonl(&home.join(".claude/projects"), since_sys, &mut files, 0);
                    let (tokens, first) = claude_tokens_streaming(&files, now - window_s);
                    let used = cfg.budget.filter(|b| *b > 0.0).map(|b| (tokens / b * 100.0).min(100.0));
                    let resets = cfg.manual_resets_at.map(|r| roll_forward(r, window_s, now)).or_else(|| first.map(|f| f + window_s));
                    (used, resets, format!("~/.claude/projects ({:.1}M tokens)", tokens / 1e6))
                };
                let resets_in = resets_at.filter(|r| *r > now).map(|r| r - now);
                let (state, text) = match used_pct {
                    Some(u) => pace(u, window_s, resets_in),
                    None => (PaceState::Unknown, "set a token budget in Settings".into()),
                };
                out.push(AIPlan {
                    provider: "Claude".into(),
                    plan: cfg.plan.clone(),
                    window: window_label(cfg.window_hours),
                    limit_kind: "tokens".into(),
                    used_pct: used_pct.unwrap_or(0.0),
                    resets_at,
                    pace_state: state,
                    pace_text: text,
                    source,
                });
            }
            other => {
                let used = cfg.manual_used_pct.unwrap_or(0.0);
                let resets_at = cfg.manual_resets_at.map(|r| roll_forward(r, window_s, now));
                let (state, text) = match cfg.manual_used_pct {
                    Some(u) => pace(u, window_s, resets_at.map(|r| r - now)),
                    None => (PaceState::Unknown, "enter usage in Settings".into()),
                };
                let provider = match other {
                    "copilot" => "GitHub Copilot".to_string(),
                    "cursor" => "Cursor".to_string(),
                    o => o.to_string(),
                };
                out.push(AIPlan {
                    provider,
                    plan: cfg.plan.clone(),
                    window: window_label(cfg.window_hours),
                    limit_kind: "requests".into(),
                    used_pct: used,
                    resets_at,
                    pace_state: state,
                    pace_text: text,
                    source: "manual".into(),
                });
            }
        }
    }
    out
}

/// Advance a periodic reset timestamp into the future.
pub fn roll_forward(reset: i64, window_s: i64, now: i64) -> i64 {
    if window_s <= 0 || reset > now {
        return reset;
    }
    reset + ((now - reset) / window_s + 1) * window_s
}

/// "Claude · Max 20x · This week 72% left · Resets 4 d 2 h"
pub fn headline(p: &AIPlan, now: i64) -> String {
    let win = if p.window == "week" { "This week".to_string() } else { format!("{} window", p.window) };
    if p.pace_state == PaceState::Unknown && p.used_pct == 0.0 && p.source != "manual" {
        return format!("{} · {} · {} usage unknown ({})", p.provider, p.plan, win, p.pace_text);
    }
    let mut s = format!("{} · {} · {} {:.0}% left", p.provider, p.plan, win, (100.0 - p.used_pct).max(0.0));
    if let Some(r) = p.resets_at.filter(|r| *r > now) {
        s.push_str(&format!(" · Resets {}", format_duration(r - now)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_cases() {
        let week = 7 * 86_400;
        // 28 % used after half the week → on pace / ahead.
        assert_eq!(pace(28.0, week, Some(week / 2)).0, PaceState::Ahead);
        assert_eq!(pace(45.0, week, Some(week / 2)).0, PaceState::OnPace);
        // 80 % used after 1 day → runs out.
        let (s, t) = pace(80.0, week, Some(week - 86_400));
        assert_eq!(s, PaceState::RunsOut);
        assert!(t.starts_with("runs out in"), "{t}");
        assert_eq!(pace(100.0, week, Some(3600)).0, PaceState::RunsOut);
    }

    #[test]
    fn codex_log() {
        let log = r#"{"timestamp":"2026-11-01T10:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":40.0,"window_minutes":10080,"resets_at":1762300000}}}}"#;
        let w = parse_codex_rate_limits(log);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].1.used_pct, 12.0);
        assert_eq!(w[0].1.resets_at, chrono::DateTime::parse_from_rfc3339("2026-11-01T11:00:00Z").unwrap().timestamp());
        assert_eq!(w[1].1.resets_at, 1762300000);
    }

    #[test]
    fn claude_tokens_dedup() {
        let l = |id: &str, ts: &str, n: u32| {
            format!(r#"{{"timestamp":"{ts}","message":{{"id":"{id}","usage":{{"input_tokens":{n},"output_tokens":{n},"cache_read_input_tokens":999}}}}}}"#)
        };
        let text = [l("a", "2026-11-01T10:00:00Z", 10), l("a", "2026-11-01T10:00:01Z", 10), l("b", "2026-11-01T11:00:00Z", 5), l("c", "2026-10-01T11:00:00Z", 1000)].join("\n");
        let since = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z").unwrap().timestamp();
        let (t, first) = sum_claude_tokens(&text, since);
        assert_eq!(t, 30.0);
        assert!(first.is_some());
    }

    #[test]
    fn rolls() {
        assert_eq!(roll_forward(100, 50, 260), 300);
        assert_eq!(roll_forward(400, 50, 260), 400);
    }
}
