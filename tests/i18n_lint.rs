//! Localisation guard rails.
//!
//! * `gui_has_no_untranslated_literals` scans `src/gui` and flags English words in string literals
//!   passed to widgets (`ui.label(..)`, `RichText::new(..)`, `.small(..)`, `Window::new(..)`,
//!   `self.toast(..)`, ...) or assigned to text fields (`caption: "..".into()`). Every literal in
//!   an argument counts, and only the key-taking helpers (`stat`, `top_app_tile`, `app_list`)
//!   may be given a bare key. `the_lint_catches_what_it_claims_to` pins the shapes it must catch.
//! * `i18n_calls_use_exact_keys_and_placeholders` checks each `t/tf/tn("key", ..)` call: the key
//!   exists (both plural forms for `tn`) and the argument names match the `{placeholders}`.
//! * `every_key_used_in_code_exists` checks any dotted key-like literal in code against en.toml.
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

/// Every string literal in `s`, unescaped, in order. Char literals such as `'"'` are skipped.
fn literals(s: &str) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let (mut out, mut i) = (vec![], 0);
    while i < c.len() {
        if c[i] == '\'' && i + 2 < c.len() && (c[i + 2] == '\'' || (c[i + 1] == '\\' && i + 3 < c.len() && c[i + 3] == '\'')) {
            i += if c[i + 1] == '\\' { 4 } else { 3 };
        } else if c[i] == '"' {
            let mut lit = String::new();
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                }
                lit.push(c[i]);
                i += 1;
            }
            out.push(lit);
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

fn is_ident(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Byte offsets of calls to `name` (`t`, `tf`, `tn`) as a method or a plain function: the
/// character before it is not part of an identifier, and `(` follows.
fn t_calls(src: &str) -> Vec<(usize, &'static str)> {
    let mut out = vec![];
    for name in ["tn", "tf", "t"] {
        let pat = format!("{name}(");
        let mut from = 0;
        while let Some(p) = src[from..].find(&pat) {
            let at = from + p;
            from = at + pat.len();
            let before = src[..at].chars().next_back();
            if before.is_some_and(is_ident) {
                continue;
            }
            // `fn t(…)` is the definition, not a call.
            if src[..at].trim_end().ends_with("fn") {
                continue;
            }
            out.push((at, name));
        }
    }
    out.sort();
    out
}

/// `arg` without its `t(…)` / `tf(…)` / `tn(…)` calls: those take keys, which the key test
/// checks, and their placeholder names are not text either.
fn without_t_calls(arg: &str) -> String {
    let chars: Vec<char> = arg.chars().collect();
    let mut remove = vec![false; chars.len()];
    for (at, name) in t_calls(arg) {
        let char_at = arg[..at].chars().count();
        if let Some((_, end)) = balanced(&chars, char_at + name.len()) {
            remove[char_at..=end].iter_mut().for_each(|r| *r = true);
        }
    }
    chars.iter().zip(remove).filter(|(_, r)| !r).map(|(c, _)| *c).collect()
}

/// Product names, technical terms and symbols that are the same in every language.
const ALLOWED: &[&str] = &["CPU", "GPU", "PID", "macOS", "Bluetooth", "Docker", "Neural Engine", "Apple Neural Engine", "PulseBar", "MCP", "RAM", "pid {}"];

/// Does this literal contain a word a person would read (three or more letters outside `{…}`)?
fn needs_translation(lit: &str) -> bool {
    if ALLOWED.contains(&lit) {
        return false;
    }
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

/// Which arguments of a call carry text the user reads.
#[derive(Clone, Copy)]
enum Args {
    All,
    Only(usize),
}

/// Calls whose string arguments the user reads.
const WIDGET_CALLS: &[(&str, Args)] = &[
    ("RichText::new(", Args::All),
    ("Button::new(", Args::All),
    ("Checkbox::new(", Args::All),
    ("Label::new(", Args::All),
    ("Window::new(", Args::All),
    (".button(", Args::All),
    (".small_button(", Args::All),
    (".label(", Args::All),
    (".heading(", Args::All),
    (".small(", Args::All),
    (".strong(", Args::All),
    (".weak(", Args::All),
    (".monospace(", Args::All),
    (".checkbox(", Args::All),
    (".on_hover_text(", Args::All),
    (".on_disabled_hover_text(", Args::All),
    (".selected_text(", Args::All),
    (".colored_label(", Args::Only(1)),
    (".selectable_label(", Args::Only(1)),
    // The second argument is a URL / the body closure.
    (".hyperlink_to(", Args::Only(0)),
    (".collapsing(", Args::Only(0)),
    // `radio_value(&mut v, value, LABEL)`.
    (".radio_value(", Args::Only(2)),
    (".selectable_value(", Args::Only(2)),
    // `painter.text(pos, align, TEXT, font, color)`.
    (".text(", Args::Only(2)),
    ("Tile::text(", Args::All),
    ("Tile::new(", Args::All),
    ("Self::section(", Args::All),
    ("chip(", Args::All),
    ("pill(", Args::All),
    (".toast(", Args::All),
];

/// Helpers that take an i18n *key* (not text) in one argument; only there may a literal be a
/// bare key. Everywhere else `ui.label("temperature")` is an untranslated word.
const KEY_CALLS: &[(&str, usize)] = &[("stat(", 0), ("top_app_tile(", 2), ("app_list(", 4)];

/// Struct fields that hold text shown on screen (`caption: "Downloading".into()`).
const TEXT_FIELDS: &[&str] = &["caption", "title", "label", "name", "small", "big", "cancel", "confirm", "subject", "value"];

/// Source text of the expression that starts at char index `from`: up to the `,` or closing
/// bracket that ends it.
fn expr_from(chars: &[char], from: usize) -> String {
    let (mut depth, mut i, mut in_str) = (0i32, from, false);
    while i < chars.len() {
        let ch = chars[i];
        if in_str {
            if ch == '\\' {
                i += 2;
                continue;
            }
            in_str = ch != '"';
        } else {
            match ch {
                '"' => in_str = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => break,
                ')' | ']' | '}' => depth -= 1,
                ',' | ';' if depth == 0 => break,
                _ => {}
            }
        }
        i += 1;
    }
    chars[from..i.min(chars.len())].iter().collect()
}

/// Untranslated English in the given source: every flagged (line, call, literal).
fn untranslated_in(src: &str, keys: &BTreeSet<String>) -> Vec<(usize, String, String)> {
    let chars: Vec<char> = src.chars().collect();
    let line_of = |byte: usize| src[..byte].matches('\n').count() + 1;
    let mut found = vec![];
    fn flag(found: &mut Vec<(usize, String, String)>, line: usize, what: &str, expr: &str) {
        // A closure body is code of its own: the calls inside it are checked where they stand.
        let e = expr.trim_start();
        if e.starts_with('|') || e.starts_with("move |") {
            return;
        }
        for lit in literals(&without_t_calls(expr)) {
            if needs_translation(&lit) {
                found.push((line, what.to_string(), lit));
            }
        }
    }
    for (name, which) in WIDGET_CALLS {
        let mut from = 0;
        while let Some(pos) = src[from..].find(name) {
            let at = from + pos;
            from = at + name.len();
            let char_at = src[..at + name.len() - 1].chars().count();
            let Some((args, _)) = balanced(&chars, char_at) else { continue };
            for (i, a) in split_args(&args).iter().enumerate() {
                if matches!(which, Args::Only(n) if *n != i) {
                    continue;
                }
                flag(&mut found, line_of(at), name, a);
            }
        }
    }
    for (name, key_arg) in KEY_CALLS {
        let mut from = 0;
        while let Some(pos) = src[from..].find(name) {
            let at = from + pos;
            from = at + name.len();
            if src[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let char_at = src[..at + name.len() - 1].chars().count();
            let Some((args, _)) = balanced(&chars, char_at) else { continue };
            for (i, a) in split_args(&args).iter().enumerate() {
                if i == *key_arg {
                    // A literal here must be an existing key, not English.
                    for lit in literals(a) {
                        if !keys.contains(&lit) {
                            found.push((line_of(at), format!("{name} (key argument)"), lit));
                        }
                    }
                } else {
                    flag(&mut found, line_of(at), name, a);
                }
            }
        }
    }
    for field in TEXT_FIELDS {
        let pat = format!("{field}:");
        let mut from = 0;
        while let Some(pos) = src[from..].find(&pat) {
            let at = from + pos;
            from = at + pat.len();
            if src[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let expr = expr_from(&chars, src[..from].chars().count());
            flag(&mut found, line_of(at), &pat, &expr);
        }
    }
    found
}

#[test]
fn gui_has_no_untranslated_literals() {
    let keys = en_keys();
    let mut files = vec![];
    rs_files(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui").as_path(), &mut files);
    let mut offenders = vec![];
    for f in files {
        let src = strip_tests(&strip_comments(&std::fs::read_to_string(&f).unwrap()));
        for (line, what, lit) in untranslated_in(&src, &keys) {
            offenders.push(format!("{}:{line}: {what}… {lit:?}", f.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display()));
        }
    }
    offenders.sort();
    offenders.dedup();
    assert!(offenders.is_empty(), "string literals that bypass i18n.t() (add a key to locales/*.toml):\n{}", offenders.join("\n"));
}

/// The lint itself: every shape a regression could take must be caught, and translated code
/// must not be.
#[test]
fn the_lint_catches_what_it_claims_to() {
    let keys = en_keys();
    let bad = [
        r#"ui.label("Hello control");"#,
        r#"ui.label("temperature");"#,
        r#"ui.label(if x { "Until Full" } else { "Remaining time" });"#,
        r#"ui.label(RichText::new("Wired").small());"#,
        r#"ui.small("Downloading");"#,
        r#"ui.strong("Downloading");"#,
        r#"ui.weak("Downloading");"#,
        r#"ui.colored_label(c, "Downloading");"#,
        r#"ui.collapsing("Details", |ui| {});"#,
        r#"ui.hyperlink_to("Open site", "https://x.y");"#,
        r#"ui.selectable_label(true, "Overview");"#,
        r#"egui::Window::new("Inspector");"#,
        r#"self.toast("Copied".to_string());"#,
        r#"let t = Tile { caption: "Downloading".into() };"#,
        r#"let s = Stat { label: "Wired".into() };"#,
        r#"let d = Dialog { cancel: "Deny".into() };"#,
        r#"let d = Dialog { confirm: "Allow".into() };"#,
        r#"let d = Dialog { title: format!("Quit {}", x) };"#,
        r#"stat("Wired", v);"#,
        r#"stat("st.user", "Value here");"#,
        r#"ui.label(format!("{}", if x { "Plain words" } else { "" }));"#,
    ];
    for code in bad {
        assert!(!untranslated_in(code, &keys).is_empty(), "lint missed: {code}");
    }
    let good = [
        r#"ui.label(self.t("temperature"));"#,
        r#"ui.label(i18n.tf("saved", &[("path", &p)]));"#,
        r#"ui.label(if x { self.t("st.until_full") } else { self.t("st.remaining") });"#,
        r#"stat("st.user", u.cpu(1.0, 8));"#,
        r#"stat(if c { "st.until_full" } else { "st.remaining" }, v);"#,
        r#"ui.label("CPU");"#,
        r#"ui.label(format!("{} / {}", a, b));"#,
        r#"ui.label(i18n.num(format!("{w:.2} W")));"#,
        r#"let s = Stat { label: self.t("st.pace"), value: "—".into() };"#,
    ];
    for code in good {
        let found = untranslated_in(code, &keys);
        assert!(found.is_empty(), "lint flagged translated code {code}: {found:?}");
    }
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

/// `{name}` placeholders in a locale value.
fn placeholders(v: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = v;
    while let Some(a) = rest.find('{') {
        let Some(b) = rest[a..].find('}') else { break };
        out.insert(rest[a + 1..a + b].to_string());
        rest = &rest[a + b + 1..];
    }
    out
}

/// Check every `t("key")`, `tf("key", &[("name", …)])` and `tn("key", n, &[…])` call in `src`
/// that names its key with a literal: the exact key must exist in en.toml (for `tn`, both
/// `.one` and `.other`) and the argument names must be exactly the key's `{placeholders}`
/// (`{n}` is filled by `tn` itself). Calls that build the key or the arguments at run time are
/// not looked at.
fn check_calls(src: &str, en: &std::collections::HashMap<String, String>) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut problems = vec![];
    for (at, name) in t_calls(src) {
        let line = src[..at].matches('\n').count() + 1;
        let char_at = src[..at].chars().count() + name.len();
        let Some((args, _)) = balanced(&chars, char_at) else { continue };
        let parts = split_args(&args);
        let Some(first) = parts.first().map(|a| a.trim()) else { continue };
        let lits = literals(first);
        if lits.len() != 1 || !first.starts_with('"') || !first.ends_with('"') {
            continue;
        }
        let key = &lits[0];
        let forms: Vec<String> = if name == "tn" { vec![format!("{key}.one"), format!("{key}.other")] } else { vec![key.clone()] };
        let Some(values): Option<Vec<&String>> = forms.iter().map(|f| en.get(f)).collect() else {
            problems.push(format!("line {line}: {name}({key:?}) — no such key in en.toml ({})", forms.join(" / ")));
            continue;
        };
        if name == "t" {
            continue;
        }
        let args_pos = if name == "tn" { 2 } else { 1 };
        let Some(arg) = parts.get(args_pos).map(|a| a.trim()) else { continue };
        let Some(list) = arg.strip_prefix("&[").and_then(|a| a.strip_suffix(']')) else { continue };
        let given: BTreeSet<String> = split_args(list).iter().filter_map(|t| literals(t).into_iter().next()).collect();
        let mut wanted: BTreeSet<String> = values.iter().flat_map(|v| placeholders(v)).collect();
        if name == "tn" {
            wanted.remove("n");
        }
        if given != wanted {
            problems.push(format!("line {line}: {name}({key:?}) passes {given:?} but en.toml expects {wanted:?}"));
        }
    }
    problems
}

#[test]
fn i18n_calls_use_exact_keys_and_placeholders() {
    let en: std::collections::HashMap<String, String> = parse(SOURCES[0]).into_iter().collect();
    let mut files = vec![];
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut problems = vec![];
    for f in files {
        if f.ends_with("i18n.rs") {
            continue;
        }
        let src = strip_tests(&strip_comments(&std::fs::read_to_string(&f).unwrap()));
        for p in check_calls(&src, &en) {
            problems.push(format!("{}: {p}", f.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display()));
        }
    }
    assert!(problems.is_empty(), "i18n calls that would show a raw key or a literal {{placeholder}}:\n{}", problems.join("\n"));
}

#[test]
fn the_call_check_catches_typos() {
    let en: std::collections::HashMap<String, String> = parse(SOURCES[0]).into_iter().collect();
    let bad = [
        r#"i18n.t("coppy");"#,
        r#"self.t("tab.nope");"#,
        r#"i18n.tf("saved", &[("pth", &x)]);"#,
        r#"i18n.tf("saved", &[]);"#,
        r#"i18n.tf("saved", &[("path", &x), ("extra", &y)]);"#,
        r#"i18n.tn("n.nope", 3, &[]);"#,
        r#"i18n.tn("idle.frees", n, &[("mem", &m)]);"#,
        r#"i18n.tn("agents.n_started", n, &[("bogus", &m)]);"#,
    ];
    for code in bad {
        assert!(!check_calls(code, &en).is_empty(), "call check missed: {code}");
    }
    let good = [
        r#"i18n.t("copy");"#,
        r#"i18n.tf("saved", &[("path", &x)]);"#,
        r#"i18n.tn("n.processes", 3, &[]);"#,
        r#"i18n.tn("idle.frees", n, &[("mem", &m), ("ports", &p)]);"#,
        r#"i18n.tf("plan.unknown", &args);"#,
        r#"self.t(label);"#,
    ];
    for code in good {
        let found = check_calls(code, &en);
        assert!(found.is_empty(), "call check flagged {code}: {found:?}");
    }
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
