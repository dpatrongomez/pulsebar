//! Shareable 1200×630 cards (light / dark), rendered as SVG and converted to PNG locally with
//! Quick Look when available. Only summarised numbers — never process lists or paths.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Card {
    pub title: String,
    pub headline: String,
    pub subtitle: String,
    /// (label, value, detail)
    pub rows: Vec<(String, String, String)>,
    pub dark: bool,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn render_svg(c: &Card) -> String {
    let (bg, fg, muted, accent, bar_bg) =
        if c.dark { ("#111317", "#F4F5F7", "#9AA0AA", "#4CC38A", "#23262D") } else { ("#FFFFFF", "#111317", "#5E6570", "#16A066", "#EEF0F3") };
    let mut s = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
<rect width="1200" height="630" rx="32" fill="{bg}"/>
<text x="72" y="104" font-family="-apple-system, Helvetica Neue, Arial" font-size="34" font-weight="600" fill="{muted}">{}</text>
<text x="72" y="210" font-family="-apple-system, Helvetica Neue, Arial" font-size="96" font-weight="700" fill="{fg}">{}</text>
<text x="72" y="262" font-family="-apple-system, Helvetica Neue, Arial" font-size="30" fill="{muted}">{}</text>
"#,
        esc(&c.title),
        esc(&c.headline),
        esc(&c.subtitle)
    );
    let n = c.rows.len().min(5);
    for (i, (label, value, detail)) in c.rows.iter().take(5).enumerate() {
        let y = 330 + i as i32 * 52;
        let frac = 1.0 - i as f64 / (n.max(1) as f64 + 1.0);
        s.push_str(&format!(
            r#"<rect x="72" y="{}" width="1056" height="40" rx="10" fill="{bar_bg}"/>
<rect x="72" y="{}" width="{:.0}" height="40" rx="10" fill="{accent}" opacity="0.25"/>
<text x="92" y="{}" font-family="-apple-system, Helvetica Neue, Arial" font-size="24" font-weight="600" fill="{fg}">{}</text>
<text x="1108" y="{}" text-anchor="end" font-family="-apple-system, Helvetica Neue, Arial" font-size="24" fill="{fg}">{} <tspan fill="{muted}">{}</tspan></text>
"#,
            y,
            y,
            1056.0 * frac,
            y + 28,
            esc(label),
            y + 28,
            esc(value),
            esc(detail)
        ));
    }
    s.push_str(&format!(
        r#"<text x="1128" y="600" text-anchor="end" font-family="-apple-system, Helvetica Neue, Arial" font-size="22" font-weight="600" fill="{accent}">PulseBar</text>
</svg>"#
    ));
    s
}

/// Write the card to `dir`; returns the PNG path when Quick Look could convert, else the SVG.
pub fn export(c: &Card, dir: &std::path::Path, stem: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let svg = dir.join(format!("{stem}-{}.svg", if c.dark { "dark" } else { "light" }));
    std::fs::write(&svg, render_svg(c))?;
    let ok = std::process::Command::new("/usr/bin/qlmanage")
        .args(["-t", "-s", "1200", "-o"])
        .arg(dir)
        .arg(&svg)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let produced = dir.join(format!("{}.png", svg.file_name().unwrap().to_string_lossy()));
    if ok && produced.exists() {
        let png = svg.with_extension("png");
        std::fs::rename(&produced, &png)?;
        Ok(png)
    } else {
        Ok(svg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn svg_is_well_formed_and_escaped() {
        let c = Card {
            title: "Memory by App".into(),
            headline: "21.23 GB".into(),
            subtitle: "Google Chrome <96 processes>".into(),
            rows: vec![("Chrome".into(), "21.23 GB".into(), "96 processes".into())],
            dark: true,
        };
        let s = render_svg(&c);
        assert!(s.starts_with("<svg") && s.ends_with("</svg>"));
        assert!(s.contains("width=\"1200\" height=\"630\""));
        assert!(s.contains("&lt;96 processes&gt;"));
    }
}
