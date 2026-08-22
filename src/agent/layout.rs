//! Turning characters into the keys that produce them.
//!
//! RDP can carry a character two ways. A *Unicode* keyboard event names the character itself
//! and lets the server work out the rest, which is how [`super::session::AgentSession`] used
//! to type everything: no layout to agree on, no dead keys, no AltGr. It has one flaw, and it
//! is a big one -- the Windows console does not read Unicode events at all. Typing a command
//! into PowerShell over RDP silently produced nothing, while `key enter` in the same window
//! worked, because a chord goes out as a *scancode*.
//!
//! So characters go out as scancodes too, which means knowing where they sit on the keyboard,
//! which means knowing the layout: a scancode is a position, and the server decides what that
//! position means. The server never tells us which layout it has active, so the layout is
//! [detected][KeyboardLayout::detect] from this machine's and can be overridden in the
//! settings. Anything the table cannot produce still falls back to a Unicode event, which is
//! right everywhere except that console.

use winit::keyboard::KeyCode;

/// The keyboard layout the *server* has active, which is what scancodes are read against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardLayout {
    #[default]
    UnitedStates,
    Norwegian,
}

/// The keys to press to produce one character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keystroke {
    /// The physical key, as the position winit would report.
    pub key: KeyCode,
    pub shift: bool,
    /// Right Alt, which Windows reads as AltGr.
    pub altgr: bool,
    /// A dead key: it composes with what follows, so a space is needed to release it as a
    /// character of its own.
    pub dead: bool,
}

impl Keystroke {
    pub(super) const fn plain(key: KeyCode) -> Self {
        Self { key, shift: false, altgr: false, dead: false }
    }

    const fn shifted(key: KeyCode) -> Self {
        Self { key, shift: true, altgr: false, dead: false }
    }

    const fn altgr(key: KeyCode) -> Self {
        Self { key, shift: false, altgr: true, dead: false }
    }

    const fn dead(self) -> Self {
        Self { dead: true, ..self }
    }
}

impl KeyboardLayout {
    /// Parses a layout name, as XKB or a settings file spells it.
    pub fn parse(name: &str) -> Option<Self> {
        // XKB names a layout `no`, a variant `no(nodeadkeys)`, and a list `no,us`; the first
        // entry is the one that is active at logon, which is the one that matters here.
        let first = name
            .split(',')
            .next()
            .unwrap_or_default()
            .split('(')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();

        match first.as_str() {
            "us" | "en" | "en-us" | "english" | "united states" => Some(Self::UnitedStates),
            "no" | "nb" | "nn" | "nb-no" | "norwegian" | "norsk" => Some(Self::Norwegian),
            _ => None,
        }
    }

    /// The name the settings file uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::UnitedStates => "us",
            Self::Norwegian => "no",
        }
    }

    /// The layout to use, given whatever the settings say.
    ///
    /// An empty setting means "work it out", which is the default.
    pub fn resolve(configured: &str) -> Self {
        if configured.trim().is_empty() {
            return Self::detect();
        }
        match Self::parse(configured) {
            Some(layout) => layout,
            None => {
                let detected = Self::detect();
                tracing::warn!(
                    layout = configured,
                    using = detected.name(),
                    "unknown keyboard layout in the settings"
                );
                detected
            }
        }
    }

    /// Guesses the server's layout from this machine's.
    ///
    /// The guess is better than it sounds: the window sends the physical position of whatever
    /// the user pressed, so the two layouts already have to agree for their own typing to come
    /// out right. Where they do not, the settings say so.
    pub fn detect() -> Self {
        if let Ok(value) = std::env::var("IRONTSC_KEYBOARD_LAYOUT")
            && let Some(layout) = Self::parse(&value)
        {
            return layout;
        }
        if let Ok(value) = std::env::var("XKB_DEFAULT_LAYOUT")
            && let Some(layout) = Self::parse(&value)
        {
            return layout;
        }
        // Where the X11 layout lives on a Debian-family machine, and the file console-setup
        // keeps up to date.
        if let Ok(contents) = std::fs::read_to_string("/etc/default/keyboard") {
            for line in contents.lines() {
                if let Some(value) = line.trim().strip_prefix("XKBLAYOUT=")
                    && let Some(layout) = Self::parse(value.trim_matches('"'))
                {
                    return layout;
                }
            }
        }
        Self::default()
    }

    /// The keys that produce `character`, or `None` for anything not on this layout.
    pub fn keystroke(self, character: char) -> Option<Keystroke> {
        // Letters, digits and space sit in the same places on both layouts.
        if let Some(shared) = shared_keystroke(character) {
            return Some(shared);
        }
        match self {
            Self::UnitedStates => united_states(character),
            Self::Norwegian => norwegian(character),
        }
    }
}

/// The part every Latin layout agrees on.
fn shared_keystroke(character: char) -> Option<Keystroke> {
    if let Some(lowered) = character.to_lowercase().next()
        && character.is_ascii_alphabetic()
    {
        let key = letter_key(lowered)?;
        return Some(if character.is_ascii_uppercase() {
            Keystroke::shifted(key)
        } else {
            Keystroke::plain(key)
        });
    }
    if character.is_ascii_digit() {
        return Some(Keystroke::plain(digit_key(character)?));
    }
    if character == ' ' {
        return Some(Keystroke::plain(KeyCode::Space));
    }
    None
}

fn united_states(character: char) -> Option<Keystroke> {
    Some(match character {
        '!' => Keystroke::shifted(KeyCode::Digit1),
        '@' => Keystroke::shifted(KeyCode::Digit2),
        '#' => Keystroke::shifted(KeyCode::Digit3),
        '$' => Keystroke::shifted(KeyCode::Digit4),
        '%' => Keystroke::shifted(KeyCode::Digit5),
        '^' => Keystroke::shifted(KeyCode::Digit6),
        '&' => Keystroke::shifted(KeyCode::Digit7),
        '*' => Keystroke::shifted(KeyCode::Digit8),
        '(' => Keystroke::shifted(KeyCode::Digit9),
        ')' => Keystroke::shifted(KeyCode::Digit0),
        '-' => Keystroke::plain(KeyCode::Minus),
        '_' => Keystroke::shifted(KeyCode::Minus),
        '=' => Keystroke::plain(KeyCode::Equal),
        '+' => Keystroke::shifted(KeyCode::Equal),
        '[' => Keystroke::plain(KeyCode::BracketLeft),
        '{' => Keystroke::shifted(KeyCode::BracketLeft),
        ']' => Keystroke::plain(KeyCode::BracketRight),
        '}' => Keystroke::shifted(KeyCode::BracketRight),
        '\\' => Keystroke::plain(KeyCode::Backslash),
        '|' => Keystroke::shifted(KeyCode::Backslash),
        ';' => Keystroke::plain(KeyCode::Semicolon),
        ':' => Keystroke::shifted(KeyCode::Semicolon),
        '\'' => Keystroke::plain(KeyCode::Quote),
        '"' => Keystroke::shifted(KeyCode::Quote),
        '`' => Keystroke::plain(KeyCode::Backquote),
        '~' => Keystroke::shifted(KeyCode::Backquote),
        ',' => Keystroke::plain(KeyCode::Comma),
        '<' => Keystroke::shifted(KeyCode::Comma),
        '.' => Keystroke::plain(KeyCode::Period),
        '>' => Keystroke::shifted(KeyCode::Period),
        '/' => Keystroke::plain(KeyCode::Slash),
        '?' => Keystroke::shifted(KeyCode::Slash),
        _ => return None,
    })
}

/// The Norwegian layout, as Windows lays it out: the top row runs `| 1 2 3 4 5 6 7 8 9 0 + \`,
/// and the brackets and braces are all on AltGr.
fn norwegian(character: char) -> Option<Keystroke> {
    Some(match character {
        '!' => Keystroke::shifted(KeyCode::Digit1),
        '"' => Keystroke::shifted(KeyCode::Digit2),
        '@' => Keystroke::altgr(KeyCode::Digit2),
        '#' => Keystroke::shifted(KeyCode::Digit3),
        '£' => Keystroke::altgr(KeyCode::Digit3),
        '¤' => Keystroke::shifted(KeyCode::Digit4),
        '$' => Keystroke::altgr(KeyCode::Digit4),
        '%' => Keystroke::shifted(KeyCode::Digit5),
        '€' => Keystroke::altgr(KeyCode::Digit5),
        '&' => Keystroke::shifted(KeyCode::Digit6),
        '/' => Keystroke::shifted(KeyCode::Digit7),
        '{' => Keystroke::altgr(KeyCode::Digit7),
        '(' => Keystroke::shifted(KeyCode::Digit8),
        '[' => Keystroke::altgr(KeyCode::Digit8),
        ')' => Keystroke::shifted(KeyCode::Digit9),
        ']' => Keystroke::altgr(KeyCode::Digit9),
        '=' => Keystroke::shifted(KeyCode::Digit0),
        '}' => Keystroke::altgr(KeyCode::Digit0),
        '+' => Keystroke::plain(KeyCode::Minus),
        '?' => Keystroke::shifted(KeyCode::Minus),
        '\\' => Keystroke::plain(KeyCode::Equal),
        'å' => Keystroke::plain(KeyCode::BracketLeft),
        'Å' => Keystroke::shifted(KeyCode::BracketLeft),
        'ø' => Keystroke::plain(KeyCode::Semicolon),
        'Ø' => Keystroke::shifted(KeyCode::Semicolon),
        'æ' => Keystroke::plain(KeyCode::Quote),
        'Æ' => Keystroke::shifted(KeyCode::Quote),
        '\'' => Keystroke::plain(KeyCode::Backslash),
        '*' => Keystroke::shifted(KeyCode::Backslash),
        '|' => Keystroke::plain(KeyCode::Backquote),
        '§' => Keystroke::shifted(KeyCode::Backquote),
        ',' => Keystroke::plain(KeyCode::Comma),
        ';' => Keystroke::shifted(KeyCode::Comma),
        '.' => Keystroke::plain(KeyCode::Period),
        ':' => Keystroke::shifted(KeyCode::Period),
        '-' => Keystroke::plain(KeyCode::Slash),
        '_' => Keystroke::shifted(KeyCode::Slash),
        'µ' => Keystroke::altgr(KeyCode::KeyM),
        // The extra key an ISO keyboard has to the left of Z, which a US one does not.
        '<' => Keystroke::plain(KeyCode::IntlBackslash),
        '>' => Keystroke::shifted(KeyCode::IntlBackslash),

        // Dead keys: pressed, they wait for the letter they compose with, so a space is what
        // turns them into the character itself.
        '`' => Keystroke::shifted(KeyCode::Equal).dead(),
        '´' => Keystroke::altgr(KeyCode::Equal).dead(),
        '¨' => Keystroke::plain(KeyCode::BracketRight).dead(),
        '^' => Keystroke::shifted(KeyCode::BracketRight).dead(),
        '~' => Keystroke::altgr(KeyCode::BracketRight).dead(),
        _ => return None,
    })
}

fn letter_key(character: char) -> Option<KeyCode> {
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

fn digit_key(character: char) -> Option<KeyCode> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything an agent is likely to type into a shell.
    const ASCII_PRINTABLE: &str =
        "abcxyzABCXYZ0189 !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

    #[test]
    fn both_layouts_cover_printable_ascii() {
        for layout in [KeyboardLayout::UnitedStates, KeyboardLayout::Norwegian] {
            for character in ASCII_PRINTABLE.chars() {
                assert!(
                    layout.keystroke(character).is_some(),
                    "{} has no key for {character:?}",
                    layout.name()
                );
            }
        }
    }

    #[test]
    fn letters_and_digits_are_shared() {
        for character in "aZ7".chars() {
            assert_eq!(
                KeyboardLayout::UnitedStates.keystroke(character),
                KeyboardLayout::Norwegian.keystroke(character)
            );
        }
    }

    #[test]
    fn shift_makes_capitals() {
        let lower = KeyboardLayout::UnitedStates
            .keystroke('q')
            .expect("q is on the layout");
        let upper = KeyboardLayout::UnitedStates
            .keystroke('Q')
            .expect("Q is on the layout");
        assert_eq!(lower.key, upper.key);
        assert!(!lower.shift && upper.shift);
    }

    /// The positions the live probe read back off a Norwegian server.
    #[test]
    fn norwegian_punctuation_sits_where_the_server_reads_it() {
        let layout = KeyboardLayout::Norwegian;
        assert_eq!(layout.keystroke('+'), Some(Keystroke::plain(KeyCode::Minus)));
        assert_eq!(layout.keystroke('-'), Some(Keystroke::plain(KeyCode::Slash)));
        assert_eq!(layout.keystroke('\\'), Some(Keystroke::plain(KeyCode::Equal)));
        assert_eq!(layout.keystroke('|'), Some(Keystroke::plain(KeyCode::Backquote)));
        assert_eq!(layout.keystroke('/'), Some(Keystroke::shifted(KeyCode::Digit7)));
        assert_eq!(layout.keystroke('_'), Some(Keystroke::shifted(KeyCode::Slash)));
        assert_eq!(layout.keystroke('@'), Some(Keystroke::altgr(KeyCode::Digit2)));
        assert!(layout.keystroke('~').expect("tilde is on the layout").dead);
    }

    #[test]
    fn parses_the_names_xkb_uses() {
        assert_eq!(KeyboardLayout::parse("no"), Some(KeyboardLayout::Norwegian));
        assert_eq!(
            KeyboardLayout::parse("no(nodeadkeys)"),
            Some(KeyboardLayout::Norwegian)
        );
        assert_eq!(
            KeyboardLayout::parse("us,no"),
            Some(KeyboardLayout::UnitedStates)
        );
        assert_eq!(KeyboardLayout::parse("de"), None);
    }
}
