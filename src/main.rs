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

use clap::{Args as ClapArgs, Parser, Subcommand};

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
    /// Run something other than the window. Without one of these, IronTSC opens as usual.
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    connection: ConnectionArgs,

    /// Connect immediately instead of showing the dialog.
    #[arg(long)]
    autologon: bool,
}

/// The connection itself, shared by the window and by MCP mode.
#[derive(ClapArgs, Debug, Default)]
struct ConnectionArgs {
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

    /// Connect without CredSSP, for a server that checks credentials itself.
    #[arg(long)]
    no_nla: bool,

    /// A `.rdp` file to load the settings from, the way `mstsc file.rdp` does.
    file: Option<std::path::PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve MCP over stdio, driving a session of its own with no window.
    ///
    /// This is the form an MCP client starts for itself. To let an agent drive a session you
    /// are already watching, leave this off and switch MCP mode on from the gear in the
    /// session island instead.
    Mcp(McpCommand),
}

#[derive(ClapArgs, Debug)]
struct McpCommand {
    #[command(flatten)]
    connection: ConnectionArgs,

    /// Desktop width to negotiate. Defaults to whatever the settings say.
    #[arg(long)]
    width: Option<u16>,

    /// Desktop height to negotiate.
    #[arg(long)]
    height: Option<u16>,

    /// Allow the UDP multitransport. Off by default: an agent works in clicks and
    /// screenshots, where a few milliseconds of latency buy nothing and the extra transport
    /// is one more thing that can go wrong.
    #[arg(long)]
    udp: bool,
}

/// Reads the settings and fills in the logon form from the command line.
fn resolve(connection: &ConnectionArgs) -> anyhow::Result<(ConnectForm, RdpSettings)> {
    // A named `.rdp` wins over the default one, which is what mstsc does with a file argument.
    let mut settings = match connection.file.as_deref() {
        Some(path) => RdpSettings::load_from_file(path)
            .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?,
        None => RdpSettings::load_default(),
    };

    if connection.no_nla {
        settings.disable_nla = true;
    }

    let form = ConnectForm {
        server: connection
            .computer
            .clone()
            .unwrap_or_else(|| settings.server.clone()),
        username: connection
            .username
            .clone()
            .unwrap_or_else(|| settings.username.clone()),
        password: connection
            .password
            .clone()
            .unwrap_or_else(|| settings.password.clone()),
        domain: connection
            .domain
            .clone()
            .unwrap_or_else(|| settings.domain.clone()),
        save: false,
    };

    Ok((form, settings))
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // stdio is the MCP channel in headless mode, so the log has to go somewhere else or it
    // corrupts the protocol stream. Everywhere else stdout is fine.
    let to_stderr = matches!(args.command, Some(Command::Mcp(_)));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_line_number(true);
    if to_stderr {
        subscriber.with_writer(std::io::stderr).init();
    } else {
        subscriber.init();
    }

    match args.command {
        Some(Command::Mcp(mcp)) => run_mcp(mcp),
        None => {
            let (form, settings) = resolve(&args.connection)?;

            // Anything short of a full set of credentials still gets the dialog, pre-filled.
            let autoconnect = (args.autologon || args.connection.computer.is_some())
                && !form.server.trim().is_empty()
                && !form.username.trim().is_empty()
                && !form.password.is_empty();

            egui_app::run(form, settings, autoconnect)
        }
    }
}

/// Headless MCP mode: open a session of our own and serve it over stdio.
fn run_mcp(command: McpCommand) -> anyhow::Result<()> {
    let (form, settings) = resolve(&command.connection)?;

    if form.server.trim().is_empty() || form.username.trim().is_empty() {
        anyhow::bail!(
            "MCP mode needs a computer and a user name, on the command line or in a .rdp file"
        );
    }

    let mut config = egui_app::build_config(&form, &settings)?;

    // An agent works in screenshots and clicks; a few milliseconds either way buy it nothing,
    // and the multitransport is one more moving part between it and the desktop.
    config.disable_udp = !command.udp;

    // Set directly rather than through the settings, whose resolution is one of a fixed set.
    if let Some(width) = command.width {
        config.connector.desktop_size.width = width & !0x3;
    }
    if let Some(height) = command.height {
        config.connector.desktop_size.height = height & !0x3;
    }

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| anyhow::anyhow!("failed to start the Tokio runtime: {error}"))?;

    runtime.block_on(async move {
        let session = irontsc::agent::AgentSession::spawn_headless(config);
        irontsc::agent::serve_stdio(session).await
    })
}
