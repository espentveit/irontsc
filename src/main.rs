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

    /// Switch MCP mode on as soon as the session is up, rather than from the gear. The logon
    /// dialog still appears if the credentials are not all there; MCP mode comes up once the
    /// session does.
    #[arg(long)]
    mcp: bool,
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

    /// Let the session record from this machine's microphone.
    ///
    /// The same as `audiocapturemode:i:1` in a `.rdp` file. Off unless asked for: the server
    /// opens its audio input channel the moment something in the session wants to record.
    #[arg(long)]
    audio_capture: bool,

    /// A `.rdp` file to load the settings from, the way `mstsc file.rdp` does.
    file: Option<std::path::PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List the MCP sessions running on this machine, and how to reach them.
    Sessions(SessionsCommand),

    /// Drive one of those sessions: `irontsc session <session> <tool> [name=value ...]`.
    ///
    /// With no tool it prints the ones that session offers, which is the palette to pick from.
    Session(SessionCommand),

    /// Read or write IronTSC's own preferences, which apply to every connection.
    ///
    /// `irontsc config` lists them, `irontsc config vision_endpoint` reads one, and
    /// `irontsc config vision_endpoint http://server:8080/v1` writes it. With `--file`, the
    /// same three forms read and write one connection's `.rdp` instead, which is how a single
    /// desktop gets a setting the others do not have.
    Config(ConfigCommand),

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

    /// The keyboard layout the server has active, such as `us` or `no`. Defaults to the
    /// settings, and then to this machine's, which is right whenever typing at the window
    /// lands correctly.
    #[arg(long)]
    keyboard_layout: Option<String>,
}

#[derive(ClapArgs, Debug)]
struct ConfigCommand {
    /// The setting to read or write. Left off, every setting is listed.
    name: Option<String>,

    /// The value to write. Left off, the setting is only read.
    value: Option<String>,

    /// Read and write this connection's `.rdp` file instead of the preferences.
    #[arg(long, value_name = "PATH")]
    file: Option<std::path::PathBuf>,
}

#[derive(ClapArgs, Debug)]
struct SessionsCommand {
    /// Print the register as JSON, for something reading rather than someone.
    #[arg(long)]
    json: bool,
}

#[derive(ClapArgs, Debug)]
struct SessionCommand {
    /// Which session: its PID, its port, or part of the computer or user name. `any` takes
    /// the one that is running, and complains if that is ambiguous. Ignored with `--url`.
    session: String,

    /// The tool to call. Left off, the session's tools are listed instead.
    tool: Option<String>,

    /// Arguments as `name=value`. Numbers and `true`/`false` go as themselves, a value
    /// starting with `{` or `[` is read as JSON, and everything else is text.
    arguments: Vec<String>,

    /// Send exactly this JSON object as the arguments, instead of `name=value` pairs.
    #[arg(long, value_name = "JSON")]
    json_arguments: Option<String>,

    /// Print the result as the MCP JSON that came back, rather than as text.
    #[arg(long)]
    json: bool,

    /// Where to write an image the tool returns. Defaults to a file in the temporary
    /// directory, whose path is printed either way.
    #[arg(long, value_name = "PATH")]
    out: Option<std::path::PathBuf>,

    /// Talk to this MCP URL instead of looking the session up in the register, for a session
    /// the register cannot see: one on another machine, or one whose URL is already to hand.
    #[arg(long, value_name = "URL")]
    url: Option<String>,
}

/// Reads the settings and fills in the logon form from the command line.
fn resolve(connection: &ConnectionArgs) -> anyhow::Result<(ConnectForm, RdpSettings)> {
    // A named `.rdp` wins over the default one, which is what mstsc does with a file argument.
    let mut settings = match connection.file.as_deref() {
        Some(path) => RdpSettings::load_from_file(path)
            .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?,
        None => RdpSettings::load_default(),
    };

    // What IronTSC prefers fills in what the connection did not say.
    irontsc::preferences::Preferences::load().apply_to(&mut settings);

    if connection.no_nla {
        settings.disable_nla = true;
    }

    if connection.audio_capture {
        settings.audio_capture = true;
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
        Some(Command::Config(command)) => run_config(&command),
        Some(Command::Sessions(command)) => run_sessions(&command),
        Some(Command::Session(command)) => run_session(&command),
        Some(Command::Mcp(mcp)) => run_mcp(mcp),
        None => {
            let (form, settings) = resolve(&args.connection)?;

            // Anything short of a full set of credentials still gets the dialog, pre-filled.
            let autoconnect = (args.autologon || args.mcp || args.connection.computer.is_some())
                && !form.server.trim().is_empty()
                && !form.username.trim().is_empty()
                && !form.password.is_empty();

            egui_app::run(form, settings, autoconnect, args.mcp)
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

    // A scancode is a position; the server decides what it means and never says which layout
    // it is reading them against, so typing needs to be told or to guess.
    let layout = irontsc::agent::KeyboardLayout::resolve(
        command
            .keyboard_layout
            .as_deref()
            .unwrap_or(&settings.keyboard_layout),
    );

    // Only offered when the models are installed; see `agent::sight`. Global, so `irontsc mcp`
    // and the window agree without either `.rdp` mentioning it.
    let sight = irontsc::preferences::Preferences::load().sight();

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| anyhow::anyhow!("failed to start the Tokio runtime: {error}"))?;

    // Announced like any other session, so `irontsc sessions` can say it is up -- though with
    // no address to hand out, since stdio belongs to whoever started it.
    let _beacon = irontsc::agent::registry::announce(
        irontsc::agent::Descriptor {
            pid: std::process::id(),
            transport: "stdio".to_owned(),
            url: None,
            computer: form.server.clone(),
            username: form.username.clone(),
            started_at: irontsc::agent::registry::now(),
        },
        runtime.handle(),
    )
    .inspect_err(|error| tracing::warn!(%error, "could not join the session register"))
    .ok();

    runtime.block_on(async move {
        let session = irontsc::agent::AgentSession::spawn_headless(config, layout);
        irontsc::agent::serve_stdio(session, sight).await
    })
}

/// Prints the register: what MCP sessions are up, and the URL each one answers on.
fn run_sessions(command: &SessionsCommand) -> anyhow::Result<()> {
    let sessions = irontsc::agent::registry::list();

    if command.json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }

    if sessions.is_empty() {
        println!("No MCP sessions are running.");
        println!("Switch MCP mode on from the gear in a window, or start one with `irontsc mcp`.");
        return Ok(());
    }

    println!(
        "{:<8} {:<6} {:<7} {:<28} {}",
        "PID", "PORT", "UP", "SESSION", "URL"
    );
    for session in &sessions {
        println!(
            "{:<8} {:<6} {:<7} {:<28} {}",
            session.pid,
            session
                .port()
                .map_or_else(|| session.transport.clone(), |port| port.to_string()),
            uptime(session.started_at),
            session.label(),
            session.url.as_deref().unwrap_or("(stdio, no address)"),
        );
    }
    Ok(())
}

/// Runs one tool against one session, or lists the tools it offers.
fn run_session(command: &SessionCommand) -> anyhow::Result<()> {
    let url = match &command.url {
        Some(url) => url.clone(),
        None => {
            let session = pick(&command.session)?;
            session.url.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is an stdio session, which only the client that started it can talk to",
                    session.label()
                )
            })?
        }
    };

    let arguments = read_arguments(command)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let client = irontsc::agent::Client::connect(&url).await?;

        let Some(tool) = command.tool.as_deref() else {
            for tool in client.tools().await? {
                // One line each: the palette, not the manual.
                let summary = tool.description.lines().next().unwrap_or_default();
                println!("{:<14} {summary}", tool.name);
            }
            return Ok(());
        };

        let result = client.call(tool, arguments).await?;
        if command.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        render(&result, tool, command.out.as_deref())
    })
}

/// Finds the session the argument names, or explains what there was to choose from.
fn pick(needle: &str) -> anyhow::Result<irontsc::agent::Descriptor> {
    let sessions = irontsc::agent::registry::list();
    if sessions.is_empty() {
        anyhow::bail!("no MCP sessions are running; `irontsc sessions` shows the same thing");
    }

    let mut matched: Vec<_> = if needle.trim().eq_ignore_ascii_case("any") {
        sessions.clone()
    } else {
        sessions
            .iter()
            .filter(|session| session.matches(needle))
            .cloned()
            .collect()
    };

    match matched.len() {
        1 => Ok(matched.remove(0)),
        0 => {
            let names: Vec<_> = sessions.iter().map(|session| session.label()).collect();
            anyhow::bail!("no session matches `{needle}`; running: {}", names.join(", "))
        }
        _ => {
            let names: Vec<_> = matched
                .iter()
                .map(|session| format!("{} ({})", session.label(), session.pid))
                .collect();
            anyhow::bail!("`{needle}` matches more than one session: {}", names.join(", "))
        }
    }
}

/// Turns `name=value` pairs, or a JSON object, into the arguments for a tool call.
fn read_arguments(command: &SessionCommand) -> anyhow::Result<serde_json::Value> {
    if let Some(json) = &command.json_arguments {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| anyhow::anyhow!("--json-arguments: {error}"))?;
        if !value.is_object() {
            anyhow::bail!("--json-arguments takes an object, such as `{{\"x\": 10}}`");
        }
        return Ok(value);
    }

    let mut arguments = serde_json::Map::new();
    for pair in &command.arguments {
        let Some((name, value)) = pair.split_once('=') else {
            anyhow::bail!("`{pair}` is not a `name=value` argument");
        };
        arguments.insert(name.trim().to_owned(), read_value(value));
    }
    Ok(serde_json::Value::Object(arguments))
}

/// Reads an argument the way a person means it: a number is a number, `true` is a boolean,
/// something in braces is JSON, and the rest is text.
fn read_value(raw: &str) -> serde_json::Value {
    match raw {
        "true" => return serde_json::Value::Bool(true),
        "false" => return serde_json::Value::Bool(false),
        "null" => return serde_json::Value::Null,
        _ => {}
    }
    if let Ok(number) = raw.parse::<i64>() {
        return serde_json::Value::from(number);
    }
    if let Ok(number) = raw.parse::<f64>()
        && raw.contains('.')
    {
        return serde_json::Value::from(number);
    }
    if (raw.starts_with('{') || raw.starts_with('['))
        && let Ok(value) = serde_json::from_str(raw)
    {
        return value;
    }
    serde_json::Value::String(raw.to_owned())
}

/// Prints what a tool sent back: text as text, and an image as the file it was written to.
fn render(
    result: &serde_json::Value,
    tool: &str,
    out: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use base64::Engine as _;

    let content = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    for block in content {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    println!("{text}");
                }
            }
            Some("image") => {
                let Some(data) = block.get("data").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|error| anyhow::anyhow!("the image came back unreadable: {error}"))?;

                let path = match out {
                    Some(path) => path.to_path_buf(),
                    None => std::env::temp_dir().join(format!(
                        "irontsc-{tool}-{}.png",
                        irontsc::agent::registry::now()
                    )),
                };
                std::fs::write(&path, &bytes)?;
                match png_size(&bytes) {
                    Some((width, height)) => println!(
                        "image: {} ({width}x{height}, {} KiB)",
                        path.display(),
                        bytes.len() / 1024
                    ),
                    None => println!("image: {} ({} KiB)", path.display(), bytes.len() / 1024),
                }
            }
            _ => {}
        }
    }

    // A tool that failed says so in the result rather than in the transport, and the shell
    // should hear about it in the way shells do.
    if result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        std::process::exit(1);
    }
    Ok(())
}

/// The width and height in a PNG's header, which saves decoding the whole thing to report it.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const HEADER: usize = 24;
    if bytes.len() < HEADER || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// How long a session has been up, in the roughest terms that are still useful.
fn uptime(started_at: u64) -> String {
    let now = irontsc::agent::registry::now();
    let seconds = now.saturating_sub(started_at);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// Reads or writes one setting in the `.rdp` file the window uses.
///
/// The settings are serde-shaped, so this goes through JSON rather than a hand-written match:
/// a field added to `RdpSettings` shows up here the day it is added, with no second list to
/// keep in step.
fn run_config(command: &ConfigCommand) -> anyhow::Result<()> {
    let (settings, preferences) = match &command.file {
        Some(path) => (
            Some(RdpSettings::load_from_file(path).map_err(|error| {
                anyhow::anyhow!("could not read {}: {error}", path.display())
            })?),
            None,
        ),
        None => (None, Some(irontsc::preferences::Preferences::load())),
    };

    let as_json = match (&settings, &preferences) {
        (Some(settings), _) => serde_json::to_value(settings)?,
        (_, Some(preferences)) => serde_json::to_value(preferences)?,
        _ => unreachable!("one of the two is always read"),
    };
    let mut fields = match as_json {
        serde_json::Value::Object(fields) => fields,
        _ => anyhow::bail!("the settings are not an object, which should not happen"),
    };

    let Some(name) = command.name.as_deref().map(str::trim) else {
        for (name, value) in &fields {
            println!("{name} = {}", show(name, value));
        }
        return Ok(());
    };

    let Some(current) = fields.get(name).cloned() else {
        let known: Vec<_> = fields.keys().map(String::as_str).collect();
        anyhow::bail!("no setting called `{name}`; there is {}", known.join(", "));
    };

    let Some(value) = command.value.as_deref() else {
        // Named explicitly, so print it as it is -- including a password, which the caller has
        // just asked for by name.
        println!("{}", as_text(&current));
        return Ok(());
    };

    // Keep the field's own type: a bool stays a bool, a number stays a number.
    let parsed = match &current {
        serde_json::Value::Bool(_) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => serde_json::Value::Bool(true),
            "false" | "no" | "off" | "0" => serde_json::Value::Bool(false),
            _ => anyhow::bail!("`{name}` is true or false, not `{value}`"),
        },
        serde_json::Value::Number(_) => value
            .trim()
            .parse::<u64>()
            .map(serde_json::Value::from)
            .map_err(|_| anyhow::anyhow!("`{name}` is a number, not `{value}`"))?,
        serde_json::Value::Null if value.trim().parse::<u64>().is_ok() => {
            serde_json::Value::from(value.trim().parse::<u64>().expect("just parsed"))
        }
        _ => serde_json::Value::String(value.to_owned()),
    };

    fields.insert(name.to_owned(), parsed.clone());
    let updated = serde_json::Value::Object(fields);

    match &command.file {
        Some(path) => {
            let settings: RdpSettings = serde_json::from_value(updated)?;
            settings.save_to_file(path).map_err(|error| {
                anyhow::anyhow!("could not write {}: {error}", path.display())
            })?;
        }
        None => {
            let preferences: irontsc::preferences::Preferences = serde_json::from_value(updated)?;
            preferences
                .save()
                .map_err(|error| anyhow::anyhow!("could not write the preferences: {error}"))?;
        }
    }

    println!("{name} = {}", as_text(&parsed));
    Ok(())
}

/// A value as a person would type it, without JSON's quotes.
fn as_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The same, but a password is not printed just because the whole file was listed.
fn show(name: &str, value: &serde_json::Value) -> String {
    if name == "password" && !as_text(value).is_empty() {
        return "(set)".to_owned();
    }
    as_text(value)
}
