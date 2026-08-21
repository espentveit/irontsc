//! The egui/winit shell: a connection dialog, and the session view it grows into.
//!
//! This is a second frontend for the same RDP core the GTK client drives: everything below
//! `RdpInputEvent` / `RdpOutputEvent` (`rdp`, `gfx`, `udp_transport`, ...) is shared verbatim
//! and only the window, the surface and the input translation are rewritten here.
//!
//! Four structural choices are worth knowing about before reading on:
//!
//! * The window, GL context and event loop are driven directly (winit + glutin + `egui_glow`)
//!   rather than through `eframe`. RDP needs the physical position of every key, and eframe
//!   only ever exposes egui's logical `Key`, which has no name for CapsLock, cannot tell the
//!   numpad from the number row, and -- most damaging -- collapses AltGr into Alt. Owning the
//!   event loop means `KeyEvent::physical_key` arrives untouched.
//! * The desktop is one egui texture that stays on the GPU. Updates that name a region go
//!   through `TextureHandle::set_partial`, which becomes a `glTexSubImage2D` of just that
//!   rectangle; a full upload happens only when the server sends a full frame or the surface
//!   is resized. Re-uploading the whole framebuffer for a blinking cursor would be 16MB a
//!   frame at 2560x1600.
//! * One window serves both phases, following mstsc and the GTK client: it opens small,
//!   sized to the connection form in *logical* units so fractional scaling does not make it
//!   tiny or enormous, and grows to the session size once connected.
//! * The session controls float over the desktop as an island rather than docking above it,
//!   so the remote surface gets the whole window.

use core::num::NonZeroU32;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_glow::EguiGlow;
use ironrdp::cliprdr::backend::CliprdrBackendFactory;
use ironrdp::input::MouseButton as RdpMouseButton;
use ironrdp::input::{Database, MousePosition, Operation, WheelRotations};
use smallvec::SmallVec;
use tokio::sync::mpsc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
use winit::raw_window_handle::HasWindowHandle as _;
use winit::window::{Cursor, CursorIcon, CustomCursor, Fullscreen, Window};

use crate::config::{ClipboardType, Config, Destination};
use crate::egui_scancode::scancode_for;
use crate::egui_shortcuts::ShortcutCapture;
use crate::rdp::{
    ArboardClipboardFactory, ConnectionStats, DvcPipeProxyFactory, ImageRegion, RdpClient,
    RdpEventSender, RdpInputEvent, RdpOutputEvent,
};
use crate::settings::{
    ColorDepth, DEFAULT_RDP_FILE, DPI_SCALE_OPTIONS, RdpSettings, Resolution, dpi_index_from_value,
    dpi_value_from_index,
};

/// Nearest-neighbour magnification keeps text crisp in the common case where the texture and
/// the widget are the same size in physical pixels; linear minification stops the picture from
/// shimmering during the window resize before the server has caught up.
const SURFACE_TEXTURE_OPTIONS: egui::TextureOptions = egui::TextureOptions {
    magnification: egui::TextureFilter::Nearest,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: None,
};

/// How long the window has to stay still before a new desktop size is negotiated. Resizing is
/// a full deactivate/reactivate on the server, so it must not run on every motion event.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);

/// RDP counts a wheel notch as 120 units.
const WHEEL_UNITS_PER_NOTCH: f32 = 120.0;

/// Points of pixel-precise scrolling that count as one notch, for touchpads and Wayland
/// high-resolution wheels, which report pixels instead of lines.
const SCROLL_POINTS_PER_NOTCH: f32 = 50.0;

/// The connection form's window, in logical points so that a scaled display gets a dialog of
/// the same apparent size. Narrow and short, in the shape mstsc uses.
const DIALOG_SIZE: (f64, f64) = (420.0, 330.0);

/// The same window with the options showing. mstsc grows its dialog rather than scrolling the
/// options inside the small one, and so does this.
const DIALOG_SIZE_OPTIONS: (f64, f64) = (470.0, 620.0);

/// The pointer has to settle inside this band at the top of the window before the island is
/// revealed in fullscreen.
const ISLAND_REVEAL_BAND: f32 = 50.0;

/// Revealing the island the instant the pointer crossed the top edge meant merely passing
/// through summoned it, and it swallowed a click aimed at the desktop. Make it wait.
const ISLAND_REVEAL_DELAY: Duration = Duration::from_millis(400);

/// How far back the frame rate is measured. Short enough to follow a stall, long enough that
/// a single late frame does not read as a collapse.
const FRAME_RATE_WINDOW: Duration = Duration::from_secs(2);

/// How long to wait for the compositor to honour the switch to the session window size before
/// negotiating a desktop size anyway.
const SESSION_RESIZE_GRACE: Duration = Duration::from_millis(2000);

#[derive(Debug)]
enum UserEvent {
    /// The RDP thread has queued at least one output event.
    RdpOutput,
    /// egui asked to be repainted, at the earliest after the given delay.
    Repaint(Duration),
}

/// Bridges the RDP session thread to the winit event loop.
///
/// The payload travels through an unbounded channel and the proxy is used only to wake the
/// loop up, with a flag so that a burst of damage rectangles cannot pile a thousand wakeups
/// into the proxy queue.
#[derive(Clone)]
struct SessionEventSender {
    sender: mpsc::UnboundedSender<RdpOutputEvent>,
    proxy: Arc<Mutex<EventLoopProxy<UserEvent>>>,
    wakeup_pending: Arc<AtomicBool>,
}

impl RdpEventSender for SessionEventSender {
    fn send_event(&self, event: RdpOutputEvent) -> Result<(), ()> {
        self.sender.send(event).map_err(|_| ())?;

        if !self.wakeup_pending.swap(true, Ordering::AcqRel) {
            if let Ok(proxy) = self.proxy.lock() {
                let _ = proxy.send_event(UserEvent::RdpOutput);
            }
        }

        Ok(())
    }
}

/// What the session wants the pointer to look like right now.
enum PointerState {
    Default,
    Hidden,
    Bitmap(CustomCursor),
}

/// The connection form's contents.
#[derive(Clone, Default)]
pub struct ConnectForm {
    pub server: String,
    pub username: String,
    pub password: String,
    pub domain: String,
    pub save: bool,
}

impl ConnectForm {
    fn is_complete(&self) -> bool {
        !self.server.trim().is_empty() && !self.username.trim().is_empty()
    }
}

/// The tabs behind mstsc's Show Options. The set is ours rather than mstsc's -- it names what
/// this client actually has -- but the arrangement is the same.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionsTab {
    General,
    Display,
    Codecs,
    Network,
    Debug,
}

impl OptionsTab {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Display,
        Self::Codecs,
        Self::Network,
        Self::Debug,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Display => "Display",
            Self::Codecs => "Codecs",
            Self::Network => "Network",
            Self::Debug => "Debug",
        }
    }
}

/// The connection dialog's own state: what it is showing, and which file it is editing.
#[derive(Clone)]
struct DialogState {
    options_open: bool,
    tab: OptionsTab,
    /// The `.rdp` these settings were opened from or last saved to. `None` means the default
    /// file, which is what mstsc's plain Save writes to.
    file: Option<PathBuf>,
    /// One line under the buttons saying what the last Save or Open did.
    notice: Option<String>,
}

impl Default for DialogState {
    fn default() -> Self {
        Self {
            options_open: false,
            tab: OptionsTab::General,
            file: None,
            notice: None,
        }
    }
}

/// Everything that exists only while connected.
struct Session {
    input_sender: mpsc::UnboundedSender<RdpInputEvent>,
    output_receiver: mpsc::UnboundedReceiver<RdpOutputEvent>,
    /// Mirrors what the server believes is held down, so releases can be synthesised.
    input_database: Database,

    /// The remote desktop, living in one GPU texture that is updated in place.
    surface: Option<egui::TextureHandle>,
    /// Size of the remote surface in pixels, which is what pointer coordinates are scaled to.
    surface_size: (u16, u16),

    /// Where the desktop was drawn last frame, in points. Pointer coordinates and the
    /// negotiated desktop size are both derived from this.
    desktop_rect: egui::Rect,
    pointer_position: Option<egui::Pos2>,
    pointer_state: PointerState,
    /// How many mouse buttons the session currently holds. A drag that wanders off the desktop
    /// -- onto the island, or out of the window entirely -- still belongs to the session.
    buttons_held: u32,

    status: String,
    connected: bool,

    /// Who this session is with. The island names the machine the way mstsc's connection bar
    /// does, and the rest goes into its details tooltip.
    server: String,
    user: String,
    stats: SessionStats,

    resize_deadline: Option<Instant>,
    last_resize_sent: Option<(u16, u16, u32)>,
    /// The window size asked for when the session started. No desktop size is negotiated until
    /// the window has actually reached it, or this expires -- otherwise the remote desktop
    /// comes up at the dimensions of the connection dialog.
    pending_window_size: Option<(winit::dpi::PhysicalSize<u32>, Instant)>,
    shutting_down: bool,
}

impl Session {
    fn send_input(
        &self,
        events: SmallVec<[ironrdp::pdu::input::fast_path::FastPathInputEvent; 2]>,
    ) {
        if !events.is_empty() {
            let _ = self.input_sender.send(RdpInputEvent::FastPath(events));
        }
    }

    fn apply_operations(&mut self, operations: impl IntoIterator<Item = Operation>) {
        let events = self.input_database.apply(operations);
        self.send_input(events);
    }

    /// Releases everything the server thinks is held.
    ///
    /// The window stops receiving key releases the moment focus leaves, so an Alt held for
    /// Alt-Tab, or a modifier held when a resize drag takes over, otherwise latches down on the
    /// server and corrupts every keystroke that follows.
    fn release_all_input(&mut self, reason: &str) {
        self.buttons_held = 0;
        let events = self.input_database.release_all();
        if !events.is_empty() {
            tracing::debug!(count = events.len(), reason, "releasing held input");
            self.send_input(events);
        }
    }

    /// Maps a position in the window onto the remote surface.
    ///
    /// The desktop is drawn to fill `desktop_rect`, so the two only differ by the display scale
    /// -- except in the window between a local resize and the server acknowledging it, where
    /// scaling by the ratio keeps the pointer under the same feature of the picture.
    fn remote_position(&self, position: egui::Pos2) -> Option<MousePosition> {
        let (width, height) = self.surface_size;
        if width == 0 || height == 0 {
            return None;
        }

        let rect = self.desktop_rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }

        let x = ((position.x - rect.min.x) / rect.width() * f32::from(width))
            .clamp(0.0, f32::from(width - 1));
        let y = ((position.y - rect.min.y) / rect.height() * f32::from(height))
            .clamp(0.0, f32::from(height - 1));

        Some(MousePosition {
            x: x as u16,
            y: y as u16,
        })
    }

    fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.release_all_input("session is closing");
        let _ = self.input_sender.send(RdpInputEvent::Close);
    }
}

/// Window, GL display, context and surface, kept together because they have to be dropped
/// together. Lifted from the upstream `egui_glow` "pure glow" example, which in turn took it
/// from eframe.
struct GlutinWindowContext {
    window: Window,
    gl_context: glutin::context::PossiblyCurrentContext,
    gl_display: glutin::display::Display,
    gl_surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
}

impl GlutinWindowContext {
    /// # Safety
    ///
    /// Creates a GL context and surface for `event_loop`'s display; the caller must keep the
    /// returned value alive for as long as the context is current.
    unsafe fn new(event_loop: &ActiveEventLoop, title: &str) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        use glutin::context::NotCurrentGlContext as _;
        use glutin::display::{GetGlDisplay as _, GlDisplay as _};
        use glutin::surface::GlSurface as _;

        let window_attributes = Window::default_attributes()
            .with_resizable(true)
            .with_title(title)
            // Logical, so the dialog is the same apparent size at 100% and at 167%.
            .with_inner_size(winit::dpi::LogicalSize::new(DIALOG_SIZE.0, DIALOG_SIZE.1))
            // Stay hidden until there is something to show, to avoid a white flash.
            .with_visible(false);

        let config_template = glutin::config::ConfigTemplateBuilder::new()
            .prefer_hardware_accelerated(None)
            .with_depth_size(0)
            .with_stencil_size(0)
            .with_transparency(false);

        let (mut window, gl_config) = glutin_winit::DisplayBuilder::new()
            .with_preference(glutin_winit::ApiPreference::FallbackEgl)
            .with_window_attributes(Some(window_attributes.clone()))
            .build(event_loop, config_template, |mut configs| {
                configs
                    .next()
                    .expect("at least one matching GL configuration")
            })
            .map_err(|error| anyhow::anyhow!("failed to create a GL config: {error}"))?;

        let gl_display = gl_config.display();

        let raw_window_handle = window
            .as_ref()
            .and_then(|window| window.window_handle().ok())
            .map(|handle| handle.as_raw());

        let context_attributes =
            glutin::context::ContextAttributesBuilder::new().build(raw_window_handle);
        // Prefer desktop GL, but fall back to GLES where that is all there is.
        let fallback_attributes = glutin::context::ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::Gles(None))
            .build(raw_window_handle);

        // SAFETY: the raw window handle above comes from a window that outlives the context.
        let not_current = unsafe {
            gl_display
                .create_context(&gl_config, &context_attributes)
                .or_else(|_| gl_display.create_context(&gl_config, &fallback_attributes))
                .context("failed to create a GL context")?
        };

        let window = match window.take() {
            Some(window) => window,
            None => glutin_winit::finalize_window(event_loop, window_attributes, &gl_config)
                .context("failed to finalize the window")?,
        };

        let (width, height): (u32, u32) = window.inner_size().into();
        let surface_attributes =
            glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new()
                .build(
                    window.window_handle().context("no window handle")?.as_raw(),
                    NonZeroU32::new(width).unwrap_or(NonZeroU32::MIN),
                    NonZeroU32::new(height).unwrap_or(NonZeroU32::MIN),
                );

        // SAFETY: same window handle, same lifetime as above.
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surface_attributes)
                .context("failed to create a GL window surface")?
        };

        let gl_context = not_current
            .make_current(&gl_surface)
            .context("failed to make the GL context current")?;

        // Vsync: the session repaints on damage, and tearing on a desktop image looks awful.
        let _ = gl_surface.set_swap_interval(
            &gl_context,
            glutin::surface::SwapInterval::Wait(NonZeroU32::MIN),
        );

        Ok(Self {
            window,
            gl_context,
            gl_display,
            gl_surface,
        })
    }

    fn window(&self) -> &Window {
        &self.window
    }

    fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        use glutin::surface::GlSurface as _;

        let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };

        self.gl_surface.resize(&self.gl_context, width, height);
    }

    fn swap_buffers(&self) {
        use glutin::surface::GlSurface as _;

        if let Err(error) = self.gl_surface.swap_buffers(&self.gl_context) {
            tracing::warn!(%error, "failed to swap buffers");
        }
    }

    fn load_gl(&self) -> glow::Context {
        use glutin::display::GlDisplay as _;

        // SAFETY: the context is current on this thread, and the loader only resolves symbols.
        unsafe {
            glow::Context::from_loader_function(|symbol| match std::ffi::CString::new(symbol) {
                Ok(symbol) => self.gl_display.get_proc_address(&symbol),
                Err(_) => core::ptr::null(),
            })
        }
    }
}

/// What the session has reported about the link, kept rolling so the island can show it.
///
/// The GTK client drew this as graphs in a separate window. Here it is a tooltip on the
/// connection name instead: the numbers are worth having, but not worth a permanent row of
/// chrome across a desktop the island is already floating over.
struct SessionStats {
    protocol: String,
    /// The previous byte counters and when they arrived, which is all a rate needs.
    previous: Option<(Instant, u64, u64)>,
    /// KiB/s over the last reporting interval.
    received_rate: f64,
    sent_rate: f64,
    received_total: u64,
    sent_total: u64,
    /// Kept across samples: the server does not put a round trip in every report.
    roundtrip_ms: Option<u32>,
    /// When recent frames arrived, for the frame rate.
    frames: VecDeque<Instant>,
}

impl SessionStats {
    fn new() -> Self {
        Self {
            protocol: "Connecting...".to_owned(),
            previous: None,
            received_rate: 0.0,
            sent_rate: 0.0,
            received_total: 0,
            sent_total: 0,
            roundtrip_ms: None,
            frames: VecDeque::new(),
        }
    }

    fn record(&mut self, stats: &ConnectionStats) {
        let now = Instant::now();

        if let Some((then, sent, received)) = self.previous {
            let seconds = now.duration_since(then).as_secs_f64();
            // Guard against a burst of reports arriving together, which would divide by
            // almost nothing and show a rate of gigabytes a second.
            if seconds >= 0.1 {
                let sent_delta = stats.bytes_sent.saturating_sub(sent);
                let received_delta = stats.bytes_received.saturating_sub(received);
                self.sent_rate = sent_delta as f64 / seconds / 1024.0;
                self.received_rate = received_delta as f64 / seconds / 1024.0;
            }
        }

        self.previous = Some((now, stats.bytes_sent, stats.bytes_received));
        self.sent_total = stats.bytes_sent;
        self.received_total = stats.bytes_received;
        if stats.roundtrip_time_ms.is_some() {
            self.roundtrip_ms = stats.roundtrip_time_ms;
        }
        self.protocol = stats.transport_protocol.clone();
    }

    fn record_frame(&mut self) {
        let now = Instant::now();
        self.frames.push_back(now);
        while self
            .frames
            .front()
            .is_some_and(|frame| now.duration_since(*frame) > FRAME_RATE_WINDOW)
        {
            self.frames.pop_front();
        }
    }

    /// Frames per second over the measurement window, `None` until two frames have arrived.
    fn frame_rate(&self) -> Option<f32> {
        let first = *self.frames.front()?;
        let last = *self.frames.back()?;

        // A desktop with nothing moving on it sends no frames at all, which is zero rather
        // than whatever it was doing when the last one arrived.
        if last.elapsed() > FRAME_RATE_WINDOW {
            return Some(0.0);
        }

        let span = last.duration_since(first).as_secs_f32();
        if self.frames.len() < 2 || span <= 0.0 {
            return None;
        }

        Some((self.frames.len() - 1) as f32 / span)
    }

    fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            protocol: self.protocol.clone(),
            received_rate: self.received_rate,
            sent_rate: self.sent_rate,
            received_total: self.received_total,
            sent_total: self.sent_total,
            roundtrip_ms: self.roundtrip_ms,
            frame_rate: self.frame_rate(),
        }
    }
}

/// The statistics as of one frame, owned so the UI closure does not borrow the session.
struct StatsSnapshot {
    protocol: String,
    received_rate: f64,
    sent_rate: f64,
    received_total: u64,
    sent_total: u64,
    roundtrip_ms: Option<u32>,
    frame_rate: Option<f32>,
}

/// The floating control island's own state, which outlives individual sessions.
struct Island {
    /// Horizontal position as a fraction of the free width, so it stays put across resizes.
    position: f32,
    /// Size measured on the previous frame, needed to turn `position` into a coordinate.
    size: egui::Vec2,
    pinned: bool,
    /// Set while the pointer has been resting in the reveal band, cleared when it leaves.
    hover_since: Option<Instant>,
    revealed: bool,
    dragging: bool,
}

impl Default for Island {
    fn default() -> Self {
        Self {
            position: 0.5,
            size: egui::vec2(320.0, 34.0),
            pinned: false,
            hover_since: None,
            revealed: false,
            dragging: false,
        }
    }
}

struct SessionApp {
    gl_window: Option<GlutinWindowContext>,
    gl: Option<Arc<glow::Context>>,
    egui_glow: Option<EguiGlow>,
    shortcut_capture: Option<ShortcutCapture>,

    /// `None` on the connection dialog, `Some` once a session thread is running.
    session: Option<Session>,
    form: ConnectForm,
    settings: RdpSettings,
    connect_error: Option<String>,

    island: Island,
    dialog: DialogState,
    fullscreen: bool,

    proxy: Arc<Mutex<EventLoopProxy<UserEvent>>>,
    wakeup_pending: Arc<AtomicBool>,

    repaint_delay: Duration,
    /// A wake-up the island's reveal delay is waiting on.
    island_deadline: Option<Instant>,
    /// Connect as soon as there is a window, skipping the dialog entirely.
    pending_autoconnect: bool,
    exiting: bool,
}

impl SessionApp {
    fn new(
        form: ConnectForm,
        settings: RdpSettings,
        proxy: Arc<Mutex<EventLoopProxy<UserEvent>>>,
        wakeup_pending: Arc<AtomicBool>,
    ) -> Self {
        Self {
            gl_window: None,
            gl: None,
            egui_glow: None,
            shortcut_capture: None,
            session: None,
            form,
            settings,
            connect_error: None,
            island: Island::default(),
            dialog: DialogState::default(),
            fullscreen: false,
            proxy,
            wakeup_pending,
            repaint_delay: Duration::MAX,
            island_deadline: None,
            pending_autoconnect: false,
            exiting: false,
        }
    }

    fn pixels_per_point(&self) -> f32 {
        self.egui_glow
            .as_ref()
            .map(|egui_glow| egui_glow.egui_ctx.pixels_per_point())
            .filter(|ppp| *ppp > 0.0)
            .unwrap_or(1.0)
    }

    /// True only while egui is actually driving a widget.
    ///
    /// Deliberately not `wants_pointer_input`, which is true whenever the pointer is over any
    /// egui area -- and the desktop is painted inside a `CentralPanel`, which is one, so that
    /// test would swallow every mouse event the session is supposed to get.
    fn egui_using_pointer(&self) -> bool {
        self.egui_glow
            .as_ref()
            .is_some_and(|egui_glow| egui_glow.egui_ctx.is_using_pointer())
    }

    /// True when the pointer is over the desktop rather than over the island.
    fn pointer_over_desktop(&self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };

        let over_island = self.island_rect().is_some_and(|island| {
            session
                .pointer_position
                .is_some_and(|position| island.contains(position))
        });

        session
            .pointer_position
            .is_some_and(|position| session.desktop_rect.contains(position))
            && !over_island
            && !self.egui_using_pointer()
    }

    /// Everything the island paints, taken off the session before the frame borrows it.
    fn island_view(&self, scale_percent: u32) -> Option<IslandView> {
        let session = self.session.as_ref()?;

        Some(IslandView {
            server: session.server.clone(),
            user: session.user.clone(),
            status: session.status.clone(),
            connected: session.connected,
            surface_size: session.surface_size,
            scale_percent,
            stats: session.stats.snapshot(),
            pinned: self.island.pinned,
            fullscreen: self.fullscreen,
            capture_available: self
                .shortcut_capture
                .as_ref()
                .is_some_and(ShortcutCapture::is_available),
            capture_enabled: self
                .shortcut_capture
                .as_ref()
                .is_some_and(ShortcutCapture::is_enabled),
            capture_status: self.shortcut_capture.as_ref().map_or_else(
                || "unavailable".to_owned(),
                |capture| {
                    if capture.is_engaged() {
                        format!("{} (active)", capture.status())
                    } else {
                        capture.status().to_owned()
                    }
                },
            ),
        })
    }

    /// Where the island was drawn, when it is showing.
    fn island_rect(&self) -> Option<egui::Rect> {
        if !self.island_visible() {
            return None;
        }
        let position = self.island_position()?;
        Some(egui::Rect::from_min_size(position, self.island.size))
    }

    fn island_position(&self) -> Option<egui::Pos2> {
        let session = self.session.as_ref()?;
        let available = session.desktop_rect;
        if available.width() <= 0.0 {
            return None;
        }
        let free = (available.width() - self.island.size.x).max(0.0);
        // Hug the top edge of the session view: no gap between the island and the window.
        Some(egui::pos2(
            available.min.x + free * self.island.position.clamp(0.0, 1.0),
            available.min.y,
        ))
    }

    /// Windowed, pinned or mid-drag: always available. Fullscreen and unpinned: only once the
    /// pointer has settled near the top edge.
    fn island_visible(&self) -> bool {
        if self.session.is_none() {
            return false;
        }
        // Hiding belongs to fullscreen, as it does in mstsc and as it did in the GTK client:
        // windowed there is a title bar on screen anyway, and an auto-hiding toolbar in a
        // window the user can already see the edges of is just something to hunt for.
        if !self.fullscreen {
            return true;
        }
        self.island.pinned || self.island.dragging || self.island.revealed
    }

    /// Recomputes the reveal state from where the pointer is now.
    ///
    /// While the island is up for some other reason `revealed` is held true rather than
    /// cleared, so that unpinning it or letting go of a drag with the pointer still on it
    /// leaves it where it is instead of blinking out and serving the reveal delay again.
    fn update_island_reveal(&mut self) {
        if !self.fullscreen || self.island.pinned || self.island.dragging {
            self.island.hover_since = None;
            self.island.revealed = true;
            self.island_deadline = None;
            return;
        }

        // The band has to reach past the island itself: the pointer resting on its buttons is
        // not the pointer having left the top edge.
        let band = ISLAND_REVEAL_BAND.max(self.island.size.y);

        let Some(session) = self.session.as_ref() else {
            return;
        };

        let near_top = session
            .pointer_position
            .is_some_and(|position| position.y - session.desktop_rect.min.y < band);

        if !near_top {
            self.island.hover_since = None;
            self.island.revealed = false;
            self.island_deadline = None;
            return;
        }

        if self.island.revealed {
            return;
        }

        match self.island.hover_since {
            None => {
                let deadline = Instant::now() + ISLAND_REVEAL_DELAY;
                self.island.hover_since = Some(Instant::now());
                self.island_deadline = Some(deadline);
            }
            Some(since) => {
                if since.elapsed() >= ISLAND_REVEAL_DELAY {
                    self.island.revealed = true;
                    self.island_deadline = None;
                }
            }
        }
    }

    fn on_keyboard_input(&mut self, event: &winit::event::KeyEvent, is_synthetic: bool) {
        // Synthetic events are winit's replay of the keys that were already held when the
        // window took focus. Focus changes release everything anyway, so replaying them would
        // only press keys the user is no longer holding.
        if is_synthetic {
            return;
        }

        let Some(session) = self.session.as_mut() else {
            return;
        };

        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };

        let Some(scancode) = scancode_for(code) else {
            tracing::trace!(?code, "no scancode for physical key");
            return;
        };

        let operation = match event.state {
            ElementState::Pressed => Operation::KeyPressed(scancode),
            ElementState::Released => Operation::KeyReleased(scancode),
        };

        session.apply_operations(core::iter::once(operation));
    }

    fn on_mouse_button(&mut self, button: WinitMouseButton, state: ElementState) {
        let over_desktop = self.pointer_over_desktop();

        let Some(session) = self.session.as_mut() else {
            return;
        };

        let button = match button {
            WinitMouseButton::Left => RdpMouseButton::Left,
            WinitMouseButton::Middle => RdpMouseButton::Middle,
            WinitMouseButton::Right => RdpMouseButton::Right,
            WinitMouseButton::Back => RdpMouseButton::X1,
            WinitMouseButton::Forward => RdpMouseButton::X2,
            WinitMouseButton::Other(_) => return,
        };

        match state {
            ElementState::Pressed => {
                // A press only belongs to the session if it landed on the desktop.
                if !over_desktop {
                    return;
                }

                let Some(position) = session
                    .pointer_position
                    .and_then(|position| session.remote_position(position))
                else {
                    return;
                };

                session.buttons_held = session.buttons_held.saturating_add(1);
                // The press carries no coordinates of its own, so place the pointer first.
                session.apply_operations([
                    Operation::MouseMove(position),
                    Operation::MouseButtonPressed(button),
                ]);
            }
            ElementState::Released => {
                // Releases are always forwarded, wherever the pointer ended up: a button the
                // server never hears released stays down for the rest of the session. The
                // input database drops the event if it was not held in the first place.
                session.buttons_held = session.buttons_held.saturating_sub(1);
                session.apply_operations(core::iter::once(Operation::MouseButtonReleased(button)));
            }
        }
    }

    fn on_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        if !self.pointer_over_desktop() {
            return;
        }

        let ppp = f64::from(self.pixels_per_point());

        let Some(session) = self.session.as_mut() else {
            return;
        };

        let (horizontal, vertical) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                (x * WHEEL_UNITS_PER_NOTCH, y * WHEEL_UNITS_PER_NOTCH)
            }
            MouseScrollDelta::PixelDelta(delta) => {
                let scale = f64::from(WHEEL_UNITS_PER_NOTCH / SCROLL_POINTS_PER_NOTCH);
                ((delta.x / ppp * scale) as f32, (delta.y / ppp * scale) as f32)
            }
        };

        let mut operations = SmallVec::<[Operation; 2]>::new();

        // Positive rotation is away from the user in both winit and RDP.
        if vertical.abs() >= 1.0 {
            operations.push(Operation::WheelRotations(WheelRotations {
                is_vertical: true,
                rotation_units: vertical as i16,
            }));
        }

        if horizontal.abs() >= 1.0 {
            operations.push(Operation::WheelRotations(WheelRotations {
                is_vertical: false,
                rotation_units: horizontal as i16,
            }));
        }

        if !operations.is_empty() {
            session.apply_operations(operations);
        }
    }

    fn on_cursor_moved(&mut self, position: winit::dpi::PhysicalPosition<f64>) {
        let ppp = f64::from(self.pixels_per_point());
        let logical = egui::pos2((position.x / ppp) as f32, (position.y / ppp) as f32);

        if let Some(session) = self.session.as_mut() {
            session.pointer_position = Some(logical);
        }

        self.update_island_reveal();

        let over_desktop = self.pointer_over_desktop();

        let Some(session) = self.session.as_mut() else {
            return;
        };

        // Once a drag is under way it keeps going even if the pointer wanders off the desktop,
        // which is what makes a selection that overshoots the window behave sensibly.
        if !over_desktop && session.buttons_held == 0 {
            return;
        }

        if let Some(remote) = session.remote_position(logical) {
            session.apply_operations(core::iter::once(Operation::MouseMove(remote)));
        }
    }

    /// Arms the debounce that eventually negotiates a new desktop size.
    fn schedule_resize(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.resize_deadline = Some(Instant::now() + RESIZE_DEBOUNCE);
        }
    }

    /// Asks the server for a desktop the size of the area we are drawing into.
    ///
    /// The remote size is negotiated in *physical* pixels while the layout above is in logical
    /// points, so the area has to be scaled back up by the display scale; getting this wrong is
    /// what produces a blurry or half-sized desktop on a HiDPI screen. The same scale is
    /// reported to the server as a percentage so that the remote session uses matching DPI.
    fn maybe_send_resize(&mut self) {
        let ppp = f64::from(self.pixels_per_point());
        let window_size = self
            .gl_window
            .as_ref()
            .map(|gl_window| gl_window.window().inner_size());

        let Some(session) = self.session.as_mut() else {
            return;
        };

        let Some(deadline) = session.resize_deadline else {
            return;
        };

        if Instant::now() < deadline {
            return;
        }

        // Hold off while the window is still the size of the connection dialog, or the desktop
        // comes up at dialog dimensions and immediately has to renegotiate.
        if let Some((wanted, since)) = session.pending_window_size {
            let reached = window_size.is_some_and(|size| {
                size.width.abs_diff(wanted.width) <= 8 && size.height.abs_diff(wanted.height) <= 8
            });

            if reached || since.elapsed() >= SESSION_RESIZE_GRACE {
                session.pending_window_size = None;
            } else {
                session.resize_deadline = Some(Instant::now() + RESIZE_DEBOUNCE);
                return;
            }
        }

        let rect = session.desktop_rect;
        if rect.width() < 1.0 || rect.height() < 1.0 {
            // Nothing has been laid out yet; try again after the first frame.
            session.resize_deadline = Some(Instant::now() + RESIZE_DEBOUNCE);
            return;
        }

        session.resize_deadline = None;

        let max = f64::from(u16::MAX);
        let width = (f64::from(rect.width()) * ppp).round().clamp(200.0, max) as u16;
        let height = (f64::from(rect.height()) * ppp).round().clamp(200.0, max) as u16;
        let scale_factor = ((ppp * 100.0).round() as u32).clamp(100, 500);

        if session.last_resize_sent == Some((width, height, scale_factor)) {
            return;
        }
        session.last_resize_sent = Some((width, height, scale_factor));

        tracing::info!(width, height, scale_factor, "requesting desktop resize");

        let _ = session.input_sender.send(RdpInputEvent::Resize {
            width,
            height,
            scale_factor,
            physical_size: None,
        });
    }

    /// Drains everything the session thread has produced since the last wakeup.
    fn drain_session_output(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if self.egui_glow.is_none() {
            // No texture to write into yet. Leave everything queued rather than dropping the
            // activation frame, and leave the wakeup flag set: `resumed` asks for a redraw,
            // and that redraw drains this properly.
            return false;
        }

        self.wakeup_pending.store(false, Ordering::Release);

        let mut repaint = false;
        let mut disconnect: Option<Option<String>> = None;

        loop {
            let Some(session) = self.session.as_mut() else {
                break;
            };

            let Ok(event) = session.output_receiver.try_recv() else {
                break;
            };

            match event {
                RdpOutputEvent::Image {
                    buffer,
                    width,
                    height,
                    region,
                } => {
                    self.update_surface(&buffer, width.get(), height.get(), region);
                    if let Some(session) = self.session.as_mut() {
                        session.stats.record_frame();
                        if !session.connected {
                            session.connected = true;
                            session.status = "Connected".to_owned();
                        }
                    }
                    repaint = true;
                }
                RdpOutputEvent::PointerDefault => {
                    session.pointer_state = PointerState::Default;
                    repaint = true;
                }
                RdpOutputEvent::PointerHidden => {
                    session.pointer_state = PointerState::Hidden;
                    repaint = true;
                }
                RdpOutputEvent::PointerBitmap(pointer) => {
                    self.set_pointer_bitmap(event_loop, &pointer);
                    repaint = true;
                }
                RdpOutputEvent::PointerPosition { .. } => {
                    // The server owns the pointer position; nothing to do locally.
                }
                RdpOutputEvent::ConnectionStats(stats) => {
                    // Into the island's details tooltip rather than onto its face: a row of
                    // live numbers across the top of the desktop is what the space was being
                    // spent on before, and it is only ever glanced at.
                    session.stats.record(&stats);
                    repaint = true;
                }
                RdpOutputEvent::ConnectionFailure(error) => {
                    tracing::error!(?error, "RDP connection failed");
                    disconnect = Some(Some(format!("{error}")));
                    break;
                }
                RdpOutputEvent::Terminated(result) => {
                    let message = match result {
                        Ok(reason) => {
                            tracing::info!(?reason, "RDP session terminated");
                            None
                        }
                        Err(error) => {
                            tracing::error!(?error, "RDP session error");
                            Some(format!("{error}"))
                        }
                    };
                    disconnect = Some(message);
                    break;
                }
            }
        }

        if let Some(message) = disconnect {
            self.disconnect(message);
            repaint = true;
        }

        repaint
    }

    /// Pushes one server update into the surface texture.
    ///
    /// A `region` means only that rectangle changed, and it is uploaded on its own with
    /// `set_partial`; the whole framebuffer is only ever re-uploaded for a full frame or when
    /// the surface changes size.
    fn update_surface(
        &mut self,
        buffer: &[u8],
        width: u16,
        height: u16,
        region: Option<ImageRegion>,
    ) {
        let Some(egui_glow) = self.egui_glow.as_ref() else {
            return;
        };
        let ctx = egui_glow.egui_ctx.clone();

        let Some(session) = self.session.as_mut() else {
            return;
        };

        let (surface_width, surface_height) = (usize::from(width), usize::from(height));
        if surface_width == 0 || surface_height == 0 {
            session.surface = None;
            session.surface_size = (0, 0);
            return;
        }

        let resized = session
            .surface
            .as_ref()
            .is_none_or(|texture| texture.size() != [surface_width, surface_height]);

        if resized {
            // Allocate at the new size before anything can be written into it. The server
            // follows a size change with a full frame, so the black fill is only ever visible
            // if a partial update somehow overtakes it.
            let blank = egui::ColorImage {
                size: [surface_width, surface_height],
                pixels: vec![egui::Color32::BLACK; surface_width * surface_height],
            };
            session.surface = Some(ctx.load_texture("rdp-surface", blank, SURFACE_TEXTURE_OPTIONS));
        }

        session.surface_size = (width, height);

        let Some(texture) = session.surface.as_mut() else {
            return;
        };

        match region {
            None => {
                let expected = surface_width * surface_height * 4;
                if buffer.len() < expected {
                    tracing::warn!(got = buffer.len(), expected, "short full frame, ignoring");
                    return;
                }
                let image = bgra_to_color_image(surface_width, surface_height, buffer);
                texture.set(image, SURFACE_TEXTURE_OPTIONS);
            }
            Some(region) => {
                let x = usize::from(region.x);
                let y = usize::from(region.y);
                let region_width = usize::from(region.width.get());
                let region_height = usize::from(region.height.get());

                if x + region_width > surface_width || y + region_height > surface_height {
                    tracing::warn!(
                        x,
                        y,
                        region_width,
                        region_height,
                        surface_width,
                        surface_height,
                        "damage rectangle outside the surface, ignoring"
                    );
                    return;
                }

                let expected = region_width * region_height * 4;
                if buffer.len() < expected {
                    tracing::warn!(got = buffer.len(), expected, "short region, ignoring");
                    return;
                }

                let image = bgra_to_color_image(region_width, region_height, buffer);
                texture.set_partial([x, y], image, SURFACE_TEXTURE_OPTIONS);
            }
        }
    }

    fn set_pointer_bitmap(
        &mut self,
        event_loop: &ActiveEventLoop,
        pointer: &ironrdp::graphics::pointer::DecodedPointer,
    ) {
        let expected = usize::from(pointer.width) * usize::from(pointer.height) * 4;
        if pointer.width == 0 || pointer.height == 0 || pointer.bitmap_data.len() < expected {
            tracing::warn!(
                width = pointer.width,
                height = pointer.height,
                len = pointer.bitmap_data.len(),
                "unusable pointer bitmap"
            );
            return;
        }

        // `bitmap_data` is already RGBA with premultiplied alpha, which is what winit wants.
        let source = match CustomCursor::from_rgba(
            pointer.bitmap_data[..expected].to_vec(),
            pointer.width,
            pointer.height,
            pointer.hotspot_x.min(pointer.width.saturating_sub(1)),
            pointer.hotspot_y.min(pointer.height.saturating_sub(1)),
        ) {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(%error, "rejected pointer bitmap");
                return;
            }
        };

        let cursor = event_loop.create_custom_cursor(source);
        if let Some(session) = self.session.as_mut() {
            session.pointer_state = PointerState::Bitmap(cursor);
        }
    }

    /// Re-asserts the session pointer.
    ///
    /// egui sets the cursor from its own state every frame through `handle_platform_output`,
    /// so the server's pointer has to be put back afterwards whenever the pointer is over the
    /// desktop rather than over the island.
    fn apply_pointer(&self) {
        let Some(gl_window) = self.gl_window.as_ref() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };

        if !self.pointer_over_desktop() {
            return;
        }

        let window = gl_window.window();

        match &session.pointer_state {
            PointerState::Default => {
                window.set_cursor_visible(true);
                window.set_cursor(Cursor::Icon(CursorIcon::Default));
            }
            PointerState::Hidden => {
                window.set_cursor_visible(false);
            }
            PointerState::Bitmap(cursor) => {
                window.set_cursor_visible(true);
                window.set_cursor(Cursor::Custom(cursor.clone()));
            }
        }
    }

    /// Sizes the dialog window to whichever of its two states is showing.
    fn resize_dialog_window(&self) {
        let Some(gl_window) = self.gl_window.as_ref() else {
            return;
        };

        let (width, height) = if self.dialog.options_open {
            DIALOG_SIZE_OPTIONS
        } else {
            DIALOG_SIZE
        };

        let _ = gl_window
            .window()
            .request_inner_size(winit::dpi::LogicalSize::new(width, height));
    }

    /// Folds the form's fields into the settings, which are what gets written to a `.rdp`.
    fn store_form_in_settings(&mut self) {
        self.settings.server = self.form.server.trim().to_owned();
        self.settings.username = self.form.username.trim().to_owned();
        self.settings.domain = self.form.domain.trim().to_owned();
        self.settings.password = if self.settings.save_password {
            self.form.password.clone()
        } else {
            String::new()
        };
    }

    /// Writes the settings to `path`, or to the default file when there is none.
    fn save_settings(&mut self, path: Option<PathBuf>) {
        self.store_form_in_settings();

        let default_path = RdpSettings::config_dir().map(|dir| dir.join(DEFAULT_RDP_FILE));
        let target = path.or_else(|| default_path.clone());

        let result = match target.as_ref() {
            Some(path) => self.settings.save_to_file(path),
            // No config directory to fall back on; `save_as_default` reports that itself.
            None => self.settings.save_as_default(),
        };

        self.dialog.notice = Some(match (result, target) {
            (Ok(()), Some(path)) => {
                // Remember it, so a later plain Save goes back to the same file.
                self.dialog.file = Some(path.clone());
                format!("Saved to {}", path.display())
            }
            (Ok(()), None) => "Saved".to_owned(),
            (Err(error), _) => format!("Could not save: {error}"),
        });
    }

    /// Loads a `.rdp` into both the settings and the form fields it feeds.
    fn open_settings(&mut self, path: &std::path::Path) {
        match RdpSettings::load_from_file(path) {
            Ok(settings) => {
                self.form.server = settings.server.clone();
                self.form.username = settings.username.clone();
                self.form.domain = settings.domain.clone();
                // A file that was not saved with its password has an empty one; do not leave
                // the previous connection's password sitting in the field.
                self.form.password = if settings.save_password {
                    settings.password.clone()
                } else {
                    String::new()
                };

                self.settings = settings;
                self.dialog.file = Some(path.to_path_buf());
                self.dialog.notice = Some(format!("Opened {}", path.display()));
            }
            Err(error) => {
                self.dialog.notice = Some(format!("Could not open {}: {error}", path.display()));
            }
        }
    }

    /// Starts a session from the form and grows the window to fit it.
    fn connect(&mut self) {
        self.connect_error = None;

        let config = match build_config(&self.form, &self.settings) {
            Ok(config) => config,
            Err(error) => {
                self.connect_error = Some(format!("{error:#}"));
                return;
            }
        };

        if self.form.save {
            self.store_form_in_settings();
            if let Err(error) = self.settings.save_as_default() {
                tracing::warn!(%error, "failed to save settings");
            }
        }

        // The codec grid is read deep inside the decoder, which has no settings to consult.
        crate::gfx::DEBUG_CODEC_OUTLINES
            .store(self.settings.get_show_codec_grid(), Ordering::Relaxed);

        let (input_sender, input_receiver) = RdpInputEvent::create_channel();
        let (output_sender, output_receiver) = mpsc::unbounded_channel();

        let event_sender = SessionEventSender {
            sender: output_sender,
            proxy: Arc::clone(&self.proxy),
            wakeup_pending: Arc::clone(&self.wakeup_pending),
        };

        let session_size = winit::dpi::PhysicalSize::new(
            u32::from(config.connector.desktop_size.width),
            u32::from(config.connector.desktop_size.height),
        );

        spawn_session_thread(config, input_sender.clone(), input_receiver, event_sender);

        // Grow the window to the session and only negotiate a desktop size once it gets there.
        let fullscreen = self.settings.get_resolution() == Resolution::Fullscreen;
        if let Some(gl_window) = self.gl_window.as_ref() {
            let window = gl_window.window();
            if fullscreen {
                // The desktop size then comes from the window itself, through the resize that
                // the first laid-out frame schedules.
                window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            } else {
                let _ = window.request_inner_size(session_size);
            }
            window.set_title(&format!("{} - IronTSC", self.form.server));
        }
        self.fullscreen = fullscreen;

        // mstsc's connection bar names the machine; the domain only shows up in the details.
        let user = if self.form.domain.trim().is_empty() {
            self.form.username.clone()
        } else {
            format!("{}\\{}", self.form.domain.trim(), self.form.username)
        };

        self.session = Some(Session {
            input_sender,
            output_receiver,
            input_database: Database::new(),
            surface: None,
            surface_size: (0, 0),
            desktop_rect: egui::Rect::ZERO,
            pointer_position: None,
            pointer_state: PointerState::Default,
            buttons_held: 0,
            status: "Connecting...".to_owned(),
            connected: false,
            server: self.form.server.clone(),
            user,
            stats: SessionStats::new(),
            resize_deadline: Some(Instant::now() + RESIZE_DEBOUNCE),
            last_resize_sent: None,
            // Fullscreen has no size to wait for: the compositor decides, and the window is
            // already there.
            pending_window_size: (!fullscreen).then_some((session_size, Instant::now())),
            shutting_down: false,
        });
    }

    /// Ends the session and returns to the connection dialog.
    fn disconnect(&mut self, message: Option<String>) {
        // Capture is released before anything else: whatever went wrong, the user must not be
        // left unable to alt-tab out of a window that is no longer showing a desktop.
        if let Some(capture) = self.shortcut_capture.as_mut() {
            capture.release();
            capture.set_enabled(false);
        }

        if let Some(mut session) = self.session.take() {
            session.shutdown();
        }

        self.connect_error = message;
        self.island = Island::default();

        if let Some(gl_window) = self.gl_window.as_ref() {
            let window = gl_window.window();
            let _ = window.set_cursor(Cursor::Icon(CursorIcon::Default));
            window.set_cursor_visible(true);
            window.set_fullscreen(None);
            self.fullscreen = false;
            window.set_title("IronTSC");
        }
        self.resize_dialog_window();
    }

    fn shutdown(&mut self) {
        if self.exiting {
            return;
        }
        self.exiting = true;

        // Unconditional, on every exit path: an inhibitor or keyboard grab that outlives the
        // process would leave the user unable to switch away from their own desktop.
        if let Some(capture) = self.shortcut_capture.as_mut() {
            capture.release();
        }

        if let Some(session) = self.session.as_mut() {
            session.shutdown();
        }
    }

    fn request_redraw(&self) {
        if let Some(gl_window) = self.gl_window.as_ref() {
            gl_window.window().request_redraw();
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(gl_window) = self.gl_window.as_ref() else {
            return;
        };
        let window = gl_window.window();

        if self.fullscreen {
            window.set_fullscreen(None);
            self.fullscreen = false;
        } else {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            self.fullscreen = true;
        }

        // Deliberately not resetting the reveal: the pointer is on the island, having just
        // clicked this, and mstsc leaves its bar up until you move away from the top edge.
        self.island.hover_since = None;
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        // egui and the window have to be moved out for the duration of the frame, because the
        // closure below borrows the rest of `self`. Check first so that a redraw arriving
        // before the window exists cannot take them out and drop them on the floor.
        if self.egui_glow.is_none() || self.gl_window.is_none() || self.gl.is_none() {
            return;
        }

        let (Some(mut egui_glow), Some(gl_window), Some(gl)) = (
            self.egui_glow.take(),
            self.gl_window.take(),
            self.gl.clone(),
        ) else {
            return;
        };

        // The window manager can toggle fullscreen without going through the island, so take
        // the window's word for it rather than a flag we set ourselves.
        self.fullscreen = gl_window.window().fullscreen().is_some();

        self.update_island_reveal();

        let mut actions = FrameActions::default();
        let island_visible = self.island_visible();
        let island_position = self.island_position();

        // Everything the UI closure needs, copied out so it does not borrow `self`.
        let in_session = self.session.is_some();
        let surface = self.session.as_ref().and_then(|s| s.surface.clone());
        // Read from the frame's own context: `pixels_per_point` goes through `self.egui_glow`,
        // which has been moved out for the duration of the frame.
        let scale_percent = (egui_glow.egui_ctx.pixels_per_point() * 100.0)
            .round()
            .max(1.0) as u32;
        let island_view = self.island_view(scale_percent);

        let mut form = self.form.clone();
        let mut settings = self.settings.clone();
        let mut dialog = self.dialog.clone();
        let connect_error = self.connect_error.clone();
        let mut desktop_rect = egui::Rect::ZERO;
        let mut island_size = self.island.size;

        egui_glow.run(gl_window.window(), |ctx| {
            if in_session {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                    .show(ctx, |ui| {
                        let rect = ui.max_rect();
                        desktop_rect = rect;

                        match &surface {
                            Some(texture) => {
                                let uv = egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                );
                                ui.painter()
                                    .image(texture.id(), rect, uv, egui::Color32::WHITE);
                            }
                            None => {
                                ui.centered_and_justified(|ui| {
                                    ui.label("Connecting to the remote desktop...");
                                });
                            }
                        }
                    });

                if island_visible {
                    if let (Some(position), Some(view)) = (island_position, island_view.as_ref()) {
                        island_size = show_island(ctx, position, view, &mut actions);
                    }
                }
            } else {
                show_connect_dialog(
                    ctx,
                    &mut form,
                    &mut settings,
                    &mut dialog,
                    connect_error.as_deref(),
                    &mut actions,
                );
            }
        });

        self.form = form;
        // Only while the dialog owns them: a session's frame leaves both untouched, and
        // writing them back unconditionally would undo an Open that happened mid-session.
        if !in_session {
            self.settings = settings;
            self.dialog = dialog;
        }
        self.island.size = island_size;

        if in_session {
            if let Some(session) = self.session.as_mut() {
                let changed = session.desktop_rect != desktop_rect;
                session.desktop_rect = desktop_rect;
                if changed {
                    session.resize_deadline = Some(Instant::now() + RESIZE_DEBOUNCE);
                }
            }
        }

        {
            use glow::HasContext as _;

            // SAFETY: the context is current on this thread for the lifetime of the window.
            unsafe {
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
        }

        egui_glow.paint(gl_window.window());
        gl_window.swap_buffers();

        self.egui_glow = Some(egui_glow);
        self.gl_window = Some(gl_window);

        self.apply_pointer();
        self.apply_frame_actions(actions, event_loop);
    }

    fn apply_frame_actions(&mut self, actions: FrameActions, event_loop: &ActiveEventLoop) {
        if let Some(dragged) = actions.island_dragged {
            self.island.dragging = true;
            if let Some(session) = self.session.as_ref() {
                let free = (session.desktop_rect.width() - self.island.size.x).max(1.0);
                self.island.position = (self.island.position + dragged / free).clamp(0.0, 1.0);
            }
        } else {
            self.island.dragging = false;
        }

        if actions.toggle_pin {
            self.island.pinned = !self.island.pinned;
        }

        if actions.toggle_capture {
            if let Some(capture) = self.shortcut_capture.as_mut() {
                let enabled = capture.is_enabled();
                capture.set_enabled(!enabled);
            }
        }

        if actions.toggle_fullscreen {
            self.toggle_fullscreen();
        }

        if actions.minimize {
            if let Some(gl_window) = self.gl_window.as_ref() {
                gl_window.window().set_minimized(true);
            }
        }

        if actions.toggle_options {
            self.dialog.options_open = !self.dialog.options_open;
            self.resize_dialog_window();
        }

        if actions.save_settings {
            self.save_settings(self.dialog.file.clone());
        }

        if actions.save_settings_as {
            let start = self
                .dialog
                .file
                .clone()
                .or_else(|| RdpSettings::config_dir().map(|dir| dir.join(DEFAULT_RDP_FILE)));

            let mut picker = rfd::FileDialog::new()
                .set_title("Save RDP File")
                .add_filter("Remote Desktop", &["rdp"])
                .set_file_name(DEFAULT_RDP_FILE);
            if let Some(directory) = start.as_ref().and_then(|path| path.parent()) {
                picker = picker.set_directory(directory);
            }

            if let Some(path) = picker.save_file() {
                self.save_settings(Some(path));
            }
        }

        if actions.open_settings {
            let mut picker = rfd::FileDialog::new()
                .set_title("Open RDP File")
                .add_filter("Remote Desktop", &["rdp"]);
            if let Some(directory) = RdpSettings::config_dir() {
                picker = picker.set_directory(directory);
            }

            if let Some(path) = picker.pick_file() {
                self.open_settings(&path);
            }
        }

        if actions.connect {
            self.connect();
        }

        if actions.disconnect {
            self.disconnect(None);
        }

        if actions.quit {
            self.shutdown();
            event_loop.exit();
        }
    }
}

/// What the UI asked for during one frame, applied once the borrow on `self` has ended.
#[derive(Default)]
struct FrameActions {
    connect: bool,
    disconnect: bool,
    quit: bool,
    toggle_pin: bool,
    toggle_capture: bool,
    toggle_fullscreen: bool,
    minimize: bool,
    island_dragged: Option<f32>,
    toggle_options: bool,
    save_settings: bool,
    save_settings_as: bool,
    open_settings: bool,
}

/// Everything the island shows, owned, so the frame's UI closure borrows nothing else.
struct IslandView {
    server: String,
    user: String,
    status: String,
    connected: bool,
    surface_size: (u16, u16),
    scale_percent: u32,
    stats: StatsSnapshot,
    pinned: bool,
    fullscreen: bool,
    capture_available: bool,
    capture_enabled: bool,
    capture_status: String,
}

/// Draws the floating control island and reports its size for next frame's positioning.
///
/// Deliberately flat: a drop shadow here has to be recomposited against whatever the remote
/// desktop is painting underneath, which glitches as the island fades in and out. A one-pixel
/// light border does the same job of separating it from the picture.
fn show_island(
    ctx: &egui::Context,
    position: egui::Pos2,
    view: &IslandView,
    actions: &mut FrameActions,
) -> egui::Vec2 {
    let response = egui::Area::new(egui::Id::new("irontsc-island"))
        .order(egui::Order::Foreground)
        .fixed_pos(position)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(24, 24, 28, 235))
                .stroke(egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 56),
                ))
                .corner_radius(14.0)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    // The island paints its own dark background, so its contents must not
                    // inherit the ambient theme's text colour: under a light theme that is dark
                    // grey on near-black and unreadable. Pin the foreground here instead.
                    let visuals = &mut ui.style_mut().visuals;
                    let bright = egui::Color32::from_rgb(240, 240, 245);
                    let dim = egui::Color32::from_rgb(176, 176, 184);
                    visuals.override_text_color = Some(bright);
                    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, dim);
                    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, bright);
                    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, bright);
                    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, bright);

                    // Flat until touched, the way the GTK client's `.flat .circular` buttons
                    // were: no plate at rest, so the row reads as text, but a real highlight
                    // under the pointer, without which the buttons feel dead.
                    visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                    visuals.widgets.hovered.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 38);
                    visuals.widgets.active.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 58);
                    for widget in [
                        &mut visuals.widgets.inactive,
                        &mut visuals.widgets.hovered,
                        &mut visuals.widgets.active,
                    ] {
                        widget.bg_stroke = egui::Stroke::NONE;
                        widget.corner_radius = egui::CornerRadius::same(8);
                    }

                    ui.horizontal(|ui| {
                        // An explicit grip, so dragging the island can never be confused with
                        // clicking one of its buttons. Labels here are plain text rather than
                        // symbols: egui's bundled fonts do not cover glyphs like U+2261 or
                        // U+2715, which simply render as nothing.
                        let grip = ui.add(
                            egui::Label::new(egui::RichText::new("::").weak())
                                .sense(egui::Sense::drag()),
                        );
                        if grip.dragged() {
                            actions.island_dragged = Some(grip.drag_delta().x);
                        }
                        if grip.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }

                        ui.separator();

                        // The machine you are on, named the way mstsc's connection bar names
                        // it, and a second place to drag from: mstsc's bar is moved by its
                        // middle, and the grip alone is a very small target.
                        let name = ui.add(
                            egui::Label::new(egui::RichText::new(&view.server).strong())
                                .selectable(false)
                                .sense(egui::Sense::drag()),
                        );
                        if name.dragged() {
                            actions.island_dragged = Some(name.drag_delta().x);
                        }
                        if name.hovered() || name.dragged() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        name.on_hover_ui(|ui| show_connection_details(ui, view));

                        // Only while it means something. Once connected the state lives in the
                        // tooltip with the rest of the numbers.
                        if !view.connected {
                            ui.label(egui::RichText::new(&view.status).small().weak());
                        }

                        ui.separator();

                        let capture_label = if view.capture_enabled {
                            egui::RichText::new("Keys on").strong()
                        } else {
                            egui::RichText::new("Keys off").weak()
                        };
                        let capture_button = ui
                            .add_enabled(view.capture_available, egui::Button::new(capture_label));
                        if capture_button.clicked() {
                            actions.toggle_capture = true;
                        }
                        capture_button.on_hover_text(format!(
                            "Send Alt+Tab, Super and other system shortcuts to the remote \
                             desktop\n{}",
                            view.capture_status,
                        ));

                        let pin_label = if view.pinned {
                            egui::RichText::new("Pinned").strong()
                        } else {
                            egui::RichText::new("Pin").weak()
                        };
                        if ui
                            .add(egui::Button::new(pin_label))
                            .on_hover_text("Keep the controls visible in fullscreen")
                            .clicked()
                        {
                            actions.toggle_pin = true;
                        }

                        ui.separator();

                        // Window controls sit at the right-hand end, in the order mstsc uses:
                        // minimise, restore/maximise, close. The island sizes itself to its
                        // content, so being last in the row is what puts them on the right.
                        if ui
                            .add(egui::Button::new("\u{2013}"))
                            .on_hover_text("Minimise")
                            .clicked()
                        {
                            actions.minimize = true;
                        }

                        // Text rather than a glyph: U+2921/U+2922 are outside egui's bundled
                        // fonts and draw as nothing.
                        let fullscreen_label = if view.fullscreen { "Restore" } else { "Full" };
                        if ui
                            .add(egui::Button::new(fullscreen_label))
                            .on_hover_text("Toggle fullscreen")
                            .clicked()
                        {
                            actions.toggle_fullscreen = true;
                        }

                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("X").color(egui::Color32::LIGHT_RED),
                            ))
                            .on_hover_text("Disconnect")
                            .clicked()
                        {
                            actions.disconnect = true;
                        }
                    });
                });
        });

    response.response.rect.size()
}

/// The connection details, shown when the pointer rests on the island's name.
///
/// The tooltip is its own area and takes the ambient style rather than the island's dark
/// palette, so nothing here overrides colours: doing so is what would put white on white.
fn show_connection_details(ui: &mut egui::Ui, view: &IslandView) {
    let stats = &view.stats;

    egui::Grid::new("irontsc-island-details")
        .num_columns(2)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            let mut row = |name: &str, value: String| {
                ui.label(egui::RichText::new(name).weak());
                ui.label(value);
                ui.end_row();
            };

            row("Server", view.server.clone());
            if !view.user.is_empty() {
                row("User", view.user.clone());
            }
            row(
                "Resolution",
                format!("{}x{}", view.surface_size.0, view.surface_size.1),
            );
            row("Scale", format!("{}%", view.scale_percent));
            row("Status", view.status.clone());
            row("Transport", stats.protocol.clone());
            row(
                "Received",
                format!(
                    "{:.0} KiB/s ({} MiB total)",
                    stats.received_rate,
                    stats.received_total / 1024 / 1024
                ),
            );
            row(
                "Sent",
                format!(
                    "{:.0} KiB/s ({} MiB total)",
                    stats.sent_rate,
                    stats.sent_total / 1024 / 1024
                ),
            );
            row(
                "Response",
                stats
                    .roundtrip_ms
                    .map_or_else(|| "n/a".to_owned(), |rtt| format!("{rtt} ms")),
            );
            row(
                "Frame rate",
                stats
                    .frame_rate
                    .map_or_else(|| "n/a".to_owned(), |fps| format!("{fps:.0} FPS")),
            );
            row("Keys", view.capture_status.clone());
        });
}

/// The connection dialog, in the shape mstsc uses: a small window with the logon fields, and
/// Show Options to grow it into tabs. The tab set is ours -- it names what this client has --
/// but the arrangement, the group boxes and the Save/Save As/Open row are mstsc's.
fn show_connect_dialog(
    ctx: &egui::Context,
    form: &mut ConnectForm,
    settings: &mut RdpSettings,
    dialog: &mut DialogState,
    error: Option<&str>,
    actions: &mut FrameActions,
) {
    let ready = form.is_complete();

    // A bottom panel, so Connect stays on the window's bottom edge whether the options are
    // showing or not, and the pages above it get whatever room is left.
    egui::TopBottomPanel::bottom("irontsc-connect-actions")
        .show_separator_line(false)
        .show(ctx, |ui| {
            ui.add_space(6.0);

            if let Some(error) = error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
                ui.add_space(4.0);
            }
            if let Some(notice) = dialog.notice.as_deref() {
                ui.label(egui::RichText::new(notice).small().weak());
                ui.add_space(4.0);
            }

            ui.horizontal(|ui| {
                // Plain words rather than a chevron: egui's bundled fonts do not cover the
                // triangle glyphs, which would simply draw as nothing.
                let label = if dialog.options_open {
                    "Hide Options"
                } else {
                    "Show Options"
                };
                if ui.button(label).clicked() {
                    actions.toggle_options = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(ready, egui::Button::new("Connect"))
                        .clicked()
                    {
                        actions.connect = true;
                    }
                    if ui.button("Quit").clicked() {
                        actions.quit = true;
                    }
                });
            });

            ui.add_space(8.0);
        });

    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(6.0);
        ui.heading("Remote Desktop Connection");
        ui.add_space(8.0);

        if dialog.options_open {
            ui.horizontal(|ui| {
                for tab in OptionsTab::ALL {
                    ui.selectable_value(&mut dialog.tab, tab, tab.label());
                }
            });
            ui.add_space(8.0);
        }

        let mut submit = false;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Collapsed, the dialog is the General page without the file buttons, which is
                // how mstsc's small window relates to its General tab.
                let page = if dialog.options_open {
                    dialog.tab
                } else {
                    OptionsTab::General
                };

                match page {
                    OptionsTab::General => {
                        submit = show_logon_settings(ui, form, settings);
                        if dialog.options_open {
                            show_connection_settings(ui, dialog, actions);
                        }
                    }
                    OptionsTab::Display => show_display_settings(ui, settings),
                    OptionsTab::Codecs => show_codec_settings(ui, settings),
                    OptionsTab::Network => show_network_settings(ui, settings),
                    OptionsTab::Debug => show_debug_settings(ui, settings),
                }
            });

        if submit && ready {
            actions.connect = true;
        }
    });
}

/// A titled box, standing in for the group boxes mstsc builds its tabs from.
fn settings_group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(2.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    ui.add_space(10.0);
}

/// A checkbox with the explanatory line GTK's settings put under every switch.
fn switch_row(ui: &mut egui::Ui, value: &mut bool, label: &str, description: &str) {
    ui.checkbox(value, label);
    // Hand-indented rather than `Ui::indent`, which draws a vertical rule down the margin and
    // turns a page of these into a ladder.
    ui.horizontal(|ui| {
        ui.add_space(22.0);
        ui.label(egui::RichText::new(description).small().weak());
    });
    ui.add_space(6.0);
}

/// The logon fields. Returns true when Enter was pressed in one of them.
fn show_logon_settings(
    ui: &mut egui::Ui,
    form: &mut ConnectForm,
    settings: &mut RdpSettings,
) -> bool {
    let mut submit = false;

    settings_group(ui, "Logon settings", |ui| {
        ui.label("Enter the name of the remote computer.");
        ui.add_space(6.0);

        egui::Grid::new("irontsc-connect-grid")
            .num_columns(2)
            .spacing([8.0, 8.0])
            .show(ui, |ui| {
                let mut field = |ui: &mut egui::Ui,
                                 label: &str,
                                 value: &mut String,
                                 hint: &str,
                                 password: bool| {
                    ui.label(label);
                    let response = ui.add(
                        egui::TextEdit::singleline(value)
                            .hint_text(hint)
                            .password(password)
                            .desired_width(f32::INFINITY),
                    );
                    submit |=
                        response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                };

                field(
                    ui,
                    "Computer:",
                    &mut form.server,
                    "host or host:port",
                    false,
                );
                field(ui, "User name:", &mut form.username, "", false);
                field(ui, "Password:", &mut form.password, "", true);
                field(ui, "Domain:", &mut form.domain, "optional", false);
            });

        ui.add_space(6.0);
        ui.checkbox(&mut form.save, "Remember these settings")
            .on_hover_text("Write the computer, user name and domain back to the settings file when you connect");
        ui.checkbox(&mut settings.save_password, "Save password")
            .on_hover_text("Stores the password in the .rdp file in plaintext");
    });

    submit
}

/// mstsc's Connection settings group: save this connection to a file, or open a saved one.
fn show_connection_settings(ui: &mut egui::Ui, dialog: &DialogState, actions: &mut FrameActions) {
    settings_group(ui, "Connection settings", |ui| {
        ui.label("Save the current connection settings to an RDP file or open a saved connection.");
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                actions.save_settings = true;
            }
            if ui.button("Save As...").clicked() {
                actions.save_settings_as = true;
            }
            if ui.button("Open...").clicked() {
                actions.open_settings = true;
            }
        });

        ui.add_space(4.0);
        let file = dialog
            .file
            .as_ref()
            .map(|file| file.display().to_string())
            .unwrap_or_else(|| format!("the default {DEFAULT_RDP_FILE}"));
        ui.label(
            egui::RichText::new(format!("Save writes to {file}"))
                .small()
                .weak(),
        );
    });
}

fn show_display_settings(ui: &mut egui::Ui, settings: &mut RdpSettings) {
    settings_group(ui, "Display configuration", |ui| {
        ui.label("Choose the size of your remote desktop:");
        ui.add_space(4.0);

        // A Small-to-Large slider over the fixed list, which is both what mstsc shows and what
        // the GTK client did.
        let mut index = settings.get_resolution().to_index();
        if ui
            .add(egui::Slider::new(&mut index, 0..=4).show_value(false))
            .changed()
        {
            settings.set_resolution(Resolution::from_index(index));
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Small").small().weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new("Large").small().weak());
            });
        });

        ui.add_space(4.0);
        ui.label(Resolution::from_index(index).to_string());
    });

    settings_group(ui, "Colors", |ui| {
        ui.label("Select the color depth:");
        ui.add_space(4.0);

        let mut depth = settings.get_color_depth();
        egui::ComboBox::from_id_salt("irontsc-colors")
            .selected_text(depth.to_string())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for candidate in [
                    ColorDepth::Bpp32,
                    ColorDepth::Bpp24,
                    ColorDepth::Bpp16,
                    ColorDepth::Bpp15,
                ] {
                    ui.selectable_value(&mut depth, candidate, candidate.to_string());
                }
            });
        settings.set_color_depth(depth);
    });

    settings_group(ui, "DPI scaling", |ui| {
        ui.label("Set DPI scaling:");
        ui.add_space(4.0);

        let mut selected = dpi_index_from_value(settings.get_dpi_scaling());
        let label = DPI_SCALE_OPTIONS
            .get(selected as usize)
            .map_or("Current screen", |(_, label)| *label);

        egui::ComboBox::from_id_salt("irontsc-dpi")
            .selected_text(label)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for (index, (_, label)) in DPI_SCALE_OPTIONS.iter().enumerate() {
                    ui.selectable_value(&mut selected, index as u32, *label);
                }
            });
        settings.set_dpi_scaling(dpi_value_from_index(selected));
    });
}

fn show_codec_settings(ui: &mut egui::Ui, settings: &mut RdpSettings) {
    settings_group(ui, "H.264 hardware acceleration", |ui| {
        let mut enabled = settings.get_h264_hw_accel();
        switch_row(
            ui,
            &mut enabled,
            "Enable H.264 hardware acceleration",
            "Use the GPU for H.264 decoding (may not work on all systems)",
        );
        settings.set_h264_hw_accel(enabled);
    });

    settings_group(ui, "Codec options", |ui| {
        ui.label(
            egui::RichText::new(
                "AVC420 uses 4:2:0 chroma subsampling for partial screen updates (dirty \
                 regions). AVC444 uses 4:4:4 full chroma for higher quality full-screen \
                 rendering. AVC444 must be enabled on the server via group policy to be \
                 available.",
            )
            .small()
            .weak(),
        );
        ui.add_space(8.0);

        let mut disable_avc420 = settings.get_disable_avc420();
        switch_row(
            ui,
            &mut disable_avc420,
            "Disable H.264 AVC420",
            "Disable the AVC420 codec (4:2:0 chroma subsampling)",
        );
        settings.set_disable_avc420(disable_avc420);

        let mut disable_avc444 = settings.get_disable_avc444();
        switch_row(
            ui,
            &mut disable_avc444,
            "Disable H.264 AVC444",
            "Disable the AVC444 codec (4:4:4 chroma subsampling, higher quality)",
        );
        settings.set_disable_avc444(disable_avc444);
    });
}

fn show_network_settings(ui: &mut egui::Ui, settings: &mut RdpSettings) {
    settings_group(ui, "UDP transport", |ui| {
        let mut disable_udp = settings.get_disable_udp();
        switch_row(
            ui,
            &mut disable_udp,
            "Disable UDP",
            "Force TCP-only mode (disable UDP multitransport for graphics)",
        );
        settings.set_disable_udp(disable_udp);
    });
}

fn show_debug_settings(ui: &mut egui::Ui, settings: &mut RdpSettings) {
    settings_group(ui, "Visualization", |ui| {
        let mut show_grid = settings.get_show_codec_grid();
        switch_row(
            ui,
            &mut show_grid,
            "Show grid indicating codec in use for cell",
            "Outline each decoded region in the colour of the codec that produced it",
        );
        settings.set_show_codec_grid(show_grid);
    });
}

impl ApplicationHandler<UserEvent> for SessionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gl_window.is_some() {
            return;
        }

        // SAFETY: the returned context is stored in `self` and kept current for its lifetime.
        let gl_window = match unsafe { GlutinWindowContext::new(event_loop, "IronTSC") } {
            Ok(gl_window) => gl_window,
            Err(error) => {
                tracing::error!(error = format!("{error:#}"), "failed to open a window");
                event_loop.exit();
                return;
            }
        };

        let gl = Arc::new(gl_window.load_gl());
        let egui_glow = EguiGlow::new(event_loop, Arc::clone(&gl), None, None, true);

        // Nothing else drives the clock, so egui's repaint requests have to wake the loop.
        let repaint_proxy = Arc::clone(&self.proxy);
        egui_glow.egui_ctx.set_request_repaint_callback(move |info| {
            if let Ok(proxy) = repaint_proxy.lock() {
                let _ = proxy.send_event(UserEvent::Repaint(info.delay));
            }
        });

        self.shortcut_capture = Some(ShortcutCapture::new(gl_window.window()));

        gl_window.window().set_visible(true);

        self.gl_window = Some(gl_window);
        self.gl = Some(gl);
        self.egui_glow = Some(egui_glow);

        // Credentials on the command line mean the dialog has nothing to ask, so go straight
        // to the session and let the window grow before it is ever painted small.
        if core::mem::take(&mut self.pending_autoconnect) {
            self.connect();
        } else {
            // The window is created at the collapsed size; this is what makes the dialog state
            // rather than the constructor decide how big it actually is.
            self.resize_dialog_window();
        }

        self.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match &event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.shutdown();
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.drain_session_output(event_loop);
                self.redraw(event_loop);
                return;
            }
            WindowEvent::Resized(size) => {
                if let Some(gl_window) = self.gl_window.as_ref() {
                    gl_window.resize(*size);
                }
                self.schedule_resize();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.schedule_resize();
            }
            WindowEvent::Focused(focused) => {
                // Capture follows focus, exactly as the GTK client does: taken on the way in,
                // dropped on the way out so the compositor's own shortcuts come back.
                if let Some(capture) = self.shortcut_capture.as_mut() {
                    capture.set_focused(*focused);
                }

                // Both directions: focus can come back with the server still holding a key we
                // never saw released, and this costs nothing when nothing is held.
                if let Some(session) = self.session.as_mut() {
                    let reason = if *focused {
                        "focus returned to the session"
                    } else {
                        "focus left the session"
                    };
                    session.release_all_input(reason);
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event,
                is_synthetic,
                ..
            } => {
                // In a session the keys belong to the remote desktop and egui never sees them:
                // the island has no text entry, and letting egui consume Tab or Space would
                // silently swallow them. On the connection dialog it is the other way round.
                if self.session.is_some() {
                    self.on_keyboard_input(key_event, *is_synthetic);
                    return;
                }
            }
            WindowEvent::ModifiersChanged(_) => {
                if self.session.is_some() {
                    // Modifiers reach the session as ordinary physical keys.
                    return;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_moved(*position);
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(session) = self.session.as_mut() {
                    session.pointer_position = None;
                }
                self.island.hover_since = None;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.on_mouse_button(*button, *state);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_mouse_wheel(*delta);
            }
            _ => {}
        }

        // Everything that was not consumed above still goes to egui so the chrome stays alive.
        let Some(egui_glow) = self.egui_glow.as_mut() else {
            return;
        };
        let Some(gl_window) = self.gl_window.as_ref() else {
            return;
        };

        let response = egui_glow.on_window_event(gl_window.window(), &event);
        if response.repaint {
            gl_window.window().request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::RdpOutput => {
                if self.drain_session_output(event_loop) {
                    self.request_redraw();
                }
            }
            UserEvent::Repaint(delay) => self.repaint_delay = delay,
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.maybe_send_resize();

        if self.repaint_delay.is_zero() {
            self.request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }

        let mut deadline = self
            .session
            .as_ref()
            .and_then(|session| session.resize_deadline);

        if let Some(island_deadline) = self.island_deadline {
            deadline = Some(match deadline {
                Some(existing) => existing.min(island_deadline),
                None => island_deadline,
            });
        }

        if let Some(repaint_at) = Instant::now().checked_add(self.repaint_delay) {
            deadline = Some(match deadline {
                Some(existing) => existing.min(repaint_at),
                None => repaint_at,
            });
        }

        event_loop.set_control_flow(match deadline {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown();
        // Belt and braces on top of `ShortcutCapture`'s own `Drop`.
        self.shortcut_capture = None;
        if let Some(egui_glow) = self.egui_glow.as_mut() {
            egui_glow.destroy();
        }
    }
}

/// Converts a BGRA rectangle from the session into an egui image.
///
/// The framebuffer the session produces is BGRA with an alpha byte that is not always
/// meaningful, so the channels are swapped here and the desktop is forced opaque.
pub fn bgra_to_color_image(width: usize, height: usize, bgra: &[u8]) -> egui::ColorImage {
    let mut pixels = Vec::with_capacity(width * height);

    for pixel in bgra.chunks_exact(4).take(width * height) {
        pixels.push(egui::Color32::from_rgb(pixel[2], pixel[1], pixel[0]));
    }

    pixels.resize(width * height, egui::Color32::BLACK);

    egui::ColorImage {
        size: [width, height],
        pixels,
    }
}

/// Runs a session on its own thread with its own Tokio runtime; the two only ever meet
/// through the channels.
///
/// Generic over the sender so that a shell with more than one session can tag events with
/// whichever one produced them.
pub fn spawn_session_thread<S>(
    config: Config,
    input_sender: mpsc::UnboundedSender<RdpInputEvent>,
    input_receiver: mpsc::UnboundedReceiver<RdpInputEvent>,
    event_sender: S,
) where
    S: RdpEventSender + Clone + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("rdp-session".to_owned())
        .spawn(move || {
            let dvc_pipe_proxy_factory = DvcPipeProxyFactory::new(input_sender.clone());

            let cliprdr_factory: Option<Box<dyn CliprdrBackendFactory + Send>> =
                match config.clipboard_type {
                    ClipboardType::None => None,
                    _ => Some(Box::new(ArboardClipboardFactory::new(input_sender.clone()))),
                };

            let client = RdpClient {
                config,
                event_loop_proxy: event_sender,
                input_event_receiver: input_receiver,
                cliprdr_factory,
                dvc_pipe_proxy_factory,
            };

            match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime.block_on(client.run()),
                Err(error) => tracing::error!(%error, "failed to start the Tokio runtime"),
            }
        });

    if let Err(error) = spawned {
        tracing::error!(%error, "failed to start the session thread");
    }
}

/// Builds the session configuration from the form and the persisted settings.
///
/// This mirrors `create_rdp_config` in the GTK client so that both frontends present the same
/// client fingerprint to the server.
pub fn build_config(form: &ConnectForm, settings: &RdpSettings) -> anyhow::Result<Config> {
    use anyhow::Context as _;
    use ironrdp::connector;
    use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
    use ironrdp::pdu::rdp::client_info::PerformanceFlags;

    let destination = Destination::new(form.server.trim().to_owned())
        .context("invalid destination address")?;

    // Widths that are not a multiple of four upset some servers' encoders.
    let align4 = |value: u16| -> u16 {
        let aligned = value & !0x3;
        if aligned == 0 { 4 } else { aligned }
    };

    let (width, height) = settings
        .get_resolution()
        .to_dimensions()
        .unwrap_or((1920, 1080));

    let connector_config = connector::Config {
        credentials: connector::Credentials::UsernamePassword {
            username: form.username.trim().to_owned(),
            password: form.password.clone(),
        },
        domain: Some(form.domain.trim().to_owned()).filter(|domain| !domain.is_empty()),
        client_name: "IronTSC".to_owned(),
        desktop_size: connector::DesktopSize {
            width: align4(width),
            height: align4(height),
        },
        enable_server_pointer: true,
        pointer_software_rendering: false,
        autologon: true,
        desktop_scale_factor: settings.get_dpi_scaling().unwrap_or(0),
        enable_tls: true,
        enable_credssp: !settings.disable_nla,
        keyboard_type: ironrdp::pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        // Match a modern mstsc build so the server enables the RDPEGFX paths.
        client_build: 18363,
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        platform: MajorPlatformType::UNIX,
        keyboard_layout: 0,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        hardware_id: None,
        bitmap: {
            #[cfg(feature = "h264")]
            {
                match crate::h264_codec_caps::create_bitmap_config_with_h264(
                    false,
                    u32::from(settings.session_bpp),
                    settings.get_disable_avc420(),
                    settings.get_disable_avc444(),
                ) {
                    Ok(config) => Some(config),
                    Err(error) => {
                        tracing::warn!(%error, "failed to build the H.264 bitmap config");
                        None
                    }
                }
            }
            #[cfg(not(feature = "h264"))]
            {
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
        correlation_id: None,
    };

    Ok(Config {
        log_file: None,
        gw: None,
        destination,
        connector: connector_config,
        clipboard_type: ClipboardType::Default,
        rdcleanpath: None,
        dvc_pipe_proxies: Vec::new(),
        h264_hw_accel: settings.get_h264_hw_accel(),
        disable_avc420: settings.get_disable_avc420(),
        disable_avc444: settings.get_disable_avc444(),
        disable_udp: settings.get_disable_udp(),
    })
}

/// Opens the window and runs it until the user quits.
pub fn run(form: ConnectForm, settings: RdpSettings, autoconnect: bool) -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|error| anyhow::anyhow!("failed to create the event loop: {error}"))?;

    let proxy = Arc::new(Mutex::new(event_loop.create_proxy()));
    let wakeup_pending = Arc::new(AtomicBool::new(false));

    let mut app = SessionApp::new(form, settings, proxy, wakeup_pending);

    // Deferred until the window exists: connecting resizes it, so doing it any earlier would
    // flash the dialog first.
    app.pending_autoconnect = autoconnect;

    event_loop
        .run_app(&mut app)
        .map_err(|error| anyhow::anyhow!("event loop failed: {error}"))?;

    Ok(())
}
