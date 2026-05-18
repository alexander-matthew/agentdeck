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
    None,
}

pub fn map_deck_key(ev: KeyEvent) -> Action {
    if ev.kind != KeyEventKind::Press {
        return Action::None;
    }

    if ev.code == KeyCode::F(1) {
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
        
        _ => Action::None,
    }
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
