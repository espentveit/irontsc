//! IronTSC, a Remote Desktop client.
//!
//! The shell is egui on winit and glutin, chosen over eframe because eframe only surfaces
//! egui's logical `Key`, which has no CapsLock, cannot separate the numpad from the number row
//! and folds AltGr into Alt. Keys are read from winit's `physical_key` before egui sees them
//! and translated to Windows Set 1 scancodes, which is what RDP transmits.
//!
//! The protocol layers below are frontend-agnostic and talk over `RdpInputEvent` and
//! `RdpOutputEvent` channels.

// The client itself lives in the library next door, so that the tabbed shell can be built on
// the same code rather than compiling these files a second time.

use clap::Parser;

use irontsc::egui_app::{self, ConnectForm};
use irontsc::settings::RdpSettings;

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

    /// A `.rdp` file to load the settings from, the way `mstsc file.rdp` does.
    file: Option<std::path::PathBuf>,
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

    // A named `.rdp` wins over the default one, which is what mstsc does with a file argument.
    let settings = match args.file.as_deref() {
        Some(path) => RdpSettings::load_from_file(path)
            .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?,
        None => RdpSettings::load_default(),
    };

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
