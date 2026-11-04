//! Unit-aware formatting. Every number that reaches the UI, CLI or share cards goes through
//! here, so a settings change (bytes → bits, °C → °F) is reflected everywhere.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataUnit {
    #[default]
    Bytes,
    Bits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuUnit {
    /// 0–100 % of the whole Mac.
    #[default]
    PercentOfMac,
    /// 0–(cores×100) %, like `top`.
    PerCore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Units {
    pub temperature: TempUnit,
    pub data: DataUnit,
    pub cpu: CpuUnit,
    /// Write the decimal separator as a comma ("3,07 GB"). Set by the GUI for Spanish; not a
    /// setting (never saved), and the CLI/MCP leave it off so their output stays English.
    #[serde(skip)]
    pub decimal_comma: bool,
}

impl Units {
    /// Memory/storage sizes are always shown in bytes (bits make no sense for RAM); the
    /// bytes/bits choice applies to *rates* (disk and network throughput) and to totals of
    /// transferred data.
    pub fn bytes(&self, b: u64) -> String {
        self.loc(format_bytes(b as f64))
    }

    pub fn rate(&self, bytes_per_sec: f64) -> String {
        self.loc(match self.data {
            DataUnit::Bytes => format!("{}/s", format_bytes(bytes_per_sec)),
            DataUnit::Bits => format!("{}ps", format_bits(bytes_per_sec * 8.0)),
        })
    }

    pub fn transferred(&self, bytes: u64) -> String {
        self.loc(match self.data {
            DataUnit::Bytes => format_bytes(bytes as f64),
            DataUnit::Bits => format_bits(bytes as f64 * 8.0),
        })
    }

    pub fn temp(&self, celsius: f64) -> String {
        match self.temperature {
            TempUnit::Celsius => format!("{celsius:.0} °C"),
            TempUnit::Fahrenheit => format!("{:.0} °F", celsius * 9.0 / 5.0 + 32.0),
        }
    }

    /// Power in watts with a fixed number of decimals ("1.23 W", "1,23 W" when the GUI asks).
    pub fn watts(&self, w: f64, decimals: usize) -> String {
        self.loc(format!("{w:.decimals$} W"))
    }

    /// `pct_of_mac` is always 0–100 internally.
    pub fn cpu(&self, pct_of_mac: f64, cores: usize) -> String {
        match self.cpu {
            CpuUnit::PercentOfMac => format!("{pct_of_mac:.0} %"),
            CpuUnit::PerCore => format!("{:.0} %", pct_of_mac * cores.max(1) as f64),
        }
    }

    /// Swap the decimal point for a comma when asked to. Only ever fed numbers with units.
    fn loc(&self, s: String) -> String {
        if self.decimal_comma { s.replace('.', ",") } else { s }
    }

    /// Short form for the menu bar.
    pub fn rate_short(&self, bytes_per_sec: f64) -> String {
        self.rate(bytes_per_sec).replace(' ', "")
    }
}

pub fn format_bytes(b: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    match i {
        0 => format!("{v:.0} {}", UNITS[i]),
        1 | 2 => format!("{} {}", one_decimal(v), UNITS[i]),
        _ => format!("{v:.2} {}", UNITS[i]),
    }
}

/// "2.4", "458", "1" (one decimal below 100, trailing ".0" dropped).
fn one_decimal(v: f64) -> String {
    if v >= 100.0 {
        return format!("{v:.0}");
    }
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

pub fn format_bits(b: f64) -> String {
    const UNITS: [&str; 5] = ["b", "Kb", "Mb", "Gb", "Tb"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    match i {
        0 => format!("{v:.0} {}", UNITS[i]),
        1 | 2 => format!("{} {}", one_decimal(v), UNITS[i]),
        _ => format!("{v:.2} {}", UNITS[i]),
    }
}

/// "3 h 12 min", "4 d 2 h", "45 s".
pub fn format_duration(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_and_bits() {
        let u = Units::default();
        assert_eq!(format_bytes(21_230_000_000.0), "21.23 GB");
        assert_eq!(format_bytes(458_000_000.0), "458 MB");
        assert_eq!(u.rate(1_000_000.0), "1 MB/s");
        assert_eq!(u.rate(2_400_000.0), "2.4 MB/s");
        let bits = Units { data: DataUnit::Bits, ..Default::default() };
        assert_eq!(bits.rate(1_000_000.0), "8 Mbps");
        // memory is always bytes
        assert_eq!(bits.bytes(240_000_000), "240 MB");
    }

    #[test]
    fn decimal_comma_is_opt_in() {
        let es = Units { decimal_comma: true, ..Default::default() };
        assert_eq!(es.bytes(3_070_000_000), "3,07 GB");
        assert_eq!(es.rate(2_400_000.0), "2,4 MB/s");
        assert_eq!(Units::default().bytes(3_070_000_000), "3.07 GB");
        assert_eq!(es.watts(1.234, 2), "1,23 W");
        assert_eq!(Units::default().watts(1.234, 2), "1.23 W");
        assert_eq!(Units::default().watts(1.26, 1), "1.3 W");
        // Never serialised: settings.json is unchanged.
        assert!(!serde_json::to_string(&es).unwrap().contains("decimal"));
    }

    #[test]
    fn temps_and_cpu() {
        let f = Units { temperature: TempUnit::Fahrenheit, cpu: CpuUnit::PerCore, ..Default::default() };
        assert_eq!(f.temp(100.0), "212 °F");
        assert_eq!(f.cpu(25.0, 8), "200 %");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(4 * 86_400 + 2 * 3600), "4 d 2 h");
        assert_eq!(format_duration(3 * 3600 + 60), "3 h 1 min");
        assert_eq!(format_duration(42), "42 s");
    }
}
