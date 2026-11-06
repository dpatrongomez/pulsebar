//! Shared data model. Everything "live" (processes, apps, containers) stays in memory;
//! only `HistoryBucket` and `AlertRule` are persisted (see `history.rs`, `alerts.rs`).

use serde::{Deserialize, Serialize};

/// Kinds of metric a collector can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
    Neural,
    Temperature,
    Fans,
    Battery,
    Power,
    Sound,
    Bluetooth,
}

impl MetricKind {
    pub const ALL: [MetricKind; 12] = [
        MetricKind::Cpu,
        MetricKind::Memory,
        MetricKind::Disk,
        MetricKind::Network,
        MetricKind::Gpu,
        MetricKind::Neural,
        MetricKind::Temperature,
        MetricKind::Fans,
        MetricKind::Battery,
        MetricKind::Power,
        MetricKind::Sound,
        MetricKind::Bluetooth,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MetricKind::Cpu => "cpu",
            MetricKind::Memory => "memory",
            MetricKind::Disk => "disk",
            MetricKind::Network => "network",
            MetricKind::Gpu => "gpu",
            MetricKind::Neural => "neural",
            MetricKind::Temperature => "temperature",
            MetricKind::Fans => "fans",
            MetricKind::Battery => "battery",
            MetricKind::Power => "power",
            MetricKind::Sound => "sound",
            MetricKind::Bluetooth => "bluetooth",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One named component of a metric (e.g. CPU "user" / "system").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breakdown {
    pub label: String,
    pub value: f64,
}

impl Breakdown {
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self { label: label.into(), value }
    }
}

/// Output of a collector for one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricFrame {
    /// Unix milliseconds.
    pub timestamp: i64,
    pub metric_kind: MetricKind,
    /// Headline value (unit depends on kind: %, bytes, bytes/s, °C, W, RPM…).
    pub value: f64,
    pub unit: String,
    /// False when the sensor/API is not present on this Mac.
    pub available: bool,
    pub breakdown: Vec<Breakdown>,
}

// ---------------------------------------------------------------------------------------------
// Machine-level metrics
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuMetrics {
    pub total_pct: f64,
    pub user_pct: f64,
    pub system_pct: f64,
    pub idle_pct: f64,
    pub per_core: Vec<f64>,
    pub load_avg: [f64; 3],
    pub core_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryMetrics {
    pub total: u64,
    pub used: u64,
    pub app: u64,
    pub wired: u64,
    pub compressed: u64,
    pub cached: u64,
    pub free: u64,
    pub swap_used: u64,
    pub swap_total: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Volume {
    pub name: String,
    pub mount_point: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskMetrics {
    pub read_bps: f64,
    pub write_bps: f64,
    pub volumes: Vec<Volume>,
}

impl DiskMetrics {
    /// Free space of the boot volume ("/"), or the largest volume.
    pub fn boot_free(&self) -> Option<&Volume> {
        self.volumes
            .iter()
            .find(|v| v.mount_point == "/")
            .or_else(|| self.volumes.iter().max_by_key(|v| v.total))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceMetrics {
    pub name: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub total_rx: u64,
    pub total_tx: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkMetrics {
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub interfaces: Vec<InterfaceMetrics>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuMetrics {
    pub available: bool,
    pub name: String,
    pub utilization_pct: f64,
    pub vram_used: u64,
    pub vram_total: u64,
    pub power_w: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NeuralMetrics {
    pub available: bool,
    /// ANE power from IOReport "Energy Model" (Apple silicon only).
    pub power_w: f64,
    /// Rough utilisation estimate: power / reference max power.
    pub utilization_pct: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fan {
    pub index: usize,
    pub rpm: f64,
    pub min_rpm: f64,
    pub max_rpm: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SensorMetrics {
    pub cpu_temp_c: Option<f64>,
    pub gpu_temp_c: Option<f64>,
    pub fans: Vec<Fan>,
    pub all: Vec<Breakdown>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatteryMetrics {
    pub present: bool,
    pub percent: f64,
    pub charging: bool,
    pub on_ac: bool,
    pub time_remaining_min: Option<i64>,
    /// Battery discharge (+) / charge (−) power, watts.
    pub power_w: Option<f64>,
    pub cycle_count: Option<i64>,
    pub health_pct: Option<f64>,
    pub temperature_c: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerMetrics {
    /// Whole-system power draw (W) when known (SMC/battery telemetry or IOReport sum).
    pub system_w: Option<f64>,
    pub cpu_w: Option<f64>,
    pub gpu_w: Option<f64>,
    pub ane_w: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioState {
    Playing,
    Silent,
    Muted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioApp {
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub state: AudioState,
    pub using_mic: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioMetrics {
    pub output_device: Option<String>,
    pub output_volume: Option<f64>,
    pub output_muted: bool,
    pub input_volume: Option<f64>,
    pub apps: Vec<AudioApp>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BluetoothDevice {
    pub name: String,
    pub address: String,
    pub kind: String,
    pub connected: bool,
    pub battery_main: Option<f64>,
    pub battery_left: Option<f64>,
    pub battery_right: Option<f64>,
    pub battery_case: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BluetoothMetrics {
    pub powered: bool,
    pub devices: Vec<BluetoothDevice>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineMetrics {
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub disk: DiskMetrics,
    pub network: NetworkMetrics,
    pub gpu: GpuMetrics,
    pub neural: NeuralMetrics,
    pub sensors: SensorMetrics,
    pub battery: BatteryMetrics,
    pub power: PowerMetrics,
    pub audio: AudioMetrics,
    pub bluetooth: BluetoothMetrics,
}

// ---------------------------------------------------------------------------------------------
// Processes and apps
// ---------------------------------------------------------------------------------------------

/// One process as sampled this tick. Pure data so grouping/projects/agents are testable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Process {
    /// Linux only, never serialised: the systemd unit the process runs in, which can name its app.
    #[serde(skip)]
    pub app_hint: Option<String>,
    pub pid: i32,
    pub ppid: i32,
    /// macOS "responsible" process (what Activity Monitor uses for attribution); 0 = unknown.
    #[serde(default)]
    pub responsible_pid: i32,
    pub uid: u32,
    pub name: String,
    pub executable_path: String,
    pub argv: Vec<String>,
    pub cwd: String,
    /// Unix seconds.
    pub start_time: i64,
    pub cpu_pct: f64,
    /// Physical footprint when available (what Activity Monitor calls "Memory"), else RSS.
    pub mem_bytes: u64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    /// Watts, from `ri_billed_energy` deltas.
    pub energy_w: f64,
    pub cpu_time_user_s: f64,
    pub cpu_time_system_s: f64,
    pub mem_peak: u64,
    /// Resolved by grouping.
    pub app_bundle_id: Option<String>,
    pub app_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct App {
    pub bundle_id: Option<String>,
    pub name: String,
    pub bundle_path: Option<String>,
    pub version: Option<String>,
    pub signature_team_id: Option<String>,
}

/// One "row per app" with metrics summed over all its processes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppGroup {
    pub key: String,
    pub app: App,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub energy_w: f64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub process_count: usize,
    pub pids: Vec<i32>,
    pub root_pid: i32,
    /// `root_pid` is a real main process of the app (an executable in `Contents/MacOS/`, or a
    /// parentless process of a name-grouped app), not just the first process seen. Not serialized.
    #[serde(skip)]
    pub root_is_main: bool,
}

impl AppGroup {
    pub fn metric(&self, m: RankMetric) -> f64 {
        match m {
            RankMetric::Cpu => self.cpu_pct,
            RankMetric::Memory => self.mem_bytes as f64,
            RankMetric::Energy => self.energy_w,
            RankMetric::Disk => self.disk_read_bps + self.disk_write_bps,
            RankMetric::Network => self.net_rx_bps + self.net_tx_bps,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankMetric {
    Cpu,
    Memory,
    Energy,
    Disk,
    Network,
}

impl RankMetric {
    pub const ALL: [RankMetric; 5] = [Self::Cpu, Self::Memory, Self::Energy, Self::Disk, Self::Network];
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cpu" => Some(Self::Cpu),
            "mem" | "memory" | "ram" => Some(Self::Memory),
            "energy" | "power" => Some(Self::Energy),
            "disk" | "io" => Some(Self::Disk),
            "net" | "network" => Some(Self::Network),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Energy => "energy",
            Self::Disk => "disk",
            Self::Network => "network",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListeningPort {
    pub pid: i32,
    pub port: u16,
    pub address: String,
    pub protocol: String,
    pub command: String,
}

// ---------------------------------------------------------------------------------------------
// Developer-first: projects, agents, plans, containers
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub folder_path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevServer {
    pub project: String,
    pub process_pid: i32,
    pub command: String,
    pub port: u16,
    pub started_at: i64,
    pub last_active_at: i64,
    pub mem_bytes: u64,
    pub idle_secs: i64,
    pub idle: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSummary {
    pub project: Project,
    pub process_count: usize,
    pub mem_bytes: u64,
    pub cpu_pct: f64,
    pub ports: Vec<u16>,
    pub pids: Vec<i32>,
    pub servers: Vec<DevServer>,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    GeminiCli,
    CursorAgent,
    GithubCopilot,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::GeminiCli => "Gemini CLI",
            AgentKind::CursorAgent => "Cursor Agent",
            AgentKind::GithubCopilot => "GitHub Copilot",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    Waiting,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnedProcess {
    pub pid: i32,
    pub name: String,
    pub role: String,
    pub mem_bytes: u64,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    pub agent_kind: AgentKind,
    pub pid: i32,
    pub project: String,
    pub state: AgentState,
    pub started_at: i64,
    /// Memory of agent + everything it started.
    pub mem_bytes: u64,
    pub cpu_percent: f64,
    pub spawned_pids: Vec<i32>,
    pub started: Vec<SpawnedProcess>,
    pub branch_or_task: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceState {
    OnPace,
    Ahead,
    RunsOut,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AIPlan {
    pub provider: String,
    pub plan: String,
    /// e.g. "5h", "week".
    pub window: String,
    pub limit_kind: String,
    pub used_pct: f64,
    pub resets_at: Option<i64>,
    pub pace_state: PaceState,
    /// Human text: "on pace", "runs out in 4d".
    pub pace_text: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComposeStack {
    pub name: String,
    pub project_hint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub stack: Option<String>,
    pub project_hint: Option<String>,
    pub state: String,
    pub status: String,
    pub mem_bytes: u64,
    pub mem_limit: u64,
    pub cpu_pct: f64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub published_ports: Vec<u16>,
}

impl Container {
    pub fn up(&self) -> bool {
        self.state == "running"
    }
    pub fn mem_pct_of_limit(&self) -> Option<f64> {
        (self.mem_limit > 0).then(|| self.mem_bytes as f64 * 100.0 / self.mem_limit as f64)
    }
}

// ---------------------------------------------------------------------------------------------
// History / alerts
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryBucket {
    /// Bucket start, unix seconds.
    pub time_bucket: i64,
    pub resolution_s: i64,
    pub app_bundle_id: Option<String>,
    pub metric_kind: String,
    pub value_min: f64,
    pub value_avg: f64,
    pub value_max: f64,
    pub samples: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    CpuSustained,
    AppMemoryGrowth,
    DiskWriteSustained,
    NetworkSustained,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    pub kind: AlertKind,
    pub threshold: f64,
    pub duration_s: i64,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------------------------

/// Everything known at one instant. Written to `live.json` by the app so the CLI / MCP can read
/// fresh data (including idle tracking) without sampling themselves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Unix milliseconds.
    pub timestamp: i64,
    pub hostname: String,
    pub machine: MachineMetrics,
    pub frames: Vec<MetricFrame>,
    pub processes: Vec<Process>,
    pub apps: Vec<AppGroup>,
    pub ports: Vec<ListeningPort>,
    pub projects: Vec<ProjectSummary>,
    pub agents: Vec<AgentSession>,
    pub plans: Vec<AIPlan>,
    pub containers: Vec<Container>,
    pub docker_available: bool,
}

impl Snapshot {
    pub fn process(&self, pid: i32) -> Option<&Process> {
        self.processes.iter().find(|p| p.pid == pid)
    }

    pub fn top_apps(&self, metric: RankMetric, limit: usize) -> Vec<&AppGroup> {
        let mut v: Vec<&AppGroup> = self.apps.iter().collect();
        v.sort_by(|a, b| b.metric(metric).total_cmp(&a.metric(metric)).then(a.app.name.cmp(&b.app.name)));
        v.truncate(limit);
        v
    }

    pub fn app_for_pid(&self, pid: i32) -> Option<&AppGroup> {
        self.apps.iter().find(|a| a.pids.contains(&pid))
    }

    pub fn ports_of(&self, pid: i32) -> Vec<u16> {
        let mut v: Vec<u16> = self.ports.iter().filter(|p| p.pid == pid).map(|p| p.port).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}
