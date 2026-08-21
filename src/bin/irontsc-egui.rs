//! IronTSC with an egui frontend.
//!
//! A second binary next to the GTK client, sharing the whole RDP stack with it. GTK is only
//! reachable on Linux/BSD in practice, so this exists to get the same session onto Windows and
//! macOS; the protocol modules below are pulled in by path rather than copied, so there is
//! exactly one implementation of the session and two shells around it.
//!
//! Run it the same way as the GTK client:
//!
//! ```text
//! irontsc-egui --computer server --username espen --password ... [--domain ...]
//! ```

// Every module here is the same file the GTK binary compiles; `#[path]` keeps them shared
// without turning the crate into a library, which would mean touching the working client.
#[path = "../config.rs"]
mod config;
#[path = "../core_input_channel.rs"]
mod core_input_channel;
#[path = "../dtls_udp.rs"]
mod dtls_udp;
#[path = "../gfx.rs"]
mod gfx;
#[path = "../gfx_channel.rs"]
mod gfx_channel;
#[path = "../h264_codec_caps.rs"]
mod h264_codec_caps;
#[path = "../mouse_cursor_channel.rs"]
mod mouse_cursor_channel;
#[path = "../rdp.rs"]
mod rdp;
#[path = "../stub_dvc.rs"]
mod stub_dvc;
#[path = "../transport_rules.rs"]
mod transport_rules;
#[path = "../udp_gfx.rs"]
mod udp_gfx;
#[path = "../udp_transport.rs"]
mod udp_transport;

#[cfg(feature = "video-redirection")]
#[path = "../geometry_channel.rs"]
mod geometry_channel;
#[cfg(feature = "video-redirection")]
#[path = "../video_control_channel.rs"]
mod video_control_channel;
#[cfg(feature = "video-redirection")]
#[path = "../video_data_channel.rs"]
mod video_data_channel;
#[cfg(feature = "video-redirection")]
#[path = "../video_redirect.rs"]
mod video_redirect;

// The egui shell itself.
#[path = "../egui_app.rs"]
mod egui_app;
#[path = "../egui_scancode.rs"]
mod egui_scancode;
#[path = "../egui_shortcuts.rs"]
mod egui_shortcuts;
#[path = "../settings.rs"]
mod settings;

use clap::Parser;

use crate::egui_app::ConnectForm;
use crate::settings::RdpSettings;

/// IronTSC, egui frontend.
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
