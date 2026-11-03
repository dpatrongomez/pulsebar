//! Keyboard shortcuts of the window: the key → action mapping and the app-list navigation rules
//! are pure functions so they can be unit-tested; `app.rs` only applies the results.

use eframe::egui::{Key, Modifiers};

use crate::settings::Tab;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// ⌘1…⌘9: the n-th visible tab (0-based).
    TabNumber(usize),
    PrevTab,
    NextTab,
    OpenSettings,
    CloseWindow,
    Up,
    Down,
    Expand,
    Collapse,
    Inspect,
    Quit,
    ForceQuit,
}

impl Shortcut {
    /// Holding the key repeats the action (only moving through the list does).
    pub fn repeats(self) -> bool {
        matches!(self, Shortcut::Up | Shortcut::Down)
    }

    /// Plain keys (no ⌘) that act on the app list and must not hijack focus navigation or a
    /// focused button's own Return.
    pub fn is_list_key(self) -> bool {
        matches!(self, Shortcut::Up | Shortcut::Down | Shortcut::Expand | Shortcut::Collapse | Shortcut::Inspect)
    }
}

/// Maps a key press to a shortcut. ⌘ combinations must not carry ⇧ or ⌃ (and ⌥ only for
/// ⌥⌘⌫); the list keys must be pressed without any modifier.
pub fn map(key: Key, m: Modifiers) -> Option<Shortcut> {
    // On macOS ⌃ is its own modifier next to ⌘; elsewhere `command` is ⌃ itself.
    let extra_ctrl = m.ctrl && m.mac_cmd;
    let cmd_only = m.command && !m.alt && !m.shift && !extra_ctrl;
    if cmd_only {
        return match key {
            Key::Num1 => Some(Shortcut::TabNumber(0)),
            Key::Num2 => Some(Shortcut::TabNumber(1)),
            Key::Num3 => Some(Shortcut::TabNumber(2)),
            Key::Num4 => Some(Shortcut::TabNumber(3)),
            Key::Num5 => Some(Shortcut::TabNumber(4)),
            Key::Num6 => Some(Shortcut::TabNumber(5)),
            Key::Num7 => Some(Shortcut::TabNumber(6)),
            Key::Num8 => Some(Shortcut::TabNumber(7)),
            Key::Num9 => Some(Shortcut::TabNumber(8)),
            Key::OpenBracket => Some(Shortcut::PrevTab),
            Key::CloseBracket => Some(Shortcut::NextTab),
            Key::Comma => Some(Shortcut::OpenSettings),
            Key::W => Some(Shortcut::CloseWindow),
            Key::Backspace | Key::Delete => Some(Shortcut::Quit),
            _ => None,
        };
    }
    if m.command && m.alt && !m.shift && !extra_ctrl && matches!(key, Key::Backspace | Key::Delete) {
        return Some(Shortcut::ForceQuit);
    }
    if m.is_none() {
        return match key {
            Key::ArrowUp => Some(Shortcut::Up),
            Key::ArrowDown => Some(Shortcut::Down),
            Key::ArrowRight => Some(Shortcut::Expand),
            Key::ArrowLeft => Some(Shortcut::Collapse),
            Key::Enter => Some(Shortcut::Inspect),
            _ => None,
        };
    }
    None
}

/// The n-th (0-based) visible tab.
pub fn tab_by_number(visible: &[Tab], n: usize) -> Option<Tab> {
    visible.get(n).copied()
}

/// The visible tab `delta` steps from `cur`, wrapping around. If `cur` is not visible (it was
/// hidden in Settings → Tabs) next goes to the first and previous to the last tab.
pub fn cycle_tab(visible: &[Tab], cur: Tab, delta: i32) -> Option<Tab> {
    let n = visible.len() as i32;
    if n == 0 {
        return None;
    }
    let i = match visible.iter().position(|t| *t == cur) {
        Some(i) => (i as i32 + delta).rem_euclid(n),
        None if delta >= 0 => 0,
        None => n - 1,
    };
    Some(visible[i as usize])
}

/// One selectable row of the app list: an app or one of its processes (when expanded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    App(String),
    Proc { app: String, pid: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEffect {
    Select(Row),
    SetExpanded(String, bool),
    Nothing,
}

/// What ↑ ↓ → ← do on `rows` (the rows on screen, top to bottom) with `cur` selected.
/// `is_open` tells whether an app is expanded.
pub fn list_effect(sc: Shortcut, rows: &[Row], cur: Option<&Row>, is_open: &dyn Fn(&str) -> bool) -> ListEffect {
    let pos = cur.and_then(|c| rows.iter().position(|r| r == c));
    let select = |i: usize| rows.get(i).cloned().map(ListEffect::Select).unwrap_or(ListEffect::Nothing);
    match sc {
        Shortcut::Down => match pos {
            Some(i) => select((i + 1).min(rows.len().saturating_sub(1))),
            None => select(0),
        },
        Shortcut::Up => match pos {
            Some(i) => select(i.saturating_sub(1)),
            None => select(rows.len().saturating_sub(1)),
        },
        // → expands a collapsed app; on an expanded one it steps into its first process.
        Shortcut::Expand => match (pos, cur) {
            (Some(_), Some(Row::App(k))) if !is_open(k) => ListEffect::SetExpanded(k.clone(), true),
            (Some(i), Some(Row::App(k))) => match rows.get(i + 1) {
                Some(next @ Row::Proc { app, .. }) if app == k => ListEffect::Select(next.clone()),
                _ => ListEffect::Nothing,
            },
            (None, _) => select(0),
            _ => ListEffect::Nothing,
        },
        // ← collapses an expanded app; on a process it goes back to the app.
        Shortcut::Collapse => match cur {
            Some(Row::App(k)) if pos.is_some() && is_open(k) => ListEffect::SetExpanded(k.clone(), false),
            Some(Row::Proc { app, .. }) if pos.is_some() => ListEffect::Select(Row::App(app.clone())),
            _ => ListEffect::Nothing,
        },
        _ => ListEffect::Nothing,
    }
}

/// Rows shown in Settings → Keyboard Shortcuts: (keys, i18n key of the description).
pub const HELP: &[(&str, &str)] = &[
    ("⌘1 – ⌘9", "kbd.tab_n"),
    ("⌘[   ⌘]", "kbd.tab_step"),
    ("⌘,", "kbd.settings"),
    ("⌘W", "kbd.close"),
    ("↑  ↓", "kbd.move"),
    ("→  ←", "kbd.expand"),
    ("↩", "kbd.inspect"),
    ("⌘⌫", "kbd.quit"),
    ("⌥⌘⌫", "kbd.force_quit"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> Modifiers {
        Modifiers { command: true, mac_cmd: true, ..Default::default() }
    }

    #[test]
    fn maps_command_keys() {
        assert_eq!(map(Key::Num1, cmd()), Some(Shortcut::TabNumber(0)));
        assert_eq!(map(Key::Num9, cmd()), Some(Shortcut::TabNumber(8)));
        assert_eq!(map(Key::OpenBracket, cmd()), Some(Shortcut::PrevTab));
        assert_eq!(map(Key::CloseBracket, cmd()), Some(Shortcut::NextTab));
        assert_eq!(map(Key::Comma, cmd()), Some(Shortcut::OpenSettings));
        assert_eq!(map(Key::W, cmd()), Some(Shortcut::CloseWindow));
        assert_eq!(map(Key::Backspace, cmd()), Some(Shortcut::Quit));
        assert_eq!(map(Key::Backspace, Modifiers { alt: true, ..cmd() }), Some(Shortcut::ForceQuit));
        assert_eq!(map(Key::Num0, cmd()), None);
    }

    #[test]
    fn maps_list_keys_only_without_modifiers() {
        let none = Modifiers::NONE;
        assert_eq!(map(Key::ArrowUp, none), Some(Shortcut::Up));
        assert_eq!(map(Key::ArrowDown, none), Some(Shortcut::Down));
        assert_eq!(map(Key::ArrowRight, none), Some(Shortcut::Expand));
        assert_eq!(map(Key::ArrowLeft, none), Some(Shortcut::Collapse));
        assert_eq!(map(Key::Enter, none), Some(Shortcut::Inspect));
        assert_eq!(map(Key::ArrowDown, Modifiers::SHIFT), None);
        assert_eq!(map(Key::Enter, cmd()), None);
        assert_eq!(map(Key::Backspace, none), None);
    }

    #[test]
    fn rejects_extra_modifiers() {
        assert_eq!(map(Key::Num1, Modifiers { shift: true, ..cmd() }), None);
        assert_eq!(map(Key::W, Modifiers { alt: true, ..cmd() }), None);
        assert_eq!(map(Key::Backspace, Modifiers { shift: true, ..cmd() }), None);
        assert_eq!(map(Key::Num1, Modifiers { ctrl: true, ..cmd() }), None);
        assert_eq!(map(Key::Num1, Modifiers { ctrl: true, ..Default::default() }), None);
    }

    #[test]
    fn tab_numbers_follow_visible_order() {
        let vis = [Tab::Overview, Tab::Memory, Tab::Settings];
        assert_eq!(tab_by_number(&vis, 1), Some(Tab::Memory));
        assert_eq!(tab_by_number(&vis, 3), None);
    }

    #[test]
    fn tabs_cycle_and_wrap() {
        let vis = [Tab::Overview, Tab::Memory, Tab::Settings];
        assert_eq!(cycle_tab(&vis, Tab::Overview, 1), Some(Tab::Memory));
        assert_eq!(cycle_tab(&vis, Tab::Settings, 1), Some(Tab::Overview));
        assert_eq!(cycle_tab(&vis, Tab::Overview, -1), Some(Tab::Settings));
        assert_eq!(cycle_tab(&vis, Tab::Cpu, 1), Some(Tab::Overview));
        assert_eq!(cycle_tab(&vis, Tab::Cpu, -1), Some(Tab::Settings));
        assert_eq!(cycle_tab(&[], Tab::Cpu, 1), None);
    }

    fn app(k: &str) -> Row {
        Row::App(k.into())
    }
    fn proc(k: &str, pid: i32) -> Row {
        Row::Proc { app: k.into(), pid }
    }

    #[test]
    fn up_down_move_and_clamp() {
        let rows = [app("a"), app("b"), app("c")];
        let closed = |_: &str| false;
        assert_eq!(list_effect(Shortcut::Down, &rows, None, &closed), ListEffect::Select(app("a")));
        assert_eq!(list_effect(Shortcut::Up, &rows, None, &closed), ListEffect::Select(app("c")));
        assert_eq!(list_effect(Shortcut::Down, &rows, Some(&app("a")), &closed), ListEffect::Select(app("b")));
        assert_eq!(list_effect(Shortcut::Down, &rows, Some(&app("c")), &closed), ListEffect::Select(app("c")));
        assert_eq!(list_effect(Shortcut::Up, &rows, Some(&app("a")), &closed), ListEffect::Select(app("a")));
        assert_eq!(list_effect(Shortcut::Down, &[], None, &closed), ListEffect::Nothing);
        // A selection that is no longer on screen starts over from the top.
        assert_eq!(list_effect(Shortcut::Down, &rows, Some(&app("gone")), &closed), ListEffect::Select(app("a")));
    }

    #[test]
    fn down_steps_into_expanded_processes() {
        let rows = [app("a"), proc("a", 1), proc("a", 2), app("b")];
        let open = |k: &str| k == "a";
        assert_eq!(list_effect(Shortcut::Down, &rows, Some(&app("a")), &open), ListEffect::Select(proc("a", 1)));
        assert_eq!(list_effect(Shortcut::Down, &rows, Some(&proc("a", 2)), &open), ListEffect::Select(app("b")));
        assert_eq!(list_effect(Shortcut::Up, &rows, Some(&app("b")), &open), ListEffect::Select(proc("a", 2)));
    }

    #[test]
    fn right_left_expand_collapse() {
        let rows = [app("a"), proc("a", 1), app("b")];
        let open = |k: &str| k == "a";
        assert_eq!(list_effect(Shortcut::Expand, &rows, Some(&app("b")), &open), ListEffect::SetExpanded("b".into(), true));
        assert_eq!(list_effect(Shortcut::Expand, &rows, Some(&app("a")), &open), ListEffect::Select(proc("a", 1)));
        assert_eq!(list_effect(Shortcut::Expand, &rows, Some(&proc("a", 1)), &open), ListEffect::Nothing);
        assert_eq!(list_effect(Shortcut::Collapse, &rows, Some(&app("a")), &open), ListEffect::SetExpanded("a".into(), false));
        assert_eq!(list_effect(Shortcut::Collapse, &rows, Some(&app("b")), &open), ListEffect::Nothing);
        assert_eq!(list_effect(Shortcut::Collapse, &rows, Some(&proc("a", 1)), &open), ListEffect::Select(app("a")));
    }

    #[test]
    fn expanded_app_with_no_processes_shown_stays_put() {
        let rows = [app("a"), app("b")];
        assert_eq!(list_effect(Shortcut::Expand, &rows, Some(&app("a")), &|_| true), ListEffect::Nothing);
    }
}
