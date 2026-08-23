//! A camera for the session (MS-RDPECAM).
//!
//! What the session sees is either a webcam on this machine or a picture this client draws.
//! The protocol does not know the difference: the same enumeration, the same formats, the same
//! samples, with only the last step -- where a frame comes from -- differing. A drawn one is
//! what makes it possible to test a session that expects a camera, and to stand in when there
//! is nothing to point at a face.
//!
//! A webcam of the usual kind already produces Motion JPEG, which is what this protocol asks
//! for, so its frames are passed through exactly as the device gave them: nothing is decoded
//! and nothing is re-encoded. A camera that only offers raw frames is converted, and the drawn
//! one is drawn and encoded.
//!
//! Two channels. The server opens `RDCamera_Device_Enumerator` at connection time and waits;
//! the client picks a version and then announces its devices, each with a channel name of its
//! own, and the server opens a channel per device. On that channel the server asks what streams
//! there are, what formats they come in, and then pulls samples one at a time -- so the frame
//! rate is the server's to choose, and nothing has to be paced here.
//!
//! Frames go as Motion JPEG. Uncompressed would be a megabyte a frame through a channel that
//! fragments at 1,590 bytes.

use ironrdp_core::{Encode, EncodeResult, WriteCursor};
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use ironrdp_dvc::{DvcEncode, DvcMessage, DvcProcessor};
use std::sync::{Arc, Mutex};

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
/// How long to wait for a camera to produce its first frame before giving up on it.
const FIRST_FRAME_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);
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

/// A camera running on its own thread, because the capture handle cannot leave it.
struct Device {
    /// The most recent frame, as the device gave it. Older ones are dropped: a session asking
    /// for a sample wants what the camera sees now, not a queue of what it saw.
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    orders: std::sync::mpsc::Sender<Order>,
}

/// What the thread holding the camera is told to do.
enum Order {
    Roll(MediaType),
    Stop,
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.orders.send(Order::Stop);
    }
}

/// Where the picture comes from.
enum Lens {
    /// A webcam on this machine.
    Device(Device),
    /// A picture this client draws, for a machine with no camera or a test that wants a known
    /// one.
    Drawn,
}

impl core::fmt::Debug for Lens {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Device(_) => f.write_str("a camera on this machine"),
            Self::Drawn => f.write_str("a drawn picture"),
        }
    }
}

/// The camera itself, on the channel the server opened for it.
pub struct Camera {
    channel_id: Option<u32>,
    lens: Lens,
    /// What the lens can produce, which is what the session gets to choose from.
    offered: Vec<MediaType>,
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
    /// Opens whichever camera the preferences ask for, or draws one when there is none.
    pub fn new() -> Self {
        let (lens, offered) = open_lens();
        info!(?lens, formats = offered.len(), "📷 camera ready for the session");
        Self {
            channel_id: None,
            lens,
            offered,
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
                    self.stop();
                }
                Ok(vec![Pdu::empty(MSG_SUCCESS)])
            }
            MSG_STREAM_LIST_REQUEST => {
                // One stream: colour, capture, selected, and shareable.
                let body = vec![0x01, 0x00, 0x01, 0x01, 0x01];
                Ok(vec![Pdu::new(MSG_STREAM_LIST_RESPONSE, body)])
            }
            MSG_MEDIA_TYPE_LIST_REQUEST => {
                let mut out = Vec::with_capacity(self.offered.len() * MediaType::SIZE);
                for media in &self.offered {
                    debug!(
                        width = media.width,
                        height = media.height,
                        fps = media.frame_rate_numerator,
                        "📷 offering a format"
                    );
                    media.write(&mut out);
                }
                Ok(vec![Pdu::new(MSG_MEDIA_TYPE_LIST_RESPONSE, out)])
            }
            MSG_CURRENT_MEDIA_TYPE_REQUEST => {
                let mut out = Vec::with_capacity(MediaType::SIZE);
                self.streaming
                    .or_else(|| self.offered.first().copied())
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
                        if let Err(error) = self.start(&media) {
                            warn!(%error, "📷 could not start the camera");
                            return Ok(vec![Pdu::new(
                                MSG_ERROR,
                                ERROR_UNEXPECTED.to_le_bytes().to_vec(),
                            )]);
                        }
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
                self.stop();
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
                match self.capture(&media) {
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
        self.stop();
    }
}

impl Camera {
    /// Starts the device rolling, in the format the session chose.
    fn start(&mut self, media: &MediaType) -> Result<(), String> {
        let Lens::Device(device) = &mut self.lens else {
            return Ok(());
        };
        device
            .orders
            .send(Order::Roll(*media))
            .map_err(|_| "the camera thread has gone".to_owned())?;

        // The session starts asking for samples the moment this is answered, and a camera takes
        // a moment to wake. Answering "no frame yet" to the first request is not a delay to the
        // caller -- it is a failure, and a browser will drop the stream on it. So the success
        // waits here for the first frame, which is the one place in the exchange where waiting
        // is what the protocol expects.
        let waited = std::time::Instant::now();
        while waited.elapsed() < FIRST_FRAME_PATIENCE {
            if device
                .latest
                .lock()
                .is_ok_and(|latest| latest.is_some())
            {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err("the camera produced no picture".to_owned())
    }

    /// Stops it again, so the light goes out when nothing is watching.
    fn stop(&mut self) {
        self.streaming = None;
        if let Lens::Device(device) = &mut self.lens {
            let _ = device.orders.send(Order::Stop);
        }
    }

    /// One frame, as Motion JPEG.
    ///
    /// A webcam that already speaks Motion JPEG is passed through untouched -- that is the
    /// whole point of asking for it -- and anything else is decoded and encoded once.
    fn capture(&mut self, media: &MediaType) -> Result<Vec<u8>, String> {
        let frame = self.frame;
        let Lens::Device(device) = &mut self.lens else {
            return draw(media, frame);
        };

        // The newest frame, left in place. The session asks for samples on its own schedule,
        // which is not the camera's, and a request that falls between two frames should get the
        // picture as it was rather than an error.
        device
            .latest
            .lock()
            .ok()
            .and_then(|latest| latest.clone())
            .ok_or_else(|| "the camera has not produced a frame yet".to_owned())
    }
}

/// Finds the camera to use, and what it can produce.
///
/// The `camera` preference names a device: any part of its name will do, and the word
/// `pattern` asks for the drawn one however many cameras are attached. With nothing named, the
/// first camera on the machine is used, and a machine with none gets the drawn picture rather
/// than no camera at all -- a session that was promised one should find one.
fn open_lens() -> (Lens, Vec<MediaType>) {
    let wanted = crate::preferences::Preferences::load().camera;
    let wanted = wanted.trim().to_lowercase();
    if wanted == "pattern" {
        return (Lens::Drawn, vec![MediaType::offered()]);
    }

    let cameras = nokhwa::query(ApiBackend::Auto).unwrap_or_default();
    if cameras.is_empty() {
        info!("📷 no camera on this machine, drawing one instead");
        return (Lens::Drawn, vec![MediaType::offered()]);
    }

    // A machine lists more than one node per camera -- the capture one, and others that carry
    // metadata or infrared and will not open or will offer nothing. So each is tried in turn
    // rather than only the first, and only a failure of all of them means drawing instead.
    let candidates = cameras
        .iter()
        .filter(|camera| wanted.is_empty() || camera.human_name().to_lowercase().contains(&wanted));

    for camera in candidates {
        let name = camera.human_name();
        match open_device(camera.index().clone()) {
            Ok((device, formats)) if !formats.is_empty() => {
                info!(camera = %name, formats = formats.len(), "📷 using a camera on this machine");
                return (Lens::Device(device), formats);
            }
            Ok(_) => debug!(camera = %name, "📷 that one offers nothing this can send"),
            Err(error) => debug!(camera = %name, %error, "📷 that one would not open"),
        }
    }

    warn!(
        cameras = cameras.len(),
        "📷 no camera on this machine could be used, drawing one instead"
    );
    (Lens::Drawn, vec![MediaType::offered()])
}

/// Opens one camera on a thread of its own, and asks it what it can do.
///
/// The capture handle is not `Send`, so it never leaves the thread that made it. What comes
/// back is the list of formats it can produce and a way to tell it to start and stop.
fn open_device(index: CameraIndex) -> Result<(Device, Vec<MediaType>), String> {
    let latest: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let (orders, taking) = std::sync::mpsc::channel::<Order>();
    let (opened, ready) = std::sync::mpsc::channel::<Result<Vec<MediaType>, String>>();

    let held = Arc::clone(&latest);
    std::thread::Builder::new()
        .name("camera".to_owned())
        .spawn(move || run_camera(index, held, &taking, &opened))
        .map_err(|error| error.to_string())?;

    let formats = ready
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "the camera did not answer".to_owned())??;

    Ok((Device { latest, orders }, formats))
}

/// The thread that owns the camera: opens it, answers what it can do, then rolls when told.
fn run_camera(
    index: CameraIndex,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    orders: &std::sync::mpsc::Receiver<Order>,
    opened: &std::sync::mpsc::Sender<Result<Vec<MediaType>, String>>,
) {
    // Nothing in particular is asked for here: this open is only to find out what the camera
    // can do, and a camera that cannot satisfy a specific request would refuse to open at all.
    let mut camera = match nokhwa::Camera::new(
        index,
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::None),
    ) {
        Ok(camera) => camera,
        Err(error) => {
            let _ = opened.send(Err(error.to_string()));
            return;
        }
    };

    let formats = match camera.compatible_camera_formats() {
        Ok(formats) => usable_formats(formats),
        Err(error) => {
            let _ = opened.send(Err(error.to_string()));
            return;
        }
    };
    if opened.send(Ok(formats)).is_err() {
        return;
    }

    let mut rolling = false;
    loop {
        // While rolling, orders are picked up between frames; while idle, waiting on one is
        // what keeps the thread from spinning.
        let order = if rolling {
            orders.try_recv().ok()
        } else {
            match orders.recv() {
                Ok(order) => Some(order),
                Err(_) => return,
            }
        };

        match order {
            Some(Order::Roll(media)) => {
                let wanted = CameraFormat::new(
                    Resolution::new(media.width, media.height),
                    FrameFormat::MJPEG,
                    media.frame_rate_numerator.max(1),
                );
                if let Err(error) = camera.set_camera_requset(RequestedFormat::new::<RgbFormat>(
                    RequestedFormatType::Closest(wanted),
                )) {
                    warn!(%error, "📷 the camera would not take that format");
                }
                match camera.open_stream() {
                    Ok(()) => {
                        info!(
                            width = media.width,
                            height = media.height,
                            "📷 the camera is rolling"
                        );
                        rolling = true;
                    }
                    Err(error) => warn!(%error, "📷 the camera would not start"),
                }
            }
            Some(Order::Stop) => {
                if rolling {
                    let _ = camera.stop_stream();
                    info!("📷 the camera has stopped");
                }
                rolling = false;
                if let Ok(mut latest) = latest.lock() {
                    *latest = None;
                }
            }
            None => {}
        }

        if !rolling {
            continue;
        }

        // A webcam of the usual kind already speaks Motion JPEG, which is what goes on the
        // wire, so its bytes are kept exactly as they came. Anything else is converted.
        match camera.frame() {
            Ok(buffer) => {
                let encoded = match jpeg_within(buffer.buffer()) {
                    Some(jpeg) => Some(jpeg.to_vec()),
                    None => buffer
                        .decode_image::<RgbFormat>()
                        .ok()
                        .and_then(|image| encode(&image).ok()),
                };
                if let (Some(encoded), Ok(mut latest)) = (encoded, latest.lock()) {
                    *latest = Some(encoded);
                }
            }
            Err(error) => {
                warn!(%error, "📷 the camera stopped giving frames");
                let _ = camera.stop_stream();
                rolling = false;
            }
        }
    }
}

/// The formats worth offering the session, most useful first.
///
/// Motion JPEG only, one entry per size, and not the whole list a camera with dozens of modes
/// would give -- the session picks one and the rest are noise.
fn usable_formats(formats: Vec<CameraFormat>) -> Vec<MediaType> {
    let mut usable: Vec<MediaType> = formats
        .into_iter()
        .filter(|format| format.format() == FrameFormat::MJPEG)
        .map(|format| MediaType {
            format: FORMAT_MJPG,
            width: format.width(),
            height: format.height(),
            frame_rate_numerator: format.frame_rate(),
            frame_rate_denominator: 1,
            aspect_numerator: 1,
            aspect_denominator: 1,
            flags: 0,
        })
        .collect();

    usable.sort_by_key(|media| {
        core::cmp::Reverse((
            u64::from(media.height) * u64::from(media.width),
            u64::from(media.frame_rate_numerator),
        ))
    });
    usable.dedup_by_key(|media| (media.width, media.height));
    usable.truncate(8);
    usable
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

    encode(&image)
}

/// The JPEG inside a camera buffer, if that is what it holds.
///
/// A camera that speaks Motion JPEG hands back a buffer of the size the driver allocated rather
/// than the size of the picture in it, so a 640x480 frame arrives as 614,400 bytes with a
/// 30,000 byte JPEG at the front and nothing after. Sending the whole buffer works -- a decoder
/// stops at the end marker -- and wastes twenty times the bandwidth on a channel that fragments
/// every 1,590 bytes. The end marker cannot occur inside the compressed data, which is what
/// makes finding it safe.
fn jpeg_within(buffer: &[u8]) -> Option<&[u8]> {
    const START_OF_IMAGE: [u8; 2] = [0xFF, 0xD8];
    const END_OF_IMAGE: [u8; 2] = [0xFF, 0xD9];

    if !buffer.starts_with(&START_OF_IMAGE) {
        return None;
    }
    let end = buffer
        .windows(2)
        .rposition(|pair| pair == END_OF_IMAGE)?
        .checked_add(2)?;
    buffer.get(..end)
}

/// Turns a picture into the Motion JPEG frame the session expects.
fn encode(image: &image::RgbImage) -> Result<Vec<u8>, String> {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(image)
        .map_err(|error| error.to_string())?;
    Ok(jpeg)
}
