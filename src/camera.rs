//! A camera for the session (MS-RDPECAM).
//!
//! There is no webcam in this arrangement, and that is the point: what the session sees is a
//! picture this client draws, frame by frame. It is the same shape as redirecting a real
//! camera -- the same enumeration, the same formats, the same samples -- with the capture
//! replaced by something synthetic, which is what makes it useful for testing a session that
//! expects a camera and for standing in when there is nothing to point at a face.
//!
//! Two channels. The server opens `RDCamera_Device_Enumerator` at connection time and waits;
//! the client picks a version and then announces its devices, each with a channel name of its
//! own, and the server opens a channel per device. On that channel the server asks what streams
//! there are, what formats they come in, and then pulls samples one at a time -- so the frame
//! rate is the server's to choose, and nothing has to be paced here.
//!
//! Frames go as Motion JPEG. Uncompressed would be a megabyte a frame through a channel that
//! fragments at 1,590 bytes; JPEG is what a real webcam of this kind sends anyway.

use ironrdp_core::{Encode, EncodeResult, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use tracing::{debug, info, warn};

/// Where the server asks what cameras there are.
pub const ENUMERATOR_CHANNEL: &str = "RDCamera_Device_Enumerator";
/// The channel this client's one camera is announced on. The name is the client's to choose.
pub const DEVICE_CHANNEL: &str = "IRONTSC_CAMERA";
/// What the session calls it.
const DEVICE_NAME: &str = "IronTSC Camera";

const MSG_SUCCESS: u8 = 0x01;
const MSG_ERROR: u8 = 0x02;
const MSG_SELECT_VERSION_REQUEST: u8 = 0x03;
const MSG_SELECT_VERSION_RESPONSE: u8 = 0x04;
const MSG_DEVICE_ADDED: u8 = 0x05;
const MSG_ACTIVATE_DEVICE: u8 = 0x07;
const MSG_DEACTIVATE_DEVICE: u8 = 0x08;
const MSG_STREAM_LIST_REQUEST: u8 = 0x09;
const MSG_STREAM_LIST_RESPONSE: u8 = 0x0A;
const MSG_MEDIA_TYPE_LIST_REQUEST: u8 = 0x0B;
const MSG_MEDIA_TYPE_LIST_RESPONSE: u8 = 0x0C;
const MSG_CURRENT_MEDIA_TYPE_REQUEST: u8 = 0x0D;
const MSG_CURRENT_MEDIA_TYPE_RESPONSE: u8 = 0x0E;
const MSG_START_STREAMS: u8 = 0x0F;
const MSG_STOP_STREAMS: u8 = 0x10;
const MSG_SAMPLE_REQUEST: u8 = 0x11;
const MSG_SAMPLE_RESPONSE: u8 = 0x12;
const MSG_SAMPLE_ERROR_RESPONSE: u8 = 0x13;

/// Version 1. Version 2 adds device properties -- brightness and the like -- which a drawn
/// picture has nothing to say about.
const VERSION: u8 = 1;

/// Motion JPEG.
const FORMAT_MJPG: u8 = 0x02;
/// `E_FAIL`, for a sample that could not be produced.
const ERROR_UNEXPECTED: u32 = 0x8000_FFFF;

/// One camera message: the two-byte shared header and then its body.
struct Pdu {
    message: u8,
    body: Vec<u8>,
}

impl DvcEncode for Pdu {}

impl Encode for Pdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ironrdp_core::ensure_size!(in: dst, size: self.size());
        dst.write_u8(VERSION);
        dst.write_u8(self.message);
        dst.write_slice(&self.body);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "CAM_MSG"
    }

    fn size(&self) -> usize {
        2 + self.body.len()
    }
}

impl Pdu {
    fn new(message: u8, body: Vec<u8>) -> DvcMessage {
        Box::new(Self { message, body }) as DvcMessage
    }

    fn empty(message: u8) -> DvcMessage {
        Self::new(message, Vec::new())
    }
}

/// A stream format, which is 26 bytes on the wire either way it travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaType {
    format: u8,
    width: u32,
    height: u32,
    frame_rate_numerator: u32,
    frame_rate_denominator: u32,
    aspect_numerator: u32,
    aspect_denominator: u32,
    flags: u8,
}

impl MediaType {
    const SIZE: usize = 26;

    /// What this camera offers: a modest picture at a modest rate, which is all a drawn one
    /// has any reason to be.
    const fn offered() -> Self {
        Self {
            format: FORMAT_MJPG,
            width: 640,
            height: 480,
            frame_rate_numerator: 30,
            frame_rate_denominator: 1,
            aspect_numerator: 1,
            aspect_denominator: 1,
            flags: 0,
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.push(self.format);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.frame_rate_numerator.to_le_bytes());
        out.extend_from_slice(&self.frame_rate_denominator.to_le_bytes());
        out.extend_from_slice(&self.aspect_numerator.to_le_bytes());
        out.extend_from_slice(&self.aspect_denominator.to_le_bytes());
        out.push(self.flags);
    }

    fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        Some(Self {
            format: bytes[0],
            width: word(1),
            height: word(5),
            frame_rate_numerator: word(9),
            frame_rate_denominator: word(13),
            aspect_numerator: word(17),
            aspect_denominator: word(21),
            flags: bytes[25],
        })
    }
}

/// The channel the server asks what cameras there are.
pub struct CameraEnumerator {
    channel_id: Option<u32>,
}

impl Default for CameraEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraEnumerator {
    pub fn new() -> Self {
        Self { channel_id: None }
    }
}

impl ironrdp_core::AsAny for CameraEnumerator {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for CameraEnumerator {
    fn channel_name(&self) -> &str {
        ENUMERATOR_CHANNEL
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel_id, "📷 the session asked what cameras there are");
        self.channel_id = Some(channel_id);
        // The client opens by naming the highest version it speaks.
        Ok(vec![Pdu::empty(MSG_SELECT_VERSION_REQUEST)])
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let Some(message) = payload.get(1).copied() else {
            return Ok(Vec::new());
        };

        match message {
            MSG_SELECT_VERSION_RESPONSE => {
                debug!(version = payload[0], "📷 version agreed");
                // One device, named for the session and for the channel it will be opened on.
                let mut body = Vec::new();
                for unit in DEVICE_NAME.encode_utf16().chain(core::iter::once(0)) {
                    body.extend_from_slice(&unit.to_le_bytes());
                }
                body.extend_from_slice(DEVICE_CHANNEL.as_bytes());
                body.push(0);
                info!(channel = DEVICE_CHANNEL, "📷 offering a camera to the session");
                Ok(vec![Pdu::new(MSG_DEVICE_ADDED, body)])
            }
            other => {
                debug!(message = other, "📷 ignoring an enumerator message");
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel_id, "📷 the camera enumerator closed");
        self.channel_id = None;
    }
}

/// The camera itself, on the channel the server opened for it.
pub struct Camera {
    channel_id: Option<u32>,
    /// The format the server started the stream in, when it has started one.
    streaming: Option<MediaType>,
    frame: u64,
}

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

impl Camera {
    pub fn new() -> Self {
        Self {
            channel_id: None,
            streaming: None,
            frame: 0,
        }
    }
}

impl ironrdp_core::AsAny for Camera {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for Camera {
    fn channel_name(&self) -> &str {
        DEVICE_CHANNEL
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel_id, "📷 the session opened the camera");
        self.channel_id = Some(channel_id);
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let Some(message) = payload.get(1).copied() else {
            return Ok(Vec::new());
        };
        let body = &payload[2..];

        match message {
            MSG_ACTIVATE_DEVICE | MSG_DEACTIVATE_DEVICE => {
                debug!(message, "📷 device activation");
                if message == MSG_DEACTIVATE_DEVICE {
                    self.streaming = None;
                }
                Ok(vec![Pdu::empty(MSG_SUCCESS)])
            }
            MSG_STREAM_LIST_REQUEST => {
                // One stream: colour, capture, selected, and shareable.
                let body = vec![0x01, 0x00, 0x01, 0x01, 0x01];
                Ok(vec![Pdu::new(MSG_STREAM_LIST_RESPONSE, body)])
            }
            MSG_MEDIA_TYPE_LIST_REQUEST => {
                let mut out = Vec::with_capacity(MediaType::SIZE);
                MediaType::offered().write(&mut out);
                Ok(vec![Pdu::new(MSG_MEDIA_TYPE_LIST_RESPONSE, out)])
            }
            MSG_CURRENT_MEDIA_TYPE_REQUEST => {
                let mut out = Vec::with_capacity(MediaType::SIZE);
                self.streaming
                    .unwrap_or_else(MediaType::offered)
                    .write(&mut out);
                Ok(vec![Pdu::new(MSG_CURRENT_MEDIA_TYPE_RESPONSE, out)])
            }
            MSG_START_STREAMS => {
                // An array of stream index and format; this camera has the one stream.
                match body.get(1..).and_then(MediaType::read) {
                    Some(media) => {
                        info!(
                            width = media.width,
                            height = media.height,
                            format = media.format,
                            "📷 the session started the stream"
                        );
                        self.streaming = Some(media);
                        Ok(vec![Pdu::empty(MSG_SUCCESS)])
                    }
                    None => {
                        warn!("📷 the session started a stream in no format this could read");
                        Ok(vec![Pdu::new(
                            MSG_ERROR,
                            ERROR_UNEXPECTED.to_le_bytes().to_vec(),
                        )])
                    }
                }
            }
            MSG_STOP_STREAMS => {
                info!("📷 the session stopped the stream");
                self.streaming = None;
                Ok(vec![Pdu::empty(MSG_SUCCESS)])
            }
            MSG_SAMPLE_REQUEST => {
                let stream = body.first().copied().unwrap_or(0);
                let Some(media) = self.streaming else {
                    warn!("📷 a sample was asked for before any stream was started");
                    let mut out = vec![stream];
                    out.extend_from_slice(&ERROR_UNEXPECTED.to_le_bytes());
                    return Ok(vec![Pdu::new(MSG_SAMPLE_ERROR_RESPONSE, out)]);
                };

                self.frame = self.frame.wrapping_add(1);
                match draw(&media, self.frame) {
                    Ok(jpeg) => {
                        // One line a second at thirty frames: enough to see the stream is
                        // alive and what it costs, without a line per frame.
                        if self.frame % 30 == 1 {
                            debug!(
                                frame = self.frame,
                                bytes = jpeg.len(),
                                "📷 sending frames to the session"
                            );
                        }
                        let mut out = Vec::with_capacity(1 + jpeg.len());
                        out.push(stream);
                        out.extend_from_slice(&jpeg);
                        Ok(vec![Pdu::new(MSG_SAMPLE_RESPONSE, out)])
                    }
                    Err(error) => {
                        warn!(%error, "📷 could not draw a frame");
                        let mut out = vec![stream];
                        out.extend_from_slice(&ERROR_UNEXPECTED.to_le_bytes());
                        Ok(vec![Pdu::new(MSG_SAMPLE_ERROR_RESPONSE, out)])
                    }
                }
            }
            other => {
                debug!(message = other, "📷 ignoring a camera message");
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel_id, "📷 the session closed the camera");
        self.channel_id = None;
        self.streaming = None;
    }
}

/// Draws one frame: colour bars, a bar sweeping across them, and a counter block that moves on
/// every frame, so a still picture and a stalled stream cannot be mistaken for each other.
fn draw(media: &MediaType, frame: u64) -> Result<Vec<u8>, String> {
    use image::{Rgb, RgbImage};

    let width = media.width.clamp(16, 1920);
    let height = media.height.clamp(16, 1080);

    const BARS: [[u8; 3]; 8] = [
        [192, 192, 192],
        [192, 192, 0],
        [0, 192, 192],
        [0, 192, 0],
        [192, 0, 192],
        [192, 0, 0],
        [0, 0, 192],
        [16, 16, 16],
    ];

    let mut image = RgbImage::new(width, height);
    let bar_width = (width / BARS.len() as u32).max(1);
    let sweep = ((frame * 7) % u64::from(width)) as u32;
    let sweep_width = (width / 40).max(2);

    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let bar = (x / bar_width).min(BARS.len() as u32 - 1) as usize;
        let mut colour = BARS[bar];

        // The sweep, so movement is visible even in a single glance at a still.
        if x >= sweep && x < sweep + sweep_width {
            colour = [255, 255, 255];
        }

        // A block that steps down the frame, one row of blocks per second at thirty frames.
        let block = (frame / 30) % 8;
        if y >= height * 7 / 8 && x / (width / 8).max(1) == block as u32 {
            colour = [255, 128, 0];
        }

        *pixel = Rgb(colour);
    }

    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(&image)
        .map_err(|e| e.to_string())?;
    Ok(jpeg)
}
