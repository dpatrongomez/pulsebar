//! User settings (JSON in Application Support). Layout survives restarts.

use crate::paths;
use crate::sorting::ListSort;
use crate::units::Units;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuMetric {
    Cpu,
    Memory,
    Network,
    Disk,
    Gpu,
    Neural,
    Temperature,
    Fans,
    Battery,
    Power,
}

impl MenuMetric {
    pub const ALL: [MenuMetric; 10] = [
        MenuMetric::Cpu,
        MenuMetric::Memory,
        MenuMetric::Network,
        MenuMetric::Disk,
        MenuMetric::Gpu,
        MenuMetric::Neural,
        MenuMetric::Temperature,
        MenuMetric::Fans,
        MenuMetric::Battery,
        MenuMetric::Power,
    ];
    pub fn label(self) -> &'static str {
        match self {
            MenuMetric::Cpu => "CPU",
            MenuMetric::Memory => "Memory",
            MenuMetric::Network => "Network",
            MenuMetric::Disk => "Disk",
            MenuMetric::Gpu => "GPU",
            MenuMetric::Neural => "Neural Engine",
            MenuMetric::Temperature => "Temperature",
            MenuMetric::Fans => "Fans",
            MenuMetric::Battery => "Battery",
            MenuMetric::Power => "Power",
        }
    }
    /// i18n key of the metric's display name.
    pub fn key(self) -> &'static str {
        match self {
            MenuMetric::Cpu => "mm.cpu",
            MenuMetric::Memory => "mm.memory",
            MenuMetric::Network => "mm.network",
            MenuMetric::Disk => "mm.disk",
            MenuMetric::Gpu => "mm.gpu",
            MenuMetric::Neural => "mm.neural",
            MenuMetric::Temperature => "mm.temperature",
            MenuMetric::Fans => "mm.fans",
            MenuMetric::Battery => "mm.battery",
            MenuMetric::Power => "mm.power",
        }
    }
    /// Default red-tint threshold (in the metric's natural unit).
    pub fn default_threshold(self) -> Option<f64> {
        match self {
            MenuMetric::Cpu => Some(80.0),
            MenuMetric::Memory => Some(90.0),
            MenuMetric::Gpu => Some(85.0),
            MenuMetric::Temperature => Some(90.0),
            MenuMetric::Disk => Some(95.0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuDisplay {
    Number,
    Sparkline,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuBarItem {
    pub metric: MenuMetric,
    pub visible: bool,
    pub display: MenuDisplay,
    pub framed: bool,
    /// When the value exceeds this, the item is tinted red.
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    Overview,
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
    Neural,
    Battery,
    Sound,
    Bluetooth,
    Projects,
    Agents,
    Containers,
    History,
    Settings,
}

impl Tab {
    pub const DEFAULT_ORDER: [Tab; 15] = [
        Tab::Overview,
        Tab::Cpu,
        Tab::Memory,
        Tab::Disk,
        Tab::Network,
        Tab::Gpu,
        Tab::Neural,
        Tab::Battery,
        Tab::Sound,
        Tab::Bluetooth,
        Tab::Projects,
        Tab::Agents,
        Tab::Containers,
        Tab::History,
        Tab::Settings,
    ];
    /// i18n key.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Overview => "tab.overview",
            Tab::Cpu => "tab.cpu",
            Tab::Memory => "tab.memory",
            Tab::Disk => "tab.disk",
            Tab::Network => "tab.network",
            Tab::Gpu => "tab.gpu",
            Tab::Neural => "tab.neural",
            Tab::Battery => "tab.battery",
            Tab::Sound => "tab.sound",
            Tab::Bluetooth => "tab.bluetooth",
            Tab::Projects => "tab.projects",
            Tab::Agents => "tab.agents",
            Tab::Containers => "tab.containers",
            Tab::History => "tab.history",
            Tab::Settings => "tab.settings",
        }
    }
}

impl Tab {
    /// Linux machines have no Neural Engine to report, so its tab stays out of the way there.
    pub fn supported(self) -> bool {
        !(cfg!(target_os = "linux") && self == Tab::Neural)
    }
}

impl MenuMetric {
    /// See [`Tab::supported`].
    pub fn supported(self) -> bool {
        !(cfg!(target_os = "linux") && self == MenuMetric::Neural)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSetting {
    pub tab: Tab,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Monochrome,
    #[default]
    Brand,
    /// Okabe–Ito palette, distinguishable with common colour-vision deficiencies.
    ColorBlind,
    /// Monochrome, flat and type-led, with its own fonts (see `gui::nothing`).
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberFont {
    #[default]
    Proportional,
    Monospace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Days30,
    #[default]
    Days90,
    Months6,
    Year1,
}

impl Retention {
    pub fn seconds(self) -> i64 {
        const DAY: i64 = 86_400;
        match self {
            Retention::Days30 => 30 * DAY,
            Retention::Days90 => 90 * DAY,
            Retention::Months6 => 183 * DAY,
            Retention::Year1 => 365 * DAY,
        }
    }
}

/// Local-only description of an AI plan. PulseBar never calls provider APIs; usage is derived
/// from the agents' own local logs (see `limits.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanConfig {
    /// "claude" | "codex" | "copilot" | "cursor"
    pub provider: String,
    pub plan: String,
    /// Rolling window length in hours (e.g. 5 or 168).
    pub window_hours: f64,
    /// Budget in output+input tokens for the window (Claude), or premium requests (Copilot).
    pub budget: Option<f64>,
    /// Manually entered usage % (for providers without local logs: Cursor / Copilot).
    pub manual_used_pct: Option<f64>,
    /// Manually entered reset (unix seconds).
    pub manual_resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub units: Units,
    pub theme: Theme,
    pub number_font: NumberFont,
    pub compact: bool,
    /// None = follow the system language.
    pub language: Option<String>,
    pub menubar: Vec<MenuBarItem>,
    pub tabs: Vec<TabSetting>,
    pub retention: Retention,
    /// MCP server is OFF by default.
    pub mcp_enabled: bool,
    pub sample_interval_ms: u64,
    pub background_interval_ms: u64,
    pub idle_threshold_min: i64,
    pub alert_disk_write_bps: f64,
    pub alert_network_bps: f64,
    pub notifications: bool,
    pub plans: Vec<PlanConfig>,
    /// Per-tab list sorting and optional columns; tabs without an entry use their default.
    #[serde(deserialize_with = "lenient_list_sort")]
    pub list_sort: HashMap<Tab, ListSort>,
}

/// Drops invalid `list_sort` entries (unknown tab or key, wrong type, missing field) instead of
/// failing the whole `Settings` parse, which would reset every setting to its default.
fn lenient_list_sort<'de, D: serde::Deserializer<'de>>(d: D) -> Result<HashMap<Tab, ListSort>, D::Error> {
    let serde_json::Value::Object(map) = serde_json::Value::deserialize(d)? else { return Ok(HashMap::new()) };
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| {
            let tab = serde_json::from_value(serde_json::Value::String(k)).ok()?;
            Some((tab, serde_json::from_value(v).ok()?))
        })
        .collect())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            units: Units::default(),
            theme: Theme::default(),
            number_font: NumberFont::default(),
            compact: false,
            language: None,
            menubar: default_menubar(),
            tabs: default_tabs(),
            retention: Retention::default(),
            mcp_enabled: false,
            sample_interval_ms: 1000,
            background_interval_ms: 2000,
            idle_threshold_min: 60,
            alert_disk_write_bps: 200_000_000.0,
            alert_network_bps: 50_000_000.0,
            notifications: true,
            plans: vec![
                PlanConfig {
                    provider: "claude".into(),
                    plan: "Max 20x".into(),
                    window_hours: 168.0,
                    budget: None,
                    manual_used_pct: None,
                    manual_resets_at: None,
                },
                PlanConfig {
                    provider: "codex".into(),
                    plan: "Pro".into(),
                    window_hours: 168.0,
                    budget: None,
                    manual_used_pct: None,
                    manual_resets_at: None,
                },
            ],
            list_sort: HashMap::new(),
        }
    }
}

pub fn default_menubar() -> Vec<MenuBarItem> {
    MenuMetric::ALL
        .into_iter()
        .map(|m| MenuBarItem {
            metric: m,
            visible: matches!(m, MenuMetric::Cpu | MenuMetric::Memory | MenuMetric::Network),
            display: if m == MenuMetric::Cpu { MenuDisplay::Both } else { MenuDisplay::Number },
            framed: false,
            threshold: m.default_threshold(),
        })
        .collect()
}

pub fn default_tabs() -> Vec<TabSetting> {
    Tab::DEFAULT_ORDER.into_iter().map(|tab| TabSetting { tab, visible: true }).collect()
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(paths::settings_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .map(|mut s| {
                s.repair();
                s
            })
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        paths::ensure_dirs()?;
        let tmp = paths::settings_file().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, paths::settings_file())?;
        Ok(())
    }

    /// Add items introduced by newer versions; drop duplicates.
    pub fn repair(&mut self) {
        for m in MenuMetric::ALL {
            if !self.menubar.iter().any(|i| i.metric == m) {
                self.menubar.push(MenuBarItem {
                    metric: m,
                    visible: false,
                    display: MenuDisplay::Number,
                    framed: false,
                    threshold: m.default_threshold(),
                });
            }
        }
        let mut seen = std::collections::HashSet::new();
        self.menubar.retain(|i| seen.insert(i.metric));
        for t in Tab::DEFAULT_ORDER {
            if !self.tabs.iter().any(|s| s.tab == t) {
                self.tabs.push(TabSetting { tab: t, visible: true });
            }
        }
        let mut seen = std::collections::HashSet::new();
        self.tabs.retain(|t| seen.insert(t.tab));
        // Settings must always be reachable.
        if let Some(s) = self.tabs.iter_mut().find(|t| t.tab == Tab::Settings) {
            s.visible = true;
        }
    }

    pub fn reset_layout(&mut self) {
        self.menubar = default_menubar();
        self.tabs = default_tabs();
        self.compact = false;
    }

    pub fn move_menubar_item(&mut self, from: usize, to: usize) {
        if from < self.menubar.len() && to < self.menubar.len() {
            let it = self.menubar.remove(from);
            self.menubar.insert(to, it);
        }
    }

    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from < self.tabs.len() && to < self.tabs.len() {
            let it = self.tabs.remove(from);
            self.tabs.insert(to, it);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sorting::{SortKey, SortSpec};

    #[test]
    fn layout_roundtrip_preserves_order() {
        let mut s = Settings::default();
        for i in s.menubar.iter_mut() {
            i.visible = matches!(i.metric, MenuMetric::Cpu | MenuMetric::Memory);
        }
        s.move_menubar_item(1, 0); // Memory before CPU
        let json = serde_json::to_string(&s).unwrap();
        let mut back: Settings = serde_json::from_str(&json).unwrap();
        back.repair();
        let visible: Vec<_> = back.menubar.iter().filter(|i| i.visible).map(|i| i.metric).collect();
        assert_eq!(visible, vec![MenuMetric::Memory, MenuMetric::Cpu]);
        assert!(!back.mcp_enabled, "MCP must be off by default");
    }

    #[test]
    fn list_sort_roundtrips_per_tab() {
        let mut s = Settings::default();
        s.list_sort.insert(Tab::Cpu, ListSort { apps: Some(SortSpec { key: SortKey::Name, desc: false }), procs: Some(SortSpec { key: SortKey::Pid, desc: true }), extra_columns: true });
        s.list_sort.insert(Tab::Memory, ListSort { procs: Some(SortSpec { key: SortKey::Energy, desc: false }), ..Default::default() });
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("\"cpu\""), "tabs serialise as snake_case keys: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.list_sort, s.list_sort);
        assert_eq!(back, s);
    }

    #[test]
    fn old_settings_without_list_sort_still_load() {
        let s: Settings = serde_json::from_str(r#"{"compact":true,"theme":"brand"}"#).unwrap();
        assert!(s.compact);
        assert!(s.list_sort.is_empty());
        // A partially specified entry fills the rest from defaults.
        let s: Settings = serde_json::from_str(r#"{"list_sort":{"memory":{"extra_columns":true}}}"#).unwrap();
        let m = &s.list_sort[&Tab::Memory];
        assert!(m.extra_columns && m.apps.is_none() && m.procs.is_none());
    }

    #[test]
    fn invalid_list_sort_entries_are_dropped_not_fatal() {
        let json = r#"{"compact":true,"list_sort":{
            "cpu":{"apps":{"key":"gpu","desc":true}},
            "memory":{"procs":{"key":"name"}},
            "disk":{"apps":"oops"},
            "bogus_tab":{"extra_columns":true},
            "network":{"apps":{"key":"pid","desc":false},"extra_columns":true}}}"#;
        let s: Settings = serde_json::from_str(json).expect("bad list_sort must not fail the whole file");
        assert!(s.compact, "other settings survive");
        assert_eq!(s.list_sort.len(), 1, "only the valid entry is kept: {:?}", s.list_sort);
        assert_eq!(s.list_sort[&Tab::Network].apps, Some(SortSpec { key: SortKey::Pid, desc: false }));
        // A list_sort that is not even an object is ignored too.
        let s: Settings = serde_json::from_str(r#"{"compact":true,"list_sort":[1]}"#).unwrap();
        assert!(s.compact && s.list_sort.is_empty());
    }

    #[test]
    fn nothing_theme_round_trips() {
        let s: Settings = serde_json::from_str(r#"{"theme":"nothing"}"#).unwrap();
        assert_eq!(s.theme, Theme::Nothing);
        assert!(serde_json::to_string(&s).unwrap().contains(r#""theme":"nothing""#));
    }

    #[test]
    fn repair_adds_missing() {
        let mut s: Settings = serde_json::from_str(r#"{"menubar":[],"tabs":[]}"#).unwrap();
        s.repair();
        assert_eq!(s.menubar.len(), MenuMetric::ALL.len());
        assert_eq!(s.tabs.len(), Tab::DEFAULT_ORDER.len());
    }
}
