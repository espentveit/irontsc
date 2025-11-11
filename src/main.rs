use glib::prelude::Cast;
use gtk::gio::prelude::ListModelExt;
use gtk::{
    Application, ApplicationWindow, Button, Image, gdk, gdk::prelude::*, glib, glib::ControlFlow,
    prelude::*,
};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

mod config;
mod core_input_channel;
mod dtls_udp;
mod gfx;
mod gfx_channel;
mod h264_codec_caps;
mod mouse_cursor_channel;
mod rdp;
mod stub_dvc;
mod udp_gfx;
mod udp_transport;

// Video Redirection support (MS-RDPEVOR)
#[cfg(feature = "video-redirection")]
mod geometry_channel;
#[cfg(feature = "video-redirection")]
mod video_control_channel;
#[cfg(feature = "video-redirection")]
mod video_data_channel;
#[cfg(feature = "video-redirection")]
mod video_redirect;

use crate::config::{ClipboardType, Config, Destination};
use crate::rdp::{
    ArboardClipboardFactory, DvcPipeProxyFactory, ImageRegion, RdpClient, RdpInputEvent,
    RdpOutputEvent,
};
use ironrdp::cliprdr::backend::CliprdrBackendFactory;

const APP_ID: &str = "org.gtk_rs.IronTsc";
const DEFAULT_RDP_FILE: &str = "default.rdp";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseIntent {
    None,
    User,
    Programmatic,
}

/// Convert Windows NT status code to user-friendly error message
fn get_friendly_error_message(ntstatus_code: u32) -> Option<(&'static str, &'static str)> {
    // Returns (title, detailed_message)
    match ntstatus_code {
        0xc000006d => Some((
            "Login Failed - Invalid Credentials",
            "The username or password is incorrect.\n\n\
            Please check:\n\
            • Username is spelled correctly\n\
            • Password is correct\n\
            • Domain name (if required)\n\
            • Caps Lock is not on",
        )),
        0xc0000234 => Some((
            "Account Locked Out",
            "Your account has been locked due to too many failed login attempts.\n\n\
            Please:\n\
            • Wait 30 minutes for automatic unlock, or\n\
            • Contact your system administrator to unlock the account\n\n\
            On the RDP server, run: net user USERNAME /active:yes",
        )),
        0xc0000071 => Some((
            "Password Expired",
            "Your password has expired and must be changed.\n\n\
            Please log in to the RDP server directly (console access) and change your password.",
        )),
        0xc0000072 => Some((
            "Account Disabled",
            "This user account is disabled.\n\n\
            Contact your system administrator to enable the account.\n\n\
            On the RDP server, run: net user USERNAME /active:yes",
        )),
        0xc000006f => Some((
            "Account Restriction",
            "Your account has restrictions that prevent you from logging in at this time.\n\n\
            Possible causes:\n\
            • Time-based login restrictions\n\
            • Workstation login restrictions\n\
            • Account is only allowed to log in at certain times",
        )),
        0xc0000070 => Some((
            "Invalid Workstation",
            "You are not allowed to log in from this computer.\n\n\
            Contact your system administrator to grant access from this workstation.",
        )),
        0xc0000193 => Some((
            "Account Expired",
            "This user account has expired.\n\n\
            Contact your system administrator to reactivate the account.",
        )),
        0xc0000064 => Some((
            "User Does Not Exist",
            "The specified user account does not exist.\n\n\
            Please check:\n\
            • Username is spelled correctly\n\
            • Account exists on the RDP server",
        )),
        0xc000006a => Some((
            "Wrong Password",
            "The password is incorrect.\n\n\
            Please check:\n\
            • Password is correct\n\
            • Caps Lock is not on\n\
            • Correct keyboard layout is selected",
        )),
        0xc0000224 => Some((
            "Password Must Change",
            "You must change your password before logging in.\n\n\
            This is typically required on first login or after a password reset.",
        )),
        0xc0000413 => Some((
            "Authentication Firewall Restriction",
            "A firewall restriction prevented authentication.\n\n\
            Contact your system administrator to check firewall rules.",
        )),
        _ => None,
    }
}

/// Format error message with NT status code information
fn format_rdp_error(error: &impl std::fmt::Debug) -> (String, String) {
    let error_str = format!("{:?}", error);

    // Try to extract NStatusCode from the error
    if let Some(start) = error_str.find("NStatusCode(0x") {
        if let Some(end) = error_str[start..].find(')') {
            let code_str = &error_str[start + 14..start + end]; // Skip "NStatusCode(0x"
            if let Ok(code) = u32::from_str_radix(code_str, 16) {
                if let Some((title, message)) = get_friendly_error_message(code) {
                    return (
                        title.to_string(),
                        format!(
                            "{}\n\nTechnical details: Error code 0x{:08x}",
                            message, code
                        ),
                    );
                }
            }
        }
    }

    // Check for common error patterns
    if error_str.contains("CredSSP") {
        return (
            "Authentication Failed".to_string(),
            format!(
                "Network Level Authentication (CredSSP) failed.\n\n\
                This usually means invalid credentials or account issues.\n\n\
                Technical details:\n{:?}",
                error
            ),
        );
    }

    if error_str.contains("TLS") || error_str.contains("SSL") {
        return (
            "Secure Connection Failed".to_string(),
            format!(
                "Failed to establish a secure (TLS) connection.\n\n\
                Please check:\n\
                • Server certificate is valid\n\
                • Server supports TLS\n\n\
                Technical details:\n{:?}",
                error
            ),
        );
    }

    if error_str.contains("TCP") || error_str.contains("Connection refused") {
        return (
            "Cannot Connect to Server".to_string(),
            format!(
                "Failed to connect to the RDP server.\n\n\
                Please check:\n\
                • Server address is correct\n\
                • Server is running and reachable\n\
                • Port 3389 is open\n\
                • Network/firewall settings\n\n\
                Technical details:\n{:?}",
                error
            ),
        );
    }

    // Default generic error
    (
        "RDP Connection Failed".to_string(),
        format!(
            "An error occurred while connecting to the RDP server.\n\n\
            Technical details:\n{:?}",
            error
        ),
    )
}

fn create_rdp_config(
    server: &str,
    username: &str,
    domain: &str,
    password: &str,
    rdp_settings: &RdpSettings,
    desktop_scale_percent: u32,
) -> Config {
    use ironrdp::connector;
    use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
    use ironrdp::pdu::rdp::client_info::PerformanceFlags;

    let destination = Destination::new(server.to_string()).unwrap();

    // Get desktop size from settings (handle fullscreen separately)
    let (requested_width, requested_height) =
        if let Some(dims) = rdp_settings.get_resolution().to_dimensions() {
            dims
        } else {
            // Fullscreen - use a reasonable default, will be updated on connection
            (1920, 1080)
        };

    let align4 = |value: u16| -> u16 {
        let aligned = value & !0x3;
        if aligned == 0 { 4 } else { aligned }
    };

    let width = align4(requested_width);
    let height = align4(requested_height);

    if width != requested_width || height != requested_height {
        tracing::debug!(
            requested_width,
            requested_height,
            aligned_width = width,
            aligned_height = height,
            "Adjusted requested resolution to multiples of four"
        );
    }

    let connector_config = connector::Config {
        credentials: connector::Credentials::UsernamePassword {
            username: username.to_string(),
            password: password.to_string(),
        },
        domain: if domain.is_empty() {
            None
        } else {
            Some(domain.to_string())
        },
        client_name: "IronTSC".to_string(),
        desktop_size: connector::DesktopSize { width, height },
        enable_server_pointer: true,
        pointer_software_rendering: false,
        autologon: true,
        desktop_scale_factor: desktop_scale_percent,
        enable_tls: true,
        enable_credssp: true,
        keyboard_type: ironrdp::pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        // Match modern mstsc fingerprint so the server enables RDPEGFX paths
        client_build: 18363,
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_string(),
        platform: MajorPlatformType::UNIX,
        keyboard_layout: 0,
        ime_file_name: "".to_string(),
        dig_product_id: "".to_string(),
        hardware_id: None,
        bitmap: {
            #[cfg(feature = "h264")]
            {
                // Inject H.264/AVC444 codec support for RDPEGFX hardware encoding
                match crate::h264_codec_caps::create_bitmap_config_with_h264(
                    false,
                    32,
                    rdp_settings.get_disable_avc420(),
                    rdp_settings.get_disable_avc444(),
                ) {
                    Ok(config) => Some(config),
                    Err(e) => {
                        tracing::warn!("Failed to create H.264 bitmap config: {}", e);
                        None
                    }
                }
            }
            #[cfg(not(feature = "h264"))]
            {
                // Without H.264, use default bitmap config (RemoteFX Progressive only)
                None
            }
        },
        request_data: None,
        enable_audio_playback: true,
        performance_flags: PerformanceFlags::DISABLE_WALLPAPER
            | PerformanceFlags::DISABLE_FULLWINDOWDRAG
            | PerformanceFlags::DISABLE_MENUANIMATIONS
            | PerformanceFlags::DISABLE_THEMING,
        license_cache: None,
        timezone_info: crate::config::get_system_timezone_info(),
        correlation_id: None, // Will be auto-generated if needed
    };

    Config {
        log_file: None,
        gw: None,
        destination,
        connector: connector_config,
        clipboard_type: ClipboardType::Default,
        rdcleanpath: None,
        dvc_pipe_proxies: Vec::new(),
        h264_hw_accel: rdp_settings.get_h264_hw_accel(),
        disable_avc420: rdp_settings.get_disable_avc420(),
        disable_avc444: rdp_settings.get_disable_avc444(),
        disable_udp: rdp_settings.get_disable_udp(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Resolution {
    R640x480,
    R800x600,
    R1024x768,
    R1920x1080,
    Fullscreen,
}

impl Resolution {
    fn to_dimensions(&self) -> Option<(u16, u16)> {
        match self {
            Resolution::R640x480 => Some((640, 480)),
            Resolution::R800x600 => Some((800, 600)),
            Resolution::R1024x768 => Some((1024, 768)),
            Resolution::R1920x1080 => Some((1920, 1080)),
            Resolution::Fullscreen => None, // Will be determined at runtime
        }
    }

    fn from_dimensions(width: u16, height: u16) -> Self {
        match (width, height) {
            (640, 480) => Resolution::R640x480,
            (800, 600) => Resolution::R800x600,
            (1024, 768) => Resolution::R1024x768,
            (1920, 1080) => Resolution::R1920x1080,
            _ => Resolution::R1024x768, // Default
        }
    }

    fn to_string(&self) -> &'static str {
        match self {
            Resolution::R640x480 => "640x480",
            Resolution::R800x600 => "800x600",
            Resolution::R1024x768 => "1024x768",
            Resolution::R1920x1080 => "1920x1080",
            Resolution::Fullscreen => "Full screen",
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            0 => Resolution::R640x480,
            1 => Resolution::R800x600,
            2 => Resolution::R1024x768,
            3 => Resolution::R1920x1080,
            4 => Resolution::Fullscreen,
            _ => Resolution::R1024x768,
        }
    }

    fn to_index(&self) -> usize {
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
enum ColorDepth {
    Bpp15, // High Color (15 bit)
    Bpp16, // High Color (16 bit)
    Bpp24, // True color (24 bit)
    Bpp32, // Highest quality (32 bit)
}

const DPI_SCALE_OPTIONS: &[(Option<u32>, &str)] = &[
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

fn dpi_value_from_index(index: u32) -> Option<u32> {
    DPI_SCALE_OPTIONS
        .get(index as usize)
        .map(|(value, _)| *value)
        .unwrap_or(None)
}

fn dpi_index_from_value(value: Option<u32>) -> u32 {
    DPI_SCALE_OPTIONS
        .iter()
        .position(|(candidate, _)| *candidate == value)
        .unwrap_or(0) as u32
}

fn is_supported_dpi_value(value: u32) -> bool {
    DPI_SCALE_OPTIONS
        .iter()
        .any(|(candidate, _)| candidate.map(|v| v == value).unwrap_or(false))
}

impl ColorDepth {
    fn to_bpp(&self) -> u16 {
        match self {
            ColorDepth::Bpp15 => 15,
            ColorDepth::Bpp16 => 16,
            ColorDepth::Bpp24 => 24,
            ColorDepth::Bpp32 => 32,
        }
    }

    fn from_bpp(bpp: u16) -> Self {
        match bpp {
            15 => ColorDepth::Bpp15,
            16 => ColorDepth::Bpp16,
            24 => ColorDepth::Bpp24,
            32 | _ => ColorDepth::Bpp32,
        }
    }

    fn to_string(&self) -> &'static str {
        match self {
            ColorDepth::Bpp15 => "High Color (15 bit)",
            ColorDepth::Bpp16 => "High Color (16 bit)",
            ColorDepth::Bpp24 => "True color (24 bit)",
            ColorDepth::Bpp32 => "Highest quality (32 bit)",
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            0 => ColorDepth::Bpp32,
            1 => ColorDepth::Bpp24,
            2 => ColorDepth::Bpp16,
            3 => ColorDepth::Bpp15,
            _ => ColorDepth::Bpp32,
        }
    }

    fn to_index(&self) -> usize {
        match self {
            ColorDepth::Bpp32 => 0,
            ColorDepth::Bpp24 => 1,
            ColorDepth::Bpp16 => 2,
            ColorDepth::Bpp15 => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RdpSettings {
    #[serde(default)]
    server: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    save_password: bool,
    #[serde(default = "default_width")]
    desktopwidth: u16,
    #[serde(default = "default_height")]
    desktopheight: u16,
    #[serde(default = "default_session_bpp")]
    session_bpp: u16,
    #[serde(default)]
    full_screen: bool,
    #[serde(default)]
    dpi_scaling: Option<u32>,
    #[serde(default)]
    h264_hw_accel: bool,
    #[serde(default)]
    disable_avc420: bool,
    #[serde(default)]
    disable_avc444: bool,
    #[serde(default)]
    disable_udp: bool,
    #[serde(default)]
    show_codec_grid: bool,
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
            h264_hw_accel: false, // Default to software decoding for compatibility
            disable_avc420: false,
            disable_avc444: false,
            disable_udp: false,
            show_codec_grid: false,
        }
    }
}

impl RdpSettings {
    fn load_from_file(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        Self::parse_rdp(&contents)
    }

    fn load_default() -> Self {
        if let Some(dir) = Self::config_dir() {
            let path = dir.join(DEFAULT_RDP_FILE);
            if let Ok(settings) = Self::load_from_file(&path) {
                return settings;
            }
        }

        Self::default()
    }

    fn parse_rdp(contents: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut settings = Self::default();

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || !line.contains(':') {
                continue;
            }

            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() < 3 {
                continue;
            }

            let key = parts[0];
            let value = parts[2];

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

    fn to_rdp_format(&self) -> String {
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

    fn save_to_file(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(path, self.to_rdp_format())?;
        Ok(())
    }

    fn save_as_default(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(dir) = Self::config_dir() {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(DEFAULT_RDP_FILE);
            self.save_to_file(&path)?;
        }

        Ok(())
    }

    fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("irontsc"))
    }

    fn get_resolution(&self) -> Resolution {
        if self.full_screen {
            Resolution::Fullscreen
        } else {
            Resolution::from_dimensions(self.desktopwidth, self.desktopheight)
        }
    }

    fn set_resolution(&mut self, resolution: Resolution) {
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

    fn get_color_depth(&self) -> ColorDepth {
        ColorDepth::from_bpp(self.session_bpp)
    }

    fn set_color_depth(&mut self, depth: ColorDepth) {
        self.session_bpp = depth.to_bpp();
    }

    fn get_dpi_scaling(&self) -> Option<u32> {
        self.dpi_scaling
    }

    fn set_dpi_scaling(&mut self, scaling: Option<u32>) {
        self.dpi_scaling = scaling;
    }

    fn get_h264_hw_accel(&self) -> bool {
        self.h264_hw_accel
    }

    fn set_h264_hw_accel(&mut self, enabled: bool) {
        self.h264_hw_accel = enabled;
    }

    fn get_disable_avc420(&self) -> bool {
        self.disable_avc420
    }

    fn set_disable_avc420(&mut self, disabled: bool) {
        self.disable_avc420 = disabled;
    }

    fn get_disable_avc444(&self) -> bool {
        self.disable_avc444
    }

    fn set_disable_avc444(&mut self, disabled: bool) {
        self.disable_avc444 = disabled;
    }

    fn get_disable_udp(&self) -> bool {
        self.disable_udp
    }

    fn set_disable_udp(&mut self, disabled: bool) {
        self.disable_udp = disabled;
    }

    fn get_show_codec_grid(&self) -> bool {
        self.show_codec_grid
    }

    fn set_show_codec_grid(&mut self, enabled: bool) {
        self.show_codec_grid = enabled;
    }
}

// GTK RDP Widget Implementation
#[derive(Clone)]
struct GtkRdpWidget {
    root: gtk::Overlay,
    picture: gtk::Picture,
    placeholder_label: gtk::Label,
    size_probe: gtk::DrawingArea,
    buffer_size: Rc<RefCell<(u16, u16)>>,
    framebuffer: Arc<Mutex<FrameState>>,
    input_event_sender: mpsc::UnboundedSender<RdpInputEvent>,
    input_database: Rc<RefCell<ironrdp::input::Database>>,
    custom_cursor: Rc<RefCell<Option<gtk::gdk::Cursor>>>,
    pending_upload: Rc<RefCell<Option<PendingUpload>>>,
    upload_source: Rc<RefCell<Option<glib::SourceId>>>,
}

impl GtkRdpWidget {
    fn new(input_event_sender: mpsc::UnboundedSender<RdpInputEvent>) -> Self {
        let picture = gtk::Picture::new();
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_content_fit(gtk::ContentFit::Fill);

        let placeholder_label = gtk::Label::new(Some("Connecting to RDP server..."));
        placeholder_label.set_halign(gtk::Align::Center);
        placeholder_label.set_valign(gtk::Align::Center);
        placeholder_label.add_css_class("title-3");
        placeholder_label.set_can_target(false);

        let size_probe = gtk::DrawingArea::new();
        size_probe.set_hexpand(true);
        size_probe.set_vexpand(true);
        size_probe.set_can_target(false);
        size_probe.set_focusable(false);
        size_probe.set_can_focus(false);
        size_probe.set_draw_func(|_, _, _, _| {});

        let root = gtk::Overlay::new();
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_can_focus(true);
        root.set_focusable(true);
        root.set_child(Some(&picture));
        root.add_overlay(&placeholder_label);
        root.add_overlay(&size_probe);

        let buffer_size = Rc::new(RefCell::new((0u16, 0u16)));
        let input_database = Rc::new(RefCell::new(ironrdp::input::Database::new()));
        let custom_cursor = Rc::new(RefCell::new(None));

        let framebuffer = Arc::new(Mutex::new(FrameState::default()));
        let pending_upload = Rc::new(RefCell::new(None));
        let upload_source = Rc::new(RefCell::new(None));

        let widget = Self {
            root: root.clone(),
            picture: picture.clone(),
            placeholder_label: placeholder_label.clone(),
            size_probe: size_probe.clone(),
            buffer_size: buffer_size.clone(),
            framebuffer: framebuffer.clone(),
            input_event_sender: input_event_sender.clone(),
            input_database: input_database.clone(),
            custom_cursor: custom_cursor.clone(),
            pending_upload: pending_upload.clone(),
            upload_source: upload_source.clone(),
        };

        // Set up input event handlers
        widget.setup_input_handlers();

        widget
    }

    fn setup_input_handlers(&self) {
        // Keyboard events
        let key_controller = gtk::EventControllerKey::new();
        // Forward all key events to the RDP session (don't let GTK consume them)
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        let input_sender_key = self.input_event_sender.clone();
        let input_database_key = self.input_database.clone();

        let input_sender_key_pressed = input_sender_key.clone();
        let input_database_key_pressed = input_database_key.clone();
        key_controller.connect_key_pressed(move |_, _key, keycode, _modifiers| {
            if let Some(scancode) = Self::keycode_to_scancode(keycode) {
                let operation = ironrdp::input::Operation::KeyPressed(scancode);
                let input_events = input_database_key_pressed
                    .borrow_mut()
                    .apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_key_pressed, input_events);
            }
            // Return Stop to prevent GTK from processing shortcuts
            glib::Propagation::Stop
        });

        key_controller.connect_key_released(move |_, _key, keycode, _modifiers| {
            if let Some(scancode) = Self::keycode_to_scancode(keycode) {
                let operation = ironrdp::input::Operation::KeyReleased(scancode);
                let input_events = input_database_key
                    .borrow_mut()
                    .apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_key, input_events);
            }
        });

        self.root.add_controller(key_controller);

        // Mouse events - configure to handle all mouse buttons
        let click_controller = gtk::GestureClick::new();
        click_controller.set_button(0); // 0 means listen to all mouse buttons
        let input_sender_click = self.input_event_sender.clone();
        let input_database_click = self.input_database.clone();
        let buffer_size_click = self.buffer_size.clone();
        let overlay_click = self.root.clone();

        let input_sender_click_pressed = input_sender_click.clone();
        let input_database_click_pressed = input_database_click.clone();
        let buffer_size_click_pressed = buffer_size_click.clone();
        let overlay_click_pressed = overlay_click.clone();
        click_controller.connect_pressed(move |gesture, _n_press, x, y| {
            let button = gesture.current_button();
            let mouse_button = Self::gtk_button_to_rdp_button(button);
            if let Some(mouse_button) = mouse_button {
                // Translate widget coordinates to RDP coordinates
                let widget_width = overlay_click_pressed.width() as f64;
                let widget_height = overlay_click_pressed.height() as f64;
                let (buf_width, buf_height) = *buffer_size_click_pressed.borrow();

                if buf_width > 0 && buf_height > 0 && widget_width > 0.0 && widget_height > 0.0 {
                    let rdp_x = (x / widget_width * buf_width as f64) as u16;
                    let rdp_y = (y / widget_height * buf_height as f64) as u16;

                    // Send mouse position update before button press
                    let move_op =
                        ironrdp::input::Operation::MouseMove(ironrdp::input::MousePosition {
                            x: rdp_x,
                            y: rdp_y,
                        });
                    let press_op = ironrdp::input::Operation::MouseButtonPressed(mouse_button);
                    let input_events = input_database_click_pressed
                        .borrow_mut()
                        .apply([move_op, press_op]);
                    Self::send_fast_path_events(&input_sender_click_pressed, input_events);
                }
            }
        });

        click_controller.connect_released(move |gesture, _n_press, x, y| {
            let button = gesture.current_button();
            let mouse_button = Self::gtk_button_to_rdp_button(button);
            if let Some(mouse_button) = mouse_button {
                // Translate widget coordinates to RDP coordinates
                let widget_width = overlay_click.width() as f64;
                let widget_height = overlay_click.height() as f64;
                let (buf_width, buf_height) = *buffer_size_click.borrow();

                if buf_width > 0 && buf_height > 0 && widget_width > 0.0 && widget_height > 0.0 {
                    let rdp_x = (x / widget_width * buf_width as f64) as u16;
                    let rdp_y = (y / widget_height * buf_height as f64) as u16;

                    // Send mouse position update before button release
                    let move_op =
                        ironrdp::input::Operation::MouseMove(ironrdp::input::MousePosition {
                            x: rdp_x,
                            y: rdp_y,
                        });
                    let release_op = ironrdp::input::Operation::MouseButtonReleased(mouse_button);
                    let input_events = input_database_click
                        .borrow_mut()
                        .apply([move_op, release_op]);
                    Self::send_fast_path_events(&input_sender_click, input_events);
                }
            }
        });

        self.root.add_controller(click_controller);

        // Mouse motion
        let motion_controller = gtk::EventControllerMotion::new();
        let input_sender_motion = self.input_event_sender.clone();
        let input_database_motion = self.input_database.clone();
        let buffer_size_motion = self.buffer_size.clone();
        let overlay_motion = self.root.clone();

        motion_controller.connect_motion(move |_, x, y| {
            let widget_width = overlay_motion.width() as f64;
            let widget_height = overlay_motion.height() as f64;
            let (buf_width, buf_height) = *buffer_size_motion.borrow();

            if buf_width > 0 && buf_height > 0 && widget_width > 0.0 && widget_height > 0.0 {
                let rdp_x = (x / widget_width * buf_width as f64) as u16;
                let rdp_y = (y / widget_height * buf_height as f64) as u16;

                let operation =
                    ironrdp::input::Operation::MouseMove(ironrdp::input::MousePosition {
                        x: rdp_x,
                        y: rdp_y,
                    });
                let input_events = input_database_motion
                    .borrow_mut()
                    .apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_motion, input_events);
            }
        });

        self.root.add_controller(motion_controller);

        // Mouse scroll wheel
        let scroll_controller =
            gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let input_sender_scroll = self.input_event_sender.clone();
        let input_database_scroll = self.input_database.clone();

        scroll_controller.connect_scroll(move |_, dx, dy| {
            // RDP uses 120 units per "notch" of the wheel
            // GTK scroll delta is typically in the range of -1.0 to 1.0 per notch
            // Negative dy means scroll up, positive means scroll down

            let mut operations = smallvec::SmallVec::<[ironrdp::input::Operation; 2]>::new();

            // Handle vertical scrolling
            if dy.abs() > 0.001 {
                let vertical_delta = (-dy * 120.0) as i16;
                operations.push(ironrdp::input::Operation::WheelRotations(
                    ironrdp::input::WheelRotations {
                        is_vertical: true,
                        rotation_units: vertical_delta,
                    },
                ));
            }

            // Handle horizontal scrolling
            if dx.abs() > 0.001 {
                let horizontal_delta = (dx * 120.0) as i16;
                operations.push(ironrdp::input::Operation::WheelRotations(
                    ironrdp::input::WheelRotations {
                        is_vertical: false,
                        rotation_units: horizontal_delta,
                    },
                ));
            }

            if !operations.is_empty() {
                let input_events = input_database_scroll.borrow_mut().apply(operations);
                Self::send_fast_path_events(&input_sender_scroll, input_events);
            }

            gtk::glib::Propagation::Stop
        });

        self.root.add_controller(scroll_controller);
    }

    fn keycode_to_scancode(keycode: u32) -> Option<ironrdp::input::Scancode> {
        // Map X11/GTK keycodes to RDP scancodes (Windows scancodes)
        // GTK uses X11 keycodes which are Linux evdev codes + 8
        // We need to convert to Windows scancode (Set 1)

        // Subtract 8 to get Linux evdev code
        let evdev = keycode.saturating_sub(8);

        // Comprehensive Linux evdev to Windows scancode mapping table
        // Format: (evdev_code, windows_scancode)
        // Extended scancodes have 0xE0 prefix encoded in upper byte
        const SCANCODE_MAP: &[(u32, u16)] = &[
            // Function and control keys
            (1, 0x01),  // ESC
            (59, 0x3B), // F1
            (60, 0x3C), // F2
            (61, 0x3D), // F3
            (62, 0x3E), // F4
            (63, 0x3F), // F5
            (64, 0x40), // F6
            (65, 0x41), // F7
            (66, 0x42), // F8
            (67, 0x43), // F9
            (68, 0x44), // F10
            (87, 0x57), // F11
            (88, 0x58), // F12
            // Number row
            (41, 0x29), // ` ~
            (2, 0x02),  // 1 !
            (3, 0x03),  // 2 @
            (4, 0x04),  // 3 #
            (5, 0x05),  // 4 $
            (6, 0x06),  // 5 %
            (7, 0x07),  // 6 ^
            (8, 0x08),  // 7 &
            (9, 0x09),  // 8 *
            (10, 0x0A), // 9 (
            (11, 0x0B), // 0 )
            (12, 0x0C), // - _
            (13, 0x0D), // = +
            (14, 0x0E), // Backspace
            // Top letter row
            (15, 0x0F), // Tab
            (16, 0x10), // Q
            (17, 0x11), // W
            (18, 0x12), // E
            (19, 0x13), // R
            (20, 0x14), // T
            (21, 0x15), // Y
            (22, 0x16), // U
            (23, 0x17), // I
            (24, 0x18), // O
            (25, 0x19), // P
            (26, 0x1A), // [ {
            (27, 0x1B), // ] }
            (28, 0x1C), // Enter
            // Middle letter row
            (58, 0x3A), // Caps Lock
            (30, 0x1E), // A
            (31, 0x1F), // S
            (32, 0x20), // D
            (33, 0x21), // F
            (34, 0x22), // G
            (35, 0x23), // H
            (36, 0x24), // J
            (37, 0x25), // K
            (38, 0x26), // L
            (39, 0x27), // ; :
            (40, 0x28), // ' "
            (43, 0x2B), // \ |
            // Bottom letter row
            (42, 0x2A), // Left Shift
            (86, 0x56), // ISO key (< > | on European keyboards)
            (44, 0x2C), // Z
            (45, 0x2D), // X
            (46, 0x2E), // C
            (47, 0x2F), // V
            (48, 0x30), // B
            (49, 0x31), // N
            (50, 0x32), // M
            (51, 0x33), // , <
            (52, 0x34), // . >
            (53, 0x35), // / ?
            (54, 0x36), // Right Shift
            // Bottom row
            (29, 0x1D),    // Left Ctrl
            (97, 0xE01D),  // Right Ctrl (extended)
            (56, 0x38),    // Left Alt
            (100, 0xE038), // Right Alt / AltGr (extended)
            (57, 0x39),    // Space
            (125, 0xE05B), // Left Windows/Super (extended)
            (126, 0xE05C), // Right Windows/Super (extended)
            (127, 0xE05D), // Menu/Application key (extended)
            // Navigation cluster (extended keys)
            (102, 0xE047), // Home
            (103, 0xE048), // Up Arrow
            (104, 0xE049), // Page Up
            (105, 0xE04B), // Left Arrow
            (106, 0xE04D), // Right Arrow
            (107, 0xE04F), // End
            (108, 0xE050), // Down Arrow
            (109, 0xE051), // Page Down
            (110, 0xE052), // Insert
            (111, 0xE053), // Delete
            // Numpad
            (69, 0x45),   // Num Lock
            (98, 0xE035), // Numpad / (extended)
            (55, 0x37),   // Numpad *
            (74, 0x4A),   // Numpad -
            (78, 0x4E),   // Numpad +
            (96, 0xE01C), // Numpad Enter (extended)
            (71, 0x47),   // Numpad 7 / Home
            (72, 0x48),   // Numpad 8 / Up
            (73, 0x49),   // Numpad 9 / PgUp
            (75, 0x4B),   // Numpad 4 / Left
            (76, 0x4C),   // Numpad 5
            (77, 0x4D),   // Numpad 6 / Right
            (79, 0x4F),   // Numpad 1 / End
            (80, 0x50),   // Numpad 2 / Down
            (81, 0x51),   // Numpad 3 / PgDn
            (82, 0x52),   // Numpad 0 / Ins
            (83, 0x53),   // Numpad . / Del
            // Special keys
            (70, 0x46),    // Scroll Lock
            (99, 0xE037),  // Print Screen (extended)
            (119, 0xE05F), // Pause/Break (extended - simplified, full sequence is complex)
        ];

        // Binary search would be faster for large tables, but linear search is fine here
        SCANCODE_MAP
            .iter()
            .find(|(code, _)| *code == evdev)
            .map(|(_, scancode)| ironrdp::input::Scancode::from_u16(*scancode))
    }

    fn gtk_button_to_rdp_button(button: u32) -> Option<ironrdp::input::MouseButton> {
        match button {
            1 => Some(ironrdp::input::MouseButton::Left),
            2 => Some(ironrdp::input::MouseButton::Middle),
            3 => Some(ironrdp::input::MouseButton::Right),
            8 => Some(ironrdp::input::MouseButton::X1), // Browser Back button
            9 => Some(ironrdp::input::MouseButton::X2), // Browser Forward button
            _ => None,
        }
    }

    fn send_fast_path_events(
        input_event_sender: &mpsc::UnboundedSender<RdpInputEvent>,
        input_events: smallvec::SmallVec<[ironrdp::pdu::input::fast_path::FastPathInputEvent; 2]>,
    ) {
        if !input_events.is_empty() {
            let _ = input_event_sender.send(RdpInputEvent::FastPath(input_events));
        }
    }

    fn update_image(
        &self,
        buffer: Arc<Vec<u8>>,
        width: u16,
        height: u16,
        region: Option<ImageRegion>,
    ) {
        tracing::debug!(
            "📸 update_image called: {}x{} ({} bytes) region={:?}",
            width,
            height,
            buffer.len(),
            region
        );

        if width == 0 || height == 0 {
            self.cancel_pending_upload();
            self.picture
                .set_paintable(Option::<&gtk::gdk::Texture>::None);
            self.placeholder_label.set_visible(true);
            *self.buffer_size.borrow_mut() = (0, 0);
            if let Ok(mut state) = self.framebuffer.lock() {
                state.clear();
            }
            self.root.queue_draw();
            return;
        }

        let stride = width as usize * 4;
        let frame_len = stride * height as usize;

        {
            let mut state = self.framebuffer.lock().expect("framebuffer mutex poisoned");

            match region {
                None => {
                    debug_assert_eq!(buffer.len(), frame_len);

                    state.frame = buffer.clone();
                    state.staging = Some(buffer);
                    state.staging_ready = true;
                    state.frame_version = state.frame_version.wrapping_add(1);
                }
                Some(region) => {
                    if state.frame.len() != frame_len {
                        state.frame = Arc::new(vec![0; frame_len]);
                    }

                    state.staging = None;
                    state.staging_ready = false;

                    let frame_vec = Arc::make_mut(&mut state.frame);

                    if frame_vec.len() != frame_len {
                        frame_vec.resize(frame_len, 0);
                    }

                    let region_width = usize::from(region.width.get());
                    let region_height = usize::from(region.height.get());
                    let bytes_per_row = region_width * 4;

                    debug_assert_eq!(buffer.len(), bytes_per_row * region_height);

                    for row in 0..region_height {
                        let src_offset = row * bytes_per_row;
                        let dst_offset =
                            (usize::from(region.y) + row) * stride + usize::from(region.x) * 4;

                        frame_vec[dst_offset..dst_offset + bytes_per_row]
                            .copy_from_slice(&buffer[src_offset..src_offset + bytes_per_row]);
                    }

                    state.frame_version = state.frame_version.wrapping_add(1);
                }
            }
        }

        self.schedule_upload(width, height);
        self.placeholder_label.set_visible(false);
        *self.buffer_size.borrow_mut() = (width, height);
    }

    fn schedule_upload(&self, width: u16, height: u16) {
        // Upload immediately to avoid frame skipping during fast updates
        self.upload_framebuffer(width, height);
        self.root.queue_draw();
    }

    fn cancel_pending_upload(&self) {
        if let Some(source) = self.upload_source.borrow_mut().take() {
            source.remove();
        }
        self.pending_upload.borrow_mut().take();
    }

    fn upload_framebuffer(&self, width: u16, height: u16) {
        tracing::debug!("🖼️ upload_framebuffer called: {}x{}", width, height);

        let stride = width as usize * 4;
        let frame_len = stride * height as usize;
        let bytes = {
            let mut state = self.framebuffer.lock().expect("framebuffer mutex poisoned");

            if state.frame.len() != frame_len {
                state.frame = Arc::new(vec![0; frame_len]);
                state.staging_ready = false;
            }

            if !state.staging_ready
                || state
                    .staging
                    .as_ref()
                    .map(|arc| arc.len() != frame_len)
                    .unwrap_or(true)
            {
                state.staging = Some(state.frame.clone());
                state.staging_ready = true;
            }

            let staging_arc = state.staging.as_ref().unwrap().clone();

            gtk::glib::Bytes::from_owned(FrameBytes::new(staging_arc))
        };

        let texture = gtk::gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gtk::gdk::MemoryFormat::B8g8r8a8, // Native BGRA format - no conversion needed!
            &bytes,
            stride,
        );

        self.picture.set_paintable(Some(&texture));
        tracing::debug!("✅ Texture uploaded and set on picture widget");
    }

    fn surface_fractional_scale(surface: &gdk::Surface) -> f64 {
        let scale = surface.scale();
        if scale > 0.0 {
            return scale;
        }

        surface.scale_factor().max(1) as f64
    }

    fn monitor_fractional_scale(monitor: &gdk::Monitor) -> f64 {
        let scale = monitor.scale();
        if scale > 0.0 {
            return scale;
        }

        monitor.scale_factor().max(1) as f64
    }

    fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    fn size_probe(&self) -> &gtk::DrawingArea {
        &self.size_probe
    }

    fn set_cursor_default(&self) {
        *self.custom_cursor.borrow_mut() = None;
        self.root.set_cursor_from_name(Some("default"));
    }

    fn set_cursor_hidden(&self) {
        *self.custom_cursor.borrow_mut() = None;
        self.root.set_cursor_from_name(Some("none"));
    }

    fn set_cursor_from_bitmap(&self, pointer: Arc<ironrdp::graphics::pointer::DecodedPointer>) {
        // Create a cursor from the RDP pointer bitmap
        let width = pointer.width as i32;
        let height = pointer.height as i32;
        let hotspot_x = pointer.hotspot_x as i32;
        let hotspot_y = pointer.hotspot_y as i32;

        // The bitmap_data is already in RGBA format (4 bytes per pixel)
        // IronRDP provides the data as Vec<u8> in RGBA order with premultiplied alpha
        let pixel_count = (width * height) as usize;
        let expected_size = pixel_count * 4;

        if pointer.bitmap_data.len() < expected_size {
            eprintln!(
                "Cursor bitmap data too small: got {}, expected {}",
                pointer.bitmap_data.len(),
                expected_size
            );
            return;
        }

        // Data is already in RGBA format, just clone it
        let rgba_data = pointer.bitmap_data.clone();

        // Create GDK texture from the RGBA data
        let bytes = gtk::glib::Bytes::from_owned(rgba_data);
        let texture = gtk::gdk::MemoryTexture::new(
            width,
            height,
            gtk::gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            (width * 4) as usize, // stride: bytes per row
        );

        // Create cursor from texture
        let cursor = gtk::gdk::Cursor::from_texture(&texture, hotspot_x, hotspot_y, None);

        *self.custom_cursor.borrow_mut() = Some(cursor.clone());
        self.root.set_cursor(Some(&cursor));
    }
}

struct FrameState {
    frame: Arc<Vec<u8>>,
    staging: Option<Arc<Vec<u8>>>,
    staging_ready: bool,
    frame_version: u64,
}

impl FrameState {
    fn clear(&mut self) {
        self.frame = Arc::new(Vec::new());
        self.staging = None;
        self.staging_ready = false;
        self.frame_version = 0;
    }
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            frame: Arc::new(Vec::new()),
            staging: None,
            staging_ready: false,
            frame_version: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct PendingUpload {
    width: u16,
    height: u16,
}

struct FrameBytes {
    data: Arc<Vec<u8>>,
}

impl FrameBytes {
    fn new(data: Arc<Vec<u8>>) -> Self {
        Self { data }
    }
}

impl AsRef<[u8]> for FrameBytes {
    fn as_ref(&self) -> &[u8] {
        self.data.as_slice()
    }
}

// Adapter for RDP event loop proxy to work with GTK's glib MainContext
#[derive(Clone)]
struct RdpEventLoopProxy {
    sender: tokio::sync::mpsc::UnboundedSender<RdpOutputEvent>,
}

impl RdpEventLoopProxy {
    fn new(sender: tokio::sync::mpsc::UnboundedSender<RdpOutputEvent>) -> Self {
        Self { sender }
    }
}

impl rdp::RdpEventSender for RdpEventLoopProxy {
    fn send_event(&self, event: RdpOutputEvent) -> Result<(), ()> {
        self.sender.send(event).map_err(|_| ())
    }
}

fn create_remote_desktop_window(
    app: &Application,
    server: &str,
    username: &str,
    domain: &str,
    password: &str,
    main_window: &ApplicationWindow,
    rdp_settings: &RdpSettings,
) {
    // Convert to owned strings to avoid lifetime issues
    let server = server.to_string();
    let username = username.to_string();
    let domain = domain.to_string();
    let password = password.to_string();
    let mut rdp_settings = rdp_settings.clone();

    // Hide the main window when opening remote desktop
    main_window.set_visible(false);

    // Get window size from RDP settings
    let (default_width, default_height) =
        if let Some((w, h)) = rdp_settings.get_resolution().to_dimensions() {
            (w as i32, h as i32)
        } else {
            // Fullscreen - use reasonable defaults, will maximize later
            (1920, 1080)
        };

    // Create a new window for the remote desktop with normal decorations
    let rd_window = ApplicationWindow::builder()
        .application(app)
        .title(&format!("{} - Remote Desktop", server))
        .default_width(default_width)
        .default_height(default_height)
        .resizable(true)
        .decorated(true) // Keep normal window decorations
        .build();

    // If fullscreen mode is selected, maximize the window
    if rdp_settings.full_screen {
        rd_window.maximize();
    }

    let logical_config_width = rdp_settings.desktopwidth;
    let logical_config_height = rdp_settings.desktopheight;

    let display = gtk::prelude::WidgetExt::display(&rd_window);
    let primary_monitor = rd_window
        .surface()
        .and_then(|surface| display.monitor_at_surface(&surface))
        .or_else(|| {
            let monitors = display.monitors();
            (0..monitors.n_items()).find_map(|idx| {
                monitors
                    .item(idx)
                    .and_then(|obj| obj.downcast::<gdk::Monitor>().ok())
            })
        });

    let detected_scale = primary_monitor
        .as_ref()
        .map(|monitor| GtkRdpWidget::monitor_fractional_scale(monitor))
        .unwrap_or(1.0)
        .clamp(1.0, 5.0);

    let manual_scale_percent = rdp_settings
        .get_dpi_scaling()
        .map(|scale| scale.clamp(100, 500));

    let (effective_scale, initial_scale_percent) = if let Some(scale_percent) = manual_scale_percent
    {
        (
            (scale_percent as f64 / 100.0).clamp(1.0, 5.0),
            scale_percent,
        )
    } else {
        (
            detected_scale,
            ((detected_scale * 100.0).round() as u32).clamp(100, 500),
        )
    };

    let mut initial_width = logical_config_width as f64;
    let mut initial_height = logical_config_height as f64;

    if rdp_settings.full_screen {
        if let Some(ref monitor) = primary_monitor {
            let geometry = monitor.geometry();
            initial_width = geometry.width().max(1) as f64 * effective_scale;
            initial_height = geometry.height().max(1) as f64 * effective_scale;
        }
    } else {
        initial_width = initial_width * effective_scale;
        initial_height = initial_height * effective_scale;
    }

    let clamp_dimension =
        |value: f64| -> u16 { value.round().clamp(200.0, u16::MAX as f64) as u16 };

    rdp_settings.desktopwidth = clamp_dimension(initial_width);
    rdp_settings.desktopheight = clamp_dimension(initial_height);

    // Create RDP input/output channels
    let (input_event_sender, input_event_receiver) = RdpInputEvent::create_channel();
    let (output_event_sender, mut output_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<RdpOutputEvent>();

    // Track why the window is being closed
    let close_intent = Rc::new(Cell::new(CloseIntent::None));

    // Create the RDP widget
    let rdp_widget = GtkRdpWidget::new(input_event_sender.clone());
    let rdp_widget = Rc::new(rdp_widget);

    // Create the top control bar (only visible in fullscreen/when pinned)
    let control_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    // Set initial position estimate for centering (will be corrected later)
    // Assume ~250px control bar width and 800px window = center at ~275px
    control_bar.set_margin_start(275);
    control_bar.set_margin_end(8);
    control_bar.set_margin_top(0);
    control_bar.set_margin_bottom(4);
    control_bar.add_css_class("osd"); // Overlay style
    control_bar.set_halign(gtk::Align::Start); // Always start from left, we'll position with margin
    control_bar.set_valign(gtk::Align::Start);

    // State for tracking control bar position in pixels
    let control_bar_x_position = std::rc::Rc::new(std::cell::RefCell::new(0.5)); // Start centered (0.5 = 50% of width)

    // Helper function for converting absolute position to relative position
    let absolute_to_relative =
        |absolute_pos: f64, window_width: f64, control_bar_width: f64| -> f64 {
            if window_width <= control_bar_width {
                0.5 // Default to center if window is too small
            } else {
                (absolute_pos / (window_width - control_bar_width)).clamp(0.0, 1.0)
            }
        };
    let user_has_moved_toolbar = std::rc::Rc::new(std::cell::RefCell::new(false));

    // State for hotkey capture (Win key, Alt+Tab, etc.)
    let hotkey_capture_enabled = std::rc::Rc::new(std::cell::Cell::new(false)); // Global hotkey capture disabled by default

    // Connection name label
    let connection_label = gtk::Label::new(Some(&format!("{} ({})", server, username)));
    connection_label.set_margin_start(8);
    connection_label.set_margin_end(8);
    connection_label.add_css_class("caption");

    // Hotkey capture button
    let hotkey_button = Button::new();
    hotkey_button.set_icon_name("preferences-desktop-keyboard-shortcuts-symbolic");
    hotkey_button.set_tooltip_text(Some("Global hotkeys disabled (click to enable)"));
    hotkey_button.add_css_class("flat");
    hotkey_button.add_css_class("circular");
    hotkey_button.set_opacity(0.4); // Start with reduced opacity since disabled by default
    hotkey_button.set_focus_on_click(false);

    // Pin button
    let pin_button = Button::new();
    pin_button.set_icon_name("view-pin-symbolic");
    pin_button.set_tooltip_text(Some("Pin controls"));
    pin_button.add_css_class("flat");
    pin_button.add_css_class("circular");
    pin_button.set_opacity(0.4); // Start with reduced opacity since not pinned by default
    pin_button.set_focus_on_click(false);

    // Menu button (fullscreen toggle)
    let menu_button = Button::new();
    menu_button.set_icon_name("view-fullscreen-symbolic");
    menu_button.set_tooltip_text(Some("Toggle fullscreen"));
    menu_button.add_css_class("flat");
    menu_button.add_css_class("circular");
    menu_button.set_focus_on_click(false);

    // Minimize button
    let minimize_button = Button::new();
    minimize_button.set_icon_name("window-minimize-symbolic");
    minimize_button.set_tooltip_text(Some("Minimize window"));
    minimize_button.add_css_class("flat");
    minimize_button.add_css_class("circular");
    minimize_button.set_focus_on_click(false);

    // Close button
    let close_button = Button::new();
    close_button.set_icon_name("window-close-symbolic");
    close_button.set_tooltip_text(Some("Disconnect"));
    close_button.add_css_class("flat");
    close_button.add_css_class("circular");
    close_button.set_focus_on_click(false);

    // Info button
    let info_button = Button::new();
    info_button.set_icon_name("dialog-information-symbolic");
    info_button.set_tooltip_text(Some("Connection information"));
    info_button.add_css_class("flat");
    info_button.add_css_class("circular");
    info_button.set_focus_on_click(false);

    // Pack control bar with new order: connection_label, hotkey_button, pin, info, minimize, fullscreen, close
    control_bar.append(&connection_label);
    control_bar.append(&hotkey_button);
    control_bar.append(&pin_button);
    control_bar.append(&info_button);
    control_bar.append(&minimize_button);
    control_bar.append(&menu_button);
    control_bar.append(&close_button);

    // Create overlay to layer control bar over drawing area
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(rdp_widget.widget()));
    overlay.add_overlay(&control_bar);

    // Helper to keep the toolbar aligned to its stored relative position
    let overlay_for_position = overlay.clone();
    let control_bar_for_position = control_bar.clone();
    let control_bar_x_position_for_position = control_bar_x_position.clone();
    let apply_toolbar_position: std::rc::Rc<dyn Fn(Option<f64>)> =
        std::rc::Rc::new(move |desired_relative: Option<f64>| {
            let window_width = overlay_for_position.width() as f64;
            let control_bar_width = control_bar_for_position.width() as f64;

            if window_width <= 0.0 || control_bar_width <= 0.0 {
                return;
            }

            let requested =
                desired_relative.unwrap_or_else(|| *control_bar_x_position_for_position.borrow());
            let clamped = requested.clamp(0.0, 1.0);
            let margin = if window_width <= control_bar_width {
                0.0
            } else {
                clamped * (window_width - control_bar_width)
            };

            control_bar_for_position.set_margin_start(margin.round() as i32);
            *control_bar_x_position_for_position.borrow_mut() = clamped;
        });

    // Setup RDP output event handling
    let rdp_widget_events = rdp_widget.clone();
    let rd_window_events = rd_window.clone();

    // Helper: try to fetch a GdkToplevel from the window, if realized.
    fn with_toplevel<F: FnOnce(&gdk::Toplevel)>(win: &ApplicationWindow, f: F) {
        if let Some(surface) = win.surface() {
            if let Ok(tl) = surface.downcast::<gdk::Toplevel>() {
                f(&tl);
            }
        }
    }

    // 1) Inhibit shortcuts when pointer ENTERS the RDP overlay; restore on LEAVE
    {
        let enter_leave_ctrl = gtk::EventControllerMotion::new();
        let win_weak = rd_window.downgrade();
        let hotkey_capture_for_enter = hotkey_capture_enabled.clone();

        // Inhibit on enter (pass current event if present)
        enter_leave_ctrl.connect_enter(move |ctrl, _, _| {
            if let Some(win) = win_weak.upgrade() {
                // Only inhibit if hotkey capture is enabled
                if hotkey_capture_for_enter.get() {
                    with_toplevel(&win, |tl| {
                        if let Some(ev) = ctrl.current_event() {
                            tl.inhibit_system_shortcuts(Some(&ev));
                        } else {
                            tl.inhibit_system_shortcuts(None::<&gdk::Event>);
                        }
                    });
                }
            }
        });

        // Restore on leave
        let win_weak = rd_window.downgrade();
        enter_leave_ctrl.connect_leave(move |_| {
            if let Some(win) = win_weak.upgrade() {
                with_toplevel(&win, |tl| tl.restore_system_shortcuts());
            }
        });

        // Attach to the overlay that wraps the RDP view
        rdp_widget.widget().add_controller(enter_leave_ctrl);
    }

    // 2) Also handle keyboard focus (e.g., when alt-tabbing into/out of the window)
    {
        let focus_ctrl = gtk::EventControllerFocus::new();
        let win_weak = rd_window.downgrade();
        let hotkey_capture_for_focus = hotkey_capture_enabled.clone();

        focus_ctrl.connect_enter(move |_| {
            if let Some(win) = win_weak.upgrade() {
                // Only inhibit if hotkey capture is enabled
                if hotkey_capture_for_focus.get() {
                    with_toplevel(&win, |tl| tl.inhibit_system_shortcuts(None::<&gdk::Event>));
                }
            }
        });

        let win_weak = rd_window.downgrade();
        focus_ctrl.connect_leave(move |_| {
            if let Some(win) = win_weak.upgrade() {
                with_toplevel(&win, |tl| tl.restore_system_shortcuts());
            }
        });

        rd_window.add_controller(focus_ctrl);
    }

    // 3) Safety: always restore before closing
    {
        let win_weak = rd_window.downgrade();
        rd_window.connect_close_request(move |_| {
            if let Some(win) = win_weak.upgrade() {
                with_toplevel(&win, |tl| tl.restore_system_shortcuts());
            }
            gtk::glib::Propagation::Proceed
        });
    }

    let last_frame_size = Rc::new(RefCell::new((0u16, 0u16)));
    let last_frame_size_clone = last_frame_size.clone();

    // Shared connection statistics for info dialog
    use std::collections::VecDeque;
    #[derive(Clone)]
    struct StatsHistory {
        bandwidth_sent: VecDeque<(f64, u64)>,     // (time, bytes)
        bandwidth_received: VecDeque<(f64, u64)>, // (time, bytes)
        rtt_history: VecDeque<(f64, u32)>,         // (time, ms)
        frame_times: VecDeque<f64>,                // frame timestamps
        protocol: String,
        start_time: std::time::Instant,
        last_bytes_sent: u64,
        last_bytes_received: u64,
        last_frame_time: Option<std::time::Instant>,
    }
    
    impl StatsHistory {
        fn new() -> Self {
            Self {
                bandwidth_sent: VecDeque::with_capacity(240),     // 120 seconds at 0.5s intervals
                bandwidth_received: VecDeque::with_capacity(240),
                rtt_history: VecDeque::with_capacity(240),
                frame_times: VecDeque::with_capacity(1000),       // Keep last 1000 frames (~16s at 60fps)
                protocol: "Connecting...".to_string(),
                start_time: std::time::Instant::now(),
                last_bytes_sent: 0,
                last_bytes_received: 0,
                last_frame_time: None,
            }
        }
        
        fn update(&mut self, stats: &rdp::ConnectionStats) {
            let elapsed = self.start_time.elapsed().as_secs_f64();
            
            // Add bandwidth samples (keep 2 minutes)
            if self.bandwidth_sent.len() >= 240 {
                self.bandwidth_sent.pop_front();
            }
            self.bandwidth_sent.push_back((elapsed, stats.bytes_sent));
            
            if self.bandwidth_received.len() >= 240 {
                self.bandwidth_received.pop_front();
            }
            self.bandwidth_received.push_back((elapsed, stats.bytes_received));
            
            // Add RTT sample if available
            if let Some(rtt) = stats.roundtrip_time_ms {
                if self.rtt_history.len() >= 240 {
                    self.rtt_history.pop_front();
                }
                self.rtt_history.push_back((elapsed, rtt));
            }
            
            self.protocol = stats.transport_protocol.clone();
            self.last_bytes_sent = stats.bytes_sent;
            self.last_bytes_received = stats.bytes_received;
        }
        
        fn record_frame(&mut self) {
            let now = std::time::Instant::now();
            let elapsed = self.start_time.elapsed().as_secs_f64();
            
            if self.frame_times.len() >= 1000 {
                self.frame_times.pop_front();
            }
            self.frame_times.push_back(elapsed);
            self.last_frame_time = Some(now);
        }
        
        fn get_fps(&self) -> f64 {
            if self.frame_times.len() < 2 {
                return 0.0;
            }
            
            // Calculate FPS from last second of frames
            let current_time = self.start_time.elapsed().as_secs_f64();
            let one_sec_ago = current_time - 1.0;
            
            let recent_frames = self.frame_times.iter()
                .filter(|&&t| t >= one_sec_ago)
                .count();
            
            recent_frames as f64
        }
    }

    let stats_history = Rc::new(RefCell::new(StatsHistory::new()));
    let stats_history_for_events = stats_history.clone();
    let stats_history_for_frames = stats_history.clone();

    // Bridge tokio channel to GTK main thread
    let close_intent_for_events = close_intent.clone();

    glib::spawn_future_local(async move {
        while let Some(event) = output_event_receiver.recv().await {
            match event {
                RdpOutputEvent::Image {
                    buffer,
                    width,
                    height,
                    region,
                } => {
                    tracing::debug!(
                        "🎨 GTK: Received Image event: {}x{} ({} bytes) region={:?}",
                        width.get(),
                        height.get(),
                        buffer.len(),
                        region
                    );
                    rdp_widget_events.update_image(buffer, width.get(), height.get(), region);
                    
                    // Track frame for FPS calculation
                    stats_history_for_frames.borrow_mut().record_frame();
                    
                    let mut last_size = last_frame_size_clone.borrow_mut();
                    let new_size = (width.get(), height.get());
                    if *last_size != new_size {
                        tracing::info!(
                            width = new_size.0,
                            height = new_size.1,
                            "Received frame with new dimensions"
                        );
                        *last_size = new_size;
                    }
                }
                RdpOutputEvent::ConnectionFailure(error) => {
                    eprintln!("RDP Connection failed: {:?}", error);

                    // Get user-friendly error message
                    let (title, message) = format_rdp_error(&error);

                    // Show error dialog and close window when user dismisses it
                    let dialog = gtk::AlertDialog::builder()
                        .message(&title)
                        .detail(&message)
                        .build();

                    let window_to_close = rd_window_events.clone();
                    let close_intent_for_dialog = close_intent_for_events.clone();
                    dialog.choose(
                        Some(&rd_window_events),
                        None::<&gtk::gio::Cancellable>,
                        move |_result| {
                            // Close window after user dismisses the error dialog
                            close_intent_for_dialog.set(CloseIntent::Programmatic);
                            window_to_close.close();
                        },
                    );
                }
                RdpOutputEvent::Terminated(result) => {
                    match result {
                        Ok(reason) => println!("RDP session terminated: {:?}", reason),
                        Err(error) => eprintln!("RDP session error: {:?}", error),
                    }
                    close_intent_for_events.set(CloseIntent::Programmatic);
                    rd_window_events.close();
                }
                RdpOutputEvent::PointerDefault => {
                    rdp_widget_events.set_cursor_default();
                }
                RdpOutputEvent::PointerHidden => {
                    rdp_widget_events.set_cursor_hidden();
                }
                RdpOutputEvent::PointerPosition { x: _, y: _ } => {
                    // Position is handled by the server's cursor rendering
                    // We don't need to do anything here as the server controls cursor position
                }
                RdpOutputEvent::PointerBitmap(pointer) => {
                    rdp_widget_events.set_cursor_from_bitmap(pointer);
                }
                RdpOutputEvent::ConnectionStats(stats) => {
                    stats_history_for_events.borrow_mut().update(&stats);
                }
            }
        }
    });

    // Add drag functionality using the overlay for consistent coordinates
    let gesture_click = gtk::GestureClick::new();
    gesture_click.set_button(1); // Left mouse button

    let is_dragging = std::rc::Rc::new(std::cell::RefCell::new(false));
    let drag_start_x = std::rc::Rc::new(std::cell::RefCell::new(0.0));
    let drag_start_position = std::rc::Rc::new(std::cell::RefCell::new(0.0));
    let drag_offset_x = std::rc::Rc::new(std::cell::RefCell::new(0.0)); // Offset within control bar

    // Handle mouse press to start drag
    let is_dragging_press = is_dragging.clone();
    let drag_start_x_press = drag_start_x.clone();
    let drag_start_position_press = drag_start_position.clone();
    let drag_offset_x_press = drag_offset_x.clone();
    let control_bar_x_position_press = control_bar_x_position.clone();
    let control_bar_for_press = control_bar.clone();

    gesture_click.connect_pressed(move |_, _, x, y| {
        // Check if click is within the control bar area using bounds
        // Extend the draggable area to include padding above the control bar
        let control_bar_x = control_bar_for_press.margin_start() as f64;
        let control_bar_y = 0.0; // Start draggable area from top of window instead of margin_top
        let control_bar_width = control_bar_for_press.width() as f64;
        let control_bar_height =
            control_bar_for_press.height() as f64 + control_bar_for_press.margin_top() as f64; // Include the top margin in draggable height

        if x >= control_bar_x
            && x <= control_bar_x + control_bar_width
            && y >= control_bar_y
            && y <= control_bar_y + control_bar_height
        {
            *is_dragging_press.borrow_mut() = true;
            *drag_start_x_press.borrow_mut() = x;
            *drag_start_position_press.borrow_mut() = *control_bar_x_position_press.borrow();
            *drag_offset_x_press.borrow_mut() = x - control_bar_x; // Offset within control bar
        }
    });

    // Handle mouse release to end drag
    let is_dragging_release = is_dragging.clone();

    gesture_click.connect_released(move |_, _, _, _| {
        *is_dragging_release.borrow_mut() = false;
    });

    // Handle cancellation
    let is_dragging_cancel = is_dragging.clone();

    gesture_click.connect_cancel(move |_, _| {
        *is_dragging_cancel.borrow_mut() = false;
    });

    // Add the gesture to the overlay for consistent coordinate system
    overlay.add_controller(gesture_click);

    // Add motion tracking for real-time drag feedback
    let motion_controller = gtk::EventControllerMotion::new();
    let is_dragging_motion = is_dragging.clone();
    let drag_offset_x_motion = drag_offset_x.clone();
    let control_bar_for_motion = control_bar.clone();
    let control_bar_x_position_motion = control_bar_x_position.clone();
    let user_has_moved_toolbar_motion = user_has_moved_toolbar.clone();
    let overlay_for_motion = overlay.clone();
    let apply_toolbar_position_motion = apply_toolbar_position.clone();

    motion_controller.connect_motion(move |controller, x, _| {
        // Only process drag if we're flagged as dragging AND there's an actual button press
        let is_currently_dragging = *is_dragging_motion.borrow();

        if is_currently_dragging {
            // Check if we have a current event that includes button state
            if let Some(event) = controller.current_event() {
                // Check if this is a motion event with no button pressed
                if let Some(_) = event.downcast_ref::<gtk::gdk::ButtonEvent>() {
                    // This is actually a button event, not motion
                    return;
                }

                // For motion events, check modifier state for button press
                let state = event.modifier_state();
                // GDK_BUTTON1_MASK indicates left mouse button is pressed
                if !state.contains(gtk::gdk::ModifierType::BUTTON1_MASK) {
                    // No button pressed - spurious motion event, ignore it
                    return;
                }
            }

            let offset_within_bar = *drag_offset_x_motion.borrow();

            // Calculate the new control bar position
            // x is the current mouse position, we want the control bar to be positioned
            // so that the mouse is still at the same offset within the control bar
            let new_control_bar_x = x - offset_within_bar;

            // Get window width for boundary checking
            let window_width = overlay_for_motion.width() as f64;
            let control_bar_width = control_bar_for_motion.width() as f64;

            // Only apply boundaries if we have valid dimensions
            if window_width > 0.0 && control_bar_width > 0.0 {
                // Clamp position to stay within window bounds
                let clamped_x = new_control_bar_x
                    .max(0.0)
                    .min(window_width - control_bar_width);

                // Convert absolute position to relative position (0.0 to 1.0)
                let relative_pos = absolute_to_relative(clamped_x, window_width, control_bar_width);

                // Check if position actually changed (with small tolerance for floating point precision)
                let current_relative_position = *control_bar_x_position_motion.borrow();
                let position_changed = (relative_pos - current_relative_position).abs() > 0.01;

                // Update position in real-time during drag with exact precision
                apply_toolbar_position_motion(Some(relative_pos));

                // Only mark as user-moved if position actually changed
                if position_changed {
                    *user_has_moved_toolbar_motion.borrow_mut() = true;
                }
            }
        }
    });

    // Handle cursor leaving the window during drag
    let is_dragging_leave = is_dragging.clone();
    motion_controller.connect_leave(move |_| {
        // Stop dragging when cursor leaves the window
        *is_dragging_leave.borrow_mut() = false;
        // Note: Visibility will be handled by the show/hide motion controller
    });

    overlay.add_controller(motion_controller);

    // Add a move cursor when hovering over the draggable area (extended control bar area)
    let motion_controller_cursor = gtk::EventControllerMotion::new();
    let control_bar_for_cursor = control_bar.clone();

    motion_controller_cursor.connect_motion(move |controller, x, y| {
        // Check if cursor is in the extended draggable area
        let control_bar_x = control_bar_for_cursor.margin_start() as f64;
        let control_bar_y = 0.0; // Start from top of window
        let control_bar_width = control_bar_for_cursor.width() as f64;
        let control_bar_height =
            control_bar_for_cursor.height() as f64 + control_bar_for_cursor.margin_top() as f64; // Include top margin

        if x >= control_bar_x
            && x <= control_bar_x + control_bar_width
            && y >= control_bar_y
            && y <= control_bar_y + control_bar_height
        {
            if let Some(widget) = controller.widget() {
                widget.set_cursor_from_name(Some("grab"));
            }
        } else {
            if let Some(widget) = controller.widget() {
                widget.set_cursor_from_name(Some("default"));
            }
        }
    });

    motion_controller_cursor.connect_leave(move |controller| {
        if let Some(widget) = controller.widget() {
            widget.set_cursor_from_name(Some("default"));
        }
    });

    overlay.add_controller(motion_controller_cursor);

    // Initially hide control bar until it's been centered to avoid showing it at wrong position
    control_bar.set_visible(false);

    // State management for control bar visibility
    let is_pinned = std::rc::Rc::new(std::cell::RefCell::new(false));
    let is_fullscreen = std::rc::Rc::new(std::cell::RefCell::new(false));

    // Hotkey capture button functionality
    let hotkey_capture_for_button = hotkey_capture_enabled.clone();
    let rd_window_for_hotkey = rd_window.clone();
    let rdp_focus_for_hotkey = rdp_widget.widget().clone();
    hotkey_button.connect_clicked(move |button| {
        let enabled = hotkey_capture_for_button.get();
        let new_state = !enabled;
        hotkey_capture_for_button.set(new_state);

        if new_state {
            // Enabled - set full opacity and re-enable shortcuts
            button.set_opacity(1.0);
            button.set_tooltip_text(Some("Global hotkeys enabled (Win, Alt+Tab, etc.)"));

            // Inhibit system shortcuts again if mouse is over window
            if let Some(surface) = rd_window_for_hotkey.surface() {
                if let Ok(tl) = surface.downcast::<gdk::Toplevel>() {
                    tl.inhibit_system_shortcuts(None::<&gdk::Event>);
                }
            }
        } else {
            // Disabled - set reduced opacity and restore shortcuts
            button.set_opacity(0.4);
            button.set_tooltip_text(Some("Global hotkeys disabled (click to enable)"));

            // Restore system shortcuts
            if let Some(surface) = rd_window_for_hotkey.surface() {
                if let Ok(tl) = surface.downcast::<gdk::Toplevel>() {
                    tl.restore_system_shortcuts();
                }
            }
        }

        rdp_focus_for_hotkey.grab_focus();
    });

    // Pin button functionality
    let control_bar_for_pin = control_bar.clone();
    let is_pinned_clone = is_pinned.clone();
    let is_fullscreen_for_pin = is_fullscreen.clone();
    let apply_toolbar_position_for_pin = apply_toolbar_position.clone();
    let rdp_focus_for_pin = rdp_widget.widget().clone();
    pin_button.connect_clicked(move |button| {
        let mut pinned = is_pinned_clone.borrow_mut();
        *pinned = !*pinned;

        if *pinned {
            // Pinned - set full opacity
            button.set_opacity(1.0);
            button.set_tooltip_text(Some("Unpin controls"));
            control_bar_for_pin.set_visible(true);
            apply_toolbar_position_for_pin(None);
        } else {
            // Unpinned - set reduced opacity
            button.set_opacity(0.4);
            button.set_tooltip_text(Some("Pin controls"));
            // In windowed mode, always show controls
            // In fullscreen mode, hide unless mouse is at top
            if *is_fullscreen_for_pin.borrow() {
                control_bar_for_pin.set_visible(false);
            } else {
                apply_toolbar_position_for_pin(None);
            }
        }

        rdp_focus_for_pin.grab_focus();
    });

    // Minimize button functionality
    let rd_window_for_minimize = rd_window.clone();
    minimize_button.connect_clicked(move |_| {
        rd_window_for_minimize.minimize();
    });

    // Close button functionality
    let rd_window_for_close = rd_window.clone();
    let close_intent_for_button = close_intent.clone();
    close_button.connect_clicked(move |_| {
        close_intent_for_button.set(CloseIntent::User);
        rd_window_for_close.close();
    });

    // Info button functionality - show connection info dialog
    let rd_window_for_info = rd_window.clone();
    let server_for_info = server.clone();
    let username_for_info = username.clone();
    let domain_for_info = domain.clone();
    let stats_history_for_info = stats_history.clone();
    info_button.connect_clicked(move |_| {
        let dialog = gtk::Window::builder()
            .transient_for(&rd_window_for_info)
            .modal(false)
            .title("Connection Information")
            .default_width(600)
            .default_height(500)
            .resizable(true)
            .build();

        let dialog_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        dialog_box.set_margin_top(20);
        dialog_box.set_margin_bottom(20);
        dialog_box.set_margin_start(20);
        dialog_box.set_margin_end(20);

        // Connection details section
        let details_frame = gtk::Frame::new(Some("Connection Details"));
        let details_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
        details_box.set_margin_top(5);
        details_box.set_margin_bottom(5);
        details_box.set_margin_start(10);
        details_box.set_margin_end(10);

        // Server info
        let server_label = gtk::Label::new(None);
        server_label.set_markup(&format!("<b>Server:</b> {}", server_for_info));
        server_label.set_xalign(0.0);
        details_box.append(&server_label);

        // Username info
        let username_label = gtk::Label::new(None);
        let username_text = if domain_for_info.is_empty() {
            username_for_info.clone()
        } else {
            format!("{}\\{}", domain_for_info, username_for_info)
        };
        username_label.set_markup(&format!("<b>User:</b> {}", username_text));
        username_label.set_xalign(0.0);
        details_box.append(&username_label);

        // Resolution info
        let resolution_label = gtk::Label::new(None);
        resolution_label.set_markup(&format!(
            "<b>Resolution:</b> {}×{}",
            logical_config_width, logical_config_height
        ));
        resolution_label.set_xalign(0.0);
        details_box.append(&resolution_label);

        // Scale info
        let scale_label = gtk::Label::new(None);
        scale_label.set_markup(&format!(
            "<b>DPI Scale:</b> {}%",
            initial_scale_percent
        ));
        scale_label.set_xalign(0.0);
        details_box.append(&scale_label);

        details_frame.set_child(Some(&details_box));
        dialog_box.append(&details_frame);

        // Live statistics section
        let stats_frame = gtk::Frame::new(Some("Live Statistics"));
        let stats_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
        stats_box.set_margin_top(5);
        stats_box.set_margin_bottom(5);
        stats_box.set_margin_start(10);
        stats_box.set_margin_end(10);

        // Protocol label
        let protocol_label = gtk::Label::new(None);
        protocol_label.set_xalign(0.0);
        stats_box.append(&protocol_label);

        // Bandwidth labels
        let bandwidth_sent_label = gtk::Label::new(None);
        bandwidth_sent_label.set_xalign(0.0);
        stats_box.append(&bandwidth_sent_label);

        let bandwidth_recv_label = gtk::Label::new(None);
        bandwidth_recv_label.set_xalign(0.0);
        stats_box.append(&bandwidth_recv_label);

        // RTT label
        let rtt_label = gtk::Label::new(None);
        rtt_label.set_xalign(0.0);
        stats_box.append(&rtt_label);

        // FPS label
        let fps_label = gtk::Label::new(None);
        fps_label.set_xalign(0.0);
        stats_box.append(&fps_label);

        // Simple text-based graphs (using block characters)
        let bandwidth_sent_graph_label = gtk::Label::new(None);
        bandwidth_sent_graph_label.set_xalign(0.0);
        bandwidth_sent_graph_label.set_use_markup(true);
        bandwidth_sent_graph_label.set_wrap(false);
        bandwidth_sent_graph_label.set_selectable(false);
        stats_box.append(&bandwidth_sent_graph_label);

        let bandwidth_recv_graph_label = gtk::Label::new(None);
        bandwidth_recv_graph_label.set_xalign(0.0);
        bandwidth_recv_graph_label.set_use_markup(true);
        bandwidth_recv_graph_label.set_wrap(false);
        bandwidth_recv_graph_label.set_selectable(false);
        stats_box.append(&bandwidth_recv_graph_label);

        let rtt_graph_label = gtk::Label::new(None);
        rtt_graph_label.set_xalign(0.0);
        rtt_graph_label.set_use_markup(true);
        rtt_graph_label.set_wrap(false);
        stats_box.append(&rtt_graph_label);

        stats_frame.set_child(Some(&stats_box));
        dialog_box.append(&stats_frame);

        // Close button for dialog
        let close_btn = Button::builder()
            .label("Close")
            .halign(gtk::Align::End)
            .build();
        let dialog_for_close = dialog.clone();
        close_btn.connect_clicked(move |_| {
            dialog_for_close.close();
        });
        dialog_box.append(&close_btn);

        dialog.set_child(Some(&dialog_box));

        // Update timer for live stats
        let protocol_label_update = protocol_label.clone();
        let bandwidth_sent_label_update = bandwidth_sent_label.clone();
        let bandwidth_recv_label_update = bandwidth_recv_label.clone();
        let rtt_label_update = rtt_label.clone();
        let fps_label_update = fps_label.clone();
        let bandwidth_sent_graph_label_update = bandwidth_sent_graph_label.clone();
        let bandwidth_recv_graph_label_update = bandwidth_recv_graph_label.clone();
        let rtt_graph_label_update = rtt_graph_label.clone();
        let stats_history_update = stats_history_for_info.clone();

        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let stats = stats_history_update.borrow();
            
            // Update protocol
            protocol_label_update.set_markup(&format!("<b>Protocol:</b> {}", stats.protocol));
            
            // Calculate bandwidth rates
            let (tx_rate, rx_rate) = if stats.bandwidth_sent.len() >= 2 {
                let recent = &stats.bandwidth_sent[stats.bandwidth_sent.len() - 1];
                let prev = &stats.bandwidth_sent[stats.bandwidth_sent.len() - 2];
                let time_diff = recent.0 - prev.0;
                let tx_diff = recent.1.saturating_sub(prev.1);
                
                let recent_rx = &stats.bandwidth_received[stats.bandwidth_received.len() - 1];
                let prev_rx = &stats.bandwidth_received[stats.bandwidth_received.len() - 2];
                let rx_diff = recent_rx.1.saturating_sub(prev_rx.1);
                
                if time_diff > 0.0 {
                    let tx = (tx_diff as f64 / time_diff / 1024.0) as u64; // KB/s
                    let rx = (rx_diff as f64 / time_diff / 1024.0) as u64;
                    (tx, rx)
                } else {
                    (0, 0)
                }
            } else {
                (0, 0)
            };
            
            bandwidth_sent_label_update.set_markup(&format!(
                "<b>Sent:</b> {} KB/s ({} MB total)",
                tx_rate,
                stats.last_bytes_sent / 1024 / 1024
            ));
            bandwidth_recv_label_update.set_markup(&format!(
                "<b>Received:</b> {} KB/s ({} MB total)",
                rx_rate,
                stats.last_bytes_received / 1024 / 1024
            ));
            
            // Update RTT
            if let Some(last_rtt) = stats.rtt_history.back() {
                rtt_label_update.set_markup(&format!("<b>Response Time:</b> {} ms", last_rtt.1));
            } else {
                rtt_label_update.set_markup("<b>Response Time:</b> N/A");
            }
            
            // Update FPS
            let fps = stats.get_fps();
            fps_label_update.set_markup(&format!("<b>Frame Rate:</b> {:.1} FPS", fps));
            
            // Create simple text-based graphs with colors - show last 2 minutes with narrow segments
            // Convert bandwidth from cumulative bytes to KB/s rate
            let bandwidth_sent_graph = create_bandwidth_graph(
                &stats.bandwidth_sent,
                "Bandwidth Sent (KB/s)",
                80,  // Reduced from 120 for narrower segments
                "#f66151" // red
            );
            bandwidth_sent_graph_label_update.set_markup(&bandwidth_sent_graph);
            
            let bandwidth_recv_graph = create_bandwidth_graph(
                &stats.bandwidth_received,
                "Bandwidth Received (KB/s)",
                80,  // Reduced from 120 for narrower segments
                "#2ec27e" // green
            );
            bandwidth_recv_graph_label_update.set_markup(&bandwidth_recv_graph);
            
            let rtt_graph = create_text_graph_u32(
                &stats.rtt_history,
                "Response Time (ms)",
                80,  // Reduced from 120 for narrower segments
                "#1c71d8" // blue
            );
            rtt_graph_label_update.set_markup(&rtt_graph);
            
            glib::ControlFlow::Continue
        });

        dialog.present();
    });

    // Helper function to create bandwidth graph (converts from cumulative bytes to KB/s rate)
    fn create_bandwidth_graph(
        data: &VecDeque<(f64, u64)>,
        title: &str,
        max_points: usize,
        color: &str,
    ) -> String {
        if data.len() < 2 {
            return format!("<span font_family='monospace'>\n{}: <span foreground='dim'>No data yet</span></span>", title);
        }
        
        // Calculate rates from cumulative bytes
        let mut rates = Vec::new();
        for i in 1..data.len() {
            let (time_prev, bytes_prev) = data[i - 1];
            let (time_curr, bytes_curr) = data[i];
            let time_diff = time_curr - time_prev;
            if time_diff > 0.0 {
                let bytes_diff = bytes_curr.saturating_sub(bytes_prev);
                let rate_kbps = bytes_diff as f64 / time_diff / 1024.0; // KB/s
                rates.push(rate_kbps);
            }
        }
        
        if rates.is_empty() {
            return format!("<span font_family='monospace'>\n{}: <span foreground='dim'>No activity</span></span>", title);
        }
        
        // Take only the last max_points
        let start_idx = if rates.len() > max_points { rates.len() - max_points } else { 0 };
        let values: Vec<f64> = rates.iter().skip(start_idx).copied().collect();
        
        let max_val = values.iter().cloned().fold(0.0f64, f64::max);
        
        if max_val == 0.0 {
            return format!("<span font_family='monospace'>\n{}: <span foreground='dim'>No activity</span></span>", title);
        }
        
        // Create multi-line graph for more height
        let bars = " ▁▂▃▄▅▆▇█";
        let num_lines = 3;  // Number of vertical lines for the graph
        
        // Build graph lines from top to bottom
        let mut lines = vec![String::new(); num_lines];
        
        for v in values.iter() {
            let normalized = (v / max_val * (bars.len() - 1) as f64 * num_lines as f64).round() as usize;
            
            // Distribute the height across multiple lines
            for line_idx in 0..num_lines {
                let line_threshold = (num_lines - line_idx) * (bars.len() - 1);
                let line_value = if normalized >= line_threshold {
                    let overflow = normalized - line_threshold + 1;
                    overflow.min(bars.len() - 1)
                } else if line_idx == num_lines - 1 {
                    // Bottom line shows at least the base level
                    normalized.min(bars.len() - 1)
                } else {
                    0
                };
                
                let bar = bars.chars().nth(line_value).unwrap_or(' ');
                lines[line_idx].push(bar);
            }
        }
        
        // Pad lines with spaces if less than max_points
        for line in lines.iter_mut() {
            for _ in 0..(max_points.saturating_sub(values.len())) {
                line.push(' ');
            }
        }
        
        // Join lines with newlines
        let graph = lines.join("\n");
        
        format!(
            "<span font_family='monospace'>\n{} (max: {:.0} KB/s)\n<span foreground='{}'>{}</span></span>",
            title, max_val, color, graph
        )
    }

    // Helper function to create text-based graphs (unused now, keeping for reference)
    #[allow(dead_code)]
    fn create_text_graph(
        data: &VecDeque<(f64, u64)>,
        title: &str,
        transform: impl Fn(u64) -> f64,
        max_points: usize,
        color: &str,
    ) -> String {
        if data.is_empty() {
            return format!("<span font_family='monospace'>\n{}: <span foreground='dim'>No data yet</span></span>", title);
        }
        
        // Take only the last max_points
        let start_idx = if data.len() > max_points { data.len() - max_points } else { 0 };
        let values: Vec<f64> = data.iter()
            .skip(start_idx)
            .map(|(_, v)| transform(*v))
            .collect();
        
        let max_val = values.iter().cloned().fold(0.0f64, f64::max);
        
        if max_val == 0.0 {
            return format!("<span font_family='monospace'>\n{}: <span foreground='dim'>No activity</span></span>", title);
        }
        
        let bars = "▁▂▃▄▅▆▇█";
        
        // Pad to fixed width if needed
        let mut graph = String::new();
        for v in values.iter() {
            let normalized = (v / max_val * 7.0).round() as usize;
            let bar = bars.chars().nth(normalized.min(7)).unwrap_or('▁');
            graph.push(bar);
        }
        
        // Pad with spaces if less than max_points
        for _ in 0..(max_points.saturating_sub(values.len())) {
            graph.push(' ');
        }
        
        format!(
            "<span font_family='monospace'>\n{} (max: {:.1} MB)\n<span foreground='{}'>{}</span></span>",
            title, max_val, color, graph
        )
    }

    fn create_text_graph_u32(
        data: &VecDeque<(f64, u32)>,
        title: &str,
        max_points: usize,
        color: &str,
    ) -> String {
        if data.is_empty() {
            return format!("<span font_family='monospace'>\n{}: <span foreground='dim'>No data yet</span></span>", title);
        }
        
        // Take only the last max_points
        let start_idx = if data.len() > max_points { data.len() - max_points } else { 0 };
        let values: Vec<u32> = data.iter()
            .skip(start_idx)
            .map(|(_, v)| *v)
            .collect();
        
        let max_val = values.iter().cloned().max().unwrap_or(1);
        
        // Create multi-line graph for more height
        let bars = " ▁▂▃▄▅▆▇█";
        let num_lines = 3;  // Number of vertical lines for the graph
        
        // Build graph lines from top to bottom
        let mut lines = vec![String::new(); num_lines];
        
        for v in values.iter() {
            let normalized = (*v as f64 / max_val as f64 * (bars.len() - 1) as f64 * num_lines as f64).round() as usize;
            
            // Distribute the height across multiple lines
            for line_idx in 0..num_lines {
                let line_threshold = (num_lines - line_idx) * (bars.len() - 1);
                let line_value = if normalized >= line_threshold {
                    let overflow = normalized - line_threshold + 1;
                    overflow.min(bars.len() - 1)
                } else if line_idx == num_lines - 1 {
                    // Bottom line shows at least the base level
                    normalized.min(bars.len() - 1)
                } else {
                    0
                };
                
                let bar = bars.chars().nth(line_value).unwrap_or(' ');
                lines[line_idx].push(bar);
            }
        }
        
        // Pad lines with spaces if less than max_points
        for line in lines.iter_mut() {
            for _ in 0..(max_points.saturating_sub(values.len())) {
                line.push(' ');
            }
        }
        
        // Join lines with newlines
        let graph = lines.join("\n");
        
        format!(
            "<span font_family='monospace'>\n{} (max: {} ms)\n<span foreground='{}'>{}</span></span>",
            title, max_val, color, graph
        )
    }

    // Toggle fullscreen button
    let rd_window_for_menu = rd_window.clone();
    let is_fullscreen_for_menu = is_fullscreen.clone();
    let menu_button_for_toggle = menu_button.clone();
    let rdp_focus_for_menu = rdp_widget.widget().clone();
    menu_button.connect_clicked(move |_| {
        let fullscreen = is_fullscreen_for_menu.borrow();
        if *fullscreen {
            rd_window_for_menu.unfullscreen();
            menu_button_for_toggle.set_icon_name("view-fullscreen-symbolic");
            menu_button_for_toggle.set_tooltip_text(Some("Enter fullscreen"));
        } else {
            rd_window_for_menu.fullscreen();
            menu_button_for_toggle.set_icon_name("view-restore-symbolic");
            menu_button_for_toggle.set_tooltip_text(Some("Exit fullscreen"));
        }

        rdp_focus_for_menu.grab_focus();
    });

    // Mouse motion to show/hide controls (only in fullscreen when not pinned)
    let motion_controller_show_hide = gtk::EventControllerMotion::new();
    let control_bar_for_motion_show_hide = control_bar.clone();
    let is_pinned_for_motion = is_pinned.clone();
    let is_fullscreen_for_motion = is_fullscreen.clone();
    let is_dragging_for_motion = is_dragging.clone(); // Add dragging state check
    let apply_toolbar_position_for_show_hide = apply_toolbar_position.clone();

    motion_controller_show_hide.connect_motion(move |_, _x, y| {
        let pinned = *is_pinned_for_motion.borrow();
        let fullscreen = *is_fullscreen_for_motion.borrow();
        let dragging = *is_dragging_for_motion.borrow();

        // In windowed mode or when pinned, always show controls
        // In fullscreen mode when not pinned, show only when mouse is near top OR when dragging
        if !fullscreen || pinned || dragging || (fullscreen && y < 50.0) {
            control_bar_for_motion_show_hide.set_visible(true);
            apply_toolbar_position_for_show_hide(None);
        } else if fullscreen && !pinned && !dragging && y >= 50.0 {
            control_bar_for_motion_show_hide.set_visible(false);
        }
    });
    rd_window.add_controller(motion_controller_show_hide);

    // Window state tracking for fullscreen changes and resizing
    let is_fullscreen_for_state = is_fullscreen.clone();
    let control_bar_for_state = control_bar.clone();
    let user_has_moved_toolbar_for_state = user_has_moved_toolbar.clone();
    let apply_toolbar_position_for_state = apply_toolbar_position.clone();

    rd_window.connect_notify_local(Some("fullscreened"), move |window, _| {
        let mut fullscreen = is_fullscreen_for_state.borrow_mut();
        let new_fullscreen = window.is_fullscreen();

        if new_fullscreen != *fullscreen {
            *fullscreen = new_fullscreen;

            // Enable/disable keyboard grab based on fullscreen state
            // In fullscreen, we want to capture all keyboard shortcuts (Alt+Tab, etc.)
            if new_fullscreen {
                // Request keyboard focus and set input mode to capture all keys
                window.set_focus_visible(true);
            }

            // Re-center toolbar on both entering and exiting fullscreen (if user hasn't moved it)
            if !*user_has_moved_toolbar_for_state.borrow() {
                let apply_toolbar_position_for_center = apply_toolbar_position_for_state.clone();
                gtk::glib::idle_add_local_once(move || {
                    apply_toolbar_position_for_center(Some(0.5));
                });
            } else {
                let apply_toolbar_position_for_restore = apply_toolbar_position_for_state.clone();
                gtk::glib::idle_add_local_once(move || {
                    apply_toolbar_position_for_restore(None);
                });
            }

            if !*fullscreen {
                // Exiting fullscreen - always show controls in windowed mode
                control_bar_for_state.set_visible(true);
                apply_toolbar_position_for_state(None);
            }
        }
    });

    // Handle maximize state changes to re-center toolbar if not moved by user
    let user_has_moved_toolbar_for_maximize = user_has_moved_toolbar.clone();
    let apply_toolbar_position_for_maximize = apply_toolbar_position.clone();

    rd_window.connect_notify_local(Some("maximized"), move |_, _| {
        // Re-center if user hasn't moved the toolbar
        if !*user_has_moved_toolbar_for_maximize.borrow() {
            let apply_toolbar_position_for_idle = apply_toolbar_position_for_maximize.clone();
            gtk::glib::idle_add_local_once(move || {
                apply_toolbar_position_for_idle(Some(0.5));
            });
        }
    });

    // Handle window restore (from minimize/hide) to reposition toolbar
    let user_has_moved_toolbar_for_restore = user_has_moved_toolbar.clone();
    let apply_toolbar_position_for_restore = apply_toolbar_position.clone();

    rd_window.connect_notify_local(Some("is-active"), move |_, _| {
        // When window becomes active again (e.g., restored from minimize), reposition toolbar
        gtk::glib::idle_add_local_once({
            let user_has_moved_toolbar = user_has_moved_toolbar_for_restore.clone();
            let apply_toolbar_position_for_idle = apply_toolbar_position_for_restore.clone();

            move || {
                if !*user_has_moved_toolbar.borrow() {
                    apply_toolbar_position_for_idle(Some(0.5));
                } else {
                    apply_toolbar_position_for_idle(None);
                }
            }
        });
    });

    // Handle window resize to keep toolbar in view and re-center if needed
    let user_has_moved_toolbar_for_resize = user_has_moved_toolbar.clone();
    let apply_toolbar_position_for_resize = apply_toolbar_position.clone();

    // Add a size allocation handler to handle resizing
    rd_window.connect_notify_local(Some("default-width"), move |_, _| {
        gtk::glib::idle_add_local_once({
            let user_has_moved_toolbar_for_resize_inner = user_has_moved_toolbar_for_resize.clone();
            let apply_toolbar_position_for_idle = apply_toolbar_position_for_resize.clone();

            move || {
                if !*user_has_moved_toolbar_for_resize_inner.borrow() {
                    apply_toolbar_position_for_idle(Some(0.5));
                } else {
                    apply_toolbar_position_for_idle(None);
                }
            }
        });
    });

    rd_window.set_child(Some(&overlay));

    // Handle window close
    let main_window_for_close = main_window.clone();
    let input_sender_window_close = input_event_sender.clone();
    let close_intent_for_request = close_intent.clone();
    let app_for_close = app.clone();
    rd_window.connect_close_request(move |_| {
        // Send close event to RDP client
        let _ = input_sender_window_close.send(RdpInputEvent::Close);

        match close_intent_for_request.get() {
            CloseIntent::Programmatic => {
                // Show the main window again when remote desktop closes on its own
                main_window_for_close.set_visible(true);

                // Restore focus without re-centering the window so it stays put
                with_toplevel(&main_window_for_close, |tl| {
                    tl.focus(gdk::CURRENT_TIME);
                });
            }
            _ => {
                // User initiated close – quit the entire application
                app_for_close.quit();
            }
        }

        close_intent_for_request.set(CloseIntent::None);

        gtk::glib::Propagation::Proceed
    });

    // Create and start RDP client
    let config = create_rdp_config(
        &server,
        &username,
        &domain,
        &password,
        &rdp_settings,
        initial_scale_percent,
    );

    // Clone the output sender for the RDP client
    let output_sender_for_client = output_event_sender.clone();

    // Clone input sender for resize events before moving into thread
    let input_sender_resize = input_event_sender.clone();

    // Start RDP client in a separate thread with Tokio runtime
    std::thread::spawn(move || {
        let dvc_pipe_proxy_factory = DvcPipeProxyFactory::new(input_event_sender.clone());

        let clipboard_factory: Option<Box<dyn CliprdrBackendFactory + Send>> =
            match config.clipboard_type {
                ClipboardType::None => None,
                _ => Some(Box::new(ArboardClipboardFactory::new(
                    input_event_sender.clone(),
                ))),
            };

        let rdp_client = RdpClient {
            config,
            event_loop_proxy: RdpEventLoopProxy::new(output_sender_for_client),
            input_event_receiver,
            cliprdr_factory: clipboard_factory,
            dvc_pipe_proxy_factory,
        };

        // Create Tokio runtime and run the RDP client
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        runtime.block_on(async move {
            rdp_client.run().await;
        });
    });

    // Show window and focus RDP widget
    rd_window.present();
    rdp_widget.widget().grab_focus();

    let last_sent_resize = Rc::new(RefCell::new(Some((
        rdp_settings.desktopwidth,
        rdp_settings.desktopheight,
        initial_scale_percent,
    ))));
    let manual_scale_percent_for_resize = manual_scale_percent;
    let compute_resize: Rc<
        dyn Fn(
            &ApplicationWindow,
            &gtk::DrawingArea,
            i32,
            i32,
        ) -> Option<(u16, u16, u32, u32, u32)>,
    > = Rc::new(move |window, area, width, height| {
        if width <= 0 || height <= 0 {
            return None;
        }

        let widget_scale = area.scale_factor().max(1) as f64;
        let window_scale = window.scale_factor().max(1) as f64;
        let fallback_scale = widget_scale.max(window_scale);
        let mut detected_scale = fallback_scale;
        let mut monitor_limit: Option<(u32, u32)> = None;

        if let Some(surface) = window.surface() {
            let surface_scale = GtkRdpWidget::surface_fractional_scale(&surface);
            if surface_scale > 0.0 {
                detected_scale = surface_scale;
            }

            let display = surface.display();
            if let Some(monitor) = display.monitor_at_surface(&surface) {
                let monitor_scale = GtkRdpWidget::monitor_fractional_scale(&monitor);
                let geometry = monitor.geometry();
                let monitor_width = (geometry.width().max(1) as f64 * monitor_scale)
                    .round()
                    .clamp(200.0, u32::from(u16::MAX) as f64)
                    as u32;
                let monitor_height = (geometry.height().max(1) as f64 * monitor_scale)
                    .round()
                    .clamp(200.0, u32::from(u16::MAX) as f64)
                    as u32;
                monitor_limit = Some((monitor_width, monitor_height));

                if surface_scale <= 0.0 && monitor_scale > 0.0 {
                    detected_scale = monitor_scale;
                }
            }
        }

        detected_scale = detected_scale.clamp(1.0, 5.0);

        let (effective_scale, scale_factor_percent) =
            if let Some(manual_percent) = manual_scale_percent_for_resize {
                (
                    (manual_percent as f64 / 100.0).clamp(1.0, 5.0),
                    manual_percent,
                )
            } else {
                let percent = ((detected_scale * 100.0).round() as u32).clamp(100, 500);
                (detected_scale, percent)
            };

        let logical_width = width.max(1) as f64;
        let logical_height = height.max(1) as f64;

        let mut width_pixels = (logical_width * effective_scale)
            .round()
            .clamp(200.0, u32::from(u16::MAX) as f64) as u32;
        let mut height_pixels = (logical_height * effective_scale)
            .round()
            .clamp(200.0, u32::from(u16::MAX) as f64) as u32;

        if let Some((max_width, max_height)) = monitor_limit {
            width_pixels = width_pixels.min(max_width);
            height_pixels = height_pixels.min(max_height);
        }

        Some((
            width_pixels.min(u16::MAX as u32) as u16,
            height_pixels.min(u16::MAX as u32) as u16,
            scale_factor_percent,
            width_pixels,
            height_pixels,
        ))
    });

    // Handle window resize to request new desktop size from RDP server
    let window_for_initial = rd_window.clone();
    let sender_for_initial = input_sender_resize.clone();
    let compute_resize_for_initial = compute_resize.clone();
    let last_sent_initial = last_sent_resize.clone();
    let size_probe_for_initial = rdp_widget.size_probe().clone();
    size_probe_for_initial.connect_map(move |area| {
        let width = area.width();
        let height = area.height();

        if let Some((width_u16, height_u16, scale_percent, width_pixels, height_pixels)) =
            compute_resize_for_initial(&window_for_initial, area, width, height)
        {
            let logical_width = width.max(1) as u32;
            let logical_height = height.max(1) as u32;

            let mut last_sent = last_sent_initial.borrow_mut();
            if last_sent.as_ref() == Some(&(width_u16, height_u16, scale_percent)) {
                return;
            }

            tracing::info!(
                logical_width,
                logical_height,
                width_pixels,
                height_pixels,
                scale_factor_percent = scale_percent,
                "Queueing initial resize request"
            );

            let _ = sender_for_initial.send(RdpInputEvent::Resize {
                width: width_u16,
                height: height_u16,
                scale_factor: scale_percent,
                physical_size: None,
            });

            *last_sent = Some((width_u16, height_u16, scale_percent));
        }
    });

    let resize_debounce = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let window_weak = rd_window.downgrade();
    let input_sender_resize_widget = input_sender_resize.clone();

    let compute_resize_for_resize = compute_resize.clone();
    let last_sent_for_resize = last_sent_resize.clone();
    rdp_widget.size_probe().connect_resize({
        let resize_debounce = resize_debounce.clone();
        let window_weak = window_weak.clone();
        let sender_clone = input_sender_resize_widget.clone();
        let compute_resize_for_resize = compute_resize_for_resize.clone();
        let last_sent_for_resize = last_sent_for_resize.clone();
        move |area, width, height| {
            tracing::info!(width, height, "GTK widget resize event");

            if let Some(source) = resize_debounce.borrow_mut().take() {
                source.remove();
            }

            let window_weak_inner = window_weak.clone();
            let area_weak = area.downgrade();
            let sender_inner = sender_clone.clone();
            let debounce_holder = resize_debounce.clone();
            let compute_resize_timeout = compute_resize_for_resize.clone();
            let last_sent_timeout = last_sent_for_resize.clone();

            let source_id =
                gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                    let Some(window) = window_weak_inner.upgrade() else {
                        debounce_holder.borrow_mut().take();
                        return ControlFlow::Break;
                    };

                    let Some(area) = area_weak.upgrade() else {
                        debounce_holder.borrow_mut().take();
                        return ControlFlow::Break;
                    };

                    let logical_width = width.max(1) as u32;
                    let logical_height = height.max(1) as u32;

                    let Some((
                        width_u16,
                        height_u16,
                        scale_factor_percent,
                        width_pixels,
                        height_pixels,
                    )) = compute_resize_timeout(&window, &area, width, height)
                    else {
                        debounce_holder.borrow_mut().take();
                        return ControlFlow::Break;
                    };

                    let mut last_sent = last_sent_timeout.borrow_mut();
                    if last_sent.as_ref() == Some(&(width_u16, height_u16, scale_factor_percent)) {
                        debounce_holder.borrow_mut().take();
                        return ControlFlow::Break;
                    }

                    tracing::info!(
                        logical_width,
                        logical_height,
                        width_pixels,
                        height_pixels,
                        scale_factor_percent,
                        "Queueing resize request"
                    );

                    let _ = sender_inner.send(RdpInputEvent::Resize {
                        width: width_u16,
                        height: height_u16,
                        scale_factor: scale_factor_percent,
                        physical_size: None,
                    });

                    *last_sent = Some((width_u16, height_u16, scale_factor_percent));
                    debounce_holder.borrow_mut().take();
                    ControlFlow::Break
                });

            *resize_debounce.borrow_mut() = Some(source_id);
        }
    });

    // Center the control bar after the window is fully presented and laid out
    let control_bar_for_timeout_center = control_bar.clone();
    let overlay_for_timeout_center = overlay.clone();
    let apply_toolbar_position_for_timeout = apply_toolbar_position.clone();

    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
        // Force allocation to ensure we have proper dimensions
        control_bar_for_timeout_center.queue_allocate();
        overlay_for_timeout_center.queue_allocate();

        // Give it one more idle cycle to ensure allocation
        gtk::glib::idle_add_local_once({
            let control_bar_clone = control_bar_for_timeout_center.clone();
            let apply_toolbar_position_for_idle = apply_toolbar_position_for_timeout.clone();

            move || {
                apply_toolbar_position_for_idle(Some(0.5));

                // Show only after positioning
                control_bar_clone.set_visible(true);
            }
        });
    });
}

fn main() -> glib::ExitCode {
    // Initialize tracing subscriber for debug logging
    // Set RUST_LOG environment variable to control log level, e.g.:
    // RUST_LOG=debug ./irontsc
    // RUST_LOG=ironrdp=trace ./irontsc
    // RUST_LOG=ironrdp=debug,ironrdp_connector=trace ./irontsc
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_line_number(true)
        .init();

    // Create a new application with command-line handling
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    // Add command line options
    app.add_main_option(
        "autologon",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Automatically connect using saved credentials",
        None,
    );

    app.add_main_option(
        "computer",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Computer name or IP address",
        Some("ADDRESS"),
    );

    app.add_main_option(
        "username",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Username for authentication",
        Some("USERNAME"),
    );

    app.add_main_option(
        "password",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Password for authentication",
        Some("PASSWORD"),
    );

    app.add_main_option(
        "domain",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Domain name",
        Some("DOMAIN"),
    );

    // Handle command line
    app.connect_command_line(|app, cmd_line| {
        let options = cmd_line.options_dict();
        let autologon = options.contains("autologon");
        eprintln!("Command line handler: autologon={}", autologon);

        // Extract command line parameters
        let computer = options.lookup::<String>("computer").ok().flatten();
        let username = options.lookup::<String>("username").ok().flatten();
        let password = options.lookup::<String>("password").ok().flatten();
        let domain = options.lookup::<String>("domain").ok().flatten();

        // Store parameters in app data BEFORE activating
        unsafe {
            app.set_data("autologon", autologon);
            if let Some(comp) = computer {
                app.set_data("computer", comp);
            }
            if let Some(user) = username {
                app.set_data("username", user);
            }
            if let Some(pass) = password {
                app.set_data("password", pass);
            }
            if let Some(dom) = domain {
                app.set_data("domain", dom);
            }
        }

        app.activate();

        glib::ExitCode::SUCCESS
    });

    // Connect to "activate" signal of `app`
    app.connect_activate(|app| {
        let autologon = unsafe {
            app.data::<bool>("autologon")
                .map(|ptr| *ptr.as_ptr())
                .unwrap_or(false)
        };
        eprintln!("Activate handler: retrieved autologon={}", autologon);
        build_ui(app, autologon);
    });

    // Run the application
    app.run()
}

// Complete replacement for build_ui function with tabbed interface

fn build_ui(app: &Application, autologon: bool) {
    eprintln!("build_ui called with autologon={}", autologon);

    let style_manager = adw::StyleManager::default();
    style_manager.set_color_scheme(adw::ColorScheme::Default);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Remote Desktop Connection")
        .build();

    let window_clone = window.clone();

    // Main container
    let dialog_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    dialog_box.set_margin_top(12);
    dialog_box.set_margin_bottom(12);
    dialog_box.set_margin_start(12);
    dialog_box.set_margin_end(12);

    // Heading
    let heading = gtk::Label::new(Some("Remote Desktop Connection"));
    heading.add_css_class("title-1");
    heading.set_margin_bottom(12);
    dialog_box.append(&heading);

    // Load RDP settings
    let rdp_settings = Rc::new(RefCell::new(RdpSettings::load_default()));
    let mut settings_for_ui = rdp_settings.borrow().clone();

    // Override with command line parameters if provided
    unsafe {
        if let Some(computer) = app.data::<String>("computer") {
            settings_for_ui.server = (*computer.as_ptr()).clone();
            eprintln!(
                "Overriding server with command line: {}",
                settings_for_ui.server
            );
        }
        if let Some(username) = app.data::<String>("username") {
            settings_for_ui.username = (*username.as_ptr()).clone();
            eprintln!(
                "Overriding username with command line: {}",
                settings_for_ui.username
            );
        }
        if let Some(password) = app.data::<String>("password") {
            settings_for_ui.password = (*password.as_ptr()).clone();
            settings_for_ui.save_password = false; // Don't save command line passwords
            eprintln!("Overriding password with command line value");
        }
        if let Some(domain) = app.data::<String>("domain") {
            settings_for_ui.domain = (*domain.as_ptr()).clone();
            eprintln!(
                "Overriding domain with command line: {}",
                settings_for_ui.domain
            );
        }
    }

    // === BASIC FIELDS (Always visible when options are hidden) ===
    let basic_fields_box = gtk::Box::new(gtk::Orientation::Vertical, 5);

    // Computer (basic)
    let server_input = gtk::Entry::new();
    server_input.set_hexpand(true);
    server_input.set_text(&settings_for_ui.server);
    let server_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let server_label = gtk::Label::new(Some("Computer:"));
    server_label.set_width_chars(12);
    server_label.set_xalign(0.0);
    server_box.append(&server_label);
    server_box.append(&server_input);
    basic_fields_box.append(&server_box);

    // Username (basic)
    let username_input = gtk::Entry::new();
    username_input.set_hexpand(true);
    username_input.set_text(&settings_for_ui.username);
    let username_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let username_label = gtk::Label::new(Some("User name:"));
    username_label.set_width_chars(12);
    username_label.set_xalign(0.0);
    username_box.append(&username_label);
    username_box.append(&username_input);
    basic_fields_box.append(&username_box);

    // Domain (basic)
    let domain_input = gtk::Entry::new();
    domain_input.set_hexpand(true);
    domain_input.set_text(&settings_for_ui.domain);
    let domain_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let domain_label = gtk::Label::new(Some("Domain:"));
    domain_label.set_width_chars(12);
    domain_label.set_xalign(0.0);
    domain_box.append(&domain_label);
    domain_box.append(&domain_input);
    basic_fields_box.append(&domain_box);

    // Password (basic)
    let password_input = gtk::Entry::new();
    password_input.set_visibility(false);
    password_input.set_hexpand(true);
    if !settings_for_ui.password.is_empty() {
        password_input.set_text(&settings_for_ui.password);
    }
    let password_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let password_label = gtk::Label::new(Some("Password:"));
    password_label.set_width_chars(12);
    password_label.set_xalign(0.0);
    password_box.append(&password_label);
    password_box.append(&password_input);
    basic_fields_box.append(&password_box);

    // Save password checkbox (basic)
    let save_password_checkbox = gtk::CheckButton::with_label("Save password");
    save_password_checkbox.set_active(settings_for_ui.save_password);
    let save_password_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let save_password_label = gtk::Label::new(Some(""));
    save_password_label.set_width_chars(12);
    save_password_box.append(&save_password_label);
    save_password_box.append(&save_password_checkbox);
    basic_fields_box.append(&save_password_box);

    dialog_box.append(&basic_fields_box);

    // Create notebook for tabs
    let notebook = gtk::Notebook::new();
    notebook.set_show_border(false);

    // === GENERAL TAB ===
    let general_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    general_page.set_margin_top(10);
    general_page.set_margin_bottom(10);
    general_page.set_margin_start(10);
    general_page.set_margin_end(10);

    // Computer (in tab - synced with basic field)
    let server_input_tab = gtk::Entry::new();
    server_input_tab.set_hexpand(true);
    server_input_tab.set_text(&settings_for_ui.server);
    let server_box_tab = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let server_label_tab = gtk::Label::new(Some("Computer:"));
    server_label_tab.set_width_chars(12);
    server_label_tab.set_xalign(0.0);
    server_box_tab.append(&server_label_tab);
    server_box_tab.append(&server_input_tab);
    general_page.append(&server_box_tab);

    // Username (in tab - synced with basic field)
    let username_input_tab = gtk::Entry::new();
    username_input_tab.set_hexpand(true);
    username_input_tab.set_text(&settings_for_ui.username);
    let username_box_tab = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let username_label_tab = gtk::Label::new(Some("User name:"));
    username_label_tab.set_width_chars(12);
    username_label_tab.set_xalign(0.0);
    username_box_tab.append(&username_label_tab);
    username_box_tab.append(&username_input_tab);
    general_page.append(&username_box_tab);

    // Domain (in tab - synced with basic field)
    let domain_input_tab = gtk::Entry::new();
    domain_input_tab.set_hexpand(true);
    domain_input_tab.set_text(&settings_for_ui.domain);
    let domain_box_tab = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let domain_label_tab = gtk::Label::new(Some("Domain:"));
    domain_label_tab.set_width_chars(12);
    domain_label_tab.set_xalign(0.0);
    domain_box_tab.append(&domain_label_tab);
    domain_box_tab.append(&domain_input_tab);
    general_page.append(&domain_box_tab);

    // Password (in tab - synced with basic field)
    let password_input_tab = gtk::Entry::new();
    password_input_tab.set_visibility(false);
    password_input_tab.set_hexpand(true);
    if !settings_for_ui.password.is_empty() {
        password_input_tab.set_text(&settings_for_ui.password);
    }
    let password_box_tab = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let password_label_tab = gtk::Label::new(Some("Password:"));
    password_label_tab.set_width_chars(12);
    password_label_tab.set_xalign(0.0);
    password_box_tab.append(&password_label_tab);
    password_box_tab.append(&password_input_tab);
    general_page.append(&password_box_tab);

    // Save password checkbox (in tab - synced with basic field)
    let save_password_checkbox_tab = gtk::CheckButton::with_label("Save password");
    save_password_checkbox_tab.set_active(settings_for_ui.save_password);
    let save_password_box_tab = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let save_password_label_tab = gtk::Label::new(Some(""));
    save_password_label_tab.set_width_chars(12);
    save_password_box_tab.append(&save_password_label_tab);
    save_password_box_tab.append(&save_password_checkbox_tab);
    general_page.append(&save_password_box_tab);

    // Sync basic fields with tab fields bidirectionally with guards to prevent infinite loops
    let server_updating = Rc::new(RefCell::new(false));
    let server_input_tab_clone = server_input_tab.clone();
    let server_updating_clone = server_updating.clone();
    server_input.buffer().connect_text_notify(move |buffer| {
        if !*server_updating_clone.borrow() {
            *server_updating_clone.borrow_mut() = true;
            server_input_tab_clone.buffer().set_text(&buffer.text());
            *server_updating_clone.borrow_mut() = false;
        }
    });
    let server_input_clone = server_input.clone();
    let server_updating_clone = server_updating.clone();
    server_input_tab
        .buffer()
        .connect_text_notify(move |buffer| {
            if !*server_updating_clone.borrow() {
                *server_updating_clone.borrow_mut() = true;
                server_input_clone.buffer().set_text(&buffer.text());
                *server_updating_clone.borrow_mut() = false;
            }
        });

    let username_updating = Rc::new(RefCell::new(false));
    let username_input_tab_clone = username_input_tab.clone();
    let username_updating_clone = username_updating.clone();
    username_input.buffer().connect_text_notify(move |buffer| {
        if !*username_updating_clone.borrow() {
            *username_updating_clone.borrow_mut() = true;
            username_input_tab_clone.buffer().set_text(&buffer.text());
            *username_updating_clone.borrow_mut() = false;
        }
    });
    let username_input_clone = username_input.clone();
    let username_updating_clone = username_updating.clone();
    username_input_tab
        .buffer()
        .connect_text_notify(move |buffer| {
            if !*username_updating_clone.borrow() {
                *username_updating_clone.borrow_mut() = true;
                username_input_clone.buffer().set_text(&buffer.text());
                *username_updating_clone.borrow_mut() = false;
            }
        });

    let domain_updating = Rc::new(RefCell::new(false));
    let domain_input_tab_clone = domain_input_tab.clone();
    let domain_updating_clone = domain_updating.clone();
    domain_input.buffer().connect_text_notify(move |buffer| {
        if !*domain_updating_clone.borrow() {
            *domain_updating_clone.borrow_mut() = true;
            domain_input_tab_clone.buffer().set_text(&buffer.text());
            *domain_updating_clone.borrow_mut() = false;
        }
    });
    let domain_input_clone = domain_input.clone();
    let domain_updating_clone = domain_updating.clone();
    domain_input_tab
        .buffer()
        .connect_text_notify(move |buffer| {
            if !*domain_updating_clone.borrow() {
                *domain_updating_clone.borrow_mut() = true;
                domain_input_clone.buffer().set_text(&buffer.text());
                *domain_updating_clone.borrow_mut() = false;
            }
        });

    let password_updating = Rc::new(RefCell::new(false));
    let password_input_tab_clone = password_input_tab.clone();
    let password_updating_clone = password_updating.clone();
    password_input.buffer().connect_text_notify(move |buffer| {
        if !*password_updating_clone.borrow() {
            *password_updating_clone.borrow_mut() = true;
            password_input_tab_clone.buffer().set_text(&buffer.text());
            *password_updating_clone.borrow_mut() = false;
        }
    });
    let password_input_clone = password_input.clone();
    let password_updating_clone = password_updating.clone();
    password_input_tab
        .buffer()
        .connect_text_notify(move |buffer| {
            if !*password_updating_clone.borrow() {
                *password_updating_clone.borrow_mut() = true;
                password_input_clone.buffer().set_text(&buffer.text());
                *password_updating_clone.borrow_mut() = false;
            }
        });

    // Sync save password checkboxes
    let save_password_updating = Rc::new(RefCell::new(false));
    let save_password_checkbox_tab_clone = save_password_checkbox_tab.clone();
    let save_password_updating_clone = save_password_updating.clone();
    save_password_checkbox.connect_toggled(move |checkbox| {
        if !*save_password_updating_clone.borrow() {
            *save_password_updating_clone.borrow_mut() = true;
            save_password_checkbox_tab_clone.set_active(checkbox.is_active());
            *save_password_updating_clone.borrow_mut() = false;
        }
    });
    let save_password_checkbox_clone = save_password_checkbox.clone();
    let save_password_updating_clone = save_password_updating.clone();
    save_password_checkbox_tab.connect_toggled(move |checkbox| {
        if !*save_password_updating_clone.borrow() {
            *save_password_updating_clone.borrow_mut() = true;
            save_password_checkbox_clone.set_active(checkbox.is_active());
            *save_password_updating_clone.borrow_mut() = false;
        }
    });

    // Connection settings section
    let connection_settings_frame = gtk::Frame::new(Some("Connection settings"));
    let connection_settings_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    connection_settings_box.set_margin_top(5);
    connection_settings_box.set_margin_bottom(5);
    connection_settings_box.set_margin_start(5);
    connection_settings_box.set_margin_end(5);

    let save_button = Button::with_label("Save");
    let save_as_button = Button::with_label("Save as...");
    let open_button = Button::with_label("Open...");

    connection_settings_box.append(&open_button);
    connection_settings_box.append(&save_button);
    connection_settings_box.append(&save_as_button);
    connection_settings_frame.set_child(Some(&connection_settings_box));
    general_page.append(&connection_settings_frame);

    notebook.append_page(&general_page, Some(&gtk::Label::new(Some("General"))));

    // === DISPLAY TAB ===
    let display_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    display_page.set_margin_top(10);
    display_page.set_margin_bottom(10);
    display_page.set_margin_start(10);
    display_page.set_margin_end(10);

    // Resolution
    let resolution_frame = gtk::Frame::new(Some("Display configuration"));
    let resolution_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    resolution_box.set_margin_top(5);
    resolution_box.set_margin_bottom(5);
    resolution_box.set_margin_start(5);
    resolution_box.set_margin_end(5);

    let resolution_label = gtk::Label::new(Some("Choose the size of your remote desktop:"));
    resolution_label.set_xalign(0.0);
    resolution_box.append(&resolution_label);

    let resolution_slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 4.0, 1.0);
    resolution_slider.set_draw_value(false);
    resolution_slider.set_hexpand(true);
    resolution_slider.set_round_digits(0); // Snap to integer values

    // Add marks for resolution presets
    resolution_slider.add_mark(0.0, gtk::PositionType::Bottom, Some("Small"));
    resolution_slider.add_mark(1.0, gtk::PositionType::Bottom, Some(""));
    resolution_slider.add_mark(2.0, gtk::PositionType::Bottom, Some(""));
    resolution_slider.add_mark(3.0, gtk::PositionType::Bottom, Some(""));
    resolution_slider.add_mark(4.0, gtk::PositionType::Bottom, Some("Large"));

    let current_resolution = settings_for_ui.get_resolution();
    resolution_slider.set_value(current_resolution.to_index() as f64);

    let resolution_value_label = gtk::Label::new(Some(current_resolution.to_string()));
    resolution_value_label.set_xalign(0.0);
    resolution_value_label.set_margin_top(5);

    resolution_box.append(&resolution_slider);
    resolution_box.append(&resolution_value_label);
    resolution_frame.set_child(Some(&resolution_box));
    display_page.append(&resolution_frame);

    // Colors
    let colors_frame = gtk::Frame::new(Some("Colors"));
    let colors_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    colors_box.set_margin_top(5);
    colors_box.set_margin_bottom(5);
    colors_box.set_margin_start(5);
    colors_box.set_margin_end(5);

    let colors_label = gtk::Label::new(Some("Select the color depth:"));
    colors_label.set_xalign(0.0);
    colors_box.append(&colors_label);

    let colors_dropdown = gtk::DropDown::from_strings(&[
        "Highest quality (32 bit)",
        "True color (24 bit)",
        "High Color (16 bit)",
        "High Color (15 bit)",
    ]);

    let current_color_depth = settings_for_ui.get_color_depth();
    colors_dropdown.set_selected(current_color_depth.to_index() as u32);
    colors_box.append(&colors_dropdown);

    colors_frame.set_child(Some(&colors_box));
    display_page.append(&colors_frame);

    // DPI scaling
    let dpi_frame = gtk::Frame::new(Some("DPI scaling"));
    let dpi_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    dpi_box.set_margin_top(5);
    dpi_box.set_margin_bottom(5);
    dpi_box.set_margin_start(5);
    dpi_box.set_margin_end(5);

    let dpi_label = gtk::Label::new(Some("Set DPI scaling:"));
    dpi_label.set_xalign(0.0);
    dpi_box.append(&dpi_label);

    let dpi_labels: Vec<&str> = DPI_SCALE_OPTIONS.iter().map(|(_, label)| *label).collect();
    let dpi_dropdown = gtk::DropDown::from_strings(&dpi_labels);
    dpi_dropdown.set_selected(dpi_index_from_value(settings_for_ui.get_dpi_scaling()));
    dpi_box.append(&dpi_dropdown);

    dpi_frame.set_child(Some(&dpi_box));
    display_page.append(&dpi_frame);

    notebook.append_page(&display_page, Some(&gtk::Label::new(Some("Display"))));

    // === Codecs Page ===
    let codecs_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    codecs_page.set_margin_top(10);
    codecs_page.set_margin_bottom(10);
    codecs_page.set_margin_start(10);
    codecs_page.set_margin_end(10);

    // H.264 Hardware Acceleration
    let h264_accel_frame = gtk::Frame::new(Some("H.264 Hardware Acceleration"));
    let h264_accel_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    h264_accel_box.set_margin_top(5);
    h264_accel_box.set_margin_bottom(5);
    h264_accel_box.set_margin_start(5);
    h264_accel_box.set_margin_end(5);

    let h264_accel_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let h264_accel_label_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let h264_accel_label = gtk::Label::new(Some("Enable H.264 hardware acceleration"));
    h264_accel_label.set_xalign(0.0);
    h264_accel_label_box.append(&h264_accel_label);

    let h264_accel_description = gtk::Label::new(Some(
        "Use GPU for H.264 video decoding (may not work on all systems)",
    ));
    h264_accel_description.set_xalign(0.0);
    h264_accel_description.add_css_class("dim-label");
    h264_accel_description.add_css_class("caption");
    h264_accel_label_box.append(&h264_accel_description);

    let h264_switch = gtk::Switch::new();
    h264_switch.set_active(settings_for_ui.get_h264_hw_accel());
    h264_switch.set_valign(gtk::Align::Center);

    h264_accel_row.append(&h264_accel_label_box);
    h264_accel_row.append(&h264_switch);
    h264_accel_row.set_hexpand(true);
    h264_accel_label_box.set_hexpand(true);

    h264_accel_box.append(&h264_accel_row);
    h264_accel_frame.set_child(Some(&h264_accel_box));
    codecs_page.append(&h264_accel_frame);

    // Codec Options
    let codec_options_frame = gtk::Frame::new(Some("Codec Options"));
    let codec_options_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    codec_options_box.set_margin_top(5);
    codec_options_box.set_margin_bottom(5);
    codec_options_box.set_margin_start(5);
    codec_options_box.set_margin_end(5);

    // Info text about codecs
    let codec_info = gtk::Label::new(Some(
        "AVC420 uses 4:2:0 chroma subsampling for partial screen updates (dirty regions). \
         AVC444 uses 4:4:4 full chroma for higher quality full-screen rendering. \
         Note: AVC444 must be enabled on the server via group policy to be available."
    ));
    codec_info.set_xalign(0.0);
    codec_info.set_wrap(true);
    codec_info.set_wrap_mode(gtk::pango::WrapMode::Word);
    codec_info.add_css_class("dim-label");
    codec_info.add_css_class("caption");
    codec_info.set_margin_bottom(10);
    codec_options_box.append(&codec_info);

    // Disable AVC420
    let avc420_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let avc420_label_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let avc420_label = gtk::Label::new(Some("Disable H.264 AVC420"));
    avc420_label.set_xalign(0.0);
    avc420_label_box.append(&avc420_label);

    let avc420_description = gtk::Label::new(Some(
        "Disable AVC420 codec (4:2:0 chroma subsampling)",
    ));
    avc420_description.set_xalign(0.0);
    avc420_description.add_css_class("dim-label");
    avc420_description.add_css_class("caption");
    avc420_label_box.append(&avc420_description);

    let avc420_switch = gtk::Switch::new();
    avc420_switch.set_active(settings_for_ui.get_disable_avc420());
    avc420_switch.set_valign(gtk::Align::Center);

    avc420_row.append(&avc420_label_box);
    avc420_row.append(&avc420_switch);
    avc420_row.set_hexpand(true);
    avc420_label_box.set_hexpand(true);

    codec_options_box.append(&avc420_row);

    // Disable AVC444
    let avc444_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let avc444_label_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let avc444_label = gtk::Label::new(Some("Disable H.264 AVC444"));
    avc444_label.set_xalign(0.0);
    avc444_label_box.append(&avc444_label);

    let avc444_description = gtk::Label::new(Some(
        "Disable AVC444 codec (4:4:4 chroma subsampling, higher quality)",
    ));
    avc444_description.set_xalign(0.0);
    avc444_description.add_css_class("dim-label");
    avc444_description.add_css_class("caption");
    avc444_label_box.append(&avc444_description);

    let avc444_switch = gtk::Switch::new();
    avc444_switch.set_active(settings_for_ui.get_disable_avc444());
    avc444_switch.set_valign(gtk::Align::Center);

    avc444_row.append(&avc444_label_box);
    avc444_row.append(&avc444_switch);
    avc444_row.set_hexpand(true);
    avc444_label_box.set_hexpand(true);

    codec_options_box.append(&avc444_row);
    codec_options_frame.set_child(Some(&codec_options_box));
    codecs_page.append(&codec_options_frame);

    notebook.append_page(&codecs_page, Some(&gtk::Label::new(Some("Codecs"))));

    // === Network Page ===
    let network_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    network_page.set_margin_top(10);
    network_page.set_margin_bottom(10);
    network_page.set_margin_start(10);
    network_page.set_margin_end(10);

    // UDP Transport
    let udp_frame = gtk::Frame::new(Some("UDP Transport"));
    let udp_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    udp_box.set_margin_top(5);
    udp_box.set_margin_bottom(5);
    udp_box.set_margin_start(5);
    udp_box.set_margin_end(5);

    let udp_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let udp_label_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let udp_label = gtk::Label::new(Some("Disable UDP"));
    udp_label.set_xalign(0.0);
    udp_label_box.append(&udp_label);

    let udp_description = gtk::Label::new(Some(
        "Force TCP-only mode (disable UDP multitransport for graphics)",
    ));
    udp_description.set_xalign(0.0);
    udp_description.add_css_class("dim-label");
    udp_description.add_css_class("caption");
    udp_label_box.append(&udp_description);

    let udp_switch = gtk::Switch::new();
    udp_switch.set_active(settings_for_ui.get_disable_udp());
    udp_switch.set_valign(gtk::Align::Center);

    udp_row.append(&udp_label_box);
    udp_row.append(&udp_switch);
    udp_row.set_hexpand(true);
    udp_label_box.set_hexpand(true);

    udp_box.append(&udp_row);
    udp_frame.set_child(Some(&udp_box));
    network_page.append(&udp_frame);

    notebook.append_page(&network_page, Some(&gtk::Label::new(Some("Network"))));

    // === Debug Page ===
    let debug_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    debug_page.set_margin_top(10);
    debug_page.set_margin_bottom(10);
    debug_page.set_margin_start(10);
    debug_page.set_margin_end(10);

    // Visualization Options
    let viz_frame = gtk::Frame::new(Some("Visualization"));
    let viz_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    viz_box.set_margin_top(5);
    viz_box.set_margin_bottom(5);
    viz_box.set_margin_start(5);
    viz_box.set_margin_end(5);

    // Codec Grid
    let grid_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let grid_label_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let grid_label = gtk::Label::new(Some("Show grid indicating codec in use for cell"));
    grid_label.set_xalign(0.0);
    grid_label_box.append(&grid_label);

    let grid_description = gtk::Label::new(Some(
        "Display a colored grid overlay showing which codec is being used for each region",
    ));
    grid_description.set_xalign(0.0);
    grid_description.add_css_class("dim-label");
    grid_description.add_css_class("caption");
    grid_label_box.append(&grid_description);

    let grid_switch = gtk::Switch::new();
    grid_switch.set_active(settings_for_ui.get_show_codec_grid());
    grid_switch.set_valign(gtk::Align::Center);

    grid_row.append(&grid_label_box);
    grid_row.append(&grid_switch);
    grid_row.set_hexpand(true);
    grid_label_box.set_hexpand(true);

    viz_box.append(&grid_row);
    viz_frame.set_child(Some(&viz_box));
    debug_page.append(&viz_frame);

    notebook.append_page(&debug_page, Some(&gtk::Label::new(Some("Debug"))));

    // Show/Hide options button
    let options_button = Button::new();
    let options_button_label = gtk::Label::new(Some("Show Options"));
    options_button_label.set_xalign(0.0);
    let options_button_icon = Image::from_icon_name("go-down-symbolic");
    let options_button_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    options_button_content.set_valign(gtk::Align::Center);
    options_button_content.append(&options_button_label);
    options_button_content.append(&options_button_icon);
    options_button.set_child(Some(&options_button_content));
    let options_visible = Rc::new(RefCell::new(false));

    // Bottom box with options button and connect button
    let bottom_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bottom_box.set_margin_top(10);
    bottom_box.append(&options_button);

    let connect_button = Button::with_label("Connect");
    connect_button.add_css_class("suggested-action");
    connect_button.set_hexpand(true);
    connect_button.set_halign(gtk::Align::End);
    bottom_box.append(&connect_button);

    let options_button_for_tab = options_button.clone();
    let move_focus_to_options = gtk::EventControllerKey::new();
    move_focus_to_options.connect_key_pressed(move |_, key, _keycode, state| {
        if key == gtk::gdk::Key::Tab && !state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
            options_button_for_tab.grab_focus();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    connect_button.add_controller(move_focus_to_options);

    let connect_button_for_back = connect_button.clone();
    let move_focus_back = gtk::EventControllerKey::new();
    move_focus_back.connect_key_pressed(move |_, key, _keycode, state| {
        if key == gtk::gdk::Key::Tab && state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
            connect_button_for_back.grab_focus();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    options_button.add_controller(move_focus_back);

    // Initially hide the notebook
    notebook.set_visible(false);

    dialog_box.append(&notebook);
    dialog_box.append(&bottom_box);

    // Options button click handler
    let notebook_clone = notebook.clone();
    let basic_fields_box_clone = basic_fields_box.clone();
    let options_visible_clone = options_visible.clone();
    let options_button_label_for_toggle = options_button_label.clone();
    let options_button_icon_for_toggle = options_button_icon.clone();
    options_button.connect_clicked(move |_| {
        let mut visible = options_visible_clone.borrow_mut();
        *visible = !*visible;
        notebook_clone.set_visible(*visible);
        basic_fields_box_clone.set_visible(!*visible); // Hide basic fields when showing options
        if *visible {
            options_button_label_for_toggle.set_text("Hide Options");
            options_button_icon_for_toggle.set_icon_name(Some("go-up-symbolic"));
        } else {
            options_button_label_for_toggle.set_text("Show Options");
            options_button_icon_for_toggle.set_icon_name(Some("go-down-symbolic"));
        }
    });

    // Resolution slider handler
    let resolution_value_label_clone = resolution_value_label.clone();
    let rdp_settings_for_resolution = rdp_settings.clone();
    resolution_slider.connect_value_changed(move |slider| {
        let index = slider.value().round() as usize;
        let resolution = Resolution::from_index(index);
        resolution_value_label_clone.set_text(resolution.to_string());
        rdp_settings_for_resolution
            .borrow_mut()
            .set_resolution(resolution);
    });

    // Color depth dropdown handler
    let rdp_settings_for_colors = rdp_settings.clone();
    colors_dropdown.connect_selected_notify(move |dropdown| {
        let index = dropdown.selected() as usize;
        let depth = ColorDepth::from_index(index);
        rdp_settings_for_colors.borrow_mut().set_color_depth(depth);
    });

    // DPI dropdown handler
    let rdp_settings_for_dpi = rdp_settings.clone();
    dpi_dropdown.connect_selected_notify(move |dropdown| {
        let scale = dpi_value_from_index(dropdown.selected());
        rdp_settings_for_dpi.borrow_mut().set_dpi_scaling(scale);
    });

    // H.264 hardware acceleration switch handler
    let rdp_settings_for_h264 = rdp_settings.clone();
    h264_switch.connect_state_set(move |_switch, enabled| {
        rdp_settings_for_h264
            .borrow_mut()
            .set_h264_hw_accel(enabled);
        glib::Propagation::Proceed
    });

    // AVC420 disable switch handler
    let rdp_settings_for_avc420 = rdp_settings.clone();
    avc420_switch.connect_state_set(move |_switch, enabled| {
        rdp_settings_for_avc420
            .borrow_mut()
            .set_disable_avc420(enabled);
        glib::Propagation::Proceed
    });

    // AVC444 disable switch handler
    let rdp_settings_for_avc444 = rdp_settings.clone();
    avc444_switch.connect_state_set(move |_switch, enabled| {
        rdp_settings_for_avc444
            .borrow_mut()
            .set_disable_avc444(enabled);
        glib::Propagation::Proceed
    });

    // UDP disable switch handler
    let rdp_settings_for_udp = rdp_settings.clone();
    udp_switch.connect_state_set(move |_switch, enabled| {
        rdp_settings_for_udp
            .borrow_mut()
            .set_disable_udp(enabled);
        glib::Propagation::Proceed
    });

    // Codec grid switch handler
    let rdp_settings_for_grid = rdp_settings.clone();
    grid_switch.connect_state_set(move |_switch, enabled| {
        rdp_settings_for_grid
            .borrow_mut()
            .set_show_codec_grid(enabled);
        glib::Propagation::Proceed
    });

    // Save button handler
    let rdp_settings_for_save = rdp_settings.clone();
    let server_input_for_save = server_input.clone();
    let username_input_for_save = username_input.clone();
    let domain_input_for_save = domain_input.clone();
    let password_input_for_save = password_input.clone();
    let save_password_checkbox_for_save = save_password_checkbox.clone();
    let dpi_dropdown_for_save = dpi_dropdown.clone();
    save_button.connect_clicked(move |_| {
        let mut settings = rdp_settings_for_save.borrow_mut();
        settings.server = server_input_for_save.buffer().text().to_string();
        settings.username = username_input_for_save.buffer().text().to_string();
        settings.domain = domain_input_for_save.buffer().text().to_string();
        settings.password = password_input_for_save.buffer().text().to_string();
        settings.save_password = save_password_checkbox_for_save.is_active();
        settings.set_dpi_scaling(dpi_value_from_index(dpi_dropdown_for_save.selected()));

        if let Err(e) = settings.save_as_default() {
            eprintln!("Failed to save settings: {}", e);
        }
    });

    // Save As button handler
    let rdp_settings_for_save_as = rdp_settings.clone();
    let server_input_for_save_as = server_input.clone();
    let username_input_for_save_as = username_input.clone();
    let domain_input_for_save_as = domain_input.clone();
    let password_input_for_save_as = password_input.clone();
    let save_password_checkbox_for_save_as = save_password_checkbox.clone();
    let dpi_dropdown_for_save_as = dpi_dropdown.clone();
    let window_for_save_as = window_clone.clone();
    save_as_button.connect_clicked(move |_| {
        let file_dialog = gtk::FileDialog::new();
        file_dialog.set_title("Save RDP File");
        file_dialog.save(Some(&window_for_save_as), None::<&gtk::gio::Cancellable>, {
            let rdp_settings = rdp_settings_for_save_as.clone();
            let server_input = server_input_for_save_as.clone();
            let username_input = username_input_for_save_as.clone();
            let domain_input = domain_input_for_save_as.clone();
            let password_input = password_input_for_save_as.clone();
            let save_password_checkbox = save_password_checkbox_for_save_as.clone();
            let dpi_dropdown = dpi_dropdown_for_save_as.clone();

            move |result| {
                if let Ok(file) = result {
                    let mut settings = rdp_settings.borrow_mut();
                    settings.server = server_input.buffer().text().to_string();
                    settings.username = username_input.buffer().text().to_string();
                    settings.domain = domain_input.buffer().text().to_string();
                    settings.password = password_input.buffer().text().to_string();
                    settings.save_password = save_password_checkbox.is_active();
                    settings.set_dpi_scaling(dpi_value_from_index(dpi_dropdown.selected()));

                    if let Some(path) = file.path() {
                        if let Err(e) = settings.save_to_file(&path) {
                            eprintln!("Failed to save RDP file: {}", e);
                        }
                    }
                }
            }
        });
    });

    // Open button handler
    let rdp_settings_for_open = rdp_settings.clone();
    let server_input_for_open = server_input.clone();
    let username_input_for_open = username_input.clone();
    let domain_input_for_open = domain_input.clone();
    let password_input_for_open = password_input.clone();
    let save_password_checkbox_for_open = save_password_checkbox.clone();
    let resolution_slider_for_open = resolution_slider.clone();
    let colors_dropdown_for_open = colors_dropdown.clone();
    let resolution_value_label_for_open = resolution_value_label.clone();
    let dpi_dropdown_for_open = dpi_dropdown.clone();
    let window_for_open = window_clone.clone();

    open_button.connect_clicked(move |_| {
        let file_dialog = gtk::FileDialog::new();
        file_dialog.set_title("Open RDP File");
        file_dialog.open(Some(&window_for_open), None::<&gtk::gio::Cancellable>, {
            let rdp_settings = rdp_settings_for_open.clone();
            let server_input = server_input_for_open.clone();
            let username_input = username_input_for_open.clone();
            let domain_input = domain_input_for_open.clone();
            let password_input = password_input_for_open.clone();
            let save_password_checkbox = save_password_checkbox_for_open.clone();
            let resolution_slider = resolution_slider_for_open.clone();
            let colors_dropdown = colors_dropdown_for_open.clone();
            let resolution_value_label = resolution_value_label_for_open.clone();
            let dpi_dropdown = dpi_dropdown_for_open.clone();

            move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        if let Ok(loaded_settings) = RdpSettings::load_from_file(&path) {
                            server_input.buffer().set_text(&loaded_settings.server);
                            username_input.buffer().set_text(&loaded_settings.username);
                            domain_input.buffer().set_text(&loaded_settings.domain);
                            if loaded_settings.save_password {
                                password_input.buffer().set_text(&loaded_settings.password);
                            } else {
                                password_input.buffer().set_text("");
                            }
                            save_password_checkbox.set_active(loaded_settings.save_password);

                            let resolution = loaded_settings.get_resolution();
                            resolution_slider.set_value(resolution.to_index() as f64);
                            resolution_value_label.set_text(resolution.to_string());

                            let color_depth = loaded_settings.get_color_depth();
                            colors_dropdown.set_selected(color_depth.to_index() as u32);

                            let dpi_index = dpi_index_from_value(loaded_settings.get_dpi_scaling());
                            dpi_dropdown.set_selected(dpi_index);

                            *rdp_settings.borrow_mut() = loaded_settings;
                        }
                    }
                }
            }
        });
    });

    // Connect button logic (simplified for now - will need full implementation)
    let rdp_settings_for_connect = rdp_settings.clone();
    let server_input_for_connect = server_input.clone();
    let username_input_for_connect = username_input.clone();
    let domain_input_for_connect = domain_input.clone();
    let password_input_for_connect = password_input.clone();
    let save_password_checkbox_for_connect = save_password_checkbox.clone();
    let dpi_dropdown_for_connect = dpi_dropdown.clone();
    let window_for_connect = window_clone.clone();
    let app_for_connect = app.clone();

    connect_button.connect_clicked(move |_| {
        let server_text = server_input_for_connect.buffer().text();
        let username_text = username_input_for_connect.buffer().text();
        let domain_text = domain_input_for_connect.buffer().text();
        let password_text = password_input_for_connect.buffer().text();

        if server_text.is_empty() || username_text.is_empty() {
            let dialog = gtk::AlertDialog::builder()
                .message("Missing Information")
                .detail("Please enter both Computer and User name")
                .build();
            dialog.show(Some(&window_for_connect));
            return;
        }

        // Update settings with current values
        let mut settings = rdp_settings_for_connect.borrow_mut();
        settings.server = server_text.to_string();
        settings.username = username_text.to_string();
        settings.domain = domain_text.to_string();
        settings.password = password_text.to_string();
        settings.save_password = save_password_checkbox_for_connect.is_active();
        settings.set_dpi_scaling(dpi_value_from_index(dpi_dropdown_for_connect.selected()));

        if let Err(err) = settings.save_as_default() {
            eprintln!("Failed to save default RDP settings: {err}");
        }

        // Create remote desktop window
        create_remote_desktop_window(
            &app_for_connect,
            &server_text,
            &username_text,
            &domain_text,
            &password_text,
            &window_for_connect,
            &settings,
        );
    });

    // Set activates default for all inputs
    server_input.set_activates_default(true);
    username_input.set_activates_default(true);
    domain_input.set_activates_default(true);
    password_input.set_activates_default(true);

    window_clone.set_child(Some(&dialog_box));
    window_clone.set_default_widget(Some(&connect_button));
    window.present();

    // Auto-connect if autologon flag is set
    if autologon {
        eprintln!("Autologon is enabled, scheduling auto-connect");
        let connect_button_for_autologon = connect_button.clone();
        // Use a timeout to ensure the UI is fully initialized
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            eprintln!("Auto-clicking connect button");
            connect_button_for_autologon.emit_clicked();
        });
    }

    // Clear selection
    glib::idle_add_local_once(move || {
        server_input.set_position(-1);
        username_input.set_position(-1);
        domain_input.set_position(-1);
        password_input.set_position(-1);
    });
}
