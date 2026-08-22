//! The session an agent drives, independent of where the pixels came from.
//!
//! Both MCP modes end up here. Headless mode owns its RDP connection and feeds this from the
//! session thread; in-session mode is handed the framebuffer and input channel of the window
//! the user is already looking at. Everything above -- the tool surface, the screenshots, the
//! chords -- is written against [`AgentSession`] and does not know which of the two it has.
//!
//! The framebuffer mirror is the one part that is not free. The window uploads BGRA straight
//! into a GL texture and keeps no copy on the CPU, so a screenshot needs one kept here; that
//! is why in-session mode only starts mirroring when MCP mode is switched on, and stops when
//! it is switched off.

use std::collections::VecDeque;
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ironrdp::input::{Database, MouseButton, MousePosition, Operation, WheelRotations};
use smallvec::SmallVec;
use tokio::sync::{Notify, mpsc};
use winit::keyboard::KeyCode;

use super::layout::{KeyboardLayout, Keystroke};
use crate::config::{ClipboardType, Config};
use crate::rdp::{ConnectionStats, ImageRegion, RdpEventSender, RdpInputEvent, RdpOutputEvent};

/// How many actions are remembered for the gear menu and the `status` tool.
const ACTION_LOG_LIMIT: usize = 32;

/// Fastpath events per PDU. Well under the protocol's limit, and small enough that a long
/// `type_text` is paced by the transport rather than arriving as one burst the server may
/// coalesce or drop.
const EVENTS_PER_BATCH: usize = 16;

/// The press-and-release sequence for one keystroke, or `None` if any of its keys has no
/// scancode.
///
/// Modifiers are let go before a dead key's space, so that the space is a plain one: AltGr and
/// space together are a non-breaking space on more than one layout.
fn stroke_operations(stroke: Keystroke) -> Option<Vec<Operation>> {
    let scancode_for = crate::egui_scancode::scancode_for;

    let key = scancode_for(stroke.key)?;
    let mut modifiers = Vec::with_capacity(2);
    if stroke.shift {
        modifiers.push(scancode_for(KeyCode::ShiftLeft)?);
    }
    if stroke.altgr {
        modifiers.push(scancode_for(KeyCode::AltRight)?);
    }

    let mut operations = Vec::with_capacity(modifiers.len() * 2 + 4);
    for scancode in &modifiers {
        operations.push(Operation::KeyPressed(*scancode));
    }
    operations.push(Operation::KeyPressed(key));
    operations.push(Operation::KeyReleased(key));
    for scancode in modifiers.iter().rev() {
        operations.push(Operation::KeyReleased(*scancode));
    }

    if stroke.dead {
        let space = scancode_for(KeyCode::Space)?;
        operations.push(Operation::KeyPressed(space));
        operations.push(Operation::KeyReleased(space));
    }

    Some(operations)
}

/// What went wrong, in terms an agent can act on.
#[derive(Debug, Clone)]
pub enum AgentError {
    /// No session yet, or it has gone away.
    NotConnected(String),
    /// The request itself did not make sense.
    BadRequest(String),
    /// The session ended while we were waiting for it.
    Terminated(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected(message) => write!(formatter, "not connected: {message}"),
            Self::BadRequest(message) => write!(formatter, "{message}"),
            Self::Terminated(message) => write!(formatter, "session ended: {message}"),
        }
    }
}

impl std::error::Error for AgentError {}

type AgentResult<T> = Result<T, AgentError>;

/// The desktop as the agent sees it: a BGRA mirror plus the session's liveness.
#[derive(Debug)]
pub struct FrameState {
    /// BGRA, `width * height * 4` bytes, or empty before the first frame.
    pub bgra: Vec<u8>,
    pub width: u16,
    pub height: u16,
    /// Bumped on every frame, so a caller can tell "nothing has changed" from "not asked yet".
    pub generation: u64,
    pub last_frame_at: Option<Instant>,
    pub connected: bool,
    pub terminated: Option<String>,
    pub error: Option<String>,
    pub pointer: Option<(u16, u16)>,
    pub stats: Option<ConnectionStats>,
}

impl FrameState {
    fn new() -> Self {
        Self {
            bgra: Vec::new(),
            width: 0,
            height: 0,
            generation: 0,
            last_frame_at: None,
            connected: false,
            terminated: None,
            error: None,
            pointer: None,
            stats: None,
        }
    }
}

/// The framebuffer mirror, shared between whoever produces frames and whoever reads them.
#[derive(Debug)]
pub struct SharedFrame {
    state: Mutex<FrameState>,
    notify: Notify,
}

impl Default for SharedFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedFrame {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FrameState::new()),
            notify: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FrameState> {
        // A poisoned lock only means some other thread panicked mid-update; the pixels are
        // still structurally fine and refusing to serve them helps nobody.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Copies a full frame or a damage rectangle into the mirror.
    ///
    /// This is called from the window's paint path in in-session mode, so it does the least
    /// work it can: a resize reallocates, everything else is a row-wise copy of the damaged
    /// rectangle only.
    pub fn apply_image(
        &self,
        buffer: &[u8],
        width: NonZeroU16,
        height: NonZeroU16,
        region: Option<ImageRegion>,
    ) {
        let surface_width = usize::from(width.get());
        let surface_height = usize::from(height.get());
        let mut state = self.lock();

        if state.width != width.get() || state.height != height.get() {
            state.bgra = vec![0; surface_width * surface_height * 4];
            state.width = width.get();
            state.height = height.get();
        }

        match region {
            None => {
                let expected = surface_width * surface_height * 4;
                if buffer.len() < expected {
                    tracing::warn!(
                        got = buffer.len(),
                        expected,
                        "short full frame, not mirroring"
                    );
                    return;
                }
                state.bgra[..expected].copy_from_slice(&buffer[..expected]);
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
                        "damage rectangle outside the surface, not mirroring"
                    );
                    return;
                }
                if buffer.len() < region_width * region_height * 4 {
                    tracing::warn!("short damage rectangle, not mirroring");
                    return;
                }

                for row in 0..region_height {
                    let source = row * region_width * 4;
                    let target = ((y + row) * surface_width + x) * 4;
                    state.bgra[target..target + region_width * 4]
                        .copy_from_slice(&buffer[source..source + region_width * 4]);
                }
            }
        }

        state.connected = true;
        state.generation += 1;
        state.last_frame_at = Some(Instant::now());
        drop(state);
        self.notify.notify_waiters();
    }

    pub fn set_pointer(&self, x: u16, y: u16) {
        self.lock().pointer = Some((x, y));
    }

    pub fn set_stats(&self, stats: ConnectionStats) {
        self.lock().stats = Some(stats);
    }

    pub fn set_error(&self, message: String) {
        let mut state = self.lock();
        state.error = Some(message);
        state.connected = false;
        drop(state);
        self.notify.notify_waiters();
    }

    pub fn set_terminated(&self, message: String) {
        let mut state = self.lock();
        state.terminated = Some(message);
        state.connected = false;
        drop(state);
        self.notify.notify_waiters();
    }

    /// A copy of the current desktop, or `None` before the first frame.
    pub fn snapshot(&self) -> Option<(Vec<u8>, u16, u16, u64)> {
        let state = self.lock();
        if state.bgra.is_empty() || state.width == 0 || state.height == 0 {
            return None;
        }
        Some((
            state.bgra.clone(),
            state.width,
            state.height,
            state.generation,
        ))
    }

    /// Size, liveness and the last stats, without copying the pixels.
    pub fn describe(&self) -> FrameSummary {
        let state = self.lock();
        FrameSummary {
            width: state.width,
            height: state.height,
            generation: state.generation,
            connected: state.connected,
            terminated: state.terminated.clone(),
            error: state.error.clone(),
            pointer: state.pointer,
            transport: state
                .stats
                .as_ref()
                .map(|stats| stats.transport_protocol.clone()),
            roundtrip_time_ms: state.stats.as_ref().and_then(|stats| stats.roundtrip_time_ms),
        }
    }
}

/// The cheap half of [`SharedFrame`], for status without a framebuffer copy.
#[derive(Debug, Clone)]
pub struct FrameSummary {
    pub width: u16,
    pub height: u16,
    pub generation: u64,
    pub connected: bool,
    pub terminated: Option<String>,
    pub error: Option<String>,
    pub pointer: Option<(u16, u16)>,
    pub transport: Option<String>,
    pub roundtrip_time_ms: Option<u32>,
}

/// Forwards session output into a [`SharedFrame`]. Headless mode's sink.
#[derive(Clone)]
struct FrameSink {
    frame: Arc<SharedFrame>,
}

impl RdpEventSender for FrameSink {
    fn send_event(&self, event: RdpOutputEvent) -> Result<(), ()> {
        match event {
            RdpOutputEvent::Image {
                buffer,
                width,
                height,
                region,
            } => self.frame.apply_image(&buffer, width, height, region),
            RdpOutputEvent::PointerPosition { x, y } => self.frame.set_pointer(x, y),
            RdpOutputEvent::ConnectionStats(stats) => self.frame.set_stats(stats),
            RdpOutputEvent::ConnectionFailure(error) => self.frame.set_error(format!("{error}")),
            RdpOutputEvent::Terminated(result) => {
                let message = match result {
                    Ok(reason) => format!("{reason:?}"),
                    Err(error) => format!("{error}"),
                };
                self.frame.set_terminated(message);
            }
            // The pointer shape is the window's business; the agent works in screen coordinates.
            RdpOutputEvent::PointerDefault
            | RdpOutputEvent::PointerHidden
            | RdpOutputEvent::PointerBitmap(_) => {}
        }
        Ok(())
    }
}

/// One thing the agent did, for the gear menu and the `status` tool.
#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub at: Instant,
    pub summary: String,
}

/// A desktop an agent can look at and drive.
pub struct AgentSession {
    frame: Arc<SharedFrame>,
    input: mpsc::UnboundedSender<RdpInputEvent>,
    /// Key and button state for the agent's own input, kept apart from the window's so that
    /// the two cannot corrupt each other's idea of what is held down.
    database: Mutex<Database>,
    actions: Mutex<VecDeque<ActionRecord>>,
    /// Where the agent last put the pointer. Tracked here rather than read back from the
    /// server, which only reports the position when it moves the pointer itself.
    pointer: Mutex<Option<(u16, u16)>>,
    /// True when this session is ours to close.
    owns_session: bool,
    /// The layout the server reads scancodes against, which is what `type_text` needs to know
    /// to put a character on the right key.
    layout: KeyboardLayout,
}

impl AgentSession {
    /// Opens a session of its own, with no window. Headless mode.
    pub fn spawn_headless(mut config: Config, layout: KeyboardLayout) -> Arc<Self> {
        // Nothing here has a display to paste into, and the clipboard backend would try to
        // reach one.
        config.clipboard_type = ClipboardType::None;

        let frame = Arc::new(SharedFrame::new());
        let (input_sender, input_receiver) = RdpInputEvent::create_channel();

        crate::egui_app::spawn_session_thread(
            config,
            input_sender.clone(),
            input_receiver,
            FrameSink {
                frame: Arc::clone(&frame),
            },
        );

        Arc::new(Self::new(frame, input_sender, true, layout))
    }

    /// Joins a session that a window already owns. In-session mode.
    pub fn attach(
        frame: Arc<SharedFrame>,
        input: mpsc::UnboundedSender<RdpInputEvent>,
        layout: KeyboardLayout,
    ) -> Arc<Self> {
        Arc::new(Self::new(frame, input, false, layout))
    }

    fn new(
        frame: Arc<SharedFrame>,
        input: mpsc::UnboundedSender<RdpInputEvent>,
        owns_session: bool,
        layout: KeyboardLayout,
    ) -> Self {
        Self {
            frame,
            input,
            database: Mutex::new(Database::new()),
            actions: Mutex::new(VecDeque::with_capacity(ACTION_LOG_LIMIT)),
            pointer: Mutex::new(None),
            owns_session,
            layout,
        }
    }

    pub fn frame(&self) -> &Arc<SharedFrame> {
        &self.frame
    }

    pub fn owns_session(&self) -> bool {
        self.owns_session
    }

    /// The last few things the agent did, newest last.
    pub fn recent_actions(&self) -> Vec<ActionRecord> {
        self.actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn record(&self, summary: impl Into<String>) {
        let mut actions = self
            .actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if actions.len() == ACTION_LOG_LIMIT {
            actions.pop_front();
        }
        actions.push_back(ActionRecord {
            at: Instant::now(),
            summary: summary.into(),
        });
    }

    /// Waits for the first frame, which is the first moment anything can be clicked.
    pub async fn wait_until_ready(&self, timeout: Duration) -> AgentResult<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.frame.notify.notified();
            tokio::pin!(notified);

            {
                let summary = self.frame.describe();
                if let Some(message) = summary.terminated {
                    return Err(AgentError::Terminated(message));
                }
                if let Some(message) = summary.error {
                    return Err(AgentError::NotConnected(message));
                }
                if summary.connected && summary.generation > 0 {
                    return Ok(());
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AgentError::NotConnected(
                    "timed out waiting for the first frame".to_owned(),
                ));
            }
            let _ = tokio::time::timeout(remaining, notified).await;
        }
    }

    /// Waits until no new frame has arrived for `quiet`, so a screenshot catches the screen
    /// after it has finished reacting rather than mid-repaint.
    ///
    /// Returns whether it actually settled; on timeout the caller still gets a usable, if
    /// possibly mid-animation, framebuffer.
    pub async fn wait_until_still(&self, quiet: Duration, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.frame.notify.notified();
            tokio::pin!(notified);

            let since_last_frame = {
                let state = self.frame.lock();
                state
                    .last_frame_at
                    .map(|at| at.elapsed())
                    .unwrap_or(Duration::MAX)
            };

            if since_last_frame >= quiet {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }

            let wait = quiet
                .saturating_sub(since_last_frame)
                .min(deadline.saturating_duration_since(Instant::now()));
            let _ = tokio::time::timeout(wait, notified).await;
        }
    }

    /// Turns operations into fastpath PDUs and sends them.
    fn apply(&self, operations: impl IntoIterator<Item = Operation>) -> AgentResult<()> {
        let events: SmallVec<[_; 2]> = {
            let mut database = self
                .database
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            database.apply(operations)
        };

        for batch in events.chunks(EVENTS_PER_BATCH) {
            self.input
                .send(RdpInputEvent::FastPath(SmallVec::from_slice(batch)))
                .map_err(|_| {
                    AgentError::NotConnected("the session is no longer accepting input".to_owned())
                })?;
        }
        Ok(())
    }

    /// Checks a point is on the desktop before an agent's arithmetic sends the pointer off it.
    /// Where the pointer is, as far as anyone here knows.
    ///
    /// The agent's own last move wins; failing that, the last position the *server* moved the
    /// pointer to, which is all that is available before the agent has touched it.
    pub fn pointer_position(&self) -> Option<(u16, u16)> {
        let ours = *self
            .pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ours.or_else(|| self.frame.describe().pointer)
    }

    fn remember_pointer(&self, x: u16, y: u16) {
        *self
            .pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((x, y));
    }

    fn check_bounds(&self, x: u16, y: u16) -> AgentResult<()> {
        let summary = self.frame.describe();
        if summary.width == 0 || summary.height == 0 {
            return Err(AgentError::NotConnected(
                "no frame has arrived yet, so the desktop size is unknown".to_owned(),
            ));
        }
        if x >= summary.width || y >= summary.height {
            return Err(AgentError::BadRequest(format!(
                "({x}, {y}) is outside the {}x{} desktop",
                summary.width, summary.height
            )));
        }
        Ok(())
    }

    pub fn move_mouse(&self, x: u16, y: u16) -> AgentResult<()> {
        self.check_bounds(x, y)?;
        self.apply([Operation::MouseMove(MousePosition { x, y })])?;
        self.remember_pointer(x, y);
        self.record(format!("move to ({x}, {y})"));
        Ok(())
    }

    /// Moves to the point and clicks it `count` times.
    pub fn click(&self, button: MouseButton, x: u16, y: u16, count: u8) -> AgentResult<()> {
        self.check_bounds(x, y)?;
        if count == 0 {
            return Err(AgentError::BadRequest(
                "a click count of zero does nothing".to_owned(),
            ));
        }

        let mut operations = vec![Operation::MouseMove(MousePosition { x, y })];
        for _ in 0..count {
            operations.push(Operation::MouseButtonPressed(button));
            operations.push(Operation::MouseButtonReleased(button));
        }
        self.apply(operations)?;
        self.remember_pointer(x, y);
        self.record(format!("{button:?} click x{count} at ({x}, {y})"));
        Ok(())
    }

    /// Presses a button and leaves it down, for a drag.
    pub fn mouse_down(&self, button: MouseButton, x: u16, y: u16) -> AgentResult<()> {
        self.check_bounds(x, y)?;
        self.apply([
            Operation::MouseMove(MousePosition { x, y }),
            Operation::MouseButtonPressed(button),
        ])?;
        self.remember_pointer(x, y);
        self.record(format!("{button:?} down at ({x}, {y})"));
        Ok(())
    }

    pub fn mouse_up(&self, button: MouseButton, x: u16, y: u16) -> AgentResult<()> {
        self.check_bounds(x, y)?;
        self.apply([
            Operation::MouseMove(MousePosition { x, y }),
            Operation::MouseButtonReleased(button),
        ])?;
        self.remember_pointer(x, y);
        self.record(format!("{button:?} up at ({x}, {y})"));
        Ok(())
    }

    /// Scrolls by wheel notches; positive `vertical` scrolls up, positive `horizontal` right.
    pub fn scroll(&self, vertical: i16, horizontal: i16) -> AgentResult<()> {
        // One notch is 120 units, as everywhere else in Windows.
        const UNITS_PER_NOTCH: i16 = 120;

        let mut operations = Vec::new();
        if vertical != 0 {
            operations.push(Operation::WheelRotations(WheelRotations {
                is_vertical: true,
                rotation_units: vertical.saturating_mul(UNITS_PER_NOTCH),
            }));
        }
        if horizontal != 0 {
            operations.push(Operation::WheelRotations(WheelRotations {
                is_vertical: false,
                rotation_units: horizontal.saturating_mul(UNITS_PER_NOTCH),
            }));
        }
        if operations.is_empty() {
            return Err(AgentError::BadRequest(
                "a scroll of zero in both axes does nothing".to_owned(),
            ));
        }

        self.apply(operations)?;
        self.record(format!("scroll {vertical} vertical, {horizontal} horizontal"));
        Ok(())
    }

    /// Types text, a character at a time, as the keys that produce it.
    ///
    /// Characters go out as scancodes wherever the layout has a key for them, because that is
    /// the only kind of keyboard event the Windows console reads: a Unicode event types
    /// nothing at all into PowerShell, silently. Anything the layout cannot produce -- an
    /// accent, an emoji, a character from another script -- still goes as Unicode, which works
    /// everywhere but there.
    ///
    /// Newlines and tabs are sent as the keys of those names: a Unicode `\n` is not what an
    /// edit control is waiting for.
    pub fn type_text(&self, text: &str) -> AgentResult<()> {
        if text.is_empty() {
            return Err(AgentError::BadRequest("nothing to type".to_owned()));
        }

        let mut as_unicode = 0usize;
        for character in text.chars() {
            let stroke = match character {
                '\n' | '\r' => Some(Keystroke::plain(KeyCode::Enter)),
                '\t' => Some(Keystroke::plain(KeyCode::Tab)),
                _ => self.layout.keystroke(character),
            };

            match stroke.and_then(stroke_operations) {
                Some(operations) => self.apply(operations)?,
                None => {
                    as_unicode += 1;
                    self.apply([
                        Operation::UnicodeKeyPressed(character),
                        Operation::UnicodeKeyReleased(character),
                    ])?;
                }
            }
        }

        let preview: String = text.chars().take(40).collect();
        self.record(format!(
            "type {:?}{}{}",
            preview,
            if text.chars().count() > 40 { "..." } else { "" },
            // Worth saying: these are the characters a console would have dropped.
            match as_unicode {
                0 => String::new(),
                count => format!(" ({count} sent as Unicode)"),
            }
        ));
        Ok(())
    }

    /// Presses a chord such as `"ctrl+alt+delete"`, then releases it.
    pub fn press_chord(&self, chord: &str) -> AgentResult<()> {
        let parsed = super::keys::parse_chord(chord).map_err(AgentError::BadRequest)?;

        let modifiers = parsed.modifier_scancodes();
        if modifiers.len() != parsed.modifiers.len() {
            return Err(AgentError::BadRequest(format!(
                "`{chord}` uses a modifier with no RDP scancode"
            )));
        }
        let key = parsed.key_scancode().ok_or_else(|| {
            AgentError::BadRequest(format!("`{chord}` uses a key with no RDP scancode"))
        })?;

        let mut operations = Vec::with_capacity(modifiers.len() * 2 + 2);
        for scancode in &modifiers {
            operations.push(Operation::KeyPressed(*scancode));
        }
        operations.push(Operation::KeyPressed(key));
        operations.push(Operation::KeyReleased(key));
        for scancode in modifiers.iter().rev() {
            operations.push(Operation::KeyReleased(*scancode));
        }

        self.apply(operations)?;
        self.record(format!("key {chord}"));
        Ok(())
    }

    /// Lets go of everything the agent is holding down.
    ///
    /// Called when MCP mode is switched off, so that a half-finished chord cannot leave the
    /// user's session with a stuck Ctrl.
    pub fn release_all(&self) {
        let events = {
            let mut database = self
                .database
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            database.release_all()
        };
        if events.is_empty() {
            return;
        }
        for batch in events.chunks(EVENTS_PER_BATCH) {
            let _ = self
                .input
                .send(RdpInputEvent::FastPath(SmallVec::from_slice(batch)));
        }
        self.record("released all keys and buttons");
    }

    /// Closes the session, if it is ours to close.
    pub fn disconnect(&self) -> AgentResult<()> {
        if !self.owns_session {
            return Err(AgentError::BadRequest(
                "this session belongs to the window; disconnect it there".to_owned(),
            ));
        }
        self.release_all();
        self.input
            .send(RdpInputEvent::Close)
            .map_err(|_| AgentError::NotConnected("the session has already ended".to_owned()))?;
        self.record("disconnect");
        Ok(())
    }
}
