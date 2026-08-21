//! The persisted client settings, shared by both frontends.
//!
//! This is `~/.config/irontsc/default.rdp`, in the same `.rdp` format mstsc writes, plus a few
//! `irontsc:`-prefixed keys of our own. It lives here rather than inside either frontend
//! because the parser has a subtlety worth having exactly one copy of: a line is
//! `name:type:value` and *both* halves can contain colons -- `irontsc:h264_hw_accel:i:1` in
//! the name, `full address:s:host:3389` in the value -- so the split has to be made at the
//! first `:i:`, `:s:` or `:b:` type marker rather than at a fixed colon.
//!
//! Extracted from the GTK client unchanged; this is a move, not a rewrite.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_RDP_FILE: &str = "default.rdp";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resolution {
    R640x480,
    R800x600,
    R1024x768,
    R1920x1080,
    Fullscreen,
}

impl Resolution {
    pub fn to_dimensions(&self) -> Option<(u16, u16)> {
        match self {
            Resolution::R640x480 => Some((640, 480)),
            Resolution::R800x600 => Some((800, 600)),
            Resolution::R1024x768 => Some((1024, 768)),
            Resolution::R1920x1080 => Some((1920, 1080)),
            Resolution::Fullscreen => None, // Will be determined at runtime
        }
    }

    pub fn from_dimensions(width: u16, height: u16) -> Self {
        match (width, height) {
            (640, 480) => Resolution::R640x480,
            (800, 600) => Resolution::R800x600,
            (1024, 768) => Resolution::R1024x768,
            (1920, 1080) => Resolution::R1920x1080,
            _ => Resolution::R1024x768, // Default
        }
    }

    pub fn to_string(&self) -> &'static str {
        match self {
            Resolution::R640x480 => "640x480",
            Resolution::R800x600 => "800x600",
            Resolution::R1024x768 => "1024x768",
            Resolution::R1920x1080 => "1920x1080",
            Resolution::Fullscreen => "Full screen",
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            0 => Resolution::R640x480,
            1 => Resolution::R800x600,
            2 => Resolution::R1024x768,
            3 => Resolution::R1920x1080,
            4 => Resolution::Fullscreen,
            _ => Resolution::R1024x768,
        }
    }

    pub fn to_index(&self) -> usize {
        match self {
            Resolution::R640x480 => 0,
            Resolution::R800x600 => 1,
            Resolution::R1024x768 => 2,
            Resolution::R1920x1080 => 3,
            Resolution::Fullscreen => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorDepth {
    Bpp15, // High Color (15 bit)
    Bpp16, // High Color (16 bit)
    Bpp24, // True color (24 bit)
    Bpp32, // Highest quality (32 bit)
}

pub const DPI_SCALE_OPTIONS: &[(Option<u32>, &str)] = &[
    (None, "Current screen"),
    (Some(100), "100%"),
    (Some(125), "125%"),
    (Some(150), "150%"),
    (Some(175), "175%"),
    (Some(200), "200%"),
    (Some(225), "225%"),
    (Some(250), "250%"),
    (Some(300), "300%"),
    (Some(350), "350%"),
    (Some(400), "400%"),
    (Some(450), "450%"),
    (Some(500), "500%"),
];

pub fn dpi_value_from_index(index: u32) -> Option<u32> {
    DPI_SCALE_OPTIONS
        .get(index as usize)
        .map(|(value, _)| *value)
        .unwrap_or(None)
}

pub fn dpi_index_from_value(value: Option<u32>) -> u32 {
    DPI_SCALE_OPTIONS
        .iter()
        .position(|(candidate, _)| *candidate == value)
        .unwrap_or(0) as u32
}

pub fn is_supported_dpi_value(value: u32) -> bool {
    DPI_SCALE_OPTIONS
        .iter()
        .any(|(candidate, _)| candidate.map(|v| v == value).unwrap_or(false))
}

impl ColorDepth {
    pub fn to_bpp(&self) -> u16 {
        match self {
            ColorDepth::Bpp15 => 15,
            ColorDepth::Bpp16 => 16,
            ColorDepth::Bpp24 => 24,
            ColorDepth::Bpp32 => 32,
        }
    }

    pub fn from_bpp(bpp: u16) -> Self {
        match bpp {
            15 => ColorDepth::Bpp15,
            16 => ColorDepth::Bpp16,
            24 => ColorDepth::Bpp24,
            32 | _ => ColorDepth::Bpp32,
        }
    }

    pub fn to_string(&self) -> &'static str {
        match self {
            ColorDepth::Bpp15 => "High Color (15 bit)",
            ColorDepth::Bpp16 => "High Color (16 bit)",
            ColorDepth::Bpp24 => "True color (24 bit)",
            ColorDepth::Bpp32 => "Highest quality (32 bit)",
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            0 => ColorDepth::Bpp32,
            1 => ColorDepth::Bpp24,
            2 => ColorDepth::Bpp16,
            3 => ColorDepth::Bpp15,
            _ => ColorDepth::Bpp32,
        }
    }

    pub fn to_index(&self) -> usize {
        match self {
            ColorDepth::Bpp32 => 0,
            ColorDepth::Bpp24 => 1,
            ColorDepth::Bpp16 => 2,
            ColorDepth::Bpp15 => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RdpSettings {
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub save_password: bool,
    #[serde(default = "default_width")]
    pub desktopwidth: u16,
    #[serde(default = "default_height")]
    pub desktopheight: u16,
    #[serde(default = "default_session_bpp")]
    pub session_bpp: u16,
    #[serde(default)]
    pub full_screen: bool,
    #[serde(default)]
    pub dpi_scaling: Option<u32>,
    #[serde(default)]
    pub h264_hw_accel: bool,
    #[serde(default)]
    pub disable_avc420: bool,
    #[serde(default)]
    pub disable_avc444: bool,
    #[serde(default)]
    pub disable_udp: bool,
    #[serde(default)]
    pub show_codec_grid: bool,
    /// Connect without CredSSP, which is the only way to reach a server that authenticates
    /// against its host: NLA requires the server to know the password in advance, so a server
    /// verifying credentials itself has to be spoken to under plain TLS.
    #[serde(default)]
    pub disable_nla: bool,
}

fn default_width() -> u16 {
    1024
}
fn default_height() -> u16 {
    768
}
fn default_session_bpp() -> u16 {
    32
}

impl Default for RdpSettings {
    fn default() -> Self {
        Self {
            server: String::new(),
            username: String::new(),
            domain: String::new(),
            password: String::new(),
            save_password: false,
            desktopwidth: 1024,
            desktopheight: 768,
            session_bpp: 32,
            full_screen: false,
            dpi_scaling: None,
            // The decoder tries each hardware backend in turn and falls back to software if
            // none initialise, so defaulting this on costs nothing where it is unavailable.
            h264_hw_accel: true,
            disable_avc420: false,
            disable_avc444: false,
            disable_udp: false,
            show_codec_grid: false,
            disable_nla: false,
        }
    }
}

impl RdpSettings {
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        Self::parse_rdp(&contents)
    }

    pub fn load_default() -> Self {
        if let Some(dir) = Self::config_dir() {
            let path = dir.join(DEFAULT_RDP_FILE);
            if let Ok(settings) = Self::load_from_file(&path) {
                return settings;
            }
        }

        Self::default()
    }

    pub fn parse_rdp(contents: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut settings = Self::default();

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || !line.contains(':') {
                continue;
            }

            // An .rdp line is `name:type:value`, and both halves may contain colons: the name
            // does for our own `irontsc:` settings, and the value does for `full address` with
            // a port. Splitting on the first two colons mis-parses the former and splitting on
            // the last two mis-parses the latter, so split at the first type marker instead.
            let Some((separator, marker_len)) = [":i:", ":s:", ":b:"]
                .iter()
                .filter_map(|marker| line.find(marker).map(|at| (at, marker.len())))
                .min_by_key(|(at, _)| *at)
            else {
                continue;
            };

            let key = &line[..separator];
            let value = &line[separator + marker_len..];

            match key {
                "full address" => settings.server = value.to_string(),
                "username" => settings.username = value.to_string(),
                "domain" => settings.domain = value.to_string(),
                "password 51" => {
                    settings.password = value.to_string();
                    settings.save_password = true;
                }
                "desktopwidth" => {
                    if let Ok(w) = value.parse() {
                        settings.desktopwidth = w;
                    }
                }
                "desktopheight" => {
                    if let Ok(h) = value.parse() {
                        settings.desktopheight = h;
                    }
                }
                "session bpp" => {
                    if let Ok(bpp) = value.parse() {
                        settings.session_bpp = bpp;
                    }
                }
                "desktopscalefactor" | "devicescalefactor" => {
                    if let Ok(scale) = value.parse::<u32>() {
                        if scale == 0 {
                            settings.dpi_scaling = None;
                        } else if is_supported_dpi_value(scale) {
                            settings.dpi_scaling = Some(scale);
                        }
                    }
                }
                "screen mode id" => {
                    if let Ok(mode) = value.parse::<u8>() {
                        settings.full_screen = mode == 2;
                    }
                }
                "irontsc:h264_hw_accel" => {
                    if let Ok(val) = value.parse::<u8>() {
                        settings.h264_hw_accel = val != 0;
                    }
                }
                "irontsc:disable_avc420" => {
                    if let Ok(val) = value.parse::<u8>() {
                        settings.disable_avc420 = val != 0;
                    }
                }
                "irontsc:disable_avc444" => {
                    if let Ok(val) = value.parse::<u8>() {
                        settings.disable_avc444 = val != 0;
                    }
                }
                "irontsc:disable_udp" => {
                    if let Ok(val) = value.parse::<u8>() {
                        settings.disable_udp = val != 0;
                    }
                }
                "irontsc:show_codec_grid" => {
                    if let Ok(val) = value.parse::<u8>() {
                        settings.show_codec_grid = val != 0;
                    }
                }
                _ => {}
            }
        }

        Ok(settings)
    }

    pub fn to_rdp_format(&self) -> String {
        let mut lines = vec![
            format!("screen mode id:i:{}", if self.full_screen { 2 } else { 1 }),
            "use multimon:i:0".to_string(),
            format!("desktopwidth:i:{}", self.desktopwidth),
            format!("desktopheight:i:{}", self.desktopheight),
            format!("session bpp:i:{}", self.session_bpp),
        ];

        if let Some(scale) = self.dpi_scaling {
            lines.push(format!("desktopscalefactor:i:{scale}"));
            lines.push(format!("devicescalefactor:i:{scale}"));
        }

        lines.extend([
            "winposstr:s:0,3,0,0,800,600".to_string(),
            "compression:i:1".to_string(),
            "keyboardhook:i:2".to_string(),
            "audiocapturemode:i:0".to_string(),
            "videoplaybackmode:i:1".to_string(),
            "connection type:i:7".to_string(),
            "networkautodetect:i:1".to_string(),
            "bandwidthautodetect:i:1".to_string(),
            "displayconnectionbar:i:1".to_string(),
            "enableworkspacereconnect:i:0".to_string(),
            "disable wallpaper:i:0".to_string(),
            "allow font smoothing:i:0".to_string(),
            "allow desktop composition:i:0".to_string(),
            "disable full window drag:i:1".to_string(),
            "disable menu anims:i:1".to_string(),
            "disable themes:i:0".to_string(),
            "disable cursor setting:i:0".to_string(),
            "bitmapcachepersistenable:i:1".to_string(),
            format!("full address:s:{}", self.server),
            "audiomode:i:0".to_string(),
            "redirectprinters:i:1".to_string(),
            "redirectcomports:i:0".to_string(),
            "redirectsmartcards:i:1".to_string(),
            "redirectclipboard:i:1".to_string(),
            "redirectposdevices:i:0".to_string(),
            "autoreconnection enabled:i:1".to_string(),
            "authentication level:i:2".to_string(),
            "prompt for credentials:i:0".to_string(),
            "negotiate security layer:i:1".to_string(),
            "remoteapplicationmode:i:0".to_string(),
            "alternate shell:s:".to_string(),
            "shell working directory:s:".to_string(),
            "gatewayhostname:s:".to_string(),
            "gatewayusagemethod:i:4".to_string(),
            "gatewaycredentialssource:i:4".to_string(),
            "gatewayprofileusagemethod:i:0".to_string(),
            "promptcredentialonce:i:0".to_string(),
            "gatewaybrokeringtype:i:0".to_string(),
            "use redirection server name:i:0".to_string(),
            "rdgiskdcproxy:i:0".to_string(),
            "kdcproxyname:s:".to_string(),
            format!("username:s:{}", self.username),
            format!("domain:s:{}", self.domain),
        ]);

        // Add password if save_password is enabled (plaintext for testing)
        if self.save_password && !self.password.is_empty() {
            lines.push(format!("password 51:b:{}", self.password));
        }

        // Add irontsc-specific settings (custom extension)
        lines.push(format!(
            "irontsc:h264_hw_accel:i:{}",
            if self.h264_hw_accel { 1 } else { 0 }
        ));
        lines.push(format!(
            "irontsc:disable_avc420:i:{}",
            if self.disable_avc420 { 1 } else { 0 }
        ));
        lines.push(format!(
            "irontsc:disable_avc444:i:{}",
            if self.disable_avc444 { 1 } else { 0 }
        ));
        lines.push(format!(
            "irontsc:disable_udp:i:{}",
            if self.disable_udp { 1 } else { 0 }
        ));
        lines.push(format!(
            "irontsc:show_codec_grid:i:{}",
            if self.show_codec_grid { 1 } else { 0 }
        ));

        lines.join("\n")
    }

    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(path, self.to_rdp_format())?;
        Ok(())
    }

    pub fn save_as_default(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(dir) = Self::config_dir() {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(DEFAULT_RDP_FILE);
            self.save_to_file(&path)?;
        }

        Ok(())
    }

    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("irontsc"))
    }

    pub fn get_resolution(&self) -> Resolution {
        if self.full_screen {
            Resolution::Fullscreen
        } else {
            Resolution::from_dimensions(self.desktopwidth, self.desktopheight)
        }
    }

    pub fn set_resolution(&mut self, resolution: Resolution) {
        match resolution {
            Resolution::Fullscreen => {
                self.full_screen = true;
            }
            _ => {
                self.full_screen = false;
                if let Some((w, h)) = resolution.to_dimensions() {
                    self.desktopwidth = w;
                    self.desktopheight = h;
                }
            }
        }
    }

    pub fn get_color_depth(&self) -> ColorDepth {
        ColorDepth::from_bpp(self.session_bpp)
    }

    pub fn set_color_depth(&mut self, depth: ColorDepth) {
        self.session_bpp = depth.to_bpp();
    }

    pub fn get_dpi_scaling(&self) -> Option<u32> {
        self.dpi_scaling
    }

    pub fn set_dpi_scaling(&mut self, scaling: Option<u32>) {
        self.dpi_scaling = scaling;
    }

    pub fn get_h264_hw_accel(&self) -> bool {
        self.h264_hw_accel
    }

    pub fn set_h264_hw_accel(&mut self, enabled: bool) {
        self.h264_hw_accel = enabled;
    }

    pub fn get_disable_avc420(&self) -> bool {
        self.disable_avc420
    }

    pub fn set_disable_avc420(&mut self, disabled: bool) {
        self.disable_avc420 = disabled;
    }

    pub fn get_disable_avc444(&self) -> bool {
        self.disable_avc444
    }

    pub fn set_disable_avc444(&mut self, disabled: bool) {
        self.disable_avc444 = disabled;
    }

    pub fn get_disable_udp(&self) -> bool {
        self.disable_udp
    }

    pub fn set_disable_udp(&mut self, disabled: bool) {
        self.disable_udp = disabled;
    }

    pub fn get_show_codec_grid(&self) -> bool {
        self.show_codec_grid
    }

    pub fn set_show_codec_grid(&mut self, enabled: bool) {
        self.show_codec_grid = enabled;
    }
}
