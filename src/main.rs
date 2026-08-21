//! IronTSC, a Remote Desktop client.
//!
//! The shell is egui on winit and glutin, chosen over eframe because eframe only surfaces
//! egui's logical `Key`, which has no CapsLock, cannot separate the numpad from the number row
//! and folds AltGr into Alt. Keys are read from winit's `physical_key` before egui sees them
//! and translated to Windows Set 1 scancodes, which is what RDP transmits.
//!
//! The protocol layers below are frontend-agnostic and talk over `RdpInputEvent` and
//! `RdpOutputEvent` channels.

// Every module here is the same file the GTK binary compiles; `#[path]` keeps them shared
// without turning the crate into a library, which would mean touching the working client.
mod config;
mod core_input_channel;
mod dtls_udp;
mod gfx;
mod gfx_channel;
mod h264_codec_caps;
mod mouse_cursor_channel;
mod rdp;
mod stub_dvc;
mod transport_rules;
mod udp_gfx;
mod udp_transport;

#[cfg(feature = "video-redirection")]
mod geometry_channel;
#[cfg(feature = "video-redirection")]
mod video_control_channel;
#[cfg(feature = "video-redirection")]
mod video_data_channel;
#[cfg(feature = "video-redirection")]
mod video_redirect;

// The egui shell itself.
mod egui_app;
mod egui_scancode;
mod egui_shortcuts;
mod settings;

use clap::Parser;

use crate::egui_app::ConnectForm;
use crate::settings::RdpSettings;

/// IronTSC.
///
/// With no arguments it opens the connection dialog, pre-filled from
/// `~/.config/irontsc/default.rdp`. Given a computer, user name and password it connects
/// straight away, the way the GTK client's `--autologon` does.
#[derive(Parser, Debug)]
#[command(name = "irontsc-egui", about = "IronTSC RDP client (egui frontend)")]
struct Args {
    /// Computer name or IP address, optionally with `:port`.
    #[arg(long, alias = "destination")]
    computer: Option<String>,

    /// User name to authenticate as.
    #[arg(long, short)]
    username: Option<String>,

    /// Password to authenticate with.
    #[arg(long, short)]
    password: Option<String>,

    /// Domain to authenticate against.
    #[arg(long, short)]
    domain: Option<String>,

    /// Connect immediately instead of showing the dialog.
    #[arg(long)]
    autologon: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_line_number(true)
        .init();

    let args = Args::parse();

    // The same file the GTK client reads and writes, so the two agree on defaults.
    let settings = RdpSettings::load_default();

    let form = ConnectForm {
        server: args.computer.clone().unwrap_or_else(|| settings.server.clone()),
        username: args
            .username
            .clone()
            .unwrap_or_else(|| settings.username.clone()),
        password: args
            .password
            .clone()
            .unwrap_or_else(|| settings.password.clone()),
        domain: args.domain.clone().unwrap_or_else(|| settings.domain.clone()),
        save: false,
    };

    // Anything short of a full set of credentials still gets the dialog, pre-filled.
    let autoconnect = (args.autologon || args.computer.is_some())
        && !form.server.trim().is_empty()
        && !form.username.trim().is_empty()
        && !form.password.is_empty();

    egui_app::run(form, settings, autoconnect)
}
