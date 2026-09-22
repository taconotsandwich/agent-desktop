//! Inverse key translation shared by XTEST and EIS. XKB keycodes stay in
//! XKB units here; only the EIS transport converts them to evdev units.
use crate::{
    error::BackendError,
    platform::keymap::{Chord, Modifier, unsupported},
};
use std::collections::HashSet;
use xkbcommon::xkb;

// The Rust bindings omit this stable libxkbcommon API. Use XKB's own case
// conversion, including non-Latin keysyms, instead of an ASCII shortcut table.
unsafe extern "C" {
    fn xkb_keysym_to_lower(keysym: u32) -> u32;
}
fn lower(symbol: xkb::Keysym) -> u32 {
    // SAFETY: the function accepts every 32-bit keysym and has no pointers.
    unsafe { xkb_keysym_to_lower(symbol.raw()) }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativeState {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    pub group: u32,
}
impl NativeState {
    pub fn capture(state: &xkb::State) -> Self {
        Self {
            depressed: state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            latched: state.serialize_mods(xkb::STATE_MODS_LATCHED),
            locked: state.serialize_mods(xkb::STATE_MODS_LOCKED),
            group: state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        }
    }
    fn apply(self, keymap: &xkb::Keymap) -> xkb::State {
        let mut state = xkb::State::new(keymap);
        state.update_mask(self.depressed, self.latched, self.locked, 0, 0, self.group);
        state
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stroke {
    pub key: u32,
    pub modifiers: Vec<u32>,
}

pub(crate) struct Keyboard {
    keymap: xkb::Keymap,
    native: NativeState,
    momentary: Vec<(u32, u32)>,
}
impl Keyboard {
    pub fn from_text(text: String, native: NativeState) -> Result<Self, BackendError> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| unsupported("desktop supplied an invalid XKB keymap"))?;
        Ok(Self::new(keymap, native))
    }

    pub fn new(keymap: xkb::Keymap, native: NativeState) -> Self {
        let mut momentary = Vec::new();
        keymap.key_for_each(|map, key| {
            let mut state = NativeState {
                group: native.group,
                ..Default::default()
            }
            .apply(map);
            state.update_key(key, xkb::KeyDirection::Down);
            let changed = NativeState::capture(&state);
            if changed.depressed != 0
                && changed.latched == 0
                && changed.locked == 0
                && changed.group == native.group
            {
                momentary.push((key.raw(), changed.depressed));
            }
        });
        Self {
            keymap,
            native,
            momentary,
        }
    }

    fn state_with(&self, keys: &[u32]) -> xkb::State {
        let mut state = self.native.apply(&self.keymap);
        for &key in keys {
            state.update_key(key.into(), xkb::KeyDirection::Down);
        }
        state
    }

    pub fn modifiers(&self, modifiers: &[Modifier]) -> Result<Vec<u32>, BackendError> {
        let state = self.native.apply(&self.keymap);
        let active = state.serialize_mods(xkb::STATE_MODS_EFFECTIVE);
        let mut keys = Vec::new();
        for modifier in modifiers {
            let &(key, mask) = modifier
                .symbols()
                .iter()
                .find_map(|symbol| {
                    self.momentary.iter().find(|(key, _)| {
                        let layout = state.key_get_layout((*key).into());
                        self.keymap
                            .key_get_syms_by_level((*key).into(), layout, 0)
                            .contains(symbol)
                    })
                })
                .ok_or_else(|| unsupported(format!("no {modifier:?} key in the desktop keymap")))?;
            if mask & active != mask && !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    pub fn chord(&self, chord: &Chord) -> Result<Stroke, BackendError> {
        let mut stroke = self.resolve(chord.key, true)?;
        for key in self.modifiers(&chord.modifiers)? {
            if !stroke.modifiers.contains(&key) {
                stroke.modifiers.push(key);
            }
        }
        if stroke.modifiers.contains(&stroke.key) {
            return Err(unsupported(
                "the chord key is also one of its held modifiers",
            ));
        }
        Ok(stroke)
    }

    pub fn literal(&self, ch: char) -> Result<Stroke, BackendError> {
        self.resolve(super::keymap::literal_symbol(ch)?, false)
    }

    fn resolve(&self, symbol: xkb::Keysym, shortcut: bool) -> Result<Stroke, BackendError> {
        let baseline = self.native.apply(&self.keymap);
        let active = baseline.serialize_mods(xkb::STATE_MODS_EFFECTIVE);
        // At most one candidate per modifier mask. This derives modifier
        // combinations from the map, including Level3/Level5 and remappings.
        let mut plans = vec![(active, Vec::new())];
        let mut seen = HashSet::from([active]);
        for &(key, mask) in &self.momentary {
            for index in 0..plans.len() {
                let combined = plans[index].0 | mask;
                if seen.insert(combined) {
                    let mut keys = plans[index].1.clone();
                    keys.push(key);
                    plans.push((combined, keys));
                }
            }
        }
        plans.sort_by_key(|(_, keys)| keys.len());
        for (_, modifiers) in plans {
            let state = self.state_with(&modifiers);
            let added = state.serialize_mods(xkb::STATE_MODS_EFFECTIVE) & !active;
            for raw in self.keymap.min_keycode().raw()..=self.keymap.max_keycode().raw() {
                let key = xkb::Keycode::new(raw);
                let actual = state.key_get_one_sym(key);
                let matches = if shortcut {
                    lower(actual) == lower(symbol)
                } else {
                    actual == symbol
                };
                if !matches || modifiers.contains(&raw) {
                    continue;
                }
                // Never infer a shortcut modifier merely to reach a symbol.
                // Only modifiers consumed by that key's XKB level may be added.
                if added & !state.key_get_consumed_mods(key) != 0 {
                    continue;
                }
                return Ok(Stroke {
                    key: raw,
                    modifiers,
                });
            }
        }
        Err(unsupported(format!(
            "{} is unavailable in the active desktop keyboard layout",
            xkb::keysym_get_name(symbol)
        )))
    }
}
