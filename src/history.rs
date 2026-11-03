//! M6 — History in a single SQLite file.
//!
//! Resolution levels (bucket size → kept for):
//! 1 s → 5 min, 1 min → 24 h, 5 min → 7 d, 30 min → 30 d, 2 h → 6 months, 6 h → 1 year,
//! further capped by the user's retention choice. Samples are accumulated in memory per
//! (level, bucket, app, metric) and flushed with a merge-upsert (n-weighted average), so
//! flushing at any time is correct. Per-app series skip the 1 s level to bound the file size.
//!
//! Per-app series are stored under `app_<metric>` (cpu, memory, energy, disk, network) with the
//! app's bundle id (or name) in the `app` column, for the busiest apps only (see
//! `daemon::history_samples`). Alerts and restarts of those apps go to the `events` table.
//! Schema changes are additive (`CREATE TABLE IF NOT EXISTS`, tracked in `PRAGMA user_version`),
//! so databases written by older versions open unchanged.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::model::{AlertKind, AlertRule, AppGroup, HistoryBucket, Snapshot};

/// Schema version stored in `PRAGMA user_version`. 1 = history + alert_rules, 2 = + events.
const SCHEMA_VERSION: i64 = 2;

pub const LEVELS: [(i64, i64); 6] = [
    (1, 300),
    (60, 86_400),
    (300, 7 * 86_400),
    (1800, 30 * 86_400),
    (7200, 183 * 86_400),
    (21_600, 365 * 86_400),
];

#[derive(Debug, Clone, Copy)]
struct Agg {
    min: f64,
    sum: f64,
    max: f64,
    n: i64,
}

impl Agg {
    fn new(v: f64) -> Self {
        Self { min: v, sum: v, max: v, n: 1 }
    }
    fn add(&mut self, v: f64) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v;
        self.n += 1;
    }
}

type Key = (i64, i64, String, String);

pub struct History {
    conn: Connection,
    pending: HashMap<Key, Agg>,
    last_flush: i64,
    last_purge: i64,
}

/// A single value to record. `app` = None for whole-machine metrics.
pub struct Sample<'a> {
    pub app: Option<&'a str>,
    pub metric: &'a str,
    pub value: f64,
}

/// Key under which an app's history is stored: bundle id, else the app's name.
pub fn app_id(g: &AppGroup) -> String {
    g.app.bundle_id.clone().unwrap_or_else(|| g.app.name.clone())
}

/// Something notable that happened to an app: an alert that fired or a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct AppEvent {
    pub ts: i64,
    pub app: String,
    /// "alert" | "restart"
    pub kind: String,
    pub message: String,
}

/// Highest value of an app metric in a period and when (start of the bucket) it happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub ts: i64,
    pub value: f64,
}

/// Spots restarts: an app whose main process disappeared and was replaced by another one
/// between two ticks. Remembers each app's main pid and pid set from the previous tick.
#[derive(Default)]
pub struct RestartDetector {
    last: HashMap<String, (i32, Vec<i32>)>,
}

impl RestartDetector {
    /// `(app id, previous main pid, new main pid)` for every app that restarted since last call.
    pub fn feed(&mut self, s: &Snapshot) -> Vec<(String, i32, i32)> {
        let mut out = Vec::new();
        let mut next = HashMap::new();
        for g in &s.apps {
            if g.key == crate::grouping::SYSTEM_KEY || g.root_pid <= 0 {
                continue;
            }
            let id = app_id(g);
            if let Some((old_root, old_pids)) = self.last.get(&id)
                && *old_root != g.root_pid
                && !g.pids.contains(old_root)
                // Only if none of the old processes survived as the new main one (helpers that
                // got promoted when the main process exits aren't a restart of the app).
                && !old_pids.contains(&g.root_pid)
            {
                out.push((id.clone(), *old_root, g.root_pid));
            }
            next.insert(id, (g.root_pid, g.pids.clone()));
        }
        self.last = next;
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankRow {
    pub app: String,
    pub avg: f64,
    pub peak: f64,
    /// Σ avg × bucket seconds (e.g. joules for watts).
    pub integral: f64,
}

impl History {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS history (
                res INTEGER NOT NULL, bucket INTEGER NOT NULL, app TEXT NOT NULL DEFAULT '',
                metric TEXT NOT NULL, vmin REAL NOT NULL, vavg REAL NOT NULL, vmax REAL NOT NULL,
                n INTEGER NOT NULL, PRIMARY KEY (res, metric, app, bucket)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS alert_rules (
                kind TEXT PRIMARY KEY, threshold REAL NOT NULL, duration_s INTEGER NOT NULL,
                enabled INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS events (
                ts INTEGER NOT NULL, app TEXT NOT NULL, kind TEXT NOT NULL, message TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS events_app_ts ON events (app, ts);",
        )?;
        // Migration from v1 (no events table) is covered by the IF NOT EXISTS above; record it.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        }
        Ok(Self { conn, pending: HashMap::new(), last_flush: 0, last_purge: 0 })
    }

    pub fn record(&mut self, ts: i64, samples: &[Sample]) {
        for s in samples {
            if !s.value.is_finite() {
                continue;
            }
            for (res, _) in LEVELS {
                if s.app.is_some() && res == 1 {
                    continue;
                }
                let key = (res, ts - ts.rem_euclid(res), s.app.unwrap_or("").to_string(), s.metric.to_string());
                self.pending.entry(key).and_modify(|a| a.add(s.value)).or_insert_with(|| Agg::new(s.value));
            }
        }
    }

    pub fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO history (res, bucket, app, metric, vmin, vavg, vmax, n) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT (res, metric, app, bucket) DO UPDATE SET
                   vmin = MIN(vmin, excluded.vmin),
                   vmax = MAX(vmax, excluded.vmax),
                   vavg = (vavg * n + excluded.vavg * excluded.n) / (n + excluded.n),
                   n = n + excluded.n",
            )?;
            for ((res, bucket, app, metric), a) in self.pending.drain() {
                st.execute(params![res, bucket, app, metric, a.min, a.sum / a.n as f64, a.max, a.n])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Flush every `flush_every` s and purge hourly. Call once per tick.
    pub fn maintain(&mut self, now: i64, retention_s: i64, flush_every: i64) -> Result<()> {
        if now - self.last_flush >= flush_every {
            self.flush()?;
            self.last_flush = now;
        }
        if now - self.last_purge >= 3600 {
            self.purge(now, retention_s)?;
            self.last_purge = now;
        }
        Ok(())
    }

    pub fn purge(&mut self, now: i64, retention_s: i64) -> Result<usize> {
        let mut n = 0;
        for (res, keep) in LEVELS {
            let cutoff = now - keep.min(retention_s.max(300));
            n += self.conn.execute("DELETE FROM history WHERE res = ?1 AND bucket < ?2", params![res, cutoff])?;
        }
        n += self.conn.execute("DELETE FROM events WHERE ts < ?1", params![now - retention_s.max(300)])?;
        Ok(n)
    }

    /// Finest level whose retention still covers `from`.
    pub fn resolution_for(now: i64, from: i64) -> i64 {
        let span = now - from;
        LEVELS.iter().find(|(_, keep)| *keep >= span).map(|(r, _)| *r).unwrap_or(LEVELS[LEVELS.len() - 1].0)
    }

    pub fn series(&self, metric: &str, app: Option<&str>, from: i64, to: i64, now: i64) -> Result<Vec<HistoryBucket>> {
        let res = Self::resolution_for(now, from);
        let mut st = self.conn.prepare_cached(
            "SELECT bucket, vmin, vavg, vmax, n FROM history
             WHERE res = ?1 AND metric = ?2 AND app = ?3 AND bucket >= ?4 AND bucket <= ?5 ORDER BY bucket",
        )?;
        let app_s = app.unwrap_or("").to_string();
        let rows = st.query_map(params![res, metric, app_s, from - from.rem_euclid(res), to], |r| {
            Ok(HistoryBucket {
                time_bucket: r.get(0)?,
                resolution_s: res,
                app_bundle_id: app.map(str::to_string),
                metric_kind: metric.to_string(),
                value_min: r.get(1)?,
                value_avg: r.get(2)?,
                value_max: r.get(3)?,
                samples: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Highest recorded value of `metric` for one app in [from, to] and the bucket it was in.
    pub fn app_peak(&self, metric: &str, app: &str, from: i64, to: i64, now: i64) -> Result<Option<Peak>> {
        let res = Self::resolution_for(now, from);
        let r = self.conn.query_row(
            "SELECT bucket, vmax FROM history WHERE res = ?1 AND metric = ?2 AND app = ?3 AND bucket >= ?4 AND bucket <= ?5
             ORDER BY vmax DESC, bucket DESC LIMIT 1",
            params![res, metric, app, from - from.rem_euclid(res), to],
            |r| Ok(Peak { ts: r.get(0)?, value: r.get(1)? }),
        );
        match r {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn record_event(&self, e: &AppEvent) -> Result<()> {
        self.conn.execute("INSERT INTO events (ts, app, kind, message) VALUES (?1,?2,?3,?4)", params![e.ts, e.app, e.kind, e.message])?;
        Ok(())
    }

    /// Events of one app in [from, to], newest first.
    pub fn events(&self, app: &str, from: i64, to: i64, limit: usize) -> Result<Vec<AppEvent>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ts, app, kind, message FROM events WHERE app = ?1 AND ts >= ?2 AND ts <= ?3 ORDER BY ts DESC LIMIT ?4",
        )?;
        let rows = st.query_map(params![app, from, to, limit as i64], |r| {
            Ok(AppEvent { ts: r.get(0)?, app: r.get(1)?, kind: r.get(2)?, message: r.get(3)? })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Apps ranked by average value of `metric` over [from, to].
    pub fn ranking(&self, metric: &str, from: i64, to: i64, now: i64, limit: usize) -> Result<Vec<RankRow>> {
        let res = Self::resolution_for(now, from);
        let span = ((to - from) as f64).max(1.0);
        let mut st = self.conn.prepare_cached(
            "SELECT app, SUM(vavg) * ?5, MAX(vmax) FROM history
             WHERE res = ?1 AND metric = ?2 AND app != '' AND bucket >= ?3 AND bucket <= ?4
             GROUP BY app ORDER BY 2 DESC LIMIT ?6",
        )?;
        let rows = st.query_map(params![res, metric, from - from.rem_euclid(res), to, res as f64, limit as i64], |r| {
            let integral: f64 = r.get(1)?;
            Ok(RankRow { app: r.get(0)?, avg: integral / span, peak: r.get(2)?, integral })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// (average, peak) of a machine metric since `since` (e.g. local midnight).
    pub fn stats_since(&self, metric: &str, since: i64, now: i64) -> Result<Option<(f64, f64)>> {
        let res = if now - since <= 86_400 { 60 } else { Self::resolution_for(now, since) };
        let r: (Option<f64>, Option<f64>, Option<f64>) = self.conn.query_row(
            "SELECT SUM(vavg * n), SUM(n), MAX(vmax) FROM history WHERE res = ?1 AND metric = ?2 AND app = '' AND bucket >= ?3",
            params![res, metric, since - since.rem_euclid(res)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(match r {
            (Some(s), Some(n), Some(peak)) if n > 0.0 => Some((s / n, peak)),
            _ => None,
        })
    }

    /// Integral of a rate metric (bytes/s) since `since`: Σ bucket average × bucket length,
    /// i.e. bytes transferred. Minutes the agent wasn't running count as zero.
    pub fn total_since(&self, metric: &str, since: i64, now: i64) -> Result<f64> {
        let res = if now - since <= 86_400 { 60 } else { Self::resolution_for(now, since) };
        let s: Option<f64> = self.conn.query_row(
            "SELECT SUM(vavg) FROM history WHERE res = ?1 AND metric = ?2 AND app = '' AND bucket >= ?3",
            params![res, metric, since - since.rem_euclid(res)],
            |r| r.get(0),
        )?;
        Ok(s.unwrap_or(0.0) * res as f64)
    }

    pub fn row_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?)
    }

    // --- alert rules (the only other persisted entity) ---------------------------------------

    pub fn load_rules(&self) -> Result<Vec<AlertRule>> {
        let mut st = self.conn.prepare("SELECT kind, threshold, duration_s, enabled FROM alert_rules")?;
        let rows = st.query_map([], |r| {
            let kind: String = r.get(0)?;
            Ok((kind, r.get::<_, f64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)? != 0))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (k, threshold, duration_s, enabled) = row?;
            if let Ok(kind) = serde_json::from_value::<AlertKind>(serde_json::Value::String(k)) {
                out.push(AlertRule { kind, threshold, duration_s, enabled });
            }
        }
        Ok(out)
    }

    pub fn save_rule(&self, r: &AlertRule) -> Result<()> {
        let kind = serde_json::to_value(r.kind)?.as_str().unwrap_or_default().to_string();
        self.conn.execute(
            "INSERT INTO alert_rules (kind, threshold, duration_s, enabled) VALUES (?1,?2,?3,?4)
             ON CONFLICT(kind) DO UPDATE SET threshold=excluded.threshold, duration_s=excluded.duration_s, enabled=excluded.enabled",
            params![kind, r.threshold, r.duration_s, r.enabled as i64],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hour_of_data_roundtrip() {
        let mut h = History::open_in_memory().unwrap();
        let t0 = 1_800_000_000; // aligned hour
        for i in 0..3600 {
            let ts = t0 + i;
            // CPU ramps 0..59 within each minute; app memory constant.
            h.record(ts, &[
                Sample { app: None, metric: "cpu", value: (i % 60) as f64 },
                Sample { app: Some("com.google.Chrome"), metric: "memory", value: 2e9 },
                Sample { app: Some("com.microsoft.VSCode"), metric: "memory", value: 1e9 },
            ]);
            if i % 600 == 0 {
                h.flush().unwrap(); // partial flushes must merge correctly
            }
        }
        h.flush().unwrap();
        let now = t0 + 3600;
        // 12 h range → 1-minute level, 60 buckets each avg 29.5, min 0, max 59.
        let s = h.series("cpu", None, now - 12 * 3600, now, now).unwrap();
        assert_eq!(s.len(), 60);
        assert!(s.iter().all(|b| b.resolution_s == 60 && (b.value_avg - 29.5).abs() < 1e-9 && b.value_max == 59.0 && b.samples == 60));
        // 7 d range → 5-minute level.
        let s7 = h.series("cpu", None, now - 7 * 86_400, now, now).unwrap();
        assert_eq!(s7.len(), 12);
        assert_eq!(s7[0].resolution_s, 300);
        // Last 5 minutes at 1 s.
        let s1 = h.series("cpu", None, now - 300, now, now).unwrap();
        assert_eq!(s1.len(), 300);
        // Ranking.
        let r = h.ranking("memory", now - 86_400, now, now, 5).unwrap();
        assert_eq!(r[0].app, "com.google.Chrome");
        assert_eq!(r.len(), 2);
        assert_eq!(h.stats_since("cpu", t0, now).unwrap().unwrap().1, 59.0);
    }

    #[test]
    fn purge_respects_levels_and_retention() {
        let mut h = History::open_in_memory().unwrap();
        let now = 1_800_000_000;
        h.record(now - 400 * 86_400, &[Sample { app: None, metric: "cpu", value: 1.0 }]);
        h.record(now - 20 * 86_400, &[Sample { app: None, metric: "cpu", value: 1.0 }]);
        h.record(now - 10, &[Sample { app: None, metric: "cpu", value: 1.0 }]);
        h.flush().unwrap();
        h.purge(now, 365 * 86_400).unwrap();
        // 400 d old: gone everywhere. 20 d: only in 30 min / 2 h / 6 h. 10 s: all 6 levels.
        assert_eq!(h.row_count().unwrap(), 3 + 6);
        h.purge(now, 7 * 86_400).unwrap();
        assert_eq!(h.row_count().unwrap(), 6);
    }

    #[test]
    fn total_since_integrates_rates() {
        let mut h = History::open_in_memory().unwrap();
        let t0 = 1_800_000_000;
        // 1 MB/s for 10 minutes, then nothing recorded (agent not running).
        for i in 0..600 {
            h.record(t0 + i, &[Sample { app: None, metric: "net_down", value: 1e6 }]);
        }
        h.flush().unwrap();
        let total = h.total_since("net_down", t0, t0 + 3600).unwrap();
        assert!((total - 600e6).abs() < 1.0, "{total}");
        assert_eq!(h.total_since("disk_write", t0, t0 + 3600).unwrap(), 0.0);
    }

    #[test]
    fn per_app_series_peak_and_events() {
        let mut h = History::open_in_memory().unwrap();
        let t0 = 1_800_000_000;
        // 26 h of one app's CPU and memory, one sample every 10 s; CPU spikes once.
        for i in 0..(26 * 360) {
            let ts = t0 + i * 10;
            let cpu = if i == 5000 { 250.0 } else { 10.0 };
            h.record(ts, &[
                Sample { app: Some("com.brave.Browser"), metric: "app_cpu", value: cpu },
                Sample { app: Some("com.brave.Browser"), metric: "app_memory", value: 1e9 + i as f64 },
                Sample { app: Some("com.other"), metric: "app_cpu", value: 1.0 },
            ]);
            if i % 360 == 0 {
                h.flush().unwrap();
            }
        }
        h.flush().unwrap();
        let now = t0 + 26 * 3600;
        // 24 h → 1-minute level: 1440 buckets of app-only data, other apps not mixed in.
        let s = h.series("app_cpu", Some("com.brave.Browser"), now - 86_400, now, now).unwrap();
        assert_eq!(s.len(), 1440);
        assert!(s.iter().all(|b| b.resolution_s == 60 && b.app_bundle_id.as_deref() == Some("com.brave.Browser")));
        let mem = h.series("app_memory", Some("com.brave.Browser"), now - 86_400, now, now).unwrap();
        assert_eq!(mem.len(), 1440);
        assert!(mem.windows(2).all(|w| w[1].value_avg > w[0].value_avg));
        // 1 h and 7 d ranges pick 1 min and 5 min levels.
        assert_eq!(h.series("app_cpu", Some("com.brave.Browser"), now - 3600, now, now).unwrap().len(), 60);
        let week = h.series("app_cpu", Some("com.brave.Browser"), now - 7 * 86_400, now, now).unwrap();
        assert_eq!(week[0].resolution_s, 300);
        assert_eq!(week.len(), 26 * 12);
        // Peak with timestamp (bucket start of sample 5000).
        let p = h.app_peak("app_cpu", "com.brave.Browser", now - 86_400, now, now).unwrap().unwrap();
        assert_eq!(p.value, 250.0);
        let ts = t0 + 5000 * 10;
        assert_eq!(p.ts, ts - ts.rem_euclid(60));
        assert_eq!(h.app_peak("app_cpu", "com.nobody", now - 86_400, now, now).unwrap(), None);
        // Events: newest first, filtered by app and range, purged with retention.
        for (ts, app, kind) in [(now - 7200, "com.brave.Browser", "alert"), (now - 100, "com.brave.Browser", "restart"), (now - 50, "com.other", "alert")] {
            h.record_event(&AppEvent { ts, app: app.into(), kind: kind.into(), message: "m".into() }).unwrap();
        }
        let ev = h.events("com.brave.Browser", now - 86_400, now, 10).unwrap();
        assert_eq!(ev.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), vec!["restart", "alert"]);
        assert_eq!(h.events("com.brave.Browser", now - 3600, now, 10).unwrap().len(), 1);
        h.purge(now, 3600).unwrap();
        assert_eq!(h.events("com.brave.Browser", 0, now, 10).unwrap().len(), 1);
    }

    #[test]
    fn opens_v1_database_and_migrates() {
        let dir = std::env::temp_dir().join(format!("pulsebar-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            // A v1 file: history + alert_rules only, user_version 0, with one row in it.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE history (res INTEGER NOT NULL, bucket INTEGER NOT NULL, app TEXT NOT NULL DEFAULT '',
                    metric TEXT NOT NULL, vmin REAL NOT NULL, vavg REAL NOT NULL, vmax REAL NOT NULL,
                    n INTEGER NOT NULL, PRIMARY KEY (res, metric, app, bucket)) WITHOUT ROWID;
                 CREATE TABLE alert_rules (kind TEXT PRIMARY KEY, threshold REAL NOT NULL, duration_s INTEGER NOT NULL, enabled INTEGER NOT NULL);
                 INSERT INTO history VALUES (60, 1799999940, 'com.x', 'app_cpu', 1, 2, 3, 4);",
            )
            .unwrap();
        }
        let h = History::open(&path).unwrap();
        assert_eq!(h.row_count().unwrap(), 1);
        assert_eq!(h.series("app_cpu", Some("com.x"), 1_799_999_000, 1_800_000_000, 1_800_000_000).unwrap().len(), 1);
        h.record_event(&AppEvent { ts: 1_800_000_000, app: "com.x".into(), kind: "restart".into(), message: String::new() }).unwrap();
        assert_eq!(h.events("com.x", 0, 2_000_000_000, 5).unwrap().len(), 1);
        let v: i64 = h.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_detection() {
        let group = |root: i32, pids: Vec<i32>| AppGroup {
            key: "bundle:com.x".into(),
            app: crate::model::App { bundle_id: Some("com.x".into()), name: "X".into(), ..Default::default() },
            root_pid: root,
            pids,
            ..Default::default()
        };
        let snap = |g: AppGroup| Snapshot { apps: vec![g], ..Default::default() };
        let mut d = RestartDetector::default();
        assert!(d.feed(&snap(group(10, vec![10, 11]))).is_empty()); // first sight
        assert!(d.feed(&snap(group(10, vec![10, 11, 12]))).is_empty()); // helper spawned
        // Quit and relaunched between ticks: new main pid, old pids gone.
        assert_eq!(d.feed(&snap(group(20, vec![20, 21]))), vec![("com.x".to_string(), 10, 20)]);
        assert!(d.feed(&snap(group(20, vec![20, 21]))).is_empty());
        // Main exits but a surviving helper becomes the root: not a restart.
        assert!(d.feed(&snap(group(21, vec![21]))).is_empty());
        // App absent for a tick, then back: no event (can't tell), and no stale state.
        assert!(d.feed(&Snapshot::default()).is_empty());
        assert!(d.feed(&snap(group(30, vec![30]))).is_empty());
    }

    #[test]
    fn rules_roundtrip() {
        let h = History::open_in_memory().unwrap();
        let r = AlertRule { kind: AlertKind::CpuSustained, threshold: 70.0, duration_s: 600, enabled: true };
        h.save_rule(&r).unwrap();
        assert_eq!(h.load_rules().unwrap(), vec![r]);
    }
}
