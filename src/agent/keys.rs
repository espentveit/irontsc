//! Key chord parsing for MCP mode.
//!
//! An agent names keys, not positions: it asks for `"ctrl+alt+delete"` or `"win+r"`, whereas
//! RDP carries the *scancode* of a physical key. The translation from a physical position to
//! a scancode already exists in [`crate::egui_scancode`] for the winit frontend, so this
//! module only has to turn a name into the `KeyCode` that table is keyed by. Doing it that
//! way means there is one scancode table in the tree rather than two that can drift apart.
//!
//! Names are matched case-insensitively, and the common aliases an agent is likely to reach
//! for (`esc`, `del`, `pgup`, `return`, `cmd`) are accepted alongside the W3C code names.

use ironrdp::input::Scancode;
use winit::keyboard::KeyCode;

/// A parsed chord: some modifiers held down while one key is tapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Modifier keys to press before the main key and release after it, in order.
    pub modifiers: Vec<KeyCode>,
    /// The key the chord is actually about.
    pub key: KeyCode,
}

impl Chord {
    /// Scancodes for the modifiers, in press order.
    pub fn modifier_scancodes(&self) -> Vec<Scancode> {
        self.modifiers
            .iter()
            .filter_map(|code| crate::egui_scancode::scancode_for(*code))
            .collect()
    }

    /// Scancode for the main key.
    pub fn key_scancode(&self) -> Option<Scancode> {
        crate::egui_scancode::scancode_for(self.key)
    }
}

/// Parses a chord such as `"ctrl+shift+esc"`, `"win+r"`, `"F5"` or `"Enter"`.
///
/// Everything before the last `+` is treated as a modifier. A bare `"+"` is understood as the
/// plus key so that `"ctrl++"` works.
pub fn parse_chord(input: &str) -> Result<Chord, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty key chord".to_owned());
    }

    // Split on '+' without swallowing a trailing '+' that is itself the key.
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = trimmed;
    while let Some(index) = rest.find('+') {
        // A '+' at the end is the key, not a separator.
        if index + 1 == rest.len() {
            break;
        }
        parts.push(&rest[..index]);
        rest = &rest[index + 1..];
    }
    parts.push(rest);

    let (key_name, modifier_names) = parts
        .split_last()
        .expect("parts always has at least one element");

    let key = key_code_for(key_name)
        .ok_or_else(|| format!("unknown key `{key_name}` in chord `{trimmed}`"))?;

    let mut modifiers = Vec::with_capacity(modifier_names.len());
    for name in modifier_names {
        let code = modifier_code_for(name)
            .ok_or_else(|| format!("unknown modifier `{name}` in chord `{trimmed}`"))?;
        if !modifiers.contains(&code) {
            modifiers.push(code);
        }
    }

    Ok(Chord { modifiers, key })
}

/// Modifiers accept the same names as keys, plus the side-less aliases.
fn modifier_code_for(name: &str) -> Option<KeyCode> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" => Some(KeyCode::ControlLeft),
        "alt" | "option" => Some(KeyCode::AltLeft),
        "altgr" | "rightalt" => Some(KeyCode::AltRight),
        "shift" => Some(KeyCode::ShiftLeft),
        "win" | "super" | "meta" | "cmd" | "command" => Some(KeyCode::SuperLeft),
        other => key_code_for(other),
    }
}

/// Translates a key name into the physical position winit would report for it.
fn key_code_for(name: &str) -> Option<KeyCode> {
    let lowered = name.trim().to_ascii_lowercase();

    // Single letters and digits are by far the common case.
    if lowered.chars().count() == 1 {
        let character = lowered.chars().next().expect("one character");
        if character.is_ascii_lowercase() {
            return letter_code(character);
        }
        if character.is_ascii_digit() {
            return digit_code(character);
        }
    }

    // Function keys.
    if let Some(number) = lowered.strip_prefix('f')
        && let Ok(index) = number.parse::<u8>()
        && (1..=24).contains(&index)
    {
        return function_code(index);
    }

    // Numpad keys, spelled `numpad0` .. `numpad9`.
    if let Some(number) = lowered.strip_prefix("numpad")
        && let Ok(digit) = number.parse::<u8>()
        && digit <= 9
    {
        return numpad_code(digit);
    }

    Some(match lowered.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "numpadenter" => KeyCode::NumpadEnter,
        "tab" => KeyCode::Tab,
        "space" | "spacebar" => KeyCode::Space,
        "backspace" | "bksp" => KeyCode::Backspace,
        "escape" | "esc" => KeyCode::Escape,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" | "arrowup" => KeyCode::ArrowUp,
        "down" | "arrowdown" => KeyCode::ArrowDown,
        "left" | "arrowleft" => KeyCode::ArrowLeft,
        "right" | "arrowright" => KeyCode::ArrowRight,
        "capslock" | "caps" => KeyCode::CapsLock,
        "numlock" => KeyCode::NumLock,
        "scrolllock" => KeyCode::ScrollLock,
        "printscreen" | "prtsc" | "print" => KeyCode::PrintScreen,
        "pause" | "break" => KeyCode::Pause,
        "menu" | "apps" | "contextmenu" => KeyCode::ContextMenu,

        "controlleft" | "leftctrl" => KeyCode::ControlLeft,
        "controlright" | "rightctrl" => KeyCode::ControlRight,
        "altleft" | "leftalt" => KeyCode::AltLeft,
        "altright" => KeyCode::AltRight,
        "shiftleft" | "leftshift" => KeyCode::ShiftLeft,
        "shiftright" | "rightshift" => KeyCode::ShiftRight,
        "superleft" | "winleft" | "leftwin" => KeyCode::SuperLeft,
        "superright" | "winright" | "rightwin" => KeyCode::SuperRight,

        "minus" | "-" => KeyCode::Minus,
        "equal" | "=" => KeyCode::Equal,
        "plus" | "+" => KeyCode::Equal,
        "bracketleft" | "[" => KeyCode::BracketLeft,
        "bracketright" | "]" => KeyCode::BracketRight,
        "backslash" | "\\" => KeyCode::Backslash,
        "semicolon" | ";" => KeyCode::Semicolon,
        "quote" | "apostrophe" | "'" => KeyCode::Quote,
        "backquote" | "grave" | "`" => KeyCode::Backquote,
        "comma" | "," => KeyCode::Comma,
        "period" | "dot" | "." => KeyCode::Period,
        "slash" | "/" => KeyCode::Slash,

        "numpadadd" => KeyCode::NumpadAdd,
        "numpadsubtract" => KeyCode::NumpadSubtract,
        "numpadmultiply" => KeyCode::NumpadMultiply,
        "numpaddivide" => KeyCode::NumpadDivide,
        "numpaddecimal" => KeyCode::NumpadDecimal,

        _ => return None,
    })
}

fn letter_code(character: char) -> Option<KeyCode> {
    Some(match character {
        'a' => KeyCode::KeyA,
        'b' => KeyCode::KeyB,
        'c' => KeyCode::KeyC,
        'd' => KeyCode::KeyD,
        'e' => KeyCode::KeyE,
        'f' => KeyCode::KeyF,
        'g' => KeyCode::KeyG,
        'h' => KeyCode::KeyH,
        'i' => KeyCode::KeyI,
        'j' => KeyCode::KeyJ,
        'k' => KeyCode::KeyK,
        'l' => KeyCode::KeyL,
        'm' => KeyCode::KeyM,
        'n' => KeyCode::KeyN,
        'o' => KeyCode::KeyO,
        'p' => KeyCode::KeyP,
        'q' => KeyCode::KeyQ,
        'r' => KeyCode::KeyR,
        's' => KeyCode::KeyS,
        't' => KeyCode::KeyT,
        'u' => KeyCode::KeyU,
        'v' => KeyCode::KeyV,
        'w' => KeyCode::KeyW,
        'x' => KeyCode::KeyX,
        'y' => KeyCode::KeyY,
        'z' => KeyCode::KeyZ,
        _ => return None,
    })
}

fn digit_code(character: char) -> Option<KeyCode> {
    Some(match character {
        '0' => KeyCode::Digit0,
        '1' => KeyCode::Digit1,
        '2' => KeyCode::Digit2,
        '3' => KeyCode::Digit3,
        '4' => KeyCode::Digit4,
        '5' => KeyCode::Digit5,
        '6' => KeyCode::Digit6,
        '7' => KeyCode::Digit7,
        '8' => KeyCode::Digit8,
        '9' => KeyCode::Digit9,
        _ => return None,
    })
}

fn function_code(index: u8) -> Option<KeyCode> {
    Some(match index {
        1 => KeyCode::F1,
        2 => KeyCode::F2,
        3 => KeyCode::F3,
        4 => KeyCode::F4,
        5 => KeyCode::F5,
        6 => KeyCode::F6,
        7 => KeyCode::F7,
        8 => KeyCode::F8,
        9 => KeyCode::F9,
        10 => KeyCode::F10,
        11 => KeyCode::F11,
        12 => KeyCode::F12,
        13 => KeyCode::F13,
        14 => KeyCode::F14,
        15 => KeyCode::F15,
        16 => KeyCode::F16,
        17 => KeyCode::F17,
        18 => KeyCode::F18,
        19 => KeyCode::F19,
        20 => KeyCode::F20,
        21 => KeyCode::F21,
        22 => KeyCode::F22,
        23 => KeyCode::F23,
        24 => KeyCode::F24,
        _ => return None,
    })
}

fn numpad_code(digit: u8) -> Option<KeyCode> {
    Some(match digit {
        0 => KeyCode::Numpad0,
        1 => KeyCode::Numpad1,
        2 => KeyCode::Numpad2,
        3 => KeyCode::Numpad3,
        4 => KeyCode::Numpad4,
        5 => KeyCode::Numpad5,
        6 => KeyCode::Numpad6,
        7 => KeyCode::Numpad7,
        8 => KeyCode::Numpad8,
        9 => KeyCode::Numpad9,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_key() {
        let chord = parse_chord("Enter").expect("enter parses");
        assert!(chord.modifiers.is_empty());
        assert_eq!(chord.key, KeyCode::Enter);
    }

    #[test]
    fn parses_modifiers_in_order() {
        let chord = parse_chord("ctrl+shift+esc").expect("chord parses");
        assert_eq!(
            chord.modifiers,
            vec![KeyCode::ControlLeft, KeyCode::ShiftLeft]
        );
        assert_eq!(chord.key, KeyCode::Escape);
    }

    #[test]
    fn treats_a_trailing_plus_as_the_key() {
        let chord = parse_chord("ctrl++").expect("chord parses");
        assert_eq!(chord.modifiers, vec![KeyCode::ControlLeft]);
        assert_eq!(chord.key, KeyCode::Equal);
    }

    #[test]
    fn is_case_insensitive_and_accepts_aliases() {
        assert_eq!(parse_chord("WIN+R").expect("chord").key, KeyCode::KeyR);
        assert_eq!(
            parse_chord("WIN+R").expect("chord").modifiers,
            vec![KeyCode::SuperLeft]
        );
        assert_eq!(parse_chord("PgDn").expect("chord").key, KeyCode::PageDown);
    }

    #[test]
    fn every_chord_key_has_a_scancode() {
        for name in ["ctrl+alt+delete", "win+r", "F5", "a", "numpad7", "alt+tab"] {
            let chord = parse_chord(name).unwrap_or_else(|_| panic!("`{name}` parses"));
            assert!(chord.key_scancode().is_some(), "`{name}` has a scancode");
            assert_eq!(
                chord.modifier_scancodes().len(),
                chord.modifiers.len(),
                "`{name}` modifiers all have scancodes"
            );
        }
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_chord("").is_err());
        assert!(parse_chord("ctrl+nope").is_err());
    }
}
