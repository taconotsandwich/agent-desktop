//! evdev keycodes + xdotool-style chord parsing (ported from kde-mcp).
//!
//! Direct typing is ASCII US-QWERTY only. Non-ASCII goes through the
//! clipboard-paste path (`type_unicode`), never through here.

use crate::error::BackendError;

/// evdev raw keycodes from `/usr/include/linux/input-event-codes.h`.
/// libei expects these directly; no -8 offset.
pub const KEY_LEFTCTRL: u32 = 29;
pub const KEY_LEFTSHIFT: u32 = 42;
pub const KEY_LEFTALT: u32 = 56;
pub const KEY_LEFTMETA: u32 = 125;
pub const KEY_TAB: u32 = 15;
pub const KEY_ENTER: u32 = 28;
pub const KEY_ESC: u32 = 1;
pub const KEY_BACKSPACE: u32 = 14;
pub const KEY_SPACE: u32 = 57;
pub const KEY_UP: u32 = 103;
pub const KEY_LEFT: u32 = 105;
pub const KEY_RIGHT: u32 = 106;
pub const KEY_DOWN: u32 = 108;
pub const KEY_PAGEUP: u32 = 104;
pub const KEY_PAGEDOWN: u32 = 109;
pub const KEY_HOME: u32 = 102;
pub const KEY_END: u32 = 107;
pub const KEY_INSERT: u32 = 110;
pub const KEY_DELETE: u32 = 111;
pub const KEY_F1: u32 = 59;
pub const KEY_F2: u32 = 60;
pub const KEY_F3: u32 = 61;
pub const KEY_F4: u32 = 62;
pub const KEY_F5: u32 = 63;
pub const KEY_F6: u32 = 64;
pub const KEY_F7: u32 = 65;
pub const KEY_F8: u32 = 66;
pub const KEY_F9: u32 = 67;
pub const KEY_F10: u32 = 68;
pub const KEY_F11: u32 = 87;
pub const KEY_F12: u32 = 88;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Super,
}

impl Modifier {
    pub fn keycode(self) -> u32 {
        match self {
            Modifier::Ctrl => KEY_LEFTCTRL,
            Modifier::Shift => KEY_LEFTSHIFT,
            Modifier::Alt => KEY_LEFTALT,
            Modifier::Super => KEY_LEFTMETA,
        }
    }

    pub fn parse(s: &str) -> Result<Self, BackendError> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Modifier::Ctrl,
            "shift" => Modifier::Shift,
            "alt" => Modifier::Alt,
            "super" | "meta" | "win" => Modifier::Super,
            other => {
                return Err(BackendError::Unsupported {
                    reason: format!("unknown modifier: {other}"),
                });
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct Chord {
    pub modifiers: Vec<Modifier>,
    pub key: u32,
}

/// Parse an xdotool-style chord. Last `+` token is the key, earlier are modifiers.
pub fn parse_chord(s: &str) -> Result<Chord, BackendError> {
    let bad = |reason: String| BackendError::Unsupported { reason };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(bad("empty chord".into()));
    }
    let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(bad(format!("empty chord segment in {trimmed:?}")));
    }
    let (key_name, mod_names) = parts.split_last().unwrap();
    let mut modifiers = Vec::with_capacity(mod_names.len());
    for m in mod_names {
        let parsed = Modifier::parse(m)?;
        if modifiers.contains(&parsed) {
            return Err(bad(format!("modifier {m:?} appears twice")));
        }
        modifiers.push(parsed);
    }
    let key = keycode_for_named_key(key_name)
        .ok_or_else(|| bad(format!("unknown key name: {key_name:?}")))?;
    Ok(Chord { modifiers, key })
}

pub fn keycode_for_named_key(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        "return" | "enter" => KEY_ENTER,
        "tab" => KEY_TAB,
        "escape" | "esc" => KEY_ESC,
        "backspace" => KEY_BACKSPACE,
        "space" => KEY_SPACE,
        "up" => KEY_UP,
        "down" => KEY_DOWN,
        "left" => KEY_LEFT,
        "right" => KEY_RIGHT,
        "page_up" | "pageup" => KEY_PAGEUP,
        "page_down" | "pagedown" => KEY_PAGEDOWN,
        "home" => KEY_HOME,
        "end" => KEY_END,
        "insert" => KEY_INSERT,
        "delete" => KEY_DELETE,
        "f1" => KEY_F1,
        "f2" => KEY_F2,
        "f3" => KEY_F3,
        "f4" => KEY_F4,
        "f5" => KEY_F5,
        "f6" => KEY_F6,
        "f7" => KEY_F7,
        "f8" => KEY_F8,
        "f9" => KEY_F9,
        "f10" => KEY_F10,
        "f11" => KEY_F11,
        "f12" => KEY_F12,
        single if single.chars().count() == 1 => {
            return keycode_for_char(single.chars().next().unwrap());
        }
        _ => return None,
    })
}

/// Printable ASCII → evdev keycode. Capitals share lowercase codes; caller
/// holds Shift when [`shift_required`] is true.
pub fn keycode_for_char(ch: char) -> Option<u32> {
    Some(match ch {
        '1' | '!' => 2,
        '2' | '@' => 3,
        '3' | '#' => 4,
        '4' | '$' => 5,
        '5' | '%' => 6,
        '6' | '^' => 7,
        '7' | '&' => 8,
        '8' | '*' => 9,
        '9' | '(' => 10,
        '0' | ')' => 11,
        '-' | '_' => 12,
        '=' | '+' => 13,
        'q' | 'Q' => 16,
        'w' | 'W' => 17,
        'e' | 'E' => 18,
        'r' | 'R' => 19,
        't' | 'T' => 20,
        'y' | 'Y' => 21,
        'u' | 'U' => 22,
        'i' | 'I' => 23,
        'o' | 'O' => 24,
        'p' | 'P' => 25,
        '[' | '{' => 26,
        ']' | '}' => 27,
        'a' | 'A' => 30,
        's' | 'S' => 31,
        'd' | 'D' => 32,
        'f' | 'F' => 33,
        'g' | 'G' => 34,
        'h' | 'H' => 35,
        'j' | 'J' => 36,
        'k' | 'K' => 37,
        'l' | 'L' => 38,
        ';' | ':' => 39,
        '\'' | '"' => 40,
        '`' | '~' => 41,
        'z' | 'Z' => 44,
        'x' | 'X' => 45,
        'c' | 'C' => 46,
        'v' | 'V' => 47,
        'b' | 'B' => 48,
        'n' | 'N' => 49,
        'm' | 'M' => 50,
        ',' | '<' => 51,
        '.' | '>' => 52,
        '/' | '?' => 53,
        '\\' | '|' => 43,
        ' ' => KEY_SPACE,
        '\n' => KEY_ENTER,
        '\t' => KEY_TAB,
        _ => return None,
    })
}

/// True when the character needs Shift held on a US keymap.
pub fn shift_required(ch: char) -> bool {
    ch.is_ascii_uppercase()
        || matches!(
            ch,
            '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '_' | '+' | '{' | '}'
                | '|' | ':' | '"' | '<' | '>' | '?' | '~'
        )
}

pub fn parse_button(name: &str) -> Result<u32, BackendError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "left" => BTN_LEFT,
        "right" => BTN_RIGHT,
        "middle" => BTN_MIDDLE,
        other => {
            return Err(BackendError::Unsupported {
                reason: format!("unknown button: {other}"),
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_maps_with_shift_flags() {
        for ch in "-_.:/?=+,@#%".chars() {
            assert!(keycode_for_char(ch).is_some(), "no mapping for {ch:?}");
        }
        assert!(shift_required('A'));
        assert!(shift_required('!'));
        assert!(shift_required('_'));
        assert!(!shift_required('a'));
        assert!(!shift_required('-'));
        assert!(!shift_required('5'));
        // shifted digit shares the digit keycode
        assert_eq!(keycode_for_char('!'), keycode_for_char('1'));
        assert_eq!(keycode_for_char('_'), keycode_for_char('-'));
    }

    #[test]
    fn chords_parse() {
        let c = parse_chord("shift+Insert").unwrap();
        assert_eq!(c.modifiers, vec![Modifier::Shift]);
        assert_eq!(c.key, KEY_INSERT);
        let c = parse_chord("ctrl+s").unwrap();
        assert_eq!(c.modifiers, vec![Modifier::Ctrl]);
    }
}
