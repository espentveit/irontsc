//! Capturing the system's own keyboard shortcuts for the remote desktop.
//!
//! Without this, Alt+Tab and the Super key are eaten by the local compositor and never reach
//! the remote session, which makes the remote desktop unusable for anything that relies on
//! them. The GTK client solves it with `GdkToplevel::inhibit_system_shortcuts`; winit has no
//! equivalent, so the display server is addressed directly through the raw handles winit
//! already hands out.
//!
//! Everything here is best-effort by design. A compositor that does not implement the
//! protocol, a display server we have no backend for, or any failure along the way means the
//! feature is unavailable -- logged once, and the session carries on unchanged. A client that
//! refuses to run because it cannot grab Alt+Tab would be worse than one that cannot grab it.
//!
//! The release discipline matters more than the capture: an inhibitor or grab that outlives
//! the window leaves the user unable to switch away from it. Capture is therefore dropped on
//! focus loss, on disconnect, on exit, and again in `Drop` as a last resort.

use winit::window::Window;

/// One display server's way of taking the keyboard.
///
/// Implementations are picked at runtime from the raw handle winit reports, not at compile
/// time, because a single Linux binary can meet either Wayland or X11.
trait CaptureBackend {
    /// What this backend is, for the log line and the toggle's tooltip.
    fn describe(&self) -> &'static str;

    /// Starts capturing. An error means the feature is unavailable; the caller gives up on it
    /// rather than retrying.
    fn engage(&mut self) -> anyhow::Result<()>;

    /// Stops capturing. Must be a no-op when not engaged, and must not fail: it runs on paths
    /// that cannot handle an error, including `Drop`.
    fn disengage(&mut self);
}

/// The fallback for display servers with no implementation here.
struct UnsupportedCapture {
    reason: &'static str,
}

impl CaptureBackend for UnsupportedCapture {
    fn describe(&self) -> &'static str {
        self.reason
    }

    fn engage(&mut self) -> anyhow::Result<()> {
        anyhow::bail!("{}", self.reason)
    }

    fn disengage(&mut self) {}
}

/// Whether system shortcuts currently go to the remote desktop.
///
/// Capture is the conjunction of three things: the user has asked for it, the window has
/// keyboard focus, and the backend has not already failed. Anything that changes one of those
/// goes through here, so there is a single place where the inhibitor is created and destroyed.
pub struct ShortcutCapture {
    backend: Box<dyn CaptureBackend>,
    /// The user's toggle. Off by default, matching the GTK client.
    enabled: bool,
    focused: bool,
    engaged: bool,
    /// Cleared the first time the backend refuses, so a failure is reported once and never
    /// retried in a loop.
    available: bool,
    status: String,
}

impl ShortcutCapture {
    /// Picks a backend for the window's display server.
    ///
    /// Never fails: an unusable display server simply produces a backend that reports itself
    /// as unavailable.
    pub fn new(window: &Window) -> Self {
        let (backend, available) = match select_backend(window) {
            Ok(backend) => {
                tracing::info!(backend = backend.describe(), "keyboard capture available");
                (backend, true)
            }
            Err(reason) => {
                tracing::info!(%reason, "keyboard capture unavailable");
                let reason: &'static str = Box::leak(reason.into_boxed_str());
                (
                    Box::new(UnsupportedCapture { reason }) as Box<dyn CaptureBackend>,
                    false,
                )
            }
        };

        let status = backend.describe().to_owned();

        Self {
            backend,
            enabled: false,
            focused: false,
            engaged: false,
            available,
            status,
        }
    }

    /// True when a backend was found. The toggle is greyed out otherwise.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// True when the user has asked for capture, whether or not it is engaged right now.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// True when system shortcuts are actually being taken at this moment.
    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    /// What to show in the toggle's tooltip: the backend, or why there is none.
    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.refresh();
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.refresh();
        }
    }

    /// Drops capture unconditionally, whatever the state says.
    ///
    /// Used on the paths where being wrong is expensive -- disconnect and exit -- so that a
    /// bug in the state tracking cannot leave the compositor's shortcuts inhibited.
    pub fn release(&mut self) {
        self.backend.disengage();
        self.engaged = false;
    }

    fn refresh(&mut self) {
        let wanted = self.enabled && self.focused && self.available;

        if wanted == self.engaged {
            return;
        }

        if wanted {
            match self.backend.engage() {
                Ok(()) => {
                    self.engaged = true;
                    tracing::debug!(backend = self.backend.describe(), "capturing shortcuts");
                }
                Err(error) => {
                    // One failure is enough: mark it unavailable rather than trying again on
                    // every focus change.
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "keyboard capture failed, disabling it"
                    );
                    self.status = format!("unavailable: {error}");
                    self.available = false;
                    self.enabled = false;
                    self.engaged = false;
                }
            }
        } else {
            self.backend.disengage();
            self.engaged = false;
        }
    }
}

impl Drop for ShortcutCapture {
    fn drop(&mut self) {
        self.backend.disengage();
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn select_backend(window: &Window) -> Result<Box<dyn CaptureBackend>, String> {
    use winit::raw_window_handle::{
        HasDisplayHandle as _, HasWindowHandle as _, RawDisplayHandle, RawWindowHandle,
    };

    let window_handle = window
        .window_handle()
        .map_err(|error| format!("no window handle: {error}"))?
        .as_raw();
    let display_handle = window
        .display_handle()
        .map_err(|error| format!("no display handle: {error}"))?
        .as_raw();

    match (window_handle, display_handle) {
        (RawWindowHandle::Wayland(window), RawDisplayHandle::Wayland(display)) => {
            // SAFETY: winit owns both, and this window outlives the backend we build on them.
            let backend = unsafe { wayland::WaylandCapture::new(display.display, window.surface) }
                .map_err(|error| format!("Wayland: {error:#}"))?;
            Ok(Box::new(backend))
        }
        (RawWindowHandle::Xlib(window), RawDisplayHandle::Xlib(display)) => {
            let display = display
                .display
                .ok_or_else(|| "X11: winit reported no display pointer".to_owned())?;
            // SAFETY: as above; the display and window belong to winit and outlive this.
            let backend = unsafe { x11::X11Capture::new(display, window.window) }
                .map_err(|error| format!("X11: {error:#}"))?;
            Ok(Box::new(backend))
        }
        (RawWindowHandle::Xcb(_), _) => {
            Err("X11 through XCB has no keyboard capture backend".to_owned())
        }
        _ => Err("this display server has no keyboard capture backend".to_owned()),
    }
}

#[cfg(not(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
)))]
fn select_backend(_window: &Window) -> Result<Box<dyn CaptureBackend>, String> {
    // The seam is deliberately left empty here: adding a platform means writing one
    // `CaptureBackend` and returning it from this function, not restructuring anything.
    Err("this platform has no keyboard capture backend".to_owned())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod wayland {
    //! `zwp_keyboard_shortcuts_inhibit_manager_v1`, which is what GTK uses underneath.
    //!
    //! winit's connection is reused rather than a second one opened, because the inhibitor has
    //! to name winit's own `wl_surface` and a surface is only meaningful on the connection that
    //! created it. libwayland supports several event queues per display for exactly this, so a
    //! private queue is created on the foreign display and only our own objects are dispatched
    //! on it; winit's queue is never touched and none of its events are consumed.

    use core::ffi::c_void;
    use core::ptr::NonNull;

    use anyhow::Context as _;
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{wl_registry::WlRegistry, wl_seat::WlSeat, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy as _, QueueHandle};
    use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
        zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    };

    use super::CaptureBackend;

    /// Nothing here needs to react to an event; the state exists only because the dispatch
    /// machinery is typed on it.
    struct State;

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    wayland_client::delegate_noop!(State: ignore WlSeat);
    wayland_client::delegate_noop!(State: ZwpKeyboardShortcutsInhibitManagerV1);
    wayland_client::delegate_noop!(State: ignore ZwpKeyboardShortcutsInhibitorV1);

    pub(super) struct WaylandCapture {
        connection: Connection,
        queue: EventQueue<State>,
        manager: ZwpKeyboardShortcutsInhibitManagerV1,
        seat: WlSeat,
        surface: WlSurface,
        inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    }

    impl WaylandCapture {
        /// # Safety
        ///
        /// `display` must be winit's live `wl_display` and `surface` its live `wl_surface`;
        /// both must outlive the returned value.
        pub(super) unsafe fn new(
            display: NonNull<c_void>,
            surface: NonNull<c_void>,
        ) -> anyhow::Result<Self> {
            // Wraps the existing display without taking ownership of it: dropping this backend
            // destroys only the private event queue, never winit's connection.
            // SAFETY: guaranteed by this function's contract.
            let backend =
                unsafe { wayland_backend::client::Backend::from_foreign_display(display.as_ptr().cast()) };
            let connection = Connection::from_backend(backend);

            // One roundtrip, at startup, to learn what the compositor offers. It dispatches
            // only this queue; anything belonging to winit's queue is left there for winit.
            let (globals, queue) = registry_queue_init::<State>(&connection)
                .context("failed to enumerate Wayland globals")?;
            let qh = queue.handle();

            let manager: ZwpKeyboardShortcutsInhibitManagerV1 = globals
                .bind(&qh, 1..=1, ())
                .context("the compositor does not offer zwp_keyboard_shortcuts_inhibit_manager_v1")?;

            // Version 1 is all that is needed: the seat is only ever passed as an argument.
            let seat: WlSeat = globals
                .bind(&qh, 1..=1, ())
                .context("the compositor offers no wl_seat")?;

            // winit's surface, adopted by pointer. The inhibitor has to name that exact
            // surface, so there is no alternative to reaching into winit's objects here.
            // SAFETY: guaranteed by this function's contract.
            let surface_id = unsafe {
                wayland_client::backend::ObjectId::from_ptr(
                    WlSurface::interface(),
                    surface.as_ptr().cast(),
                )
            }
            .context("winit's wl_surface pointer was not a wl_surface")?;
            let surface = WlSurface::from_id(&connection, surface_id)
                .context("failed to adopt winit's wl_surface")?;

            Ok(Self {
                connection,
                queue,
                manager,
                seat,
                surface,
                inhibitor: None,
            })
        }
    }

    impl CaptureBackend for WaylandCapture {
        fn describe(&self) -> &'static str {
            "Wayland keyboard-shortcuts-inhibit"
        }

        fn engage(&mut self) -> anyhow::Result<()> {
            if self.inhibitor.is_some() {
                return Ok(());
            }

            // Creating a second inhibitor for the same surface and seat is a protocol error
            // that would kill the connection, hence the guard above.
            let inhibitor =
                self.manager
                    .inhibit_shortcuts(&self.surface, &self.seat, &self.queue.handle(), ());
            self.inhibitor = Some(inhibitor);

            self.connection
                .flush()
                .context("failed to flush the inhibit request")?;

            // Non-blocking, so a compositor that never answers cannot stall the session. This
            // is also where a protocol error would surface.
            self.queue
                .dispatch_pending(&mut State)
                .context("the Wayland connection reported an error")?;

            Ok(())
        }

        fn disengage(&mut self) {
            if let Some(inhibitor) = self.inhibitor.take() {
                inhibitor.destroy();
                let _ = self.connection.flush();
            }
        }
    }

    impl Drop for WaylandCapture {
        fn drop(&mut self) {
            self.disengage();
        }
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod x11 {
    //! `XGrabKeyboard`, the X11 equivalent, kept deliberately small.
    //!
    //! `owner_events` is true so that keys still reach the window through winit's normal path;
    //! the grab only redirects what the window manager would otherwise have swallowed.

    use core::ffi::c_void;
    use core::ptr::NonNull;

    use anyhow::Context as _;

    use super::CaptureBackend;

    pub(super) struct X11Capture {
        xlib: x11_dl::xlib::Xlib,
        display: *mut x11_dl::xlib::Display,
        window: x11_dl::xlib::Window,
        grabbed: bool,
    }

    impl X11Capture {
        /// # Safety
        ///
        /// `display` must be winit's live `Display*` and `window` its window id.
        pub(super) unsafe fn new(
            display: NonNull<c_void>,
            window: core::ffi::c_ulong,
        ) -> anyhow::Result<Self> {
            // Loaded rather than linked, so a machine without libX11 is a runtime miss.
            let xlib = x11_dl::xlib::Xlib::open().context("libX11 is not available")?;

            Ok(Self {
                xlib,
                display: display.as_ptr().cast(),
                window,
                grabbed: false,
            })
        }
    }

    impl CaptureBackend for X11Capture {
        fn describe(&self) -> &'static str {
            "X11 XGrabKeyboard"
        }

        fn engage(&mut self) -> anyhow::Result<()> {
            if self.grabbed {
                return Ok(());
            }

            // SAFETY: the display and window come from winit and are still alive; this runs on
            // the event loop thread, which is the only thread touching the display.
            let status = unsafe {
                (self.xlib.XGrabKeyboard)(
                    self.display,
                    self.window,
                    x11_dl::xlib::True,
                    x11_dl::xlib::GrabModeAsync,
                    x11_dl::xlib::GrabModeAsync,
                    x11_dl::xlib::CurrentTime,
                )
            };

            // Anything but GrabSuccess means someone else holds the keyboard, or the window is
            // not viewable. Report it and let the caller give up rather than spinning.
            anyhow::ensure!(
                status == x11_dl::xlib::GrabSuccess,
                "XGrabKeyboard failed with status {status}"
            );

            // SAFETY: as above.
            unsafe {
                (self.xlib.XFlush)(self.display);
            }

            self.grabbed = true;
            Ok(())
        }

        fn disengage(&mut self) {
            if !self.grabbed {
                return;
            }
            self.grabbed = false;

            // SAFETY: as above. Ungrabbing an already-released keyboard is harmless, which is
            // what makes this safe to call from every exit path.
            unsafe {
                (self.xlib.XUngrabKeyboard)(self.display, x11_dl::xlib::CurrentTime);
                (self.xlib.XFlush)(self.display);
            }
        }
    }

    impl Drop for X11Capture {
        fn drop(&mut self) {
            self.disengage();
        }
    }
}
