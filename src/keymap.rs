//! Key mapping and serialization for agentdeck.
//!
//! Includes:
//! 1. `Action` enum for high-level UI actions in "deck" (sidebar) mode.
//! 2. `map_deck_key` to translate crossterm events to `Action`.
//! 3. `key_event_to_bytes` to serialize crossterm events to byte sequences
//!    for forwarding to child PTYs in "agent" mode.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    MoveUp,
    MoveDown,
    FocusAgent,
    FocusIndex(usize),
    AddAgent,
    RemoveAgent,
    ToggleFocus,
    CycleSort,
    /// Cycle between single-pane and multi-pane grid view.
    ToggleView,
    /// Show / hide the centralized usage dashboard.
    ToggleUsage,
    /// Show / hide the keybindings help overlay.
    ToggleHelp,
    FocusNextWaiting,
    RenameAgent,
    None,
}

pub fn map_deck_key(ev: KeyEvent, toggle_key: Option<KeyEvent>) -> Action {
    if ev.kind != KeyEventKind::Press {
        return Action::None;
    }

    // Help-overlay shortcuts win over a user-configured toggle_key. Otherwise
    // a user who bound `toggle_key = "f1"` could never reach the help modal.
    if matches!(ev.code, KeyCode::Char('?') | KeyCode::F(1)) {
        return Action::ToggleHelp;
    }

    if let Some(tk) = toggle_key {
        if ev.code == tk.code && ev.modifiers == tk.modifiers {
            return Action::ToggleFocus;
        }
    } else if ev.code == KeyCode::Char(' ') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        // Fallback when [settings].toggle_key in config didn't parse.
        return Action::ToggleFocus;
    }

    match ev.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,

        KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
        KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,

        KeyCode::Enter => Action::FocusAgent,
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            let i = (c as u8 - b'1') as usize;
            Action::FocusIndex(i)
        }

        KeyCode::Char('a') | KeyCode::Char('+') => Action::AddAgent,
        KeyCode::Char('x') => Action::RemoveAgent,
        KeyCode::Char('r') => Action::RenameAgent,
        KeyCode::Char('o') => Action::CycleSort,
        KeyCode::Char('g') => Action::ToggleView,
        KeyCode::Char('u') => Action::ToggleUsage,
        KeyCode::Char('?') | KeyCode::F(1) => Action::ToggleHelp,
        KeyCode::Tab => Action::FocusNextWaiting,

        _ => Action::None,
    }
}

pub fn parse_key(s: &str) -> Option<KeyEvent> {
    let s = s.to_lowercase();
    let parts: Vec<&str> = s.split('-').collect();

    let mut mods = KeyModifiers::empty();
    let code_str = if parts.len() > 1 {
        for &p in &parts[..parts.len() - 1] {
            match p {
                "ctrl" | "control" => mods.insert(KeyModifiers::CONTROL),
                "alt" | "opt" | "option" => mods.insert(KeyModifiers::ALT),
                "shift" => mods.insert(KeyModifiers::SHIFT),
                "super" | "cmd" | "command" => mods.insert(KeyModifiers::SUPER),
                _ => {}
            }
        }
        parts[parts.len() - 1]
    } else {
        parts[0]
    };

    let code = match code_str {
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        c if c.len() == 1 => KeyCode::Char(c.chars().next().unwrap()),
        _ => return None,
    };

    Some(KeyEvent::new(code, mods))
}

pub fn key_event_to_bytes(ev: &KeyEvent) -> Option<Vec<u8>> {
    let mods = ev.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);

    let bytes = match ev.code {
        KeyCode::Char(c) => return Some(encode_char(c, ctrl, alt, shift)),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => arrow_or_modified(b'D', mods),
        KeyCode::Right => arrow_or_modified(b'C', mods),
        KeyCode::Up => arrow_or_modified(b'A', mods),
        KeyCode::Down => arrow_or_modified(b'B', mods),
        KeyCode::Home => csi_or_modified(b'H', mods),
        KeyCode::End => csi_or_modified(b'F', mods),
        KeyCode::PageUp => csi_tilde(b'5', mods),
        KeyCode::PageDown => csi_tilde(b'6', mods),
        KeyCode::Delete => csi_tilde(b'3', mods),
        KeyCode::Insert => csi_tilde(b'2', mods),
        KeyCode::F(n) => function_key(n)?,
        // Less common keys we don't bother forwarding (CapsLock, PrintScreen, etc).
        _ => return None,
    };
    Some(bytes)
}

fn encode_char(c: char, ctrl: bool, alt: bool, _shift: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(8);
    if alt {
        out.push(0x1b);
    }
    if ctrl {
        // Ctrl-<letter> -> 0x01..=0x1a; common non-letter ctrl pairs too.
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            out.push(lower as u8 - b'a' + 1);
            return out;
        }
        match c {
            ' ' | '@' => out.push(0x00),
            '[' => out.push(0x1b),
            '\\' => out.push(0x1c),
            ']' => out.push(0x1d),
            '^' => out.push(0x1e),
            '_' | '?' => out.push(0x1f),
            // Fall through: send the literal char.
            other => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
        return out;
    }

    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    out
}

fn arrow_or_modified(letter: u8, mods: KeyModifiers) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        vec![0x1b, b'[', letter]
    } else {
        let mod_str = format!("{m}");
        let mut out = vec![0x1b, b'[', b'1', b';'];
        out.extend_from_slice(mod_str.as_bytes());
        out.push(letter);
        out
    }
}

fn csi_or_modified(letter: u8, mods: KeyModifiers) -> Vec<u8> {
    arrow_or_modified(letter, mods)
}

fn csi_tilde(n: u8, mods: KeyModifiers) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        vec![0x1b, b'[', n, b'~']
    } else {
        let mod_str = format!("{m}");
        let mut out = vec![0x1b, b'[', n, b';'];
        out.extend_from_slice(mod_str.as_bytes());
        out.push(b'~');
        out
    }
}

fn modifier_code(mods: KeyModifiers) -> u8 {
    let mut m = 0u8;
    if mods.contains(KeyModifiers::SHIFT) {
        m |= 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m |= 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m |= 4;
    }
    1 + m
}

fn function_key(n: u8) -> Option<Vec<u8>> {
    Some(match n {
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn km(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    // ---- map_deck_key ------------------------------------------------------

    #[test]
    fn map_deck_key_q_quits() {
        assert_eq!(map_deck_key(k(KeyCode::Char('q')), None), Action::Quit);
    }

    #[test]
    fn map_deck_key_ctrl_c_quits() {
        let ev = km(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(map_deck_key(ev, None), Action::Quit);
    }

    #[test]
    fn map_deck_key_arrow_up_and_k_move_up() {
        assert_eq!(map_deck_key(k(KeyCode::Up), None), Action::MoveUp);
        assert_eq!(map_deck_key(k(KeyCode::Char('k')), None), Action::MoveUp);
    }

    #[test]
    fn map_deck_key_arrow_down_and_j_move_down() {
        assert_eq!(map_deck_key(k(KeyCode::Down), None), Action::MoveDown);
        assert_eq!(map_deck_key(k(KeyCode::Char('j')), None), Action::MoveDown);
    }

    #[test]
    fn map_deck_key_enter_focuses_agent() {
        assert_eq!(map_deck_key(k(KeyCode::Enter), None), Action::FocusAgent);
    }

    #[test]
    fn map_deck_key_digits_1_through_9_focus_index() {
        for (digit, idx) in ('1'..='9').zip(0usize..) {
            assert_eq!(
                map_deck_key(k(KeyCode::Char(digit)), None),
                Action::FocusIndex(idx),
                "digit {digit}",
            );
        }
    }

    #[test]
    fn map_deck_key_zero_is_not_an_index() {
        // '0' is not a focus shortcut; falls through to None.
        assert_eq!(map_deck_key(k(KeyCode::Char('0')), None), Action::None);
    }

    #[test]
    fn map_deck_key_a_and_plus_add_agent() {
        assert_eq!(map_deck_key(k(KeyCode::Char('a')), None), Action::AddAgent);
        assert_eq!(map_deck_key(k(KeyCode::Char('+')), None), Action::AddAgent);
    }

    #[test]
    fn map_deck_key_x_removes_agent() {
        assert_eq!(
            map_deck_key(k(KeyCode::Char('x')), None),
            Action::RemoveAgent
        );
    }

    #[test]
    fn map_deck_key_r_renames_agent() {
        assert_eq!(
            map_deck_key(k(KeyCode::Char('r')), None),
            Action::RenameAgent
        );
    }

    #[test]
    fn map_deck_key_o_cycles_sort() {
        assert_eq!(map_deck_key(k(KeyCode::Char('o')), None), Action::CycleSort);
    }

    #[test]
    fn map_deck_key_g_toggles_view() {
        assert_eq!(
            map_deck_key(k(KeyCode::Char('g')), None),
            Action::ToggleView
        );
    }

    #[test]
    fn map_deck_key_u_toggles_usage() {
        assert_eq!(
            map_deck_key(k(KeyCode::Char('u')), None),
            Action::ToggleUsage
        );
    }

    #[test]
    fn map_deck_key_question_mark_and_f1_toggle_help() {
        assert_eq!(
            map_deck_key(k(KeyCode::Char('?')), None),
            Action::ToggleHelp
        );
        assert_eq!(map_deck_key(k(KeyCode::F(1)), None), Action::ToggleHelp);
    }

    #[test]
    fn map_deck_key_tab_focuses_next_waiting() {
        assert_eq!(
            map_deck_key(k(KeyCode::Tab), None),
            Action::FocusNextWaiting
        );
    }

    #[test]
    fn map_deck_key_ignores_release_events() {
        let mut ev = k(KeyCode::Char('q'));
        ev.kind = KeyEventKind::Release;
        assert_eq!(map_deck_key(ev, None), Action::None);
    }

    #[test]
    fn map_deck_key_unbound_key_returns_none() {
        assert_eq!(map_deck_key(k(KeyCode::Char('z')), None), Action::None);
    }

    #[test]
    fn map_deck_key_ctrl_space_fallback_toggles_focus_when_no_override() {
        let ev = km(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert_eq!(map_deck_key(ev, None), Action::ToggleFocus);
    }

    #[test]
    fn map_deck_key_custom_toggle_key_overrides_ctrl_space() {
        // With a custom toggle key configured, Ctrl-Space no longer toggles
        // and the configured key takes its place.
        let toggle = Some(km(KeyCode::F(2), KeyModifiers::empty()));
        assert_eq!(
            map_deck_key(km(KeyCode::Char(' '), KeyModifiers::CONTROL), toggle),
            Action::None,
        );
        assert_eq!(map_deck_key(k(KeyCode::F(2)), toggle), Action::ToggleFocus,);
    }

    #[test]
    fn map_deck_key_help_keys_win_over_custom_toggle_key() {
        // F1 and ? must always open the help overlay, even if the user has
        // bound them as their focus-toggle key — otherwise the help modal
        // becomes unreachable.
        let toggle = Some(km(KeyCode::F(1), KeyModifiers::empty()));
        assert_eq!(map_deck_key(k(KeyCode::F(1)), toggle), Action::ToggleHelp);

        let toggle = Some(km(KeyCode::Char('?'), KeyModifiers::empty()));
        assert_eq!(
            map_deck_key(k(KeyCode::Char('?')), toggle),
            Action::ToggleHelp
        );
    }

    // ---- parse_key ---------------------------------------------------------

    #[test]
    fn parse_key_bare_letter() {
        let ev = parse_key("j").unwrap();
        assert_eq!(ev.code, KeyCode::Char('j'));
        assert_eq!(ev.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn parse_key_named_keys() {
        assert_eq!(parse_key("enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_key("tab").unwrap().code, KeyCode::Tab);
        assert_eq!(parse_key("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(parse_key("escape").unwrap().code, KeyCode::Esc);
        assert_eq!(parse_key("space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(parse_key("up").unwrap().code, KeyCode::Up);
        assert_eq!(parse_key("down").unwrap().code, KeyCode::Down);
        assert_eq!(parse_key("left").unwrap().code, KeyCode::Left);
        assert_eq!(parse_key("right").unwrap().code, KeyCode::Right);
    }

    #[test]
    fn parse_key_function_keys_f1_through_f12() {
        for n in 1u8..=12 {
            let s = format!("f{n}");
            let ev = parse_key(&s).unwrap_or_else(|| panic!("failed to parse {s}"));
            assert_eq!(ev.code, KeyCode::F(n));
            assert_eq!(ev.modifiers, KeyModifiers::empty());
        }
    }

    #[test]
    fn parse_key_ctrl_space_chord() {
        let ev = parse_key("ctrl-space").unwrap();
        assert_eq!(ev.code, KeyCode::Char(' '));
        assert_eq!(ev.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_key_alt_letter_chord() {
        let ev = parse_key("alt-d").unwrap();
        assert_eq!(ev.code, KeyCode::Char('d'));
        assert_eq!(ev.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parse_key_ctrl_shift_letter_chord() {
        let ev = parse_key("ctrl-shift-p").unwrap();
        assert_eq!(ev.code, KeyCode::Char('p'));
        assert_eq!(ev.modifiers, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_key_is_case_insensitive() {
        let ev = parse_key("CTRL-Space").unwrap();
        assert_eq!(ev.code, KeyCode::Char(' '));
        assert_eq!(ev.modifiers, KeyModifiers::CONTROL);

        let ev = parse_key("ENTER").unwrap();
        assert_eq!(ev.code, KeyCode::Enter);

        let ev = parse_key("Alt-D").unwrap();
        assert_eq!(ev.code, KeyCode::Char('d'));
        assert_eq!(ev.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parse_key_super_and_cmd_aliases_map_to_super() {
        assert_eq!(parse_key("super-k").unwrap().modifiers, KeyModifiers::SUPER);
        assert_eq!(parse_key("cmd-k").unwrap().modifiers, KeyModifiers::SUPER);
        assert_eq!(
            parse_key("command-k").unwrap().modifiers,
            KeyModifiers::SUPER
        );
    }

    #[test]
    fn parse_key_returns_none_for_empty_string() {
        assert!(parse_key("").is_none());
    }

    #[test]
    fn parse_key_returns_none_for_unknown_bare_token() {
        assert!(parse_key("banana").is_none());
    }

    #[test]
    fn parse_key_returns_none_for_unknown_modified_token() {
        assert!(parse_key("ctrl-banana").is_none());
    }

    // ---- key_event_to_bytes -----------------------------------------------

    #[test]
    fn key_event_to_bytes_plain_ascii_char_round_trips() {
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Char('a'))),
            Some(b"a".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Char('Z'))),
            Some(b"Z".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Char('5'))),
            Some(b"5".to_vec())
        );
    }

    #[test]
    fn key_event_to_bytes_ctrl_letters_map_to_control_codes() {
        for (i, c) in ('a'..='z').enumerate() {
            let ev = km(KeyCode::Char(c), KeyModifiers::CONTROL);
            let expected = (i as u8) + 1;
            assert_eq!(
                key_event_to_bytes(&ev),
                Some(vec![expected]),
                "ctrl-{c} should be 0x{expected:02x}",
            );
        }
    }

    #[test]
    fn key_event_to_bytes_ctrl_space_is_null() {
        let ev = km(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert_eq!(key_event_to_bytes(&ev), Some(vec![0x00]));
    }

    #[test]
    fn key_event_to_bytes_alt_letter_prepends_escape() {
        let ev = km(KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(key_event_to_bytes(&ev), Some(vec![0x1b, b'd']));
    }

    #[test]
    fn key_event_to_bytes_enter_and_tab_and_backspace() {
        assert_eq!(key_event_to_bytes(&k(KeyCode::Enter)), Some(vec![b'\r']));
        assert_eq!(key_event_to_bytes(&k(KeyCode::Tab)), Some(vec![b'\t']));
        assert_eq!(key_event_to_bytes(&k(KeyCode::Backspace)), Some(vec![0x7f]));
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::BackTab)),
            Some(vec![0x1b, b'[', b'Z'])
        );
        assert_eq!(key_event_to_bytes(&k(KeyCode::Esc)), Some(vec![0x1b]));
    }

    #[test]
    fn key_event_to_bytes_arrows_with_no_modifiers() {
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Up)),
            Some(vec![0x1b, b'[', b'A'])
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Down)),
            Some(vec![0x1b, b'[', b'B'])
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Right)),
            Some(vec![0x1b, b'[', b'C'])
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Left)),
            Some(vec![0x1b, b'[', b'D'])
        );
    }

    #[test]
    fn key_event_to_bytes_shift_arrow_uses_modifier_code_2() {
        let ev = km(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(key_event_to_bytes(&ev), Some(b"\x1b[1;2A".to_vec()));
    }

    #[test]
    fn key_event_to_bytes_ctrl_shift_left_uses_modifier_code_6() {
        // shift(1) | ctrl(4) = 5; reported value is 1 + bits = 6.
        let ev = km(KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        assert_eq!(key_event_to_bytes(&ev), Some(b"\x1b[1;6D".to_vec()));
    }

    #[test]
    fn key_event_to_bytes_home_and_end_use_csi() {
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Home)),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::End)),
            Some(b"\x1b[F".to_vec())
        );
    }

    #[test]
    fn key_event_to_bytes_csi_tilde_keys() {
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::PageUp)),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::PageDown)),
            Some(b"\x1b[6~".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Delete)),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(&k(KeyCode::Insert)),
            Some(b"\x1b[2~".to_vec())
        );
    }

    #[test]
    fn key_event_to_bytes_function_keys_documented_sequences() {
        let cases: &[(u8, &[u8])] = &[
            (2, b"\x1bOQ"),
            (3, b"\x1bOR"),
            (4, b"\x1bOS"),
            (5, b"\x1b[15~"),
            (6, b"\x1b[17~"),
            (7, b"\x1b[18~"),
            (8, b"\x1b[19~"),
            (9, b"\x1b[20~"),
            (10, b"\x1b[21~"),
            (11, b"\x1b[23~"),
            (12, b"\x1b[24~"),
        ];
        for &(n, expected) in cases {
            assert_eq!(
                key_event_to_bytes(&k(KeyCode::F(n))),
                Some(expected.to_vec()),
                "F{n}",
            );
        }
    }

    #[test]
    fn key_event_to_bytes_unsupported_function_key_is_none() {
        assert_eq!(key_event_to_bytes(&k(KeyCode::F(13))), None);
    }
}
