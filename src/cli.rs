//! M9 — `pulsebar` CLI. Table output by default, `--json` for machine-readable output that
//! validates against `schemas/*.schema.json`.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use crate::actions::{self, HumanYes, Mode, Target};
use crate::i18n::I18n;
use crate::model::{RankMetric, Snapshot};
use crate::settings::Settings;
use crate::units::{DataUnit, TempUnit, Units};
use crate::{history, live, output, paths, projects, sampler, share_card, sys, views};

#[derive(Parser, Debug)]
#[command(name = "pulsebar", version, about = "App-centric activity monitor for macOS developers")]
pub struct Cli {
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum TopMetric {
    Cpu,
    Memory,
    Mem,
    Energy,
    Disk,
    Net,
    Network,
}

impl From<TopMetric> for RankMetric {
    fn from(t: TopMetric) -> Self {
        match t {
            TopMetric::Cpu => RankMetric::Cpu,
            TopMetric::Memory | TopMetric::Mem => RankMetric::Memory,
            TopMetric::Energy => RankMetric::Energy,
            TopMetric::Disk => RankMetric::Disk,
            TopMetric::Net | TopMetric::Network => RankMetric::Network,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start the menu bar app.
    App,
    /// Open the main window (normally launched by the menu bar app).
    #[command(hide = true)]
    Window {
        #[arg(long, default_value = "overview")]
        tab: String,
        #[arg(long)]
        app: Option<String>,
    },
    /// Machine summary.
    Status,
    /// Top apps by a metric.
    Top {
        #[arg(value_enum)]
        metric: TopMetric,
        #[arg(default_value_t = 10)]
        n: usize,
    },
    /// Listening TCP ports with process, app and project.
    Ports,
    /// Processes grouped by project folder, dev servers and idle state.
    Projects,
    /// AI coding-agent sessions.
    Agents,
    /// AI plan limits (computed from local logs).
    Limits,
    /// Docker containers by compose stack.
    Containers,
    /// Process inspector card.
    Process {
        pid: i32,
        /// Skip code-signature / architecture checks.
        #[arg(long)]
        fast: bool,
        /// Output CSV instead of text.
        #[arg(long)]
        csv: bool,
    },
    /// Stop processes (always asks for confirmation unless --yes).
    Stop {
        /// Stop every idle dev server.
        #[arg(long)]
        idle: bool,
        /// A single pid.
        #[arg(long)]
        pid: Option<i32>,
        /// An app (name or bundle id): all of its processes.
        #[arg(long)]
        app: Option<String>,
        /// SIGKILL instead of SIGTERM.
        #[arg(long)]
        force: bool,
        /// Do not ask (you are confirming by passing this flag).
        #[arg(long)]
        yes: bool,
    },
    /// Historical series for a metric.
    History {
        /// cpu, memory, disk, network, gpu, power… or app_cpu / app_memory / app_energy with --app
        metric: String,
        #[arg(long, default_value = "24h")]
        range: String,
        #[arg(long)]
        app: Option<String>,
        /// Rank apps for the period instead (app_memory, app_energy, app_cpu).
        #[arg(long)]
        rank: bool,
    },
    /// Run the MCP server on stdio (must be enabled in settings).
    Mcp,
    /// Export a 1200×630 share card (SVG/PNG) to ~/Downloads.
    Card {
        #[arg(value_enum, default_value = "memory")]
        metric: TopMetric,
        #[arg(long)]
        dark: bool,
    },
    /// Read or change settings.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    Show,
    /// Enable/disable the MCP server.
    Mcp { state: OnOff },
    /// Data rates in bytes or bits.
    Data { unit: DataChoice },
    /// Temperature unit.
    Temp { unit: TempChoice },
    /// Reset menu bar + tab layout.
    ResetLayout,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DataChoice {
    Bytes,
    Bits,
}
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum TempChoice {
    C,
    F,
}

fn snapshot(settings: &Settings) -> Snapshot {
    live::read_fresh().unwrap_or_else(|| sampler::Engine::sample_once(settings))
}

fn print_json(v: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn ask(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn parse_range(r: &str) -> Result<i64> {
    Ok(match r {
        "12h" => 12 * 3600,
        "24h" | "1d" => 86_400,
        "7d" => 7 * 86_400,
        "30d" => 30 * 86_400,
        "12m" | "1y" => 365 * 86_400,
        _ => bail!("range must be 12h, 24h, 7d, 30d or 12m"),
    })
}

pub fn run(cli: Cli) -> Result<()> {
    let mut settings = Settings::load();
    let u = settings.units;
    let Some(cmd) = cli.command else { unreachable!("handled in main") };
    match cmd {
        Command::App | Command::Window { .. } => bail!("this build has no GUI (built without the `gui` feature)"),
        Command::Status => {
            let s = snapshot(&settings);
            if cli.json { print_json(&views::status(&s))? } else { print!("{}", output::status(&s, &u)) }
        }
        Command::Top { metric, n } => {
            let s = snapshot(&settings);
            let m: RankMetric = metric.into();
            if cli.json { print_json(&views::top(&s, m, n))? } else { print!("{}", output::top(&s, m, n, &u)) }
        }
        Command::Ports => {
            let s = snapshot(&settings);
            if cli.json { print_json(&views::ports(&s))? } else { print!("{}", output::ports(&s, &u)) }
        }
        Command::Projects => {
            let s = snapshot(&settings);
            if cli.json { print_json(&views::projects(&s))? } else { print!("{}", output::projects(&s, &u)) }
        }
        Command::Agents => {
            let s = snapshot(&settings);
            if cli.json { print_json(&views::agents(&s))? } else { print!("{}", output::agents(&s, &u)) }
        }
        Command::Limits => {
            let s = snapshot(&settings);
            if cli.json { print_json(&views::limits(&s))? } else { print!("{}", output::limits(&s)) }
        }
        Command::Containers => {
            let s = snapshot(&settings);
            if cli.json {
                print_json(&views::containers(&s))?
            } else if !s.docker_available {
                println!("Docker is not running (no socket found).");
            } else {
                for (stack, cs) in crate::docker::stacks(&s.containers) {
                    println!("{}", stack.name);
                    for c in cs {
                        println!("  {} {}", if c.up() { "●" } else { "○" }, crate::docker::card_line(c));
                    }
                }
            }
        }
        Command::Process { pid, fast, csv } => {
            let s = snapshot(&settings);
            let d = crate::inspector::inspect(&s, pid, !fast).with_context(|| format!("no process with pid {pid}"))?;
            if cli.json {
                print_json(&serde_json::to_value(&d)?)?
            } else if csv {
                print!("{}", crate::inspector::to_csv(&d))
            } else {
                print!("{}", crate::inspector::to_text(&d, &u, s.machine.cpu.core_count))
            }
        }
        Command::Stop { idle, pid, app, force, yes } => {
            let s = snapshot(&settings);
            let mode = if force { Mode::ForceQuit } else { Mode::Quit };
            let targets: Vec<Target> = if idle {
                projects::idle_servers(&s.projects).iter().map(|d| Target::DevServer(d.process_pid)).collect()
            } else if let Some(p) = pid {
                vec![Target::Pid(p)]
            } else if let Some(a) = app {
                vec![Target::App(a)]
            } else {
                bail!("use --idle, --pid <pid> or --app <name>");
            };
            if targets.is_empty() {
                if cli.json { print_json(&serde_json::json!({"stopped": [], "message": "no idle dev servers"}))? } else { println!("No idle dev servers.") }
                return Ok(());
            }
            let plans: Vec<actions::Plan> = targets.into_iter().map(|t| actions::plan(&s, t, mode)).collect::<Result<_>>()?;
            if !cli.json {
                for p in &plans {
                    println!("• {}", p.summary());
                }
            }
            if let Some(b) = idle.then(|| projects::idle_banner(&s.projects)).flatten()
                && !cli.json {
                    println!("\n{b}");
                }
            let confirmed = yes || ask(&format!("Stop {} target{}?", plans.len(), if plans.len() == 1 { "" } else { "s" }));
            if !confirmed {
                if cli.json {
                    print_json(&serde_json::json!({"stopped": [], "message": "not confirmed (pass --yes or answer y)"}))?;
                } else {
                    println!("Nothing stopped.");
                }
                return Ok(());
            }
            let outcomes: Vec<actions::Outcome> = plans.into_iter().map(|p| actions::execute(actions::confirm(p, HumanYes::from_cli()))).collect();
            if cli.json {
                print_json(&serde_json::json!({"stopped": outcomes}))?;
            } else {
                for o in outcomes {
                    println!("{}", o.message);
                }
            }
        }
        Command::History { metric, range, app, rank } => {
            let h = history::History::open(&paths::database_file())?;
            let now = sys::now_s();
            let from = now - parse_range(&range)?;
            if rank {
                let rows = h.ranking(&metric, from, now, now, 10)?;
                if cli.json {
                    print_json(&serde_json::json!({"metric": metric, "range": range, "ranking": rows.iter().map(|r| serde_json::json!({"app": r.app, "avg": r.avg, "peak": r.peak, "integral": r.integral})).collect::<Vec<_>>()}))?;
                } else {
                    let mut t = output::Table::new(&["#", "APP", "AVERAGE", "PEAK"], &[true, false, true, true]);
                    for (i, r) in rows.iter().enumerate() {
                        let f = |v: f64| if metric.contains("memory") { u.bytes(v as u64) } else { format!("{v:.2}") };
                        t.row(vec![(i + 1).to_string(), r.app.clone(), f(r.avg), f(r.peak)]);
                    }
                    print!("{}", t.render());
                }
            } else {
                let series = h.series(&metric, app.as_deref(), from, now, now)?;
                if cli.json {
                    print_json(&serde_json::to_value(&series)?)?;
                } else {
                    let mut t = output::Table::new(&["TIME", "AVG", "MIN", "MAX"], &[false, true, true, true]);
                    for b in &series {
                        let ts = chrono::DateTime::from_timestamp(b.time_bucket, 0)
                            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        t.row(vec![ts, format!("{:.2}", b.value_avg), format!("{:.2}", b.value_min), format!("{:.2}", b.value_max)]);
                    }
                    print!("{}", t.render());
                }
            }
        }
        Command::Mcp => crate::mcp::serve_stdio(settings)?,
        Command::Card { metric, dark } => {
            let s = snapshot(&settings);
            let m: RankMetric = metric.into();
            let card = card_for_in(&I18n::english(), &settings.units, &s, m, dark);
            let path = share_card::export(&card, &paths::exports_dir(), &format!("pulsebar-{}", m.as_str()))?;
            println!("{}", path.display());
        }
        Command::Config { action } => {
            match action {
                ConfigAction::Show => {}
                ConfigAction::Mcp { state } => settings.mcp_enabled = matches!(state, OnOff::On),
                ConfigAction::Data { unit } => {
                    settings.units.data = match unit {
                        DataChoice::Bytes => DataUnit::Bytes,
                        DataChoice::Bits => DataUnit::Bits,
                    }
                }
                ConfigAction::Temp { unit } => {
                    settings.units.temperature = match unit {
                        TempChoice::C => TempUnit::Celsius,
                        TempChoice::F => TempUnit::Fahrenheit,
                    }
                }
                ConfigAction::ResetLayout => settings.reset_layout(),
            }
            settings.save()?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
    }
    Ok(())
}

/// Summarised share card for a ranking, worded in `i18n`'s language with `u`'s number format.
/// The CLI passes `I18n::english()` and `settings.units`, which is what it always printed.
pub fn card_for_in(i18n: &I18n, u: &Units, s: &Snapshot, m: RankMetric, dark: bool) -> share_card::Card {
    let cores = s.machine.cpu.core_count;
    let top = s.top_apps(m, 5);
    let (title, headline) = match m {
        RankMetric::Memory => ("card.memory", u.bytes(s.machine.memory.used)),
        RankMetric::Cpu => ("card.cpu", u.cpu(s.machine.cpu.total_pct, cores)),
        RankMetric::Energy => ("card.energy", s.machine.power.system_w.map(|w| u.watts(w, 1)).unwrap_or("—".into())),
        RankMetric::Disk => ("card.disk", u.rate(s.machine.disk.read_bps + s.machine.disk.write_bps)),
        RankMetric::Network => ("card.network", u.rate(s.machine.network.rx_bps + s.machine.network.tx_bps)),
    };
    share_card::Card {
        title: i18n.t(title),
        headline,
        subtitle: i18n.tf(
            "card.summary",
            &[("processes", &i18n.tn("n.processes", s.processes.len(), &[])), ("apps", &i18n.tn("n.apps", s.apps.len(), &[]))],
        ),
        rows: top
            .iter()
            .map(|a| (a.app.name.clone(), output::metric_value(a, m, u, cores), i18n.tn("n.processes", a.process_count, &[])))
            .collect(),
        dark,
    }
}
