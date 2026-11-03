//! Localisation guard rails.
//!
//! * `gui_has_no_untranslated_literals` scans `src/gui` and flags English string literals passed
//!   straight to widgets (`RichText::new("…")`, `ui.button("…")`, `ui.label("…")`, `heading`, …)
//!   instead of going through `i18n.t()`.
//! * `every_key_used_in_code_exists` checks that each key referenced from code is in en.toml.
//! * CLI/MCP output staying English is covered by `tests/cli_golden.rs` and `tests/mcp.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pulsebar::i18n::{SOURCES, parse};

fn en_keys() -> BTreeSet<String> {
    parse(SOURCES[0]).into_iter().map(|(k, _)| k).collect()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Source with comments removed (string contents are kept, `//` inside strings is not a comment).
fn strip_comments(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < c.len() {
        if in_str {
            out.push(c[i]);
            if c[i] == '\\' && i + 1 < c.len() {
                out.push(c[i + 1]);
                i += 2;
                continue;
            }
            if c[i] == '"' {
                in_str = false;
            }
            i += 1;
        } else if c[i] == '"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c[i] == '\'' && i + 2 < c.len() && (c[i + 2] == '\'' || (c[i + 1] == '\\' && i + 3 < c.len() && c[i + 3] == '\'')) {
            // char literal such as '"' or '\n': copy verbatim so a quote in it doesn't open a string
            let n = if c[i + 1] == '\\' { 4 } else { 3 };
            out.extend(&c[i..i + n]);
            i += n;
        } else if c[i] == '/' && i + 1 < c.len() && c[i + 1] == '/' {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// Drop `#[cfg(test)] mod tests { … }` blocks: tests may use literals freely.
fn strip_tests(src: &str) -> String {
    match src.find("#[cfg(test)]") {
        Some(i) => src[..i].to_string(),
        None => src.to_string(),
    }
}

/// Text between the parenthesis opened at `open` (index of '(') and its match, plus the end index.
fn balanced(src: &[char], open: usize) -> Option<(String, usize)> {
    let (mut depth, mut i, mut in_str) = (0i32, open, false);
    while i < src.len() {
        let ch = src[i];
        if in_str {
            if ch == '\\' {
                i += 2;
                continue;
            }
            if ch == '"' {
                in_str = false;
            }
        } else {
            match ch {
                '"' => in_str = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((src[open + 1..i].iter().collect(), i));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn split_args(args: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut in_str) = (vec![], String::new(), 0i32, false);
    let c: Vec<char> = args.chars().collect();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if in_str {
            cur.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                cur.push(c[i + 1]);
                i += 2;
                continue;
            }
            if ch == '"' {
                in_str = false;
            }
        } else {
            match ch {
                '"' => {
                    in_str = true;
                    cur.push(ch);
                }
                '(' | '[' | '{' => {
                    depth += 1;
                    cur.push(ch);
                }
                ')' | ']' | '}' => {
                    depth -= 1;
                    cur.push(ch);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(ch),
            }
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// The leading string literal of an argument: `"x"`, `"x".to_string()`, `format!("x {}", …)`.
fn leading_literal(arg: &str) -> Option<String> {
    let a = arg.trim().trim_start_matches('&').trim();
    let a = a.strip_prefix("format!(").map(str::trim_start).unwrap_or(a);
    let rest = a.strip_prefix('"')?;
    let mut s = String::new();
    let mut it = rest.chars();
    while let Some(ch) = it.next() {
        match ch {
            '\\' => {
                if let Some(n) = it.next() {
                    s.push(n);
                }
            }
            '"' => return Some(s),
            _ => s.push(ch),
        }
    }
    None
}

/// Product names, technical terms and symbols that are the same in every language.
const ALLOWED: &[&str] = &["CPU", "GPU", "PID", "macOS", "Bluetooth", "Docker", "Neural Engine", "Apple Neural Engine", "PulseBar", "MCP", "RAM", "pid {}"];

fn needs_translation(lit: &str, keys: &BTreeSet<String>) -> bool {
    if keys.contains(lit) || ALLOWED.contains(&lit) {
        return false;
    }
    // Look at the text outside `{…}` placeholders for a word of three or more letters.
    let mut text = String::new();
    let mut depth = 0;
    for ch in lit.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ if depth == 0 => text.push(ch),
            _ => {}
        }
    }
    let mut run = 0;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            run += 1;
            if run >= 3 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Calls whose string arguments the user reads.
const WIDGET_CALLS: &[&str] = &[
    "RichText::new(",
    "Button::new(",
    "Checkbox::new(",
    "Label::new(",
    ".button(",
    ".small_button(",
    ".label(",
    ".heading(",
    ".checkbox(",
    ".on_hover_text(",
    ".on_disabled_hover_text(",
    ".selected_text(",
    "Tile::text(",
    "Tile::new(",
    "Self::section(",
    "chip(",
    "pill(",
    "stat(",
];

/// Calls whose *third* argument is the visible label (the second is the value it selects).
const VALUE_CALLS: &[&str] = &[".radio_value(", ".selectable_value("];

/// Calls that draw text through the painter: `painter.text(pos, align, TEXT, font, color)`.
const PAINTER_TEXT: &str = ".text(";

#[test]
fn gui_has_no_untranslated_literals() {
    let keys = en_keys();
    let mut files = vec![];
    rs_files(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui").as_path(), &mut files);
    let mut offenders = vec![];
    for f in files {
        let src = strip_tests(&strip_comments(&std::fs::read_to_string(&f).unwrap()));
        let chars: Vec<char> = src.chars().collect();
        let mut check = |name: &str, only_arg: Option<usize>| {
            let mut from = 0;
            while let Some(pos) = src[from..].find(name) {
                let at = from + pos;
                from = at + name.len();
                // `src` is ASCII-indexed by char below; convert the byte offset.
                let char_at = src[..at + name.len() - 1].chars().count();
                let Some((args, _)) = balanced(&chars, char_at) else { continue };
                for (i, a) in split_args(&args).iter().enumerate() {
                    if only_arg.is_some_and(|n| n != i) {
                        continue;
                    }
                    if let Some(lit) = leading_literal(a)
                        && needs_translation(&lit, &keys)
                    {
                        let line = src[..at].matches('\n').count() + 1;
                        offenders.push(format!("{}:{line}: {name}… {lit:?}", f.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display()));
                    }
                }
            }
        };
        for n in WIDGET_CALLS {
            check(n, None);
        }
        for n in VALUE_CALLS {
            check(n, Some(2));
        }
        check(PAINTER_TEXT, Some(2));
    }
    offenders.sort();
    offenders.dedup();
    assert!(offenders.is_empty(), "string literals that bypass i18n.t() (add a key to locales/*.toml):\n{}", offenders.join("\n"));
}

/// Looks like an i18n key: lowercase segments joined by dots.
fn looks_like_key(s: &str) -> bool {
    s.contains('.') && s.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')) && !s.ends_with(".rs")
}

#[test]
fn every_key_used_in_code_exists() {
    let keys = en_keys();
    // Keys that start with a namespace en.toml defines are checked wherever they appear in code.
    let namespaces: BTreeSet<&str> = keys.iter().filter_map(|k| k.split_once('.').map(|(ns, _)| ns)).collect();
    let mut files = vec![];
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut missing = vec![];
    for f in files {
        if f.ends_with("i18n.rs") {
            continue;
        }
        let src = strip_tests(&strip_comments(&std::fs::read_to_string(&f).unwrap()));
        let mut rest = src.as_str();
        while let Some(i) = rest.find('"') {
            let after = &rest[i + 1..];
            let Some(j) = after.find('"') else { break };
            let lit = &after[..j];
            rest = &after[j + 1..];
            // A closing quote followed by more text can swallow an escaped quote: ignore odd literals.
            let is_file = [".pid", ".json", ".toml", ".png", ".csv", ".db", ".lock"].iter().any(|e| lit.ends_with(e));
            if lit.contains('\\') || lit.contains(' ') || is_file || !looks_like_key(lit) {
                continue;
            }
            let ns = lit.split('.').next().unwrap();
            if !namespaces.contains(ns) {
                continue;
            }
            let ok = keys.contains(lit) || keys.contains(&format!("{lit}.one"));
            if !ok {
                missing.push(format!("{}: {lit}", f.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display()));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "keys used in code but missing from locales/en.toml:\n{}", missing.join("\n"));
}

/// Keys built at run time from enums must exist for every variant.
#[test]
fn dynamic_keys_exist() {
    use pulsebar::model::MetricKind;
    use pulsebar::settings::{MenuMetric, Tab};
    let keys = en_keys();
    for k in MetricKind::ALL {
        assert!(keys.contains(&format!("metric.{}", k.as_str())), "metric.{}", k.as_str());
    }
    for m in MenuMetric::ALL {
        assert!(keys.contains(m.key()), "{}", m.key());
    }
    for t in Tab::DEFAULT_ORDER {
        assert!(keys.contains(t.key()), "{}", t.key());
    }
    for s in ["running", "idle", "serving"] {
        assert!(keys.contains(&format!("project.{s}")), "project.{s}");
    }
}
