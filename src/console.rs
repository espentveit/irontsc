//! A terminal docked beside the remote desktop, so a local agent can be driven without
//! leaving the window.
//!
//! The point of it is MCP mode: an agent runs here, against the session filling the rest of
//! the window, and the desktop it is driving is in view the whole time. Which removes the
//! alt-tabbing that otherwise sits between watching a session and steering the thing that is
//! working it.
//!
//! The emulation is `egui_term` over `alacritty_terminal`, because Claude Code -- and any
//! other full-screen TUI -- needs a real terminal: alternate screen, colours, cursor
//! addressing, bracketed paste. A line-oriented console would not run it at all.
//!
//! That choice is behind [`Terminal`], because it is the part most likely to be replaced. The
//! pty and the VT parser are settled work; how the grid is *drawn* is where an embedded
//! terminal is better or worse than a real one, and swapping [`EguiTerm`] for something else
//! -- a hand-written painter, or a grid rendered straight into the GL context this window
//! already owns -- touches nothing above the trait.
//!
//! Nothing here reaches into the RDP session. The console only has to exist inside the same
//! window; what makes the keys go to one or the other is the routing in [`crate::egui_app`],
//! which is a single branch, and the [`Console::focused`] flag it reads.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui_term::{
    BackendCommand, BackendSettings, PtyEvent, TerminalBackend, TerminalFont, TerminalTheme,
    TerminalView,
};

/// Which edge the console is docked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dock {
    #[default]
    Bottom,
    Right,
    Left,
}

impl Dock {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bottom => "Bottom",
            Self::Right => "Right",
            Self::Left => "Left",
        }
    }

    /// Whether the dock takes width from the desktop rather than height.
    pub fn is_side(self) -> bool {
        matches!(self, Self::Right | Self::Left)
    }
}

/// What to run in the console.
#[derive(Debug, Clone)]
pub struct ConsoleSettings {
    pub shell: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
}

impl Default for ConsoleSettings {
    fn default() -> Self {
        // The user's own shell, not `claude` directly: a shell survives the agent exiting, so
        // the console is still there to start it again. The header has a button for that.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_owned());
        Self {
            shell,
            args: Vec::new(),
            working_directory: std::env::current_dir().ok(),
        }
    }
}

/// A terminal: the program, the screen it draws on, and how it is painted.
///
/// The seam exists because the *model* and the *renderer* age differently. Keeping a pty and
/// a VT parser is settled work -- `alacritty_terminal` does it well and there is little to
/// argue about -- whereas how the resulting grid is drawn is exactly where an embedded
/// terminal is better or worse than a real one. Everything outside this trait is about being
/// a panel in this window: docking, focus, the header. None of it changes if the terminal
/// underneath is replaced.
///
/// [`EguiTerm`] is the implementation in the tree today.
pub trait Terminal {
    /// Draws the terminal into the space it is given.
    fn ui(&mut self, ui: &mut egui::Ui, focused: bool);

    /// Sends bytes to the program, as though they had been typed.
    fn write(&mut self, bytes: Vec<u8>);

    /// Takes whatever the program has said since the last frame.
    fn pump(&mut self);

    /// The window title the program has asked for.
    fn title(&self) -> &str;

    /// Some(reason) once the program has finished.
    fn exited(&self) -> Option<&str>;
}

/// The terminal built on `egui_term` and `alacritty_terminal`.
pub struct EguiTerm {
    backend: TerminalBackend,
    events: Receiver<(u64, PtyEvent)>,
    theme: TerminalTheme,
    font: TerminalFont,
    title: String,
    exited: Option<String>,
}

impl EguiTerm {
    /// Opens a pty running `settings.shell` and wires its output to this window's repaints.
    pub fn spawn(ctx: &egui::Context, settings: &ConsoleSettings) -> anyhow::Result<Self> {
        install_symbol_fallbacks(ctx);

        let (sender, events) = std::sync::mpsc::channel();

        let backend = TerminalBackend::new(
            0,
            ctx.clone(),
            sender,
            BackendSettings {
                shell: settings.shell.clone(),
                args: settings.args.clone(),
                working_directory: settings.working_directory.clone(),
            },
        )
        .map_err(|error| anyhow::anyhow!("failed to start `{}`: {error}", settings.shell))?;

        Ok(Self {
            backend,
            events,
            theme: TerminalTheme::default(),
            font: TerminalFont::default(),
            title: "Console".to_owned(),
            exited: None,
        })
    }
}

impl Terminal for EguiTerm {
    fn ui(&mut self, ui: &mut egui::Ui, focused: bool) {
        let size = ui.available_size();
        // Built before `add` so the widget's borrow of `ui` (for its persistent id) is
        // finished before `add` takes its own.
        let view = TerminalView::new(ui, &mut self.backend)
            .set_focus(focused)
            .set_theme(self.theme.clone())
            .set_font(self.font.clone())
            .set_size(size);
        ui.add(view);
    }

    fn write(&mut self, bytes: Vec<u8>) {
        self.backend.process_command(BackendCommand::Write(bytes));
    }

    /// Most events only matter to the widget, which reads the terminal directly; the ones
    /// handled here are the ones that need something from outside it -- a reply written back,
    /// the clipboard, or the news that the child is gone.
    fn pump(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok((_, event)) => match event {
                    PtyEvent::Title(title) => self.title = title,
                    PtyEvent::ResetTitle => self.title = "Console".to_owned(),

                    // Queries the terminal has to answer, such as cursor position reports.
                    // Dropping these is what makes a TUI hang waiting for a reply.
                    PtyEvent::PtyWrite(text) => self.write(text.into_bytes()),
                    PtyEvent::ColorRequest(index, formatter) => {
                        let colour = self.theme.get_color(alacritty_color(index));
                        let reply = formatter(alacritty_terminal::vte::ansi::Rgb {
                            r: colour.r(),
                            g: colour.g(),
                            b: colour.b(),
                        });
                        self.write(reply.into_bytes());
                    }

                    PtyEvent::ClipboardStore(_, text) => {
                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                            let _ = clipboard.set_text(text);
                        }
                    }
                    PtyEvent::ClipboardLoad(_, formatter) => {
                        let text = arboard::Clipboard::new()
                            .and_then(|mut clipboard| clipboard.get_text())
                            .unwrap_or_default();
                        let reply = formatter(&text);
                        self.write(reply.into_bytes());
                    }

                    PtyEvent::Exit => {
                        self.exited.get_or_insert_with(|| "exited".to_owned());
                    }
                    PtyEvent::ChildExit(code) => {
                        self.exited = Some(format!("exited with status {code}"));
                    }

                    // Redraw hints and bells; the widget reads the grid itself.
                    _ => {}
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.exited.get_or_insert_with(|| "exited".to_owned());
                    break;
                }
            }
        }
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn exited(&self) -> Option<&str> {
        self.exited.as_deref()
    }
}

/// The docked panel: where the terminal sits, and who has the keys.
pub struct Console {
    terminal: Box<dyn Terminal>,

    pub dock: Dock,
    /// Height when docked to the bottom, width when docked to a side.
    pub size: f32,
    /// Whether keys go here rather than to the remote desktop.
    pub focused: bool,
    /// Where it was drawn, so a click can be attributed to it.
    pub rect: egui::Rect,
}

impl Console {
    /// Opens a console running the tree's own terminal.
    pub fn spawn(ctx: &egui::Context, settings: &ConsoleSettings) -> anyhow::Result<Self> {
        Ok(Self::with_terminal(Box::new(EguiTerm::spawn(ctx, settings)?)))
    }

    /// Opens a console around any other terminal implementation.
    pub fn with_terminal(terminal: Box<dyn Terminal>) -> Self {
        Self {
            terminal,
            dock: Dock::default(),
            size: 320.0,
            // Focused on open: opening it is the act of wanting to type in it.
            focused: true,
            rect: egui::Rect::ZERO,
        }
    }

    /// The window title the program inside has asked for.
    pub fn title(&self) -> &str {
        self.terminal.title()
    }

    /// Some(reason) once the program inside has finished.
    pub fn exited(&self) -> Option<&str> {
        self.terminal.exited()
    }

    /// Types a line into the console, as though it had been typed.
    pub fn send_line(&mut self, line: &str) {
        // A carriage return, which is what a pty in raw mode expects from Enter.
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\r');
        self.terminal.write(bytes);
    }

    /// Drains what the terminal has said since the last frame.
    pub fn pump(&mut self) {
        self.terminal.pump();
    }

    /// Draws the console and returns what the header was clicked for.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ConsoleAction {
        let mut action = ConsoleAction::default();
        let focused = self.focused;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(self.terminal.title()).strong());

            if let Some(reason) = self.terminal.exited().map(str::to_owned) {
                ui.label(
                    egui::RichText::new(reason)
                        .small()
                        .color(egui::Color32::LIGHT_RED),
                );
                if ui
                    .button("Restart")
                    .on_hover_text("Start the shell again")
                    .clicked()
                {
                    action.restart = true;
                }
            } else if ui
                .button("Start Claude")
                .on_hover_text("Type `claude` at the prompt")
                .clicked()
            {
                self.send_line("claude");
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("X").on_hover_text("Close the console").clicked() {
                    action.close = true;
                }
                egui::ComboBox::from_id_salt("irontsc-console-dock")
                    .selected_text(self.dock.label())
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        for dock in [Dock::Bottom, Dock::Right, Dock::Left] {
                            ui.selectable_value(&mut self.dock, dock, dock.label());
                        }
                    });
                // A quiet reminder of where the keys are going, since that is the one thing
                // about a terminal next to a remote desktop that is not obvious.
                ui.label(
                    egui::RichText::new(if focused {
                        "keys: console"
                    } else {
                        "keys: desktop"
                    })
                    .small()
                    .weak(),
                );
            });
        });

        ui.separator();
        self.terminal.ui(ui, focused);

        action
    }
}

/// Everything the window needs to know about the console, kept on this side of the wall.
///
/// The point of the type is containment. The console is a guess at a useful feature, and if
/// it turns out not to be one it should come out in one piece: the window holds a single
/// `ConsoleHost`, calls a handful of methods on it, and every decision about docking, focus,
/// spawning and failure lives here. Deleting the feature is deleting this module and those
/// call sites, with nothing left threaded through the session or the input path.
#[derive(Default)]
pub struct ConsoleHost {
    console: Option<Console>,
    settings: ConsoleSettings,
    /// Why it last refused to start, for the gear menu.
    error: Option<String>,
}

impl ConsoleHost {
    /// True while the console has the keys, which is the one thing the input path asks.
    pub fn is_focused(&self) -> bool {
        self.console.as_ref().is_some_and(|console| console.focused)
    }

    pub fn is_open(&self) -> bool {
        self.console.is_some()
    }

    /// Opens the console, or closes it if it is already open.
    pub fn toggle(&mut self, ctx: &egui::Context) {
        if self.console.take().is_some() {
            self.error = None;
            return;
        }
        self.open(ctx);
    }

    pub fn open(&mut self, ctx: &egui::Context) {
        match Console::spawn(ctx, &self.settings) {
            Ok(console) => {
                self.console = Some(console);
                self.error = None;
            }
            Err(error) => {
                tracing::error!(%error, "failed to open the console");
                self.error = Some(format!("{error}"));
            }
        }
    }

    pub fn close(&mut self) {
        self.console = None;
    }

    /// Takes whatever the terminal has said since the last frame.
    pub fn pump(&mut self) {
        if let Some(console) = self.console.as_mut() {
            console.pump();
        }
    }

    /// Whether a point in window coordinates landed on the console.
    pub fn hit(&self, position: egui::Pos2) -> bool {
        self.console
            .as_ref()
            .is_some_and(|console| console.rect.contains(position))
    }

    /// Points the keys at the console or away from it, reporting whether anything moved.
    pub fn set_focus(&mut self, focused: bool) -> bool {
        match self.console.as_mut() {
            Some(console) if console.focused != focused => {
                console.focused = focused;
                true
            }
            _ => false,
        }
    }

    /// Draws the console in its dock, if it is open.
    ///
    /// Declared before the window's central panel, which is what leaves the desktop the space
    /// that is left; the header's own buttons are acted on here rather than handed back.
    pub fn show_docked(&mut self, ctx: &egui::Context) {
        let Some(console) = self.console.as_mut() else {
            return;
        };

        let dock = console.dock;
        let response = if dock.is_side() {
            let side = if matches!(dock, Dock::Right) {
                egui::panel::Side::Right
            } else {
                egui::panel::Side::Left
            };
            egui::SidePanel::new(side, "irontsc-console")
                .resizable(true)
                .default_width(console.size)
                .show(ctx, |ui| console.ui(ui))
        } else {
            egui::TopBottomPanel::bottom("irontsc-console")
                .resizable(true)
                .default_height(console.size)
                .show(ctx, |ui| console.ui(ui))
        };

        console.rect = response.response.rect;
        console.size = if dock.is_side() {
            console.rect.width()
        } else {
            console.rect.height()
        };

        match response.inner {
            ConsoleAction { close: true, .. } => self.close(),
            ConsoleAction { restart: true, .. } => {
                self.close();
                self.open(ctx);
            }
            _ => {}
        }
    }

    /// What the gear menu shows: whether it is open, what is running, and any failure.
    pub fn status(&self) -> (bool, String, Option<String>) {
        (
            self.console.is_some(),
            self.console
                .as_ref()
                .map(|console| console.title().to_owned())
                .unwrap_or_default(),
            self.error.clone(),
        )
    }
}

/// One font to try, and where it lives on this platform.
struct FontCandidate {
    /// Name registered with egui. Also the dedup key, so it must be unique.
    name: &'static str,
    /// Places to look, in order; the first that parses wins.
    paths: &'static [&'static str],
    /// Which face inside the file. Collections (`.ttc`) hold several; zero is the regular one.
    index: u32,
}

/// The fallback chain, in priority order.
///
/// egui's monospace family is an ordered list: a glyph is looked for in each font in turn, so
/// appending at the lowest priority builds a chain rather than replacing anything. Hack stays
/// first and keeps setting the metrics -- which is what holds the terminal grid square -- and
/// these are only consulted for what it does not have.
///
/// The order within the chain is broad monospace first, then a dedicated symbol font, then a
/// last-resort catch-all. Nothing here is required: each entry that is missing or unparseable
/// is skipped, and the console renders with whatever was found.
#[cfg(all(unix, not(target_os = "macos")))]
const FALLBACK_FONTS: &[FontCandidate] = &[
    FontCandidate {
        name: "irontsc-dejavu-mono",
        paths: &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/DejaVuSansMono.ttf",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-noto-symbols2",
        paths: &[
            // Braille, which is what most TUI spinners are made of.
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/TTF/NotoSansSymbols2-Regular.ttf",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-noto-mono",
        paths: &[
            "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-symbola",
        paths: &[
            "/usr/share/fonts/truetype/ancient-scripts/Symbola_hint.ttf",
            "/usr/share/fonts/truetype/ttf-ancient-scripts/Symbola_hint.ttf",
            "/usr/share/fonts/TTF/Symbola.ttf",
        ],
        index: 0,
    },
];

#[cfg(target_os = "windows")]
const FALLBACK_FONTS: &[FontCandidate] = &[
    FontCandidate {
        name: "irontsc-cascadia-mono",
        paths: &[
            r"C:\Windows\Fonts\CascadiaMono.ttf",
            r"C:\Windows\Fonts\CascadiaCode.ttf",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-consolas",
        paths: &[r"C:\Windows\Fonts\consola.ttf"],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-segoe-symbol",
        paths: &[
            r"C:\Windows\Fonts\seguisym.ttf",
            r"C:\Windows\Fonts\SegoeIcons.ttf",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-lucida-console",
        paths: &[r"C:\Windows\Fonts\lucon.ttf"],
        index: 0,
    },
];

#[cfg(target_os = "macos")]
const FALLBACK_FONTS: &[FontCandidate] = &[
    FontCandidate {
        name: "irontsc-menlo",
        // A collection; face zero is Menlo Regular.
        paths: &[
            "/System/Library/Fonts/Menlo.ttc",
            "/Library/Fonts/Menlo.ttc",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-sf-mono",
        paths: &[
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/SFNSMono.ttc",
        ],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-apple-symbols",
        paths: &["/System/Library/Fonts/Apple Symbols.ttf"],
        index: 0,
    },
    FontCandidate {
        name: "irontsc-arial-unicode",
        paths: &["/Library/Fonts/Arial Unicode.ttf"],
        index: 0,
    },
];

/// How many fallbacks to actually install.
///
/// Each one is rasterised into the atlas, and by the fourth the chain is answering questions
/// nothing asks.
const MAX_FALLBACK_FONTS: usize = 4;

/// Environment override, for a font this list does not know about.
///
/// Paths separated the way the platform separates them, tried ahead of the built-in chain.
const FONT_OVERRIDE_VAR: &str = "IRONTSC_CONSOLE_FONTS";

/// Reads a font file and checks it parses, returning the bytes if it does.
///
/// The check matters: egui parses font data with `ab_glyph` and *panics* if it cannot, so
/// handing it a path that turned out to be a bitmap font, a `.dfont`, or a collection whose
/// face index is wrong would take the window down. Parsing it here first, with the same
/// library, turns that into a skip.
fn load_font(path: &str, index: u32) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;

    match ab_glyph::FontVec::try_from_vec_and_index(bytes.clone(), index) {
        Ok(_) => Some(bytes),
        Err(error) => {
            tracing::debug!(path, %error, "font file will not parse, skipping");
            None
        }
    }
}

/// Builds the monospace fallback chain, once per process.
///
/// Returns the names installed, in priority order, which is what the tests assert on.
fn install_symbol_fallbacks(ctx: &egui::Context) -> Vec<String> {
    let mut installed: Vec<String> = Vec::new();

    // A function rather than a closure, so reading `installed` to check the cap does not
    // collide with the closure's borrow of it.
    fn install(
        ctx: &egui::Context,
        installed: &mut Vec<String>,
        name: &str,
        bytes: Vec<u8>,
        index: u32,
    ) {
        use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};

        let mut data = egui::FontData::from_owned(bytes);
        data.index = index;

        ctx.add_font(FontInsert::new(
            name,
            data,
            // Lowest, so each call appends: insertion order becomes the chain order, and Hack
            // keeps the primary slot along with the metrics the grid is laid out on.
            vec![InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: FontPriority::Lowest,
            }],
        ));
        installed.push(name.to_owned());
    }

    // The override goes first, so a font named there outranks the built-in chain.
    if let Ok(value) = std::env::var(FONT_OVERRIDE_VAR) {
        for (position, path) in std::env::split_paths(&value).enumerate() {
            if installed.len() >= MAX_FALLBACK_FONTS {
                break;
            }
            let Some(path) = path.to_str() else { continue };
            if let Some(bytes) = load_font(path, 0) {
                let name = format!("irontsc-console-override-{position}");
                install(ctx, &mut installed, &name, bytes, 0);
            }
        }
    }

    for candidate in FALLBACK_FONTS {
        if installed.len() >= MAX_FALLBACK_FONTS {
            break;
        }
        if let Some((path, bytes)) = candidate
            .paths
            .iter()
            .find_map(|path| load_font(path, candidate.index).map(|bytes| (*path, bytes)))
        {
            tracing::debug!(path, name = candidate.name, "console fallback font");
            install(ctx, &mut installed, candidate.name, bytes, candidate.index);
        }
    }

    if installed.is_empty() {
        tracing::debug!(
            "no fallback font found; the console may show blanks for decorative glyphs"
        );
    } else {
        tracing::debug!(chain = ?installed, "console monospace fallback chain");
    }

    installed
}

/// Turns a palette index into the colour type the terminal asks for it by.
fn alacritty_color(index: usize) -> alacritty_terminal::vte::ansi::Color {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    match index {
        256 => Color::Named(NamedColor::Foreground),
        257 => Color::Named(NamedColor::Background),
        // Indexed covers 0..=255; anything else is out of range and foreground is the safe
        // answer, since a wrong colour is better than no reply at all.
        other => match u8::try_from(other) {
            Ok(value) => Color::Indexed(value),
            Err(_) => Color::Named(NamedColor::Foreground),
        },
    }
}

/// What the console's header asked the window to do.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsoleAction {
    pub close: bool,
    pub restart: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_chain_has_unique_names() {
        // The name is egui's dedup key, so a repeat would silently drop a link.
        let mut names: Vec<&str> = FALLBACK_FONTS.iter().map(|font| font.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate font name in the chain");
    }

    #[test]
    fn every_candidate_offers_somewhere_to_look() {
        for candidate in FALLBACK_FONTS {
            assert!(
                !candidate.paths.is_empty(),
                "`{}` has no paths",
                candidate.name
            );
        }
    }

    #[test]
    fn a_file_that_is_not_a_font_is_skipped_rather_than_installed() {
        // The whole reason for validating: egui panics on bytes it cannot parse.
        let mut path = std::env::temp_dir();
        path.push("irontsc-not-a-font.ttf");
        std::fs::write(&path, b"this is definitely not a font").expect("write the decoy");

        let loaded = load_font(path.to_str().expect("utf-8 path"), 0);
        let _ = std::fs::remove_file(&path);

        assert!(loaded.is_none(), "a non-font must not reach egui");
    }

    #[test]
    fn a_missing_path_is_a_miss_not_a_panic() {
        assert!(load_font("/nonexistent/irontsc/font.ttf", 0).is_none());
    }

    #[test]
    fn the_chain_installs_in_priority_order_and_is_capped() {
        let ctx = egui::Context::default();
        let installed = install_symbol_fallbacks(&ctx);

        assert!(
            installed.len() <= MAX_FALLBACK_FONTS,
            "installed {} fonts, over the cap",
            installed.len()
        );

        // Whatever was found must appear in the order the chain declares, since that order is
        // the priority.
        let declared: Vec<&str> = FALLBACK_FONTS.iter().map(|font| font.name).collect();
        let positions: Vec<usize> = installed
            .iter()
            .filter_map(|name| declared.iter().position(|candidate| candidate == name))
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "fonts installed out of priority order");
    }

    /// A terminal that is not `egui_term`, to show the seam is real.
    #[derive(Default)]
    struct FakeTerminal {
        written: Vec<u8>,
        pumped: usize,
        title: String,
        exited: Option<String>,
    }

    impl Terminal for FakeTerminal {
        fn ui(&mut self, _ui: &mut egui::Ui, _focused: bool) {}

        fn write(&mut self, bytes: Vec<u8>) {
            self.written.extend(bytes);
        }

        fn pump(&mut self) {
            self.pumped += 1;
        }

        fn title(&self) -> &str {
            &self.title
        }

        fn exited(&self) -> Option<&str> {
            self.exited.as_deref()
        }
    }

    #[test]
    fn a_console_can_be_built_on_any_terminal() {
        // No pty, no egui_term, no window: everything the panel does is above the seam.
        let mut console = Console::with_terminal(Box::new(FakeTerminal {
            title: "fake".to_owned(),
            ..FakeTerminal::default()
        }));

        assert_eq!(console.title(), "fake");
        assert!(console.exited().is_none());

        console.send_line("claude");
        console.pump();
    }

    #[test]
    fn a_typed_line_ends_in_a_carriage_return() {
        // What a pty in raw mode wants from Enter; a newline is not the same thing.
        struct Spy(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

        impl Terminal for Spy {
            fn ui(&mut self, _ui: &mut egui::Ui, _focused: bool) {}
            fn write(&mut self, bytes: Vec<u8>) {
                self.0.borrow_mut().extend(bytes);
            }
            fn pump(&mut self) {}
            fn title(&self) -> &str {
                "spy"
            }
            fn exited(&self) -> Option<&str> {
                None
            }
        }

        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut console = Console::with_terminal(Box::new(Spy(std::rc::Rc::clone(&written))));
        console.send_line("claude");

        assert_eq!(written.borrow().as_slice(), b"claude\r");
    }

    #[test]
    fn side_docks_take_width() {
        assert!(Dock::Right.is_side());
        assert!(Dock::Left.is_side());
        assert!(!Dock::Bottom.is_side());
    }

    #[test]
    fn defaults_to_a_shell_that_survives_the_agent_exiting() {
        let settings = ConsoleSettings::default();
        assert!(
            !settings.shell.is_empty(),
            "there is always a shell to fall back on"
        );
        assert!(settings.args.is_empty());
    }

    /// Spawns a real shell, tells it to leave, and waits for the news to come back.
    ///
    /// Cheap, and it is the only thing here that proves the parts outside this file are
    /// wired: the pty opens, a write reaches the child, and the child's exit arrives as an
    /// event rather than as silence.
    #[test]
    fn runs_a_shell_and_notices_it_leaving() {
        let ctx = egui::Context::default();
        let settings = ConsoleSettings {
            shell: "/bin/sh".to_owned(),
            args: Vec::new(),
            working_directory: None,
        };

        let Ok(mut console) = Console::spawn(&ctx, &settings) else {
            // No pty available (a sandbox without /dev/ptmx); nothing to prove here.
            return;
        };

        console.send_line("exit");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while console.exited().is_none() && std::time::Instant::now() < deadline {
            console.pump();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(
            console.exited().is_some(),
            "the shell should have exited and said so"
        );
    }

    #[test]
    fn maps_palette_indices_to_terminal_colours() {
        use alacritty_terminal::vte::ansi::{Color, NamedColor};

        assert!(matches!(alacritty_color(0), Color::Indexed(0)));
        assert!(matches!(alacritty_color(255), Color::Indexed(255)));
        assert!(matches!(
            alacritty_color(256),
            Color::Named(NamedColor::Foreground)
        ));
        assert!(matches!(
            alacritty_color(257),
            Color::Named(NamedColor::Background)
        ));
        // Out of range rather than panicking.
        assert!(matches!(
            alacritty_color(9999),
            Color::Named(NamedColor::Foreground)
        ));
    }
}
