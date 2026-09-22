//! Symbolic keyboard chords, independent of physical keyboard positions.
use crate::error::BackendError;
use xkbcommon::xkb;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Super,
    AltGr,
}

impl Modifier {
    pub fn parse(s: &str) -> Result<Self, BackendError> {
        match s.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Ok(Self::Ctrl),
            "shift" => Ok(Self::Shift),
            "alt" => Ok(Self::Alt),
            "super" | "meta" | "win" => Ok(Self::Super),
            "altgr" => Ok(Self::AltGr),
            _ => Err(unsupported(format!("unknown modifier: {s}"))),
        }
    }

    pub(crate) fn symbols(self) -> &'static [xkb::Keysym] {
        use xkb::Keysym as K;
        match self {
            Self::Ctrl => &[K::Control_L, K::Control_R],
            Self::Shift => &[K::Shift_L, K::Shift_R],
            Self::Alt => &[K::Alt_L, K::Alt_R],
            Self::Super => &[K::Super_L, K::Super_R],
            Self::AltGr => &[K::ISO_Level3_Shift],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: Vec<Modifier>,
    pub key: xkb::Keysym,
}

pub fn parse_chord(text: &str) -> Result<Chord, BackendError> {
    let text = text.trim();
    let mut parts: Vec<_> = text.split('+').map(str::trim).collect();
    let key_name = if text.ends_with('+') {
        parts.pop();
        if text != "+" && parts.pop() != Some("") {
            return Err(unsupported("use ++ or +plus for the plus key"));
        }
        if text == "+" {
            parts.clear();
        }
        "+"
    } else {
        parts.pop().unwrap_or_default()
    };
    if key_name.is_empty() || parts.iter().any(|s| s.is_empty()) {
        return Err(unsupported(format!("empty chord segment in {text:?}")));
    }
    let mut modifiers = Vec::new();
    for name in parts {
        let modifier = Modifier::parse(name)?;
        if modifiers.contains(&modifier) {
            return Err(unsupported(format!("modifier {name:?} appears twice")));
        }
        modifiers.push(modifier);
    }
    let mut chars = key_name.chars();
    let first = chars.next().expect("nonempty key name");
    let key = if chars.next().is_none() {
        literal_symbol(first)?
    } else {
        let alias = match key_name.to_ascii_lowercase().as_str() {
            "enter" => "Return",
            "esc" => "Escape",
            "pageup" => "Page_Up",
            "pagedown" => "Page_Down",
            _ => key_name,
        };
        if alias.contains('\0') {
            return Err(unsupported("NUL in key name"));
        }
        let symbol = xkb::keysym_from_name(alias, xkb::KEYSYM_CASE_INSENSITIVE);
        if symbol == xkb::Keysym::NoSymbol {
            return Err(unsupported(format!("unknown key name: {key_name:?}")));
        }
        symbol
    };
    Ok(Chord { modifiers, key })
}

pub(crate) fn literal_symbol(ch: char) -> Result<xkb::Keysym, BackendError> {
    match ch {
        '\n' | '\r' => Ok(xkb::Keysym::Return),
        '\t' => Ok(xkb::Keysym::Tab),
        ch if ch.is_control() => Err(unsupported(format!(
            "unsupported control character: {ch:?}"
        ))),
        ch => Ok(xkb::utf32_to_keysym(ch as u32)),
    }
}

pub(crate) fn unsupported(reason: impl Into<String>) -> BackendError {
    BackendError::Unsupported {
        reason: reason.into(),
    }
}

pub fn parse_button(name: &str) -> Result<u32, BackendError> {
    match name.to_ascii_lowercase().as_str() {
        "left" => Ok(BTN_LEFT),
        "right" => Ok(BTN_RIGHT),
        "middle" => Ok(BTN_MIDDLE),
        _ => Err(unsupported(format!("unknown button: {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsing_preserves_symbols_and_explicit_modifiers() {
        for text in ["Ctrl++", "Ctrl+plus"] {
            let chord = parse_chord(text).unwrap();
            assert_eq!(chord.key, xkb::Keysym::plus);
            assert_eq!(chord.modifiers, vec![Modifier::Ctrl]);
        }
        assert_eq!(parse_chord("Ctrl+?").unwrap().key, xkb::Keysym::question);
        assert_eq!(parse_chord("Ω").unwrap().key, xkb::Keysym::Greek_OMEGA);
        assert_eq!(parse_chord("F24").unwrap().key, xkb::Keysym::F24);
        assert_eq!(
            parse_chord("AltGr+Q").unwrap().modifiers,
            vec![Modifier::AltGr]
        );
        for text in ["", "Ctrl+", "Ctrl++Z", "Ctrl+Ctrl+Z", "not_a_key", "ab\0cd"] {
            assert!(parse_chord(text).is_err(), "{text:?}");
        }
    }
}
