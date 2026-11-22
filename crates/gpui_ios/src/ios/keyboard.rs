// Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/framework/Source/FlutterEmbedderKeyResponder.mm,
// KeyCodeMap.g.mm and KeyCodeMap_Internal.h @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//! Hardware keyboard presses as GPUI key events, decided on plain values (no UIKit), so the host can test them.
//!
//! Flutter's responder turns each `UIPress` into physical and logical key events; GPUI has neither, so the same
//! decisions come out as GPUI's events: a non-modifier key is a `KeyDown` / `KeyUp` carrying GPUI's key name, and a
//! modifier or Caps Lock press is a `ModifiersChanged`. Flutter's synthesized modifier events (iOS's modifier flags
//! say a modifier changed without a press for it) become one `ModifiersChanged` before the press's own event.

use gpui::{
    Capslock, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, PlatformInput,
};
use std::time::Duration;

// `UIKey.modifierFlags` bits (KeyCodeMap_Internal.h's `ModifierFlag`). The sided ones are not in Apple's docs;
// Flutter found them by experiment.
const MODIFIER_FLAG_CONTROL_LEFT: u32 = 0x1;
const MODIFIER_FLAG_SHIFT_LEFT: u32 = 0x2;
const MODIFIER_FLAG_SHIFT_RIGHT: u32 = 0x4;
const MODIFIER_FLAG_META_LEFT: u32 = 0x8;
const MODIFIER_FLAG_META_RIGHT: u32 = 0x10;
const MODIFIER_FLAG_ALT_LEFT: u32 = 0x20;
const MODIFIER_FLAG_ALT_RIGHT: u32 = 0x40;
const MODIFIER_FLAG_CONTROL_RIGHT: u32 = 0x2000;
/// `UIKeyModifierAlphaShift`.
const MODIFIER_FLAG_CAPS_LOCK: u32 = 0x01_0000;
/// `UIKeyModifierShift`.
const MODIFIER_FLAG_SHIFT_ANY: u32 = 0x02_0000;
/// `UIKeyModifierControl`.
const MODIFIER_FLAG_CONTROL_ANY: u32 = 0x04_0000;
/// `UIKeyModifierAlternate`.
const MODIFIER_FLAG_ALT_ANY: u32 = 0x08_0000;
/// `UIKeyModifierCommand`.
const MODIFIER_FLAG_META_ANY: u32 = 0x10_0000;

/// Flutter's `kModifierFlagSidedMask`.
const MODIFIER_FLAG_SIDED_MASK: u32 = MODIFIER_FLAG_CONTROL_LEFT
    | MODIFIER_FLAG_SHIFT_LEFT
    | MODIFIER_FLAG_SHIFT_RIGHT
    | MODIFIER_FLAG_META_LEFT
    | MODIFIER_FLAG_META_RIGHT
    | MODIFIER_FLAG_ALT_LEFT
    | MODIFIER_FLAG_ALT_RIGHT
    | MODIFIER_FLAG_CONTROL_RIGHT;

/// Flutter's `computeModifierFlagOfInterestMask()`: the bits it keeps in sync. Not the numeric-pad bit, which
/// marks the key's area, not a modifier (`testSpecialModiferFlags`).
const MODIFIER_FLAG_OF_INTEREST_MASK: u32 = MODIFIER_FLAG_CAPS_LOCK
    | MODIFIER_FLAG_SHIFT_ANY
    | MODIFIER_FLAG_CONTROL_ANY
    | MODIFIER_FLAG_ALT_ANY
    | MODIFIER_FLAG_META_ANY
    | MODIFIER_FLAG_SIDED_MASK;

/// `UIKeyboardHIDUsageKeyboardCapsLock`.
const KEY_CODE_CAPS_LOCK: u32 = 0x39;

/// One modifier's sides, in Flutter's `adjustModifiers` order: (any flag, left flag, right flag, left key's HID
/// usage, right key's HID usage). A key in this table is a modifier key: its press is a `ModifiersChanged`.
const SIDED_MODIFIERS: [(u32, u32, u32, u32, u32); 4] = [
    (
        MODIFIER_FLAG_SHIFT_ANY,
        MODIFIER_FLAG_SHIFT_LEFT,
        MODIFIER_FLAG_SHIFT_RIGHT,
        0xe1,
        0xe5,
    ),
    (
        MODIFIER_FLAG_CONTROL_ANY,
        MODIFIER_FLAG_CONTROL_LEFT,
        MODIFIER_FLAG_CONTROL_RIGHT,
        0xe0,
        0xe4,
    ),
    (
        MODIFIER_FLAG_ALT_ANY,
        MODIFIER_FLAG_ALT_LEFT,
        MODIFIER_FLAG_ALT_RIGHT,
        0xe2,
        0xe6,
    ),
    (
        MODIFIER_FLAG_META_ANY,
        MODIFIER_FLAG_META_LEFT,
        MODIFIER_FLAG_META_RIGHT,
        0xe3,
        0xe7,
    ),
];

/// Flutter's `specialKeyMapping`: the names UIKit gives unprintable keys in `charactersIgnoringModifiers` → GPUI's
/// key. Checked first, so Cmd-. (which UIKit names `UIKeyInputEscape`) is Escape. Flutter maps `UIKeyInputEnd` to
/// Enter's logical key (a slip in its generated table); here it is End. To extend: add UIKit's name and GPUI's.
const SPECIAL_KEYS: [(&str, &str); 21] = [
    ("UIKeyInputEscape", "escape"),
    ("UIKeyInputF1", "f1"),
    ("UIKeyInputF2", "f2"),
    ("UIKeyInputF3", "f3"),
    ("UIKeyInputF4", "f4"),
    ("UIKeyInputF5", "f5"),
    ("UIKeyInputF6", "f6"),
    ("UIKeyInputF7", "f7"),
    ("UIKeyInputF8", "f8"),
    ("UIKeyInputF9", "f9"),
    ("UIKeyInputF10", "f10"),
    ("UIKeyInputF11", "f11"),
    ("UIKeyInputF12", "f12"),
    ("UIKeyInputUpArrow", "up"),
    ("UIKeyInputDownArrow", "down"),
    ("UIKeyInputLeftArrow", "left"),
    ("UIKeyInputRightArrow", "right"),
    ("UIKeyInputHome", "home"),
    ("UIKeyInputEnd", "end"),
    ("UIKeyInputPageUp", "pageup"),
    ("UIKeyInputPageDown", "pagedown"),
];

/// Flutter's `keyCodeToLogicalKey`, HID usage → GPUI's key and the text it types (GPUI's `key_char`, as gpui_macos
/// gives it). Only the rows GPUI names: the modifier rows are `SIDED_MODIFIERS`; numeric-pad digits and operators
/// fall through to their label, as GPUI names them by their character (`"1"`); NumLock, ContextMenu, volume, Intl
/// and Lang keys have no GPUI name. Added for GPUI: Space (Flutter takes it from the label) and F21–F24 (Flutter's
/// `functionKeyCodes`, whose characters it strips). To extend: add the HID usage with GPUI's key name.
const KEY_CODE_KEYS: [(u32, &str, Option<&str>); 40] = [
    (0x28, "enter", Some("\n")),
    (0x29, "escape", None),
    (0x2a, "backspace", None),
    (0x2b, "tab", Some("\t")),
    (0x2c, "space", Some(" ")),
    (0x3a, "f1", None),
    (0x3b, "f2", None),
    (0x3c, "f3", None),
    (0x3d, "f4", None),
    (0x3e, "f5", None),
    (0x3f, "f6", None),
    (0x40, "f7", None),
    (0x41, "f8", None),
    (0x42, "f9", None),
    (0x43, "f10", None),
    (0x44, "f11", None),
    (0x45, "f12", None),
    (0x49, "insert", None),
    (0x4a, "home", None),
    (0x4b, "pageup", None),
    (0x4c, "delete", None),
    (0x4d, "end", None),
    (0x4e, "pagedown", None),
    (0x4f, "right", None),
    (0x50, "left", None),
    (0x51, "down", None),
    (0x52, "up", None),
    (0x58, "enter", Some("\n")), // NumpadEnter
    (0x68, "f13", None),
    (0x69, "f14", None),
    (0x6a, "f15", None),
    (0x6b, "f16", None),
    (0x6c, "f17", None),
    (0x6d, "f18", None),
    (0x6e, "f19", None),
    (0x6f, "f20", None),
    (0x70, "f21", None),
    (0x71, "f22", None),
    (0x72, "f23", None),
    (0x73, "f24", None),
];

/// A `UIPress`'s phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressPhase {
    Began,
    Changed,
    Stationary,
    Ended,
    Cancelled,
}

/// One keyboard `UIPress` as plain values (Flutter's `FlutterUIPressProxy`).
#[derive(Clone, Copy, Debug)]
pub struct Press<'a> {
    pub phase: PressPhase,
    /// `UIKey.keyCode`, a HID usage.
    pub key_code: u32,
    /// `UIKey.modifierFlags`.
    pub modifier_flags: u32,
    /// `UIKey.characters`.
    pub characters: &'a str,
    /// `UIKey.charactersIgnoringModifiers`.
    pub characters_ignoring_modifiers: &'a str,
}

/// What a press is for GPUI and UIKit, after any `ModifiersChanged` it caused.
#[derive(Debug)]
pub enum Primary {
    /// A down GPUI may take: UIKit's default handling (the text system typing it) runs only if GPUI leaves it
    /// unhandled. Report GPUI's answer with `KeyResponder::offered`.
    Offer(PlatformInput),
    /// GPUI is told `event`, unanswered, and UIKit gets the press if `uikit`. An up or cancel goes to UIKit exactly
    /// when its down did, so UIKit is never left with a key down. A down with no event: Flutter's empty event (a
    /// second down of a key that is down: nobody's), a key GPUI has no name for, or a press made while the field
    /// composes (UIKit's).
    Send {
        event: Option<PlatformInput>,
        uikit: bool,
    },
}

/// The GPUI events one press makes.
#[derive(Debug)]
pub struct PressEvents {
    /// Sent first, unanswered (Flutter's synthesized events): iOS's flags show a modifier changed with no press.
    pub modifiers_changed: Option<ModifiersChangedEvent>,
    pub primary: Primary,
}

/// A held key's repeats, for a key GPUI took: UIKit repeats only the keys its text system got and sends no repeat
/// presses (Flutter's iOS embedder has none either). The wait before the first repeat, then between repeats. To
/// retune: change the values; iPadOS's own pace (Settings > Accessibility > Keyboards > Key Repeat) is not readable
/// by apps.
pub const KEY_REPEAT_DELAY: Duration = Duration::from_millis(500);
pub const KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(100);

/// Flutter's `FlutterEmbedderKeyResponder`: the keys down and the modifier state GPUI was last told; added for
/// UIKit, the keys UIKit has down and the key GPUI repeats.
#[derive(Debug, Default)]
pub struct KeyResponder {
    /// Flutter's `pressingRecords`: each key down GPUI was told of (HID usage) and the GPUI key its down carried
    /// (`None`: GPUI has no name for it).
    pressing_records: Vec<(u32, Option<String>)>,
    /// The keys whose down went on to UIKit (`super`), so their up and cancel go there too.
    uikit_pressing: Vec<u32>,
    /// The held key GPUI took that repeats (HID usage, its down's keystroke): the last key down.
    repeating: Option<(u32, Keystroke)>,
    /// Flutter's `lastModifierFlagsOfInterest`: the last press's flags, sides included, as `adjust_modifiers` fixed them.
    last_modifier_flags_of_interest: u32,
    /// The Caps Lock state GPUI was last told: the lock, which a Caps Lock down toggles (Flutter's framework toggles it
    /// on each Caps Lock down, real or synthesized).
    caps_lock: bool,
}

impl KeyResponder {
    /// The modifier state GPUI was last told.
    pub fn modifiers(&self) -> Modifiers {
        let flags = self.last_modifier_flags_of_interest;
        Modifiers {
            control: flags & MODIFIER_FLAG_CONTROL_ANY != 0,
            alt: flags & MODIFIER_FLAG_ALT_ANY != 0,
            shift: flags & MODIFIER_FLAG_SHIFT_ANY != 0,
            platform: flags & MODIFIER_FLAG_META_ANY != 0,
            function: false,
        }
    }

    pub fn capslock(&self) -> Capslock {
        Capslock { on: self.caps_lock }
    }

    /// Flutter's `handlePress:callback:`. Every press runs through it, also while the field composes, so the records
    /// and modifiers stay true. A down made while composing is UIKit's alone (gpui_macos sends it to the input method
    /// first: Enter commits, arrows pick); GPUI hears only its modifier change. Changed and stationary presses go
    /// where their down went, as Flutter's keyboard manager passes them on. A cancelled press is a release
    /// (Flutter's TODO there: "Handle cancelled events as synthesized up events"), so a key UIKit cancels is not
    /// left down.
    pub fn handle_press(&mut self, press: &Press, composing: bool) -> PressEvents {
        let key_down = match press.phase {
            PressPhase::Began => true,
            PressPhase::Ended | PressPhase::Cancelled => false,
            PressPhase::Changed | PressPhase::Stationary => {
                return PressEvents {
                    modifiers_changed: None,
                    primary: Primary::Send {
                        event: None,
                        uikit: self.uikit_pressing.contains(&press.key_code),
                    },
                };
            }
        };
        let before = (self.modifiers(), self.caps_lock);
        self.synchronize_modifiers(press, key_down);
        let modifiers = self.modifiers();
        let changed = ((modifiers, self.caps_lock) != before).then_some(ModifiersChangedEvent {
            modifiers,
            capslock: self.capslock(),
        });
        let is_modifier_key = press.key_code == KEY_CODE_CAPS_LOCK
            || SIDED_MODIFIERS
                .iter()
                .any(|&(_, _, _, left, right)| press.key_code == left || press.key_code == right);
        if !key_down {
            let uikit = match self
                .uikit_pressing
                .iter()
                .position(|&key| key == press.key_code)
            {
                Some(index) => {
                    self.uikit_pressing.remove(index);
                    true
                }
                None => false,
            };
            if self.repeating_key() == Some(press.key_code) {
                self.repeating = None;
            }
            let (modifiers_changed, event) = if is_modifier_key {
                // The press is its own modifier change.
                (None, changed.map(PlatformInput::ModifiersChanged))
            } else {
                (changed, self.handle_press_end(press.key_code, modifiers))
            };
            return PressEvents {
                modifiers_changed,
                primary: Primary::Send { event, uikit },
            };
        }
        if is_modifier_key {
            // The press is its own modifier change; one GPUI sees no change in (the other side's key) is UIKit's.
            let primary = match changed {
                Some(event) if !composing => Primary::Offer(PlatformInput::ModifiersChanged(event)),
                event => self.to_uikit(press.key_code, event.map(PlatformInput::ModifiersChanged)),
            };
            return PressEvents {
                modifiers_changed: None,
                primary,
            };
        }
        // A new key down ends the last one's repeat, as on macOS.
        self.repeating = None;
        let primary = if composing {
            self.to_uikit(press.key_code, None)
        } else {
            self.handle_press_begin(press, modifiers)
        };
        PressEvents {
            modifiers_changed: changed,
            primary,
        }
    }

    /// GPUI's answer to an `Offer`ed down: one it left unhandled went on to UIKit, so its up goes there too; a key
    /// GPUI took repeats while held.
    pub fn offered(&mut self, key_code: u32, event: &PlatformInput, handled: bool) {
        match (handled, event) {
            (false, _) => self.uikit_pressing.push(key_code),
            (true, PlatformInput::KeyDown(down)) => {
                self.repeating = Some((key_code, down.keystroke.clone()));
            }
            (true, _) => {}
        }
    }

    /// The held key GPUI repeats, by HID usage.
    pub fn repeating_key(&self) -> Option<u32> {
        self.repeating.as_ref().map(|(key, _)| *key)
    }

    /// One repeat of the held key: its down again, `is_held`.
    pub fn repeat(&self) -> Option<PlatformInput> {
        self.repeating.as_ref().map(|(_, keystroke)| {
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: true,
                prefer_character_input: false,
            })
        })
    }

    /// Ends the repeat with no press: the window resigned active, so the key's up may never come.
    pub fn stop_repeat(&mut self) {
        self.repeating = None;
    }

    /// A down UIKit gets, GPUI told only `event`: its up goes to UIKit too.
    fn to_uikit(&mut self, key_code: u32, event: Option<PlatformInput>) -> Primary {
        self.uikit_pressing.push(key_code);
        Primary::Send { event, uikit: true }
    }

    /// Flutter's `synchronizeModifiers:`, keeping the state its synthesized events would leave.
    fn synchronize_modifiers(&mut self, press: &Press, key_down: bool) {
        let pressed_modifiers = self.adjust_modifiers(press, key_down);
        self.last_modifier_flags_of_interest = pressed_modifiers & MODIFIER_FLAG_OF_INTEREST_MASK;
        if press.key_code == KEY_CODE_CAPS_LOCK {
            if key_down {
                self.caps_lock = !self.caps_lock;
            }
        } else {
            // iOS's flag on another key is the lock's state; a difference is Flutter's synthesized Caps Lock tap.
            self.caps_lock = press.modifier_flags & MODIFIER_FLAG_CAPS_LOCK != 0;
        }
    }

    /// Flutter's `adjustModifiers:` with `fixSidedFlags:…`. iOS still flags a modifier on its own key up, can flag
    /// one that is no longer down (Alt released before the letter it made), and never says which side: the flags
    /// start from the press's and the sides already known, then each modifier's key adds or removes its side.
    /// Flutter's Caps Lock branch is `synchronize_modifiers`'s toggle.
    fn adjust_modifiers(&self, press: &Press, key_down: bool) -> u32 {
        let mut pressed_modifiers = press.modifier_flags
            | (self.last_modifier_flags_of_interest & MODIFIER_FLAG_SIDED_MASK);
        for (any_flag, left_side, right_side, left_key_code, right_key_code) in SIDED_MODIFIERS {
            if key_down {
                // Add in the modifier flags that correspond to this key code, if any.
                if press.key_code == left_key_code {
                    pressed_modifiers |= left_side | any_flag;
                } else if press.key_code == right_key_code {
                    pressed_modifiers |= right_side | any_flag;
                }
            } else if press.key_code == left_key_code {
                // A key up removes its side, and the "any" flag if the other side isn't also pressed.
                pressed_modifiers &= !left_side;
                if pressed_modifiers & right_side == 0 {
                    pressed_modifiers &= !any_flag;
                }
            } else if press.key_code == right_key_code {
                pressed_modifiers &= !right_side;
                if pressed_modifiers & left_side == 0 {
                    pressed_modifiers &= !any_flag;
                }
            }
            if pressed_modifiers & any_flag == 0 {
                // Turn off any sided flags, since the "any" flag is gone.
                pressed_modifiers &= !(left_side | right_side);
            }
        }
        pressed_modifiers
    }

    /// Flutter's `handlePressBegin:callback:`.
    fn handle_press_begin(&mut self, press: &Press, modifiers: Modifiers) -> Primary {
        if self
            .pressing_records
            .iter()
            .any(|(key_code, _)| *key_code == press.key_code)
        {
            // Normally iOS sends a key's up to the view its down went to; a second down means one was missed.
            return Primary::Send {
                event: None,
                uikit: false,
            };
        }
        let keystroke = keystroke_for_event(press, modifiers);
        self.pressing_records.push((
            press.key_code,
            keystroke.as_ref().map(|keystroke| keystroke.key.clone()),
        ));
        match keystroke {
            Some(keystroke) => Primary::Offer(PlatformInput::KeyDown(KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
            })),
            None => self.to_uikit(press.key_code, None),
        }
    }

    /// Flutter's `handlePressEnd:callback:`: the up carries the key its down did. `None`: Flutter's empty event (an
    /// up of a key GPUI was not told is down), or a key GPUI has no name for.
    fn handle_press_end(&mut self, key_code: u32, modifiers: Modifiers) -> Option<PlatformInput> {
        let index = self
            .pressing_records
            .iter()
            .position(|(pressed, _)| *pressed == key_code)?;
        let key = self.pressing_records.remove(index).1?;
        Some(PlatformInput::KeyUp(KeyUpEvent {
            keystroke: Keystroke {
                modifiers: Modifiers {
                    // As its down: Shift folded into a key that is not a letter.
                    shift: modifiers.shift && (modifiers.alt || !folds_shift(&key)),
                    ..modifiers
                },
                key,
                key_char: None,
            },
        }))
    }
}

/// Flutter's `GetLogicalKeyForEvent` with its `character` (`getEventCharacters`), as GPUI's keystroke; `None` for a
/// key GPUI has no name for (Flutter mints one in its iOS plane). GPUI's conventions on top, as gpui_macos's
/// `parse_keystroke`: no `key_char` with Control or Command; Shift with a key that is not a letter is folded into the
/// key (`shift-1` is `!`).
fn keystroke_for_event(press: &Press, modifiers: Modifiers) -> Option<Keystroke> {
    let types_text = !modifiers.control && !modifiers.platform;
    if let Some(&(_, key)) = SPECIAL_KEYS
        .iter()
        .find(|(name, _)| *name == press.characters_ignoring_modifiers)
    {
        return Some(Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        });
    }
    if let Some(&(_, key, key_char)) = KEY_CODE_KEYS
        .iter()
        .find(|(key_code, _, _)| *key_code == press.key_code)
    {
        return Some(Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.filter(|_| types_text).map(Into::into),
        });
    }
    let label = printable(press.characters_ignoring_modifiers)?;
    let mut key: String = label.chars().map(to_lower).collect();
    let mut modifiers = modifiers;
    let character = printable(press.characters);
    if modifiers.shift
        && !modifiers.alt
        && folds_shift(&key)
        && let Some(shifted) = character
    {
        key = shifted.into();
        modifiers.shift = false;
    }
    Some(Keystroke {
        modifiers,
        key,
        key_char: character.filter(|_| types_text).map(Into::into),
    })
}

/// Whether Shift with this one-character key is folded into the key (GPUI's keystrokes write `!`, not `shift-1`,
/// but `shift-a`).
fn folds_shift(key: &str) -> bool {
    let mut chars = key.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_ascii_lowercase())
}

/// A key label that prints: one character, not a control character (Flutter's `IsControlCharacter`) and not one of
/// the private-use code points AppKit and UIKit give unprintable keys (`IsUnprintableKey`, U+F700–U+F8FF).
fn printable(label: &str) -> Option<&str> {
    let mut chars = label.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    (!c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c)).then_some(label)
}

/// Flutter's `toLower`: upper-case letters to lower case in ASCII and Latin-1, independent of locale.
fn to_lower(c: char) -> char {
    match c {
        'A'..='Z' | '\u{C0}'..='\u{DE}' if c != '\u{D7}' => {
            char::from_u32(c as u32 + 0x20).unwrap_or(c)
        }
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    const KEY_A: u32 = 0x04;
    const KEY_E: u32 = 0x08;
    const KEY_K: u32 = 0x0e;
    const KEY_W: u32 = 0x1a;
    const KEY_ENTER: u32 = 0x28;
    const KEY_1: u32 = 0x1e;
    const KEY_PERIOD: u32 = 0x37;
    const KEY_UNDEFINED: u32 = 0x03;
    const KEY_EJECT: u32 = 0xb8;
    const KEY_F1: u32 = 0x3a;
    const KEY_LEFT: u32 = 0x50;
    const KEY_BACKSPACE: u32 = 0x2a;
    const KEY_NUMPAD_1: u32 = 0x59;
    const SHIFT_LEFT: u32 = 0xe1;
    const SHIFT_RIGHT: u32 = 0xe5;
    const ALT_LEFT: u32 = 0xe2;
    const ALT_RIGHT: u32 = 0xe6;
    const META_LEFT: u32 = 0xe3;
    const CONTROL_LEFT: u32 = 0xe0;
    const NONE: u32 = 0;
    const NUM_PAD: u32 = 0x20_0000;

    use PressPhase::{Began as D, Cancelled as C, Changed as Ch, Ended as U};

    /// (phase, key code, flags, characters, characters ignoring modifiers): Flutter's `keyDownEvent` / `keyUpEvent`.
    type Step = (PressPhase, u32, u32, &'static str, &'static str);

    /// One press through the responder, as GPUI and UIKit see it, one string each: `mods <modifiers>` for a
    /// `ModifiersChanged`, `down <keystroke> <key_char>`, `up <keystroke>`, then `uikit` if UIKit gets the press
    /// (`super`), `empty` if nobody gets anything, and `repeat <keystroke>` / `repeat stops` when the held key that
    /// repeats changes. GPUI stands in for an app whose focused field binds the keys that type no text: it handles a
    /// `KeyDown` without `key_char` and leaves every other event unhandled (GPUI's `propagate` still set).
    fn step(responder: &mut KeyResponder, press: &Press, composing: bool) -> Vec<String> {
        fn mods(event: &ModifiersChangedEvent) -> String {
            let m = event.modifiers;
            let names = [
                (m.control, "ctrl"),
                (m.alt, "alt"),
                (m.shift, "shift"),
                (m.platform, "cmd"),
                (event.capslock.on, "caps"),
            ];
            let on: Vec<_> = names
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, name)| *name)
                .collect();
            format!(
                "mods {}",
                if on.is_empty() {
                    "none".into()
                } else {
                    on.join("-")
                }
            )
        }
        fn event_name(event: &PlatformInput) -> String {
            match event {
                PlatformInput::KeyDown(event) => format!(
                    "down {} {:?}",
                    event.keystroke.unparse(gpui::OperatingSystem::Ios),
                    event.keystroke.key_char
                ),
                PlatformInput::KeyUp(event) => {
                    format!("up {}", event.keystroke.unparse(gpui::OperatingSystem::Ios))
                }
                PlatformInput::ModifiersChanged(event) => mods(event),
                other => format!("unexpected {other:?}"),
            }
        }
        let repeating = responder.repeating_key();
        let events = responder.handle_press(press, composing);
        let mut out: Vec<String> = events.modifiers_changed.iter().map(mods).collect();
        let uikit = match events.primary {
            Primary::Offer(event) => {
                out.push(event_name(&event));
                let handled = matches!(&event, PlatformInput::KeyDown(down) if down.keystroke.key_char.is_none());
                responder.offered(press.key_code, &event, handled);
                !handled
            }
            Primary::Send { event, uikit } => {
                out.extend(event.as_ref().map(event_name));
                uikit
            }
        };
        if uikit {
            out.push("uikit".into());
        }
        if out.is_empty() {
            out.push("empty".into());
        }
        if responder.repeating_key() != repeating {
            out.push(match responder.repeat() {
                Some(PlatformInput::KeyDown(down)) => {
                    format!(
                        "repeat {}",
                        down.keystroke.unparse(gpui::OperatingSystem::Ios)
                    )
                }
                _ => "repeat stops".into(),
            });
        }
        out
    }

    #[test]
    fn presses() {
        // One row per Flutter test (FlutterEmbedderKeyResponderTest.mm), its presses and what GPUI gets from each;
        // then GPUI's own rows. Flutter's synthesized modifier events are the `mods` lines before a key event.
        let rows: &[(&str, &[(Step, &[&str])])] = &[
            (
                "testBasicKeyEvent",
                &[
                    ((D, KEY_A, NONE, "a", "a"), &["down a Some(\"a\")", "uikit"]),
                    ((U, KEY_A, NONE, "", ""), &["up a", "uikit"]),
                ],
            ),
            (
                // Eject is not in the key map; Flutter mints an iOS-plane key, GPUI has no name for it.
                "testIosKeyPlane",
                &[
                    ((D, KEY_EJECT, NONE, "", ""), &["uikit"]),
                    ((U, KEY_EJECT, NONE, "", ""), &["uikit"]),
                ],
            ),
            (
                "testOutOfOrderModifiers",
                &[
                    (
                        (D, ALT_RIGHT, MODIFIER_FLAG_ALT_ANY, "", ""),
                        &["mods alt", "uikit"],
                    ),
                    (
                        (D, KEY_W, MODIFIER_FLAG_ALT_ANY, "∑", "w"),
                        &["down alt-w Some(\"∑\")", "uikit"],
                    ),
                    (
                        (U, ALT_RIGHT, MODIFIER_FLAG_ALT_ANY, "", ""),
                        &["mods none", "uikit"],
                    ),
                    // iOS still flags Alt on the letter's up, though it was released: Flutter synthesizes Alt down.
                    (
                        (U, KEY_W, MODIFIER_FLAG_ALT_ANY, "", ""),
                        &["mods alt", "up alt-w", "uikit"],
                    ),
                    // ...and its up before the next key.
                    (
                        (D, KEY_A, NONE, "å", "a"),
                        &["mods none", "down a Some(\"å\")", "uikit"],
                    ),
                ],
            ),
            (
                "testIgnoreDuplicateDownEvent",
                &[
                    ((D, KEY_A, NONE, "a", "a"), &["down a Some(\"a\")", "uikit"]),
                    ((D, KEY_A, NONE, "a", "a"), &["empty"]),
                    ((U, KEY_A, NONE, "", ""), &["up a", "uikit"]),
                ],
            ),
            (
                "testIgnoreAbruptUpEvent",
                &[((U, KEY_A, NONE, "", ""), &["empty"])],
            ),
            (
                // R-Shift, A, release R-Shift, release A: the characters of A change in between.
                "testToggleModifiersDuringKeyTap",
                &[
                    (
                        (D, SHIFT_RIGHT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods shift", "uikit"],
                    ),
                    (
                        (D, KEY_A, MODIFIER_FLAG_SHIFT_ANY, "A", "A"),
                        &["down shift-a Some(\"A\")", "uikit"],
                    ),
                    ((U, SHIFT_RIGHT, NONE, "", ""), &["mods none", "uikit"]),
                    ((U, KEY_A, NONE, "", ""), &["up a", "uikit"]),
                ],
            ),
            (
                // The numeric-pad bit marks the key's area, not a modifier.
                "testSpecialModiferFlags",
                &[
                    (
                        (D, KEY_NUMPAD_1, NUM_PAD, "1", "1"),
                        &["down 1 Some(\"1\")", "uikit"],
                    ),
                    // Fn sends HID usage 0x03 (undefined), with no characters.
                    ((D, KEY_UNDEFINED, NONE, "", ""), &["uikit"]),
                    // F1's characters are "\^P", which are not printable.
                    (
                        (D, KEY_F1, NONE, "\\^P", "\\^P"),
                        &["down f1 None", "repeat f1"],
                    ),
                    (
                        (D, KEY_A, NONE, "a", "a"),
                        &["down a Some(\"a\")", "uikit", "repeat stops"],
                    ),
                    (
                        (D, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods shift", "uikit"],
                    ),
                    // The OS no longer flags Shift: Flutter synthesizes its up.
                    (
                        (U, KEY_NUMPAD_1, NUM_PAD, "", ""),
                        &["mods none", "up 1", "uikit"],
                    ),
                    ((U, KEY_F1, NONE, "", ""), &["up f1"]),
                    ((U, KEY_UNDEFINED, NONE, "", ""), &["uikit"]),
                    ((U, KEY_A, NONE, "", ""), &["up a", "uikit"]),
                    // Shift is already up for GPUI: nothing changes, so UIKit has it.
                    ((U, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""), &["uikit"]),
                ],
            ),
            (
                // Both shifts down, then up: Shift holds until the second is released.
                "testIdentifyLeftAndRightModifiers",
                &[
                    (
                        (D, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods shift", "uikit"],
                    ),
                    (
                        (D, SHIFT_RIGHT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["uikit"],
                    ),
                    ((U, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""), &["uikit"]),
                    (
                        (U, SHIFT_RIGHT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods none", "uikit"],
                    ),
                ],
            ),
            (
                "testSynchronizeCapsLockStateOnCapsLock",
                &[
                    (
                        (D, KEY_A, MODIFIER_FLAG_CAPS_LOCK, "A", "A"),
                        &["mods caps", "down a Some(\"A\")", "uikit"],
                    ),
                    (
                        (U, KEY_A, MODIFIER_FLAG_CAPS_LOCK, "", ""),
                        &["up a", "uikit"],
                    ),
                    // Caps Lock down turns the lock off; its up changes nothing.
                    (
                        (D, KEY_CODE_CAPS_LOCK, MODIFIER_FLAG_CAPS_LOCK, "", ""),
                        &["mods none", "uikit"],
                    ),
                    (
                        (U, KEY_CODE_CAPS_LOCK, MODIFIER_FLAG_CAPS_LOCK, "", ""),
                        &["uikit"],
                    ),
                    ((D, KEY_A, NONE, "a", "a"), &["down a Some(\"a\")", "uikit"]),
                ],
            ),
            (
                "testSynchronizeCapsLockStateOnNormalKey",
                &[(
                    (D, KEY_A, MODIFIER_FLAG_CAPS_LOCK, "A", "a"),
                    &["mods caps", "down a Some(\"A\")", "uikit"],
                )],
            ),
            (
                // Cmd-. is Escape, and its up unconventionally has characters.
                "testCommandPeriodKey",
                &[
                    (
                        (D, META_LEFT, MODIFIER_FLAG_META_ANY, "", ""),
                        &["mods cmd", "uikit"],
                    ),
                    (
                        (
                            D,
                            KEY_PERIOD,
                            MODIFIER_FLAG_META_ANY,
                            "UIKeyInputEscape",
                            "UIKeyInputEscape",
                        ),
                        &["down cmd-escape None", "repeat cmd-escape"],
                    ),
                    (
                        (
                            U,
                            KEY_PERIOD,
                            MODIFIER_FLAG_META_ANY,
                            "UIKeyInputEscape",
                            "UIKeyInputEscape",
                        ),
                        &["up cmd-escape", "repeat stops"],
                    ),
                    (
                        (U, META_LEFT, MODIFIER_FLAG_META_ANY, "", ""),
                        &["mods none", "uikit"],
                    ),
                ],
            ),
            (
                "GPUI: arrows and editing keys by UIKit name or HID usage, no text",
                &[
                    (
                        (
                            D,
                            KEY_LEFT,
                            NONE,
                            "UIKeyInputLeftArrow",
                            "UIKeyInputLeftArrow",
                        ),
                        &["down left None", "repeat left"],
                    ),
                    (
                        (
                            U,
                            KEY_LEFT,
                            NONE,
                            "UIKeyInputLeftArrow",
                            "UIKeyInputLeftArrow",
                        ),
                        &["up left", "repeat stops"],
                    ),
                    (
                        (D, KEY_BACKSPACE, NONE, "\u{8}", "\u{8}"),
                        &["down backspace None", "repeat backspace"],
                    ),
                    (
                        (U, KEY_BACKSPACE, NONE, "", ""),
                        &["up backspace", "repeat stops"],
                    ),
                ],
            ),
            (
                "GPUI: Shift folds into a key that is not a letter; Control and Command type no text",
                &[
                    (
                        (D, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods shift", "uikit"],
                    ),
                    (
                        (D, KEY_1, MODIFIER_FLAG_SHIFT_ANY, "!", "1"),
                        &["down ! Some(\"!\")", "uikit"],
                    ),
                    (
                        (U, KEY_1, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["up !", "uikit"],
                    ),
                    (
                        (U, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods none", "uikit"],
                    ),
                    (
                        (D, CONTROL_LEFT, MODIFIER_FLAG_CONTROL_ANY, "", ""),
                        &["mods ctrl", "uikit"],
                    ),
                    (
                        (D, KEY_A, MODIFIER_FLAG_CONTROL_ANY, "\u{1}", "a"),
                        &["down ctrl-a None", "repeat ctrl-a"],
                    ),
                    (
                        (U, KEY_A, MODIFIER_FLAG_CONTROL_ANY, "", ""),
                        &["up ctrl-a", "repeat stops"],
                    ),
                    (
                        (U, CONTROL_LEFT, MODIFIER_FLAG_CONTROL_ANY, "", ""),
                        &["mods none", "uikit"],
                    ),
                    (
                        (D, META_LEFT, MODIFIER_FLAG_META_ANY, "", ""),
                        &["mods cmd", "uikit"],
                    ),
                    (
                        (D, KEY_A, MODIFIER_FLAG_META_ANY, "a", "a"),
                        &["down cmd-a None", "repeat cmd-a"],
                    ),
                    (
                        (U, KEY_A, MODIFIER_FLAG_META_ANY, "", ""),
                        &["up cmd-a", "repeat stops"],
                    ),
                    (
                        (U, META_LEFT, MODIFIER_FLAG_META_ANY, "", ""),
                        &["mods none", "uikit"],
                    ),
                ],
            ),
            (
                "GPUI: a cancelled press releases its key; a changed press goes where its down went",
                &[
                    ((D, KEY_A, NONE, "a", "a"), &["down a Some(\"a\")", "uikit"]),
                    ((Ch, KEY_A, NONE, "a", "a"), &["uikit"]),
                    ((C, KEY_A, NONE, "", ""), &["up a", "uikit"]),
                    ((D, KEY_A, NONE, "a", "a"), &["down a Some(\"a\")", "uikit"]),
                ],
            ),
            (
                // UIKit repeats the keys its text system got; GPUI's keys repeat on the window. A new key down ends
                // the repeat, a modifier does not; an up or cancel ends it. GPUI's key's up stays GPUI's.
                "GPUI: a held key GPUI took repeats until its up; UIKit's keys do not",
                &[
                    (
                        (D, KEY_BACKSPACE, NONE, "\u{8}", "\u{8}"),
                        &["down backspace None", "repeat backspace"],
                    ),
                    (
                        (D, KEY_A, NONE, "a", "a"),
                        &["down a Some(\"a\")", "uikit", "repeat stops"],
                    ),
                    ((U, KEY_A, NONE, "", ""), &["up a", "uikit"]),
                    ((U, KEY_BACKSPACE, NONE, "", ""), &["up backspace"]),
                    (
                        (
                            D,
                            KEY_LEFT,
                            NONE,
                            "UIKeyInputLeftArrow",
                            "UIKeyInputLeftArrow",
                        ),
                        &["down left None", "repeat left"],
                    ),
                    (
                        (D, SHIFT_LEFT, MODIFIER_FLAG_SHIFT_ANY, "", ""),
                        &["mods shift", "uikit"],
                    ),
                    ((U, SHIFT_LEFT, NONE, "", ""), &["mods none", "uikit"]),
                    ((C, KEY_LEFT, NONE, "", ""), &["up left", "repeat stops"]),
                ],
            ),
        ];
        // GPUI's rows with presses made while the focused field composes (marked text): `ime` holds their indices.
        let ime_rows: &[(&str, Range<usize>, &[(Step, &[&str])])] = &[
            (
                // Option-e is a dead key: its down starts composing, its up and Alt's come while composing, the
                // next e commits "é". Its up after the commit and every later e must still be delivered.
                "GPUI: a down before composing, its up while composing (Option-e e e)",
                2..5,
                &[
                    (
                        (D, ALT_LEFT, MODIFIER_FLAG_ALT_ANY, "", ""),
                        &["mods alt", "uikit"],
                    ),
                    (
                        (D, KEY_E, MODIFIER_FLAG_ALT_ANY, "´", "e"),
                        &["down alt-e Some(\"´\")", "uikit"],
                    ),
                    (
                        (U, KEY_E, MODIFIER_FLAG_ALT_ANY, "", ""),
                        &["up alt-e", "uikit"],
                    ),
                    ((U, ALT_LEFT, NONE, "", ""), &["mods none", "uikit"]),
                    ((D, KEY_E, NONE, "e", "e"), &["uikit"]),
                    ((U, KEY_E, NONE, "", ""), &["uikit"]),
                    ((D, KEY_E, NONE, "e", "e"), &["down e Some(\"e\")", "uikit"]),
                    ((U, KEY_E, NONE, "", ""), &["up e", "uikit"]),
                ],
            ),
            (
                // Romaji: k composes, Left moves in the marked text, Enter commits; Left's up comes after the
                // commit and goes to UIKit, which had its down, not to GPUI, which did not.
                "GPUI: a down while composing, its up after the commit",
                0..4,
                &[
                    ((D, KEY_K, NONE, "k", "k"), &["uikit"]),
                    ((U, KEY_K, NONE, "", ""), &["uikit"]),
                    (
                        (
                            D,
                            KEY_LEFT,
                            NONE,
                            "UIKeyInputLeftArrow",
                            "UIKeyInputLeftArrow",
                        ),
                        &["uikit"],
                    ),
                    ((D, KEY_ENTER, NONE, "\r", "\r"), &["uikit"]),
                    ((U, KEY_ENTER, NONE, "", ""), &["uikit"]),
                    ((U, KEY_LEFT, NONE, "", ""), &["uikit"]),
                    (
                        (
                            D,
                            KEY_LEFT,
                            NONE,
                            "UIKeyInputLeftArrow",
                            "UIKeyInputLeftArrow",
                        ),
                        &["down left None", "repeat left"],
                    ),
                    ((U, KEY_LEFT, NONE, "", ""), &["up left", "repeat stops"]),
                ],
            ),
        ];
        let all = rows
            .iter()
            .map(|(name, steps)| (*name, 0..0, *steps))
            .chain(ime_rows.iter().cloned());
        let mut failures = Vec::new();
        for (name, ime, steps) in all {
            let mut responder = KeyResponder::default();
            for (index, ((phase, key_code, modifier_flags, characters, ignoring), expected)) in
                steps.iter().enumerate()
            {
                let press = Press {
                    phase: *phase,
                    key_code: *key_code,
                    modifier_flags: *modifier_flags,
                    characters,
                    characters_ignoring_modifiers: ignoring,
                };
                let got = step(&mut responder, &press, ime.contains(&index));
                if got != *expected {
                    failures.push(format!(
                        "{name}, press {index}: got {got:?}, want {expected:?}"
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}
