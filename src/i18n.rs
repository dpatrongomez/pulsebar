//! Localisation. English and Spanish are complete; the other languages are partial and fall
//! back to English key by key. Strings live in `locales/<lang>.toml` (a flat `key = "value"`
//! subset of TOML, embedded with `include_str!`), so adding a string never means editing code
//! tables. Plurals use `key.one` / `key.other` pairs (see [`I18n::tn`]).
//!
//! CLI and MCP output deliberately never go through here: scripts and golden tests depend on it
//! staying English.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Every language that has a locale file. Index 0 is the fallback.
pub const LANGS: [&str; 12] = ["en", "es", "de", "fr", "it", "nl", "pt-BR", "sv", "uk", "ja", "ko", "zh"];

/// Languages offered in the picker: complete translations only.
pub const COMPLETE: [&str; 2] = ["en", "es"];

/// (code, name in its own language) for the translations that are still partial. Shown in the
/// picker, marked "partial", only when one of them is already selected in `settings.json`.
pub const PARTIAL: [(&str, &str); 10] = [
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("pt-BR", "Português (Brasil)"),
    ("sv", "Svenska"),
    ("uk", "Українська"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("zh", "中文"),
];

/// The locale sources, in `LANGS` order.
pub const SOURCES: [&str; 12] = [
    include_str!("../locales/en.toml"),
    include_str!("../locales/es.toml"),
    include_str!("../locales/de.toml"),
    include_str!("../locales/fr.toml"),
    include_str!("../locales/it.toml"),
    include_str!("../locales/nl.toml"),
    include_str!("../locales/pt-BR.toml"),
    include_str!("../locales/sv.toml"),
    include_str!("../locales/uk.toml"),
    include_str!("../locales/ja.toml"),
    include_str!("../locales/ko.toml"),
    include_str!("../locales/zh.toml"),
];

/// Parse the `key = "value"` subset of TOML used by the locale files: `#` comments, blank lines,
/// double-quoted values with `\"`, `\\`, `\n` and `\t` escapes. Returns the entries in file order.
pub fn parse(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let Some(inner) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else { continue };
        let mut s = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                s.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => s.push('\n'),
                Some('t') => s.push('\t'),
                Some(o) => s.push(o),
                None => {}
            }
        }
        out.push((k.trim().to_string(), s));
    }
    out
}

fn tables() -> &'static Vec<HashMap<String, String>> {
    static T: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(|| SOURCES.iter().map(|s| parse(s).into_iter().collect()).collect())
}

fn index_of(lang: &str) -> usize {
    let l = lang.replace('_', "-");
    if let Some(i) = LANGS.iter().position(|x| x.eq_ignore_ascii_case(&l)) {
        return i;
    }
    let base = l.split('-').next().unwrap_or("en");
    match base {
        "pt" => 6,
        "zh" => 11,
        _ => LANGS.iter().position(|x| x.split('-').next() == Some(base)).unwrap_or(0),
    }
}

/// The macOS UI language (first of AppleLanguages), falling back to $LANG. Read once: macOS
/// applies a language change to an app only after it relaunches anyway.
pub fn system_language() -> String {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        #[cfg(target_os = "macos")]
        if let Ok(out) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output()
            && let Some(first) = first_language(&String::from_utf8_lossy(&out.stdout))
        {
            return first.to_string();
        }
        std::env::var("LANG").ok().and_then(|l| l.split('.').next().map(str::to_string)).unwrap_or_else(|| "en".into())
    })
    .clone()
}

/// The first language in the old-style plist `defaults read -g AppleLanguages` prints.
/// `defaults` only quotes values that contain a dash, so `(en, "es-ES")` and `(es)` both occur.
fn first_language(out: &str) -> Option<&str> {
    out.lines().map(|l| l.trim().trim_end_matches(',').trim_matches('"')).find(|l| !l.is_empty() && *l != "(" && *l != ")")
}

/// "System" maps to Spanish when the system prefers `es-*`, English otherwise.
fn system_index(system: &str) -> usize {
    if system.replace('_', "-").to_ascii_lowercase().starts_with("es") { 1 } else { 0 }
}

#[derive(Debug, Clone)]
pub struct I18n {
    idx: usize,
}

impl I18n {
    /// `None` follows the system language.
    pub fn new(lang: Option<&str>) -> Self {
        match lang {
            Some(l) => Self { idx: index_of(l) },
            None => Self::from_system(&system_language()),
        }
    }

    /// What "System" resolves to for a given macOS language tag.
    pub fn from_system(system: &str) -> Self {
        Self { idx: system_index(system) }
    }

    /// Always English: CLI and MCP output.
    pub fn english() -> Self {
        Self { idx: 0 }
    }

    pub fn lang(&self) -> &'static str {
        LANGS[self.idx]
    }

    /// Spanish writes the decimal separator as a comma ("3,07 GB").
    pub fn decimal_comma(&self) -> bool {
        matches!(self.lang(), "es" | "de" | "fr" | "it" | "nl" | "pt-BR" | "sv" | "uk")
    }

    /// Localise the decimal separator of an already formatted number.
    pub fn num(&self, s: impl Into<String>) -> String {
        let s = s.into();
        if self.decimal_comma() { s.replace('.', ",") } else { s }
    }

    fn lookup(&self, key: &str) -> Option<&'static str> {
        let t = tables();
        t[self.idx].get(key).or_else(|| t[0].get(key)).map(String::as_str)
    }

    /// The string for `key` (English if this language lacks it, the key itself if nobody has it).
    pub fn t(&self, key: &str) -> String {
        self.lookup(key).map(str::to_string).unwrap_or_else(|| key.to_string())
    }

    /// `t` with `{name}` placeholders replaced.
    pub fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut s = self.t(key);
        for (k, v) in args {
            s = s.replace(&format!("{{{k}}}"), v);
        }
        s
    }

    /// Plural form: `key.one` when `n == 1`, else `key.other`. `{n}` is filled with `n`.
    pub fn tn(&self, key: &str, n: usize, args: &[(&str, &str)]) -> String {
        let k = format!("{key}.{}", if n == 1 { "one" } else { "other" });
        let n = n.to_string();
        let mut all: Vec<(&str, &str)> = vec![("n", &n)];
        all.extend_from_slice(args);
        self.tf(&k, &all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(i: usize) -> BTreeSet<String> {
        parse(SOURCES[i]).into_iter().map(|(k, _)| k).collect()
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(a) = rest.find('{') {
            let Some(b) = rest[a..].find('}') else { break };
            out.insert(rest[a + 1..a + b].to_string());
            rest = &rest[a + b + 1..];
        }
        out
    }

    #[test]
    fn lookups() {
        assert_eq!(I18n::new(Some("es-ES")).t("tab.projects"), "Proyectos");
        assert_eq!(I18n::new(Some("pt-PT")).t("now"), "Agora");
        assert_eq!(I18n::new(Some("zh-Hans-CN")).t("tab.memory"), "内存");
        assert_eq!(I18n::new(Some("xx")).t("peak"), "Peak");
        assert_eq!(I18n::new(Some("en")).t("missing.key"), "missing.key");
        assert_eq!(I18n::new(Some("es")).t("detail.open"), "Detalles");
        assert_eq!(I18n::new(Some("de")).t("detail.open"), "Details");
    }

    #[test]
    fn first_language_parses_defaults_output() {
        // Multi-line, as `defaults read -g AppleLanguages` prints it.
        assert_eq!(first_language("(\n    en,\n    \"es-ES\"\n)\n"), Some("en"));
        assert_eq!(first_language("(\n    \"es-ES\",\n    en\n)\n"), Some("es-ES"));
        assert_eq!(first_language("(\n    es\n)\n"), Some("es"));
        assert_eq!(first_language("(\n)\n"), None);
        assert_eq!(first_language(""), None);
    }

    /// Every non-blank, non-comment line of every locale file must parse (a line such as
    /// `key = "x" # note` is otherwise dropped silently), and no key may appear twice.
    #[test]
    fn every_locale_line_parses_without_duplicates() {
        for (i, src) in SOURCES.iter().enumerate() {
            let lines = src.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).count();
            let rows = parse(src);
            assert_eq!(rows.len(), lines, "{}.toml has a malformed line (expected `key = \"value\"`)", LANGS[i]);
            assert_eq!(keys(i).len(), rows.len(), "{}.toml has duplicate keys", LANGS[i]);
        }
    }

    #[test]
    fn system_follows_spanish_only() {
        assert_eq!(I18n::from_system("es-419").lang(), "es");
        assert_eq!(I18n::from_system("es_ES").lang(), "es");
        assert_eq!(I18n::from_system("de-DE").lang(), "en");
        assert_eq!(I18n::from_system("en-US").lang(), "en");
    }

    /// Every key exists in both English and Spanish, with no duplicates, no empty values and
    /// the same `{placeholders}` in both.
    #[test]
    fn english_and_spanish_are_complete() {
        let (en, es) = (keys(0), keys(1));
        let only_en: Vec<_> = en.difference(&es).collect();
        let only_es: Vec<_> = es.difference(&en).collect();
        assert!(only_en.is_empty(), "keys missing in es.toml: {only_en:?}");
        assert!(only_es.is_empty(), "keys missing in en.toml: {only_es:?}");
        for i in 0..2 {
            let rows = parse(SOURCES[i]);
            assert_eq!(rows.len(), keys(i).len(), "duplicate keys in {}.toml", LANGS[i]);
            assert!(rows.iter().all(|(k, v)| !v.is_empty() || k.ends_with(".none")), "empty value in {}.toml", LANGS[i]);
        }
        let (en_map, es_map) = (&tables()[0], &tables()[1]);
        for (k, v) in en_map {
            assert_eq!(placeholders(v), placeholders(&es_map[k]), "placeholders differ for {k}");
        }
    }

    /// Plural keys come in `.one` / `.other` pairs.
    #[test]
    fn plurals_are_paired() {
        for i in 0..2 {
            let ks = keys(i);
            for k in &ks {
                if let Some(base) = k.strip_suffix(".one") {
                    assert!(ks.contains(&format!("{base}.other")), "{base}.other missing in {}.toml", LANGS[i]);
                }
                if let Some(base) = k.strip_suffix(".other") {
                    assert!(ks.contains(&format!("{base}.one")), "{base}.one missing in {}.toml", LANGS[i]);
                }
            }
        }
    }

    /// Partial languages may only define keys that exist in English.
    #[test]
    fn partial_languages_have_no_stray_keys() {
        let en = keys(0);
        for i in 2..LANGS.len() {
            let extra: Vec<_> = keys(i).difference(&en).cloned().collect();
            assert!(extra.is_empty(), "{}.toml has keys English lacks: {extra:?}", LANGS[i]);
        }
    }

    #[test]
    fn plurals_and_decimal_comma() {
        let es = I18n::new(Some("es"));
        let en = I18n::new(Some("en"));
        assert_eq!(es.tn("n.processes", 1, &[]), "1 proceso");
        assert_eq!(es.tn("n.processes", 3, &[]), "3 procesos");
        assert_eq!(en.tn("n.processes", 1, &[]), "1 process");
        assert_eq!(en.tn("n.processes", 0, &[]), "0 processes");
        assert_eq!(es.num("3.07 GB"), "3,07 GB");
        assert_eq!(en.num("3.07 GB"), "3.07 GB");
    }

    #[test]
    fn parser_handles_escapes_and_comments() {
        let rows = parse("# c\n\na.b = \"x \\\"q\\\" y\\nz\"\nbad line\nc = \"é…\"\n");
        assert_eq!(rows, vec![("a.b".to_string(), "x \"q\" y\nz".to_string()), ("c".to_string(), "é…".to_string())]);
    }
}
