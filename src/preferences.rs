//! What IronTSC itself prefers, as opposed to what one connection wants.
//!
//! [`crate::settings::RdpSettings`] describes a *connection*: this computer, this user, this
//! desktop size, and it lives in a `.rdp` file that mstsc would also recognise. Some of what
//! IronTSC has grown since is not about a connection at all -- the vision endpoint an agent
//! should ask about the screen, the keyboard layout a server reads scancodes against -- and
//! having to set those again for every saved connection is tedious when the answer is the same
//! every time.
//!
//! So they live here as well, in `~/.config/irontsc/preferences.json`, and a connection file
//! overrides them where it says something. The order is the usual one:
//!
//! ```text
//! command line  >  the .rdp being opened  >  these preferences  >  built-in defaults
//! ```
//!
//! JSON rather than the `.rdp` line format: nothing but IronTSC reads this, and a file a person
//! may want to edit by hand is better off in a shape they already know.

use serde::{Deserialize, Serialize};

/// The file the preferences live in, inside the same directory as the default connection.
const PREFERENCES_FILE: &str = "preferences.json";

/// Settings that are about IronTSC rather than about one connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preferences {
    /// An OpenAI-shaped endpoint with a vision model behind it, such as
    /// `http://server:8080/v1` (llama.cpp) or `http://localhost:11434/v1` (Ollama).
    #[serde(default)]
    pub vision_endpoint: String,
    /// The model to ask for. Servers holding a single model ignore it.
    #[serde(default)]
    pub vision_model: String,
    /// The keyboard layout the servers you connect to have active: `us`, `no`. Empty means take
    /// this machine's.
    #[serde(default)]
    pub keyboard_layout: String,
    /// The DPI to tell the server about, as a percentage, instead of this display's own.
    ///
    /// A HiDPI laptop reports 167%, and the remote desktop then renders everything at 167% --
    /// correct for reading over someone's shoulder, and far too large for an agent working in a
    /// 1024- or 1920-wide desktop, where it wastes most of the screen on a handful of controls.
    /// `100` gives a desktop that fits what a 1920x1080 monitor would show. Zero follows the
    /// display, which is what a person watching wants.
    #[serde(default)]
    pub dpi_scale: u32,
}

impl Preferences {
    /// Reads the preferences, or the defaults if there are none yet.
    ///
    /// A file that cannot be parsed is reported and then ignored: losing the vision endpoint is
    /// not a reason to refuse to start.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str(&contents) {
            Ok(preferences) => preferences,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "ignoring unreadable preferences");
                Self::default()
            }
        }
    }

    /// Writes them back, creating the directory if this is the first time.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| {
            std::io::Error::other("no HOME or XDG_CONFIG_HOME to keep preferences in")
        })?;
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("{json}\n"))
    }

    /// Where the file is, which is beside the default `.rdp`.
    pub fn path() -> Option<std::path::PathBuf> {
        crate::settings::RdpSettings::config_dir().map(|dir| dir.join(PREFERENCES_FILE))
    }

    /// Fills in whatever a connection did not say for itself.
    ///
    /// Only the keyboard layout is shared this way: it belongs to the server, so a connection
    /// may override it, while the vision endpoint is IronTSC's alone and is never written into
    /// a `.rdp` -- those files are read by mstsc too, and stay as mstsc left them.
    pub fn apply_to(&self, settings: &mut crate::settings::RdpSettings) {
        if settings.keyboard_layout.trim().is_empty() {
            settings.keyboard_layout.clone_from(&self.keyboard_layout);
        }
    }

    /// Where to ask about the screen, if anywhere.
    pub fn vision(&self) -> Option<crate::agent::Vision> {
        crate::agent::Vision::from_settings(&self.vision_endpoint, &self.vision_model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::RdpSettings;

    #[test]
    fn a_connection_keeps_its_own_layout() {
        let preferences = Preferences {
            keyboard_layout: "us".to_owned(),
            ..Preferences::default()
        };
        let mut settings = RdpSettings {
            keyboard_layout: "no".to_owned(),
            ..RdpSettings::default()
        };
        preferences.apply_to(&mut settings);
        assert_eq!(settings.keyboard_layout, "no");
    }

    #[test]
    fn a_silent_connection_takes_the_preferred_layout() {
        let preferences = Preferences {
            keyboard_layout: "us".to_owned(),
            ..Preferences::default()
        };
        let mut settings = RdpSettings::default();
        preferences.apply_to(&mut settings);
        assert_eq!(settings.keyboard_layout, "us");
    }

    #[test]
    fn vision_is_configured_only_when_an_endpoint_is() {
        assert!(Preferences::default().vision().is_none());
        let asked = Preferences {
            vision_endpoint: "http://server:8080/v1".to_owned(),
            ..Preferences::default()
        };
        assert!(asked.vision().is_some());
    }
}
