//! `live.json`: the latest snapshot written by the running app, so the CLI and the MCP server
//! answer instantly and include state only a long-running sampler knows (idle times).

use crate::model::Snapshot;
use crate::paths;
use crate::sys;

pub const FRESH_MS: i64 = 15_000;

pub fn write(s: &Snapshot) -> anyhow::Result<()> {
    paths::ensure_dirs()?;
    let tmp = paths::live_file().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(s)?)?;
    std::fs::rename(tmp, paths::live_file())?;
    Ok(())
}

pub fn read_any() -> Option<Snapshot> {
    serde_json::from_slice(&std::fs::read(paths::live_file()).ok()?).ok()
}

/// The app's snapshot if it is fresh.
pub fn read_fresh() -> Option<Snapshot> {
    read_any().filter(|s| sys::now_ms() - s.timestamp <= FRESH_MS)
}
