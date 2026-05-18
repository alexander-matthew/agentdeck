//! Serialize crossterm `KeyEvent`s back into the byte sequences a child PTY
//! expects. We use this because in split-view mode the parent process owns the
//! stdin (via crossterm's event polling) and the focused agent never gets to
//! read raw bytes directly.
//!
//! Coverage is "what the common agent CLIs care about": printable chars + Alt-
//! prefixed chars, every Ctrl-letter, arrow keys, function keys, navigation
//! cluster, Backspace/Tab/Enter/Esc. Anything not handled returns `None` so the
//! caller can drop it rather than send garbage.
//!
//! Reference for escape sequences: xterm control sequences, ECMA-48.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

/// Arrow keys: CSI A/B/C/D or CSI 1;<mods> A/B/C/D when modifiers are present.
fn arrow_or_modified(letter: u8, mods: KeyModifiers) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        vec![0x1b, b'[', letter]
    } else {
        // ESC [ 1 ; <m> <letter>
        let mod_str = format!("{m}");
        let mut out = vec![0x1b, b'[', b'1', b';'];
        out.extend_from_slice(mod_str.as_bytes());
        out.push(letter);
        out
    }
}

/// Home/End: CSI H / CSI F (unmodified), or CSI 1;<mods> H/F otherwise.
fn csi_or_modified(letter: u8, mods: KeyModifiers) -> Vec<u8> {
    arrow_or_modified(letter, mods)
}

/// Keys that follow the `CSI <n> ~` convention: PageUp/Down/Insert/Delete.
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

/// xterm modifier code: shift=1, alt=2, ctrl=4, summed + 1 (so plain = 1).
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
    // We deliberately do NOT generate F1 — F1 is hijacked by agentdeck as the
    // focus toggle and never reaches this code path. Including it here would
    // be dead code that's confusing to read.
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
