//! Physical key positions to Windows Set 1 scancodes, for the winit/egui frontend.
//!
//! RDP carries key *positions*, not characters: the server applies its own keyboard layout to
//! whatever scancode arrives, so the client must never look at the character a key produces.
//! The GTK frontend gets X11 keycodes (Linux evdev + 8) from GDK and translates those in
//! `GtkRdpWidget::keycode_to_scancode`; winit reports the same physical positions as
//! `KeyCode`, the W3C UI Events code, which is already platform independent. This table is
//! therefore the same mapping as the GTK one, keyed by a portable name instead of an evdev
//! number -- and it is what makes the frontend work unchanged on Wayland, Windows and macOS.
//!
//! A `0xE0` high byte marks the "extended" scancodes; `Scancode::from_u16` splits that off
//! into the extended flag for us.

use ironrdp::input::Scancode;
use winit::keyboard::KeyCode;

/// Translates a physical key position into the scancode the RDP server expects.
///
/// Returns `None` for keys with no Set 1 equivalent, which are then simply not forwarded.
pub fn scancode_for(code: KeyCode) -> Option<Scancode> {
    let raw: u16 = match code {
        // Row 1: escape and function keys.
        KeyCode::Escape => 0x01,
        KeyCode::F1 => 0x3B,
        KeyCode::F2 => 0x3C,
        KeyCode::F3 => 0x3D,
        KeyCode::F4 => 0x3E,
        KeyCode::F5 => 0x3F,
        KeyCode::F6 => 0x40,
        KeyCode::F7 => 0x41,
        KeyCode::F8 => 0x42,
        KeyCode::F9 => 0x43,
        KeyCode::F10 => 0x44,
        KeyCode::F11 => 0x57,
        KeyCode::F12 => 0x58,
        KeyCode::F13 => 0x64,
        KeyCode::F14 => 0x65,
        KeyCode::F15 => 0x66,
        KeyCode::F16 => 0x67,
        KeyCode::F17 => 0x68,
        KeyCode::F18 => 0x69,
        KeyCode::F19 => 0x6A,
        KeyCode::F20 => 0x6B,
        KeyCode::F21 => 0x6C,
        KeyCode::F22 => 0x6D,
        KeyCode::F23 => 0x6E,
        KeyCode::F24 => 0x6F,

        // Number row.
        KeyCode::Backquote => 0x29,
        KeyCode::Digit1 => 0x02,
        KeyCode::Digit2 => 0x03,
        KeyCode::Digit3 => 0x04,
        KeyCode::Digit4 => 0x05,
        KeyCode::Digit5 => 0x06,
        KeyCode::Digit6 => 0x07,
        KeyCode::Digit7 => 0x08,
        KeyCode::Digit8 => 0x09,
        KeyCode::Digit9 => 0x0A,
        KeyCode::Digit0 => 0x0B,
        KeyCode::Minus => 0x0C,
        KeyCode::Equal => 0x0D,
        KeyCode::Backspace => 0x0E,

        // Top letter row.
        KeyCode::Tab => 0x0F,
        KeyCode::KeyQ => 0x10,
        KeyCode::KeyW => 0x11,
        KeyCode::KeyE => 0x12,
        KeyCode::KeyR => 0x13,
        KeyCode::KeyT => 0x14,
        KeyCode::KeyY => 0x15,
        KeyCode::KeyU => 0x16,
        KeyCode::KeyI => 0x17,
        KeyCode::KeyO => 0x18,
        KeyCode::KeyP => 0x19,
        KeyCode::BracketLeft => 0x1A,
        KeyCode::BracketRight => 0x1B,
        KeyCode::Enter => 0x1C,

        // Home row.
        KeyCode::CapsLock => 0x3A,
        KeyCode::KeyA => 0x1E,
        KeyCode::KeyS => 0x1F,
        KeyCode::KeyD => 0x20,
        KeyCode::KeyF => 0x21,
        KeyCode::KeyG => 0x22,
        KeyCode::KeyH => 0x23,
        KeyCode::KeyJ => 0x24,
        KeyCode::KeyK => 0x25,
        KeyCode::KeyL => 0x26,
        KeyCode::Semicolon => 0x27,
        KeyCode::Quote => 0x28,
        KeyCode::Backslash => 0x2B,

        // Bottom letter row. `IntlBackslash` is the extra key ISO keyboards squeeze in between
        // the left Shift and Z, which US ANSI boards do not have.
        KeyCode::ShiftLeft => 0x2A,
        KeyCode::IntlBackslash => 0x56,
        KeyCode::KeyZ => 0x2C,
        KeyCode::KeyX => 0x2D,
        KeyCode::KeyC => 0x2E,
        KeyCode::KeyV => 0x2F,
        KeyCode::KeyB => 0x30,
        KeyCode::KeyN => 0x31,
        KeyCode::KeyM => 0x32,
        KeyCode::Comma => 0x33,
        KeyCode::Period => 0x34,
        KeyCode::Slash => 0x35,
        KeyCode::ShiftRight => 0x36,

        // Modifier row. Right Alt has to stay distinct from left Alt: on the European layouts
        // the server applies, AltGr is where `@`, `$`, `{` and `[` live.
        KeyCode::ControlLeft => 0x1D,
        KeyCode::ControlRight => 0xE01D,
        KeyCode::AltLeft => 0x38,
        KeyCode::AltRight => 0xE038,
        KeyCode::Space => 0x39,
        KeyCode::SuperLeft => 0xE05B,
        KeyCode::SuperRight => 0xE05C,
        KeyCode::ContextMenu => 0xE05D,

        // Navigation cluster. These share their code with the numpad keys of the same name and
        // are told apart purely by the extended flag.
        KeyCode::Home => 0xE047,
        KeyCode::ArrowUp => 0xE048,
        KeyCode::PageUp => 0xE049,
        KeyCode::ArrowLeft => 0xE04B,
        KeyCode::ArrowRight => 0xE04D,
        KeyCode::End => 0xE04F,
        KeyCode::ArrowDown => 0xE050,
        KeyCode::PageDown => 0xE051,
        KeyCode::Insert => 0xE052,
        KeyCode::Delete => 0xE053,

        // Numpad.
        KeyCode::NumLock => 0x45,
        KeyCode::NumpadDivide => 0xE035,
        KeyCode::NumpadMultiply | KeyCode::NumpadStar => 0x37,
        KeyCode::NumpadSubtract => 0x4A,
        KeyCode::NumpadAdd => 0x4E,
        KeyCode::NumpadEnter => 0xE01C,
        KeyCode::Numpad7 => 0x47,
        KeyCode::Numpad8 => 0x48,
        KeyCode::Numpad9 => 0x49,
        KeyCode::Numpad4 => 0x4B,
        KeyCode::Numpad5 => 0x4C,
        KeyCode::Numpad6 => 0x4D,
        KeyCode::Numpad1 => 0x4F,
        KeyCode::Numpad2 => 0x50,
        KeyCode::Numpad3 => 0x51,
        KeyCode::Numpad0 => 0x52,
        KeyCode::NumpadDecimal => 0x53,
        KeyCode::NumpadComma => 0x7E,
        KeyCode::NumpadEqual => 0x59,

        // System keys. Pause really is a two-code sequence (E1 1D 45 ...) that the fast-path
        // encoding here cannot express; 0xE05F is what the GTK frontend sends, so keep the two
        // clients behaving the same rather than inventing a third answer.
        KeyCode::ScrollLock => 0x46,
        KeyCode::PrintScreen => 0xE037,
        KeyCode::Pause => 0xE05F,

        // Japanese keyboards.
        KeyCode::IntlRo => 0x73,
        KeyCode::IntlYen => 0x7D,
        KeyCode::KanaMode => 0x70,
        KeyCode::Convert => 0x79,
        KeyCode::NonConvert => 0x7B,

        // Multimedia and browser keys.
        KeyCode::MediaTrackPrevious => 0xE010,
        KeyCode::MediaTrackNext => 0xE019,
        KeyCode::AudioVolumeMute => 0xE020,
        KeyCode::LaunchApp2 => 0xE021,
        KeyCode::MediaPlayPause => 0xE022,
        KeyCode::MediaStop => 0xE024,
        KeyCode::AudioVolumeDown => 0xE02E,
        KeyCode::AudioVolumeUp => 0xE030,
        KeyCode::BrowserHome => 0xE032,
        KeyCode::BrowserSearch => 0xE065,
        KeyCode::BrowserFavorites => 0xE066,
        KeyCode::BrowserRefresh => 0xE067,
        KeyCode::BrowserStop => 0xE068,
        KeyCode::BrowserForward => 0xE069,
        KeyCode::BrowserBack => 0xE06A,
        KeyCode::LaunchApp1 => 0xE06B,
        KeyCode::LaunchMail => 0xE06C,
        KeyCode::MediaSelect => 0xE06D,

        _ => return None,
    };

    Some(Scancode::from_u16(raw))
}
