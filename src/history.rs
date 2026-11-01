//! M6 — History in a single SQLite file.
//!
//! Resolution levels (bucket size → kept for):
//! 1 s → 5 min, 1 min → 24 h, 5 min → 7 d, 30 min → 30 d, 2 h → 6 months, 6 h → 1 year,
//! further capped by the user's retention choice. Samples are accumulated in memory per
//! (level, bucket, app, metric) and flushed with a merge-upsert (n-weighted average), so
//! flushing at any time is correct. Per-app series skip the 1 s level to bound the file size.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::model::{AlertKind, AlertRule, HistoryBucket};

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
                enabled INTEGER NOT NULL);",
        )?;
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
    fn rules_roundtrip() {
        let h = History::open_in_memory().unwrap();
        let r = AlertRule { kind: AlertKind::CpuSustained, threshold: 70.0, duration_s: 600, enabled: true };
        h.save_rule(&r).unwrap();
        assert_eq!(h.load_rules().unwrap(), vec![r]);
    }
}
