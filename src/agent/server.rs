//! The MCP surface over an [`AgentSession`].
//!
//! The same tool set is served two ways. Headless mode speaks stdio, because there the client
//! starts us as its own child process and owns our lifetime. In-session mode cannot: a server
//! switched on from the gear menu of a window that is already running has to be *connected
//! to*, so it listens on loopback and hands out a URL. Both end up calling the same methods on
//! the same session.
//!
//! Coordinates are always in the desktop's own pixels, never the screenshot's -- a screenshot
//! may have been scaled down to save the model some tokens, and every reply says so.

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use ironrdp::input::MouseButton;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::session::{AgentError, AgentSession};

/// How long a `screenshot` waits for the screen to stop changing, unless told otherwise.
const DEFAULT_SETTLE_MS: u64 = 250;
/// Ceiling on that wait, so a screen with a blinking cursor still returns.
const SETTLE_TIMEOUT_MS: u64 = 3_000;
/// How long tools wait for the very first frame before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

fn bad_request(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

impl From<AgentError> for CallToolResult {
    fn from(error: AgentError) -> Self {
        bad_request(error.to_string())
    }
}

/// Turns a mouse button name into the protocol's button.
fn parse_button(name: Option<&str>) -> Result<MouseButton, String> {
    match name.unwrap_or("left").trim().to_ascii_lowercase().as_str() {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        "x1" | "back" => Ok(MouseButton::X1),
        "x2" | "forward" => Ok(MouseButton::X2),
        other => Err(format!(
            "unknown mouse button `{other}`; expected left, right, middle, x1 or x2"
        )),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotArgs {
    /// Wait until the screen has not changed for this many milliseconds first, so the shot
    /// catches the desktop after it has finished reacting. Defaults to 250.
    #[serde(default)]
    pub settle_ms: Option<u64>,
    /// Scale the image down to at most this many pixels wide to save tokens. Coordinates in
    /// the reply are still the desktop's own, unless you pass them back with `from_width`.
    #[serde(default)]
    pub max_width: Option<u32>,
    /// The width of the image a region was measured on, when it came off a scaled screenshot
    /// rather than the desktop.
    #[serde(default)]
    pub from_width: Option<u32>,
    /// How much smaller that image was, if that is easier to say than its width: `2` for a
    /// half-size screenshot.
    #[serde(default)]
    pub scale: Option<f64>,
    /// Capture only part of the desktop. Left edge, in desktop pixels.
    #[serde(default)]
    pub x: Option<u16>,
    /// Top edge of the region, in desktop pixels.
    #[serde(default)]
    pub y: Option<u16>,
    /// Width of the region. Clamped to the desktop's right edge.
    #[serde(default)]
    pub width: Option<u16>,
    /// Height of the region. Clamped to the desktop's bottom edge.
    #[serde(default)]
    pub height: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PixelArgs {
    /// X in desktop pixels. Defaults to wherever the pointer is.
    #[serde(default)]
    pub x: Option<u16>,
    /// Y in desktop pixels. Defaults to wherever the pointer is.
    #[serde(default)]
    pub y: Option<u16>,
    /// The width of the image these coordinates were measured on, when that is a scaled
    /// screenshot rather than the desktop. Leave it off for desktop pixels.
    #[serde(default)]
    pub from_width: Option<u32>,
    /// How much smaller that image was, if that is easier to say than its width: `2` for a
    /// half-size screenshot, the number `screenshot` reports as the scale.
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindTextArgs {
    /// Only report lines containing this, matched without regard to case. Left off, every line
    /// on screen comes back.
    #[serde(default)]
    pub contains: Option<String>,
    /// Wait until the screen has not changed for this many milliseconds first. Defaults to 250.
    #[serde(default)]
    pub settle_ms: Option<u64>,
    /// Drop lines the recogniser is less sure of than this, from 0 to 1. Defaults to 0.5.
    #[serde(default)]
    pub min_confidence: Option<f32>,
    /// Report each detected line separately instead of joining the lines of a heading or a
    /// paragraph into one. Off by default, since a headline set over three lines is one thing.
    #[serde(default)]
    pub lines: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindTargetsArgs {
    /// Wait until the screen has not changed for this many milliseconds first. Defaults to 250.
    #[serde(default)]
    pub settle_ms: Option<u64>,
    /// Drop anything the detector is less sure of than this, from 0 to 1. Defaults to 0.15,
    /// which is low on purpose: a missed button costs more than a spurious box.
    #[serde(default)]
    pub min_confidence: Option<f32>,
    /// Report at most this many, most confident first. Defaults to 30.
    #[serde(default)]
    pub limit: Option<u8>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RegionsArgs {
    /// Ignore blocks narrower than this. Defaults to 16.
    #[serde(default)]
    pub min_width: Option<u16>,
    /// Ignore blocks shorter than this. Defaults to 12.
    #[serde(default)]
    pub min_height: Option<u16>,
    /// Ignore blocks wider than this, to skip whole-window backgrounds.
    #[serde(default)]
    pub max_width: Option<u16>,
    /// Ignore blocks taller than this.
    #[serde(default)]
    pub max_height: Option<u16>,
    /// How many to return, largest first. Defaults to 40.
    #[serde(default)]
    pub limit: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PointArgs {
    /// X in desktop pixels, from the left edge.
    pub x: u16,
    /// Y in desktop pixels, from the top edge.
    pub y: u16,
    /// The width of the image these coordinates were measured on, when that is a scaled
    /// screenshot rather than the desktop. Leave it off for desktop pixels.
    #[serde(default)]
    pub from_width: Option<u32>,
    /// How much smaller that image was, if that is easier to say than its width: `2` for a
    /// half-size screenshot, the number `screenshot` reports as the scale.
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickArgs {
    /// X in desktop pixels, from the left edge.
    pub x: u16,
    /// Y in desktop pixels, from the top edge.
    pub y: u16,
    /// `left` (the default), `right`, `middle`, `x1` or `x2`.
    #[serde(default)]
    pub button: Option<String>,
    /// 1 for a single click, 2 for a double click. Defaults to 1.
    #[serde(default)]
    pub count: Option<u8>,
    /// The width of the image these coordinates were measured on, when that is a scaled
    /// screenshot rather than the desktop. Leave it off for desktop pixels.
    #[serde(default)]
    pub from_width: Option<u32>,
    /// How much smaller that image was, if that is easier to say than its width: `2` for a
    /// half-size screenshot, the number `screenshot` reports as the scale.
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DragArgs {
    pub from_x: u16,
    pub from_y: u16,
    pub to_x: u16,
    pub to_y: u16,
    /// `left` (the default), `right` or `middle`.
    #[serde(default)]
    pub button: Option<String>,
    /// The width of the image these coordinates were measured on, when that is a scaled
    /// screenshot rather than the desktop. Leave it off for desktop pixels.
    #[serde(default)]
    pub from_width: Option<u32>,
    /// How much smaller that image was, if that is easier to say than its width: `2` for a
    /// half-size screenshot, the number `screenshot` reports as the scale.
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollArgs {
    /// Wheel notches; positive scrolls up.
    #[serde(default)]
    pub vertical: Option<i16>,
    /// Wheel notches; positive scrolls right.
    #[serde(default)]
    pub horizontal: Option<i16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeArgs {
    /// Text to type. Sent as Unicode, so the server's keyboard layout does not have to match
    /// this machine's. Newlines are sent as Enter and tabs as Tab.
    pub text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KeyArgs {
    /// A chord such as `enter`, `F5`, `ctrl+c`, `alt+tab`, `win+r` or `ctrl+alt+delete`.
    pub chord: String,
    /// Press it this many times. Defaults to 1.
    #[serde(default)]
    pub repeat: Option<u8>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ShareArgs {
    /// The folder on the client machine to offer, as an absolute path.
    pub path: String,
    /// What the session should call it. Defaults to the folder's own name.
    #[serde(default)]
    pub name: Option<String>,
    /// Offer it without letting the session change anything.
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UnshareArgs {
    /// The name the folder was shared under.
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitArgs {
    /// Milliseconds to wait, capped at 30000.
    pub ms: u64,
}

/// The MCP server. Cheap to clone; every clone drives the same session.
#[derive(Clone)]
pub struct McpServer {
    session: Arc<AgentSession>,
    /// The models that read the screen here, when they are installed.
    sight: Option<Arc<super::Models>>,
    /// The tool table this server actually offers, which is the generated one minus whatever
    /// this session cannot do.
    router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl McpServer {
    pub fn new(session: Arc<AgentSession>, sight: Option<Arc<super::Models>>) -> Self {
        let mut router = Self::tool_router();
        if sight.is_none() {
            // No models, no tools. An agent should not be shown something that can only fail,
            // and a tool it cannot see costs it no tokens to ignore.
            router.remove_route("find_text");
            router.remove_route("find_targets");
        }
        Self {
            session,
            sight,
            router,
        }
    }

    /// Maps a coordinate back into desktop pixels from the image it was measured on.
    ///
    /// An agent that asked for `max_width=1200` on a 4K desktop reads its coordinates off a
    /// 1200-wide image; passing that width back means it never has to do the arithmetic, and
    /// the tools keep one coordinate space -- the desktop's -- underneath.
    #[expect(clippy::wrong_self_convention, reason = "reads as `from the image`, not a cast")]
    fn from_image(&self, value: u16, from_width: Option<u32>, scale: Option<f64>) -> u16 {
        let desktop = u32::from(self.session.frame().describe().width);

        // Two ways of saying the same thing: the image's width, or how much smaller it is.
        // The width wins if both arrive, being the one that cannot drift.
        let factor = match (from_width.filter(|width| *width > 0), scale) {
            (Some(width), _) if desktop > 0 => f64::from(desktop) / f64::from(width),
            (None, Some(scale)) if scale.is_finite() && scale > 0.0 => scale,
            _ => return value,
        };
        if (factor - 1.0).abs() < f64::EPSILON {
            return value;
        }

        let scaled = (f64::from(value) * factor).round().max(0.0);
        // The right and bottom edges of an image map just past the desktop, so a click on the
        // last column lands on the last pixel rather than off the end.
        let limit = f64::from(desktop.saturating_sub(1)).max(0.0);
        scaled.min(limit) as u16
    }

    /// A settled frame as an RGB image, for the models to look at.
    async fn look(&self, settle_ms: Option<u64>) -> Result<image::RgbImage, CallToolResult> {
        self.ready().await?;

        let settle = Duration::from_millis(settle_ms.unwrap_or(DEFAULT_SETTLE_MS));
        if !settle.is_zero() {
            self.session
                .wait_until_still(settle, Duration::from_millis(SETTLE_TIMEOUT_MS))
                .await;
        }

        let Some((bgra, width, height, _generation)) = self.session.frame().snapshot() else {
            return Err(bad_request("no frame has arrived from the server yet"));
        };

        let mut image = image::RgbImage::new(u32::from(width), u32::from(height));
        for (index, pixel) in image.pixels_mut().enumerate() {
            let at = index * 4;
            // The mirror is BGRA and the alpha is not meaningful; the models want RGB.
            *pixel = image::Rgb([bgra[at + 2], bgra[at + 1], bgra[at]]);
        }
        Ok(image)
    }

    /// Nothing can be clicked before the first frame, so every input tool waits for it.
    async fn ready(&self) -> Result<(), CallToolResult> {
        self.session
            .wait_until_ready(READY_TIMEOUT)
            .await
            .map_err(CallToolResult::from)
    }
}

#[rmcp::tool_router]
impl McpServer {
    /// Take a screenshot of the remote desktop.
    ///
    /// Returns a PNG plus the desktop's size. Click and move coordinates are always in the
    /// desktop's own pixels, which the reply states, even when the image has been scaled down.
    #[rmcp::tool]
    async fn screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }

        let settle = Duration::from_millis(args.settle_ms.unwrap_or(DEFAULT_SETTLE_MS));
        let settled = if settle.is_zero() {
            true
        } else {
            self.session
                .wait_until_still(settle, Duration::from_millis(SETTLE_TIMEOUT_MS))
                .await
        };

        let Some((bgra, width, height, generation)) = self.session.frame().snapshot() else {
            return Ok(bad_request("no frame has arrived from the server yet"));
        };

        // A region is cut before scaling, so `max_width` caps the crop rather than the whole
        // desktop and a small region comes back at full fidelity.
        let requested_region = args.x.is_some()
            || args.y.is_some()
            || args.width.is_some()
            || args.height.is_some();

        let (bgra, width, height, origin) = if requested_region {
            let rect = super::screenshot::Crop {
                x: self.from_image(args.x.unwrap_or(0), args.from_width, args.scale),
                y: self.from_image(args.y.unwrap_or(0), args.from_width, args.scale),
                width: self.from_image(args.width.unwrap_or(width), args.from_width, args.scale),
                height: self.from_image(args.height.unwrap_or(height), args.from_width, args.scale),
            };
            match super::screenshot::crop(&bgra, width, height, rect) {
                Ok((cropped, w, h)) => (cropped, w, h, Some((rect.x, rect.y))),
                Err(message) => return Ok(bad_request(message)),
            }
        } else {
            (bgra, width, height, None)
        };

        // `scale` is the same request as `max_width`, said the other way round: an agent that
        // wants "half size" should not have to work out what half of this desktop is.
        let max_width = match (args.max_width, args.scale) {
            (Some(width), _) => NonZeroU32::new(width),
            (None, Some(scale)) if scale.is_finite() && scale > 1.0 => {
                NonZeroU32::new((f64::from(u32::from(width)) / scale).round() as u32)
            }
            _ => None,
        };
        let shot = match super::screenshot::encode(&bgra, width, height, max_width) {
            Ok(shot) => shot,
            Err(message) => return Ok(bad_request(message)),
        };

        let encoded = base64::engine::general_purpose::STANDARD.encode(&shot.png);
        let desktop = self.session.frame().describe();
        let mut note = format!(
            "Desktop is {}x{} pixels; click coordinates use that space. Frame {generation}.",
            desktop.width, desktop.height
        );
        if let Some((origin_x, origin_y)) = origin {
            note.push_str(&format!(
                " This is the region at ({origin_x}, {origin_y}), {}x{} pixels, so add ({origin_x}, {origin_y}) to anything measured inside it.",
                shot.source_width, shot.source_height
            ));
        }
        if shot.is_scaled() {
            let scale = f64::from(shot.source_width) / f64::from(shot.width.max(1));
            note.push_str(&format!(
                " The image below is {}x{}, {scale:.2}x smaller than what it shows. Either scale \
                 coordinates read off it by {scale:.2}, or pass them as they are with \
                 `from_width={}` (or `scale={scale:.2}`) and the tool will do it.",
                shot.width, shot.height, shot.width
            ));
        }
        if !settled {
            note.push_str(" The screen was still changing when this was taken.");
        }

        Ok(CallToolResult::success(vec![
            ContentBlock::text(note),
            ContentBlock::image(encoded, "image/png"),
        ]))
    }

    /// Report whether the session is connected, its size, and what the agent did recently.
    #[rmcp::tool]
    async fn status(&self) -> Result<CallToolResult, ErrorData> {
        let summary = self.session.frame().describe();

        let mut lines = Vec::new();
        lines.push(format!(
            "connected: {}{}",
            summary.connected,
            if self.session.owns_session() {
                " (session opened by this server)"
            } else {
                " (attached to an IronTSC window)"
            }
        ));
        lines.push(format!("desktop: {}x{}", summary.width, summary.height));
        lines.push(format!("frames received: {}", summary.generation));
        if let Some(transport) = summary.transport {
            lines.push(format!("transport: {transport}"));
        }
        if let Some(rtt) = summary.roundtrip_time_ms {
            lines.push(format!("round trip: {rtt} ms"));
        }
        if let Some(error) = summary.error {
            lines.push(format!("error: {error}"));
        }
        if let Some(terminated) = summary.terminated {
            lines.push(format!("terminated: {terminated}"));
        }

        let actions = self.session.recent_actions();
        if !actions.is_empty() {
            lines.push("recent actions:".to_owned());
            for action in actions.iter().rev().take(10).rev() {
                lines.push(format!(
                    "  {:>5} ms ago  {}",
                    action.at.elapsed().as_millis(),
                    action.summary
                ));
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            lines.join("\n"),
        )]))
    }

    /// Read the colour of one pixel, by default the one under the pointer.
    ///
    /// Exact where a screenshot is approximate: useful for checking a state that shows as a
    /// colour -- an indicator, a highlighted row, a field that turns red on bad input.
    #[rmcp::tool]
    async fn pixel(
        &self,
        Parameters(args): Parameters<PixelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }

        let (x, y, source) = match (args.x, args.y) {
            (Some(x), Some(y)) => (
                self.from_image(x, args.from_width, args.scale),
                self.from_image(y, args.from_width, args.scale),
                "the given coordinates",
            ),
            (None, None) => match self.session.pointer_position() {
                Some((x, y)) => (x, y, "the pointer"),
                None => {
                    return Ok(bad_request(
                        "the pointer has not been moved yet, so give an x and a y",
                    ));
                }
            },
            _ => return Ok(bad_request("give both x and y, or neither")),
        };

        let Some((bgra, width, height, _)) = self.session.frame().snapshot() else {
            return Ok(bad_request("no frame has arrived from the server yet"));
        };

        match super::vision::sample(&bgra, width, height, x, y) {
            Some(pixel) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "{} at ({x}, {y}), read from {source}: rgb({}, {}, {})",
                pixel.hex(),
                pixel.red,
                pixel.green,
                pixel.blue
            ))])),
            None => Ok(bad_request(format!(
                "({x}, {y}) is outside the {width}x{height} desktop"
            ))),
        }
    }

    /// List blocks of flat colour on screen, largest first.
    ///
    /// These are candidate controls, not a widget tree: buttons, fields, panels and title
    /// bars in Windows chrome are mostly flat rectangles, so they show up here, but so does
    /// anything else that happens to be one. Use it to get coordinates worth looking at, then
    /// confirm against a screenshot before clicking.
    #[rmcp::tool]
    async fn find_regions(
        &self,
        Parameters(args): Parameters<RegionsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }

        let Some((bgra, width, height, _)) = self.session.frame().snapshot() else {
            return Ok(bad_request("no frame has arrived from the server yet"));
        };

        let defaults = super::vision::RegionQuery::default();
        let query = super::vision::RegionQuery {
            min_width: args.min_width.unwrap_or(defaults.min_width),
            min_height: args.min_height.unwrap_or(defaults.min_height),
            max_width: args.max_width.unwrap_or(defaults.max_width),
            max_height: args.max_height.unwrap_or(defaults.max_height),
            min_fill_percent: defaults.min_fill_percent,
            limit: usize::from(args.limit.unwrap_or(defaults.limit as u16)).clamp(1, 200),
        };

        let regions = super::vision::find_regions(&bgra, width, height, query);
        if regions.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "no flat blocks matched; try a smaller min_width and min_height",
            )]));
        }

        let mut lines = vec![format!(
            "{} flat blocks on a {width}x{height} desktop, largest first.              `centre` is where a click would go.",
            regions.len()
        )];
        for region in &regions {
            let (centre_x, centre_y) = region.centre();
            lines.push(format!(
                "  {:>4},{:<4} {:>4}x{:<4}  centre {:>4},{:<4}  {}  {}% filled",
                region.x,
                region.y,
                region.width,
                region.height,
                centre_x,
                centre_y,
                region.hex(),
                region.fill_percent
            ));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            lines.join("\n"),
        )]))
    }

    /// Move the mouse pointer without pressing anything.
    #[rmcp::tool]
    async fn move_mouse(
        &self,
        Parameters(args): Parameters<PointArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }
        let x = self.from_image(args.x, args.from_width, args.scale);
        let y = self.from_image(args.y, args.from_width, args.scale);
        match self.session.move_mouse(x, y) {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "moved to ({x}, {y})"
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Click at a point on the remote desktop.
    #[rmcp::tool]
    async fn click(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }
        let button = match parse_button(args.button.as_deref()) {
            Ok(button) => button,
            Err(message) => return Ok(bad_request(message)),
        };
        let count = args.count.unwrap_or(1);
        let x = self.from_image(args.x, args.from_width, args.scale);
        let y = self.from_image(args.y, args.from_width, args.scale);

        let session = Arc::clone(&self.session);
        let clicked = tokio::task::spawn_blocking(move || session.click(button, x, y, count))
            .await
            .unwrap_or_else(|error| Err(AgentError::Terminated(format!("the click stopped: {error}"))));

        match clicked {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "{button:?} click x{count} at ({x}, {y})"
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Press at one point, move, and release at another.
    #[rmcp::tool]
    async fn drag(
        &self,
        Parameters(args): Parameters<DragArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }
        let button = match parse_button(args.button.as_deref()) {
            Ok(button) => button,
            Err(message) => return Ok(bad_request(message)),
        };

        let from_x = self.from_image(args.from_x, args.from_width, args.scale);
        let from_y = self.from_image(args.from_y, args.from_width, args.scale);
        let to_x = self.from_image(args.to_x, args.from_width, args.scale);
        let to_y = self.from_image(args.to_y, args.from_width, args.scale);

        if let Err(error) = self.session.mouse_down(button, from_x, from_y) {
            return Ok(error.into());
        }
        // A drag that teleports is ignored by some controls, which want to see the pointer
        // travel; a handful of intermediate positions is enough to convince them.
        for step in 1..=4u32 {
            let x = interpolate(from_x, to_x, step, 5);
            let y = interpolate(from_y, to_y, step, 5);
            if let Err(error) = self.session.move_mouse(x, y) {
                return Ok(error.into());
            }
        }
        match self.session.mouse_up(button, to_x, to_y) {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "dragged from ({from_x}, {from_y}) to ({to_x}, {to_y}) with {button:?}"
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Scroll the wheel, in notches.
    #[rmcp::tool]
    async fn scroll(
        &self,
        Parameters(args): Parameters<ScrollArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }
        let vertical = args.vertical.unwrap_or(0);
        let horizontal = args.horizontal.unwrap_or(0);
        match self.session.scroll(vertical, horizontal) {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "scrolled {vertical} vertically and {horizontal} horizontally"
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Type text into whatever has focus.
    #[rmcp::tool]
    async fn type_text(
        &self,
        Parameters(args): Parameters<TypeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }
        let length = args.text.chars().count();
        // Typing is paced, so it sleeps its way through a long string; that belongs on a
        // blocking thread rather than on a runtime worker every other tool is waiting on.
        let session = Arc::clone(&self.session);
        let text = args.text.clone();
        let typed = tokio::task::spawn_blocking(move || session.type_text(&text))
            .await
            .unwrap_or_else(|error| Err(AgentError::Terminated(format!("typing stopped: {error}"))));

        match typed {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "typed {length} characters"
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Press a key or a chord, such as `enter`, `alt+tab` or `ctrl+alt+delete`.
    #[rmcp::tool]
    async fn key(
        &self,
        Parameters(args): Parameters<KeyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(result) = self.ready().await {
            return Ok(result);
        }
        let repeat = args.repeat.unwrap_or(1).max(1);
        for _ in 0..repeat {
            if let Err(error) = self.session.press_chord(&args.chord) {
                return Ok(error.into());
            }
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "pressed {} x{repeat}",
            args.chord
        ))]))
    }

    /// Read the text on the screen, with the position of every line.
    ///
    /// This is the cheap way to see: two small models run here, and you get lines and
    /// coordinates rather than a picture. `contains` narrows it to the lines you are after --
    /// `contains="OK"` to find a button before clicking it. The coordinates are the desktop's
    /// own, so they go straight to `click`.
    #[rmcp::tool]
    async fn find_text(
        &self,
        Parameters(args): Parameters<FindTextArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(sight) = self.sight.clone() else {
            return Ok(bad_request("no screen models are installed for this session"));
        };
        let image = match self.look(args.settle_ms).await {
            Ok(image) => image,
            Err(result) => return Ok(result),
        };

        let lines = match tokio::task::spawn_blocking(move || sight.read(&image)).await {
            Ok(Ok(lines)) => lines,
            Ok(Err(error)) => return Ok(bad_request(format!("could not read the screen: {error}"))),
            Err(error) => return Ok(bad_request(format!("reading the screen stopped: {error}"))),
        };

        // A headline set over several lines is one thing to read, not three.
        let lines = if args.lines.unwrap_or(false) {
            lines
        } else {
            super::into_blocks(lines)
        };

        let needle = args.contains.as_deref().map(str::to_lowercase);
        let minimum = args.min_confidence.unwrap_or(0.5);
        let matched: Vec<_> = lines
            .into_iter()
            .filter(|line| line.confidence >= minimum)
            .filter(|line| {
                needle
                    .as_ref()
                    .is_none_or(|needle| line.text.to_lowercase().contains(needle))
            })
            .collect();

        if matched.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                match &args.contains {
                    Some(needle) => format!("no text matching {needle:?} is on screen"),
                    None => "no text was found on screen".to_owned(),
                },
            )]));
        }

        let mut report = format!("{} lines\n", matched.len());
        for line in &matched {
            let (x, y) = line.centre();
            report.push_str(&format!(
                "click ({x}, {y}) [{}, {}, {}, {}] {:.2}  {}\n",
                line.left, line.top, line.right, line.bottom, line.confidence, line.text
            ));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(report)]))
    }

    /// List the things on screen that look clickable, whether or not they carry a label.
    ///
    /// Icons, toolbar buttons and tray items have no text for `find_text` to read; this finds
    /// them by shape. It says where they are, not what they do -- pair it with `find_text`, or
    /// take a screenshot of one box to see a single icon closely.
    #[rmcp::tool]
    async fn find_targets(
        &self,
        Parameters(args): Parameters<FindTargetsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(sight) = self.sight.clone() else {
            return Ok(bad_request("no screen models are installed for this session"));
        };
        let image = match self.look(args.settle_ms).await {
            Ok(image) => image,
            Err(result) => return Ok(result),
        };

        let minimum = args.min_confidence.unwrap_or(0.15);
        let targets = match tokio::task::spawn_blocking(move || sight.targets(&image, minimum)).await
        {
            Ok(Ok(targets)) => targets,
            Ok(Err(error)) => return Ok(bad_request(format!("could not scan the screen: {error}"))),
            Err(error) => return Ok(bad_request(format!("scanning the screen stopped: {error}"))),
        };

        if targets.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "nothing on screen looks clickable".to_owned(),
            )]));
        }

        let limit = usize::from(args.limit.unwrap_or(30)).max(1);
        let mut report = format!("{} clickable\n", targets.len().min(limit));
        for target in targets.iter().take(limit) {
            let (x, y) = target.centre();
            report.push_str(&format!(
                "click ({x}, {y}) [{}, {}, {}, {}] {}x{} {:.2}\n",
                target.left,
                target.top,
                target.right,
                target.bottom,
                target.right - target.left,
                target.bottom - target.top,
                target.confidence
            ));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(report)]))
    }

    /// Wait, for when something is loading and there is nothing to click yet.
    #[rmcp::tool]
    async fn wait(
        &self,
        Parameters(args): Parameters<WaitArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let capped = args.ms.min(30_000);
        tokio::time::sleep(Duration::from_millis(capped)).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "waited {capped} ms"
        ))]))
    }

    /// Offer a folder from this machine to the session, which can then open it in File
    /// Explorer or read and write it from a command prompt.
    #[rmcp::tool]
    async fn share_folder(
        &self,
        Parameters(args): Parameters<ShareArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self
            .session
            .share_folder(&args.path, args.name.as_deref(), !args.read_only)
        {
            Ok(name) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "{} is now shared with the session as \\\\tsclient\\{name}{}",
                args.path,
                if args.read_only { ", read-only" } else { "" }
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Stop offering a folder that was shared.
    #[rmcp::tool]
    async fn unshare_folder(
        &self,
        Parameters(args): Parameters<UnshareArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.session.unshare_folder(&args.name) {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "{} is no longer shared",
                args.name
            ))])),
            Err(error) => Ok(error.into()),
        }
    }

    /// Close the session. Only available when this server opened it.
    #[rmcp::tool]
    async fn disconnect(&self) -> Result<CallToolResult, ErrorData> {
        match self.session.disconnect() {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "session closed",
            )])),
            Err(error) => Ok(error.into()),
        }
    }
}

#[rmcp::tool_handler(
    router = self.router,
    name = "irontsc",
    instructions = "Drives a Windows desktop over RDP. Call `screenshot` to see the screen, \
then `click`, `type_text`, `key`, `scroll` and `drag` to act on it. Coordinates are always in \
the desktop's own pixels, which `screenshot` reports, even when the returned image has been \
scaled down. After an action that changes the screen, take another screenshot -- it waits for \
the screen to settle before capturing.\n\n\
Pass x, y, width and height to `screenshot` to grab one part of the screen at full detail \
instead of the whole desktop scaled down; that is the cheap way to read a dialog or a status \
bar. `pixel` reads one exact colour, by default under the pointer, which is how to check an \
indicator without another screenshot. `find_regions` lists flat rectangles as candidate \
controls -- treat it as a hint to check against the image, not as a widget tree.\n\n\
Where `find_text` and `find_targets` are listed, reach for them before a screenshot. They run \
small models on this machine and answer in text: `find_text` gives every line and where it is, \
`find_text` with `contains` finds one button, and `find_targets` gives the boxes worth clicking \
including icons that carry no text. A screenshot then only has to settle an ambiguity rather \
than carry the whole screen."
)]
impl ServerHandler for McpServer {}

/// Linear step from `from` to `to`, step `step` of `steps`.
fn interpolate(from: u16, to: u16, step: u32, steps: u32) -> u16 {
    let from = i64::from(from);
    let to = i64::from(to);
    let value = from + (to - from) * i64::from(step) / i64::from(steps);
    u16::try_from(value.clamp(0, i64::from(u16::MAX))).unwrap_or(0)
}

/// Serves MCP over stdio and returns when the client goes away. Headless mode.
pub async fn serve_stdio(
    session: Arc<AgentSession>,
    sight: Option<Arc<super::Models>>,
) -> anyhow::Result<()> {
    let service = McpServer::new(session, sight)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| anyhow::anyhow!("failed to start the MCP server: {error}"))?;

    service
        .waiting()
        .await
        .map_err(|error| anyhow::anyhow!("MCP server stopped: {error}"))?;
    Ok(())
}

/// A running loopback MCP server, and the way to stop it.
pub struct HttpServer {
    pub address: SocketAddr,
    pub token: String,
    cancel: CancellationToken,
}

impl HttpServer {
    /// The URL to hand to an MCP client.
    pub fn url(&self) -> String {
        format!("http://{}/mcp?t={}", self.address, self.token)
    }

    /// The command that wires Claude Code up to it.
    pub fn claude_code_command(&self) -> String {
        format!(
            "claude mcp add --transport http irontsc '{}'",
            self.url()
        )
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Starts a loopback MCP server for a session a window already owns. In-session mode.
///
/// The listener is bound before this returns, so the caller can show a URL that is already
/// live. A token is required on every request: anything running as this user could otherwise
/// drive the desktop, and "it is only on localhost" is not an access control.
pub async fn serve_http(
    session: Arc<AgentSession>,
    port: u16,
    sight: Option<Arc<super::Models>>,
) -> anyhow::Result<HttpServer> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    use tower_service::Service as _;

    let token = generate_token();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|error| anyhow::anyhow!("failed to listen on 127.0.0.1:{port}: {error}"))?;
    let address = listener.local_addr()?;

    let cancel = CancellationToken::new();
    // The config is non-exhaustive, so it is built from its default and adjusted.
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = cancel.clone();

    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(Arc::clone(&session), sight.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let accept_token = token.clone();
    let accept_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            let stream = tokio::select! {
                () = accept_cancel.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _peer)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "MCP listener stopped accepting");
                        break;
                    }
                },
            };

            let service = service.clone();
            let token = accept_token.clone();
            let connection_cancel = accept_cancel.clone();

            tokio::spawn(async move {
                let handler = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let mut service = service.clone();
                    let token = token.clone();
                    async move {
                        if !is_authorised(&request, &token) {
                            return Ok::<_, std::convert::Infallible>(unauthorised());
                        }
                        service.call(request).await
                    }
                });

                let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                let connection =
                    builder.serve_connection_with_upgrades(TokioIo::new(stream), handler);
                tokio::select! {
                    () = connection_cancel.cancelled() => {}
                    result = connection => {
                        if let Err(error) = result {
                            tracing::debug!(%error, "MCP connection ended");
                        }
                    }
                }
            });
        }
    });

    Ok(HttpServer {
        address,
        token,
        cancel,
    })
}

/// Accepts the token as a bearer header or a `t` query parameter, because not every MCP client
/// lets you set a header.
fn is_authorised<B>(request: &hyper::Request<B>, token: &str) -> bool {
    let from_header = request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    let from_query = request.uri().query().and_then(|query| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == "t")
            .map(|(_, value)| value)
    });

    // Compared byte-wise in constant time; the token is short and the comparison is cheap.
    [from_header, from_query]
        .into_iter()
        .flatten()
        .any(|candidate| constant_time_eq(candidate.as_bytes(), token.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// The body type `StreamableHttpService` responds with, spelled out because rmcp keeps its
/// own alias for it crate-private.
type BoxResponse = hyper::Response<
    http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>,
>;

fn unauthorised() -> BoxResponse {
    use http_body_util::{BodyExt as _, Full};

    let body = Full::new(bytes::Bytes::from_static(b"missing or invalid MCP token")).boxed();

    let mut response = hyper::Response::new(body);
    *response.status_mut() = hyper::StatusCode::UNAUTHORIZED;
    response
}

/// A URL-safe random token for the loopback server.
fn generate_token() -> String {
    use rand::Rng as _;

    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| char::from(ALPHABET[rng.gen_range(0..ALPHABET.len())]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_button_names() {
        assert_eq!(parse_button(None).expect("default"), MouseButton::Left);
        assert_eq!(parse_button(Some("RIGHT")).expect("right"), MouseButton::Right);
        assert!(parse_button(Some("wheel")).is_err());
    }

    #[test]
    fn interpolates_between_endpoints() {
        assert_eq!(interpolate(0, 100, 0, 5), 0);
        assert_eq!(interpolate(0, 100, 5, 5), 100);
        assert_eq!(interpolate(100, 0, 1, 5), 80);
    }

    #[test]
    fn tokens_are_long_and_distinct() {
        let first = generate_token();
        let second = generate_token();
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
    }

    #[test]
    fn constant_time_compare_matches_only_equal_slices() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
