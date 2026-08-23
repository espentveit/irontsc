//! The microphone, sent into the session (MS-RDPEAI).
//!
//! This is the direction the audio channels of [`crate::dvc_bridge`] do not go: sound captured
//! here, on the machine the person is sitting at, played into whatever is listening in the
//! remote session -- a call in Teams, a voice recorder, dictation.
//!
//! The channel is dynamic, named `AUDIO_INPUT`, and only the server opens it. It does so
//! lazily: not at connection time like the playback channel, but at the moment an application
//! in the session first asks to record. And it only does so at all when the client said, in its
//! Client Info PDU, that it can capture -- which is off unless the connection asks for it,
//! because a session that can turn on a microphone is not something to arrange by accident.
//!
//! The exchange, once opened: the server sends its version and its formats, the client answers
//! with a subset it can actually record, and the server then sends an Open naming one of them.
//! From there the client talks alone, an Incoming Data PDU before every Data PDU, for as long
//! as the channel is open.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use ironrdp::rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_core::{decode_cursor, Encode, EncodeResult, ReadCursor, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use tracing::{debug, info, warn};

/// The listener this client registers, which is the name the server opens ([MS-RDPEAI] 2.1).
pub const CHANNEL_NAME: &str = "AUDIO_INPUT";

const MSG_VERSION: u8 = 0x01;
const MSG_FORMATS: u8 = 0x02;
const MSG_OPEN: u8 = 0x03;
const MSG_OPEN_REPLY: u8 = 0x04;
const MSG_DATA_INCOMING: u8 = 0x05;
const MSG_DATA: u8 = 0x06;
const MSG_FORMAT_CHANGE: u8 = 0x07;

/// Version 1. Version 2 differs only in when a Format Change may be sent, and nothing here
/// needs the difference.
const VERSION: u32 = 1;

/// `S_OK`, and the one HRESULT that means the microphone opened.
const S_OK: u32 = 0;
/// `E_FAIL`, for when it did not.
const E_FAIL: u32 = 0x8000_4005;

/// Every sample is two bytes: the protocol says so where it sizes a packet as
/// `nChannels * 2 * FramesPerPacket`, and so only 16-bit formats are offered.
const BYTES_PER_SAMPLE: usize = 2;

/// One audio input PDU, which is a message id and then whatever that message carries.
struct Pdu {
    message: u8,
    body: Vec<u8>,
}

impl DvcEncode for Pdu {}

impl Encode for Pdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ironrdp_core::ensure_size!(in: dst, size: self.size());
        dst.write_u8(self.message);
        dst.write_slice(&self.body);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "SNDIN_PDU"
    }

    fn size(&self) -> usize {
        1 + self.body.len()
    }
}

impl Pdu {
    fn new(message: u8, body: Vec<u8>) -> DvcMessage {
        Box::new(Self { message, body }) as DvcMessage
    }

    /// A PDU whose whole body is one little-endian word, which is most of them.
    fn word(message: u8, value: u32) -> DvcMessage {
        Self::new(message, value.to_le_bytes().to_vec())
    }
}

/// What the microphone is doing.
struct Capture {
    /// Set to stop the stream; the thread that owns it is watching.
    stop: Arc<AtomicBool>,
    /// When it started, how much it has produced, and when that was last remarked on -- a
    /// microphone that has drifted off real time is otherwise invisible until someone
    /// complains that the session sounds wrong.
    since: std::time::Instant,
    reported: std::time::Instant,
    produced: usize,
    format_rate: u32,
    format_channels: u16,
    /// Captured audio, in the format that was negotiated, waiting to be sent.
    recorded: Arc<Mutex<Vec<u8>>>,
    /// How much of it makes one Data PDU.
    packet_bytes: usize,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The client half of MS-RDPEAI: one microphone, on the channel the server opened for it.
pub struct AudioInput {
    channel_id: Option<u32>,
    /// The formats offered to the server, which is the list its indices refer to.
    offered: Vec<AudioFormat>,
    capture: Option<Capture>,
}

impl Default for AudioInput {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioInput {
    pub fn new() -> Self {
        Self {
            channel_id: None,
            offered: Vec::new(),
            capture: None,
        }
    }

    /// The channel the server opened, once it has opened one.
    pub fn channel_id(&self) -> Option<u32> {
        self.channel_id
    }

    /// The audio recorded since this was last asked, as PDUs ready for the channel.
    ///
    /// Only whole packets are sent: the server sized them and expects that size.
    pub fn take_recorded(&mut self) -> Vec<DvcMessage> {
        let Some(capture) = self.capture.as_mut() else {
            return Vec::new();
        };
        let Ok(mut recorded) = capture.recorded.lock() else {
            return Vec::new();
        };

        let packets = recorded.len() / capture.packet_bytes;
        if packets == 0 {
            return Vec::new();
        }

        let taken: Vec<u8> = recorded
            .drain(..packets * capture.packet_bytes)
            .collect();
        drop(recorded);

        capture.produced += taken.len();
        if capture.reported.elapsed() > std::time::Duration::from_secs(5) {
            capture.reported = std::time::Instant::now();
            let elapsed = capture.since.elapsed().as_secs_f64();
            let frames = capture.produced as f64
                / (capture.format_channels as usize * BYTES_PER_SAMPLE) as f64;
            debug!(
                measured = frames / elapsed,
                promised = capture.format_rate,
                "🎤 capture rate"
            );
        }

        let mut messages = Vec::with_capacity(packets * 2);
        for packet in taken.chunks(capture.packet_bytes) {
            // Every Data PDU is announced by an Incoming Data PDU, which is how the server
            // times the transfer ([MS-RDPEAI] 3.2.5.2.1).
            messages.push(Pdu::new(MSG_DATA_INCOMING, Vec::new()));
            messages.push(Pdu::new(MSG_DATA, packet.to_vec()));
        }
        messages
    }

    /// Answers the server's format list with the ones this machine's microphone can record.
    fn on_formats(&mut self, body: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let offered = match decode_formats(body) {
            Ok(offered) => offered,
            Err(error) => {
                warn!(%error, "🎤 could not read the server's audio formats");
                return Ok(Vec::new());
            }
        };

        self.offered = offered
            .into_iter()
            .filter(|format| recordable(format))
            .collect();
        info!(
            formats = self.offered.len(),
            "🎤 microphone formats agreed with the server"
        );

        let mut body = Vec::new();
        body.extend_from_slice(&(self.offered.len() as u32).to_le_bytes());
        let formats: usize = self.offered.iter().map(Encode::size).sum();
        // The whole PDU, less the ExtraData this client does not send: the header, the two
        // words, and the formats themselves.
        body.extend_from_slice(&((1 + 4 + 4 + formats) as u32).to_le_bytes());
        for format in &self.offered {
            body.extend_from_slice(&ironrdp_core::encode_vec(format).map_err(|e| {
                ironrdp_pdu::encode_err!(e)
            })?);
        }

        // The Sound Formats PDU is announced the same way a Data PDU is ([MS-RDPEAI] 3.2.5.1.4).
        Ok(vec![
            Pdu::new(MSG_DATA_INCOMING, Vec::new()),
            Pdu::new(MSG_FORMATS, body),
        ])
    }

    /// Starts recording in the format the server named, and says whether it worked.
    fn on_open(&mut self, body: &[u8]) -> PduResult<Vec<DvcMessage>> {
        if body.len() < 8 {
            warn!("🎤 the server's Open PDU was too short to read");
            return Ok(Vec::new());
        }
        let frames_per_packet = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
        let initial_format = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);

        let result = match self.offered.get(initial_format as usize).cloned() {
            Some(format) => self.start(&format, frames_per_packet),
            None => {
                warn!(initial_format, "🎤 the server asked for a format never offered");
                Err("no such format".to_owned())
            }
        };

        let status = match result {
            Ok(()) => S_OK,
            Err(error) => {
                warn!(%error, "🎤 could not start recording");
                E_FAIL
            }
        };

        // The format is confirmed before the result, and with the index the server used
        // ([MS-RDPEAI] 3.2.5.1.6).
        Ok(vec![
            Pdu::word(MSG_FORMAT_CHANGE, initial_format),
            Pdu::word(MSG_OPEN_REPLY, status),
        ])
    }

    /// Moves to another of the agreed formats, and says so.
    fn on_format_change(&mut self, body: &[u8]) -> PduResult<Vec<DvcMessage>> {
        if body.len() < 4 {
            return Ok(Vec::new());
        }
        let new_format = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        let packet_bytes = self.capture.as_ref().map(|capture| capture.packet_bytes);

        if let (Some(format), Some(packet_bytes)) =
            (self.offered.get(new_format as usize).cloned(), packet_bytes)
        {
            let frames = packet_bytes / (format.n_channels as usize * BYTES_PER_SAMPLE).max(1);
            if let Err(error) = self.start(&format, frames) {
                warn!(%error, "🎤 could not change recording format");
            }
        }

        Ok(vec![Pdu::word(MSG_FORMAT_CHANGE, new_format)])
    }

    /// Opens the microphone at exactly the format that was agreed.
    fn start(&mut self, format: &AudioFormat, frames_per_packet: usize) -> Result<(), String> {
        // Dropping the old one stops it first, so the device is free.
        self.capture = None;

        let channels = format.n_channels;
        let sample_rate = format.n_samples_per_sec;
        let packet_bytes = frames_per_packet * channels as usize * BYTES_PER_SAMPLE;
        if packet_bytes == 0 {
            return Err("a packet of no audio at all".to_owned());
        }

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (started, wait) = std::sync::mpsc::channel();

        {
            let recorded = Arc::clone(&recorded);
            let stop = Arc::clone(&stop);
            // A cpal stream is not Send on every host, so it lives and dies on its own thread.
            std::thread::Builder::new()
                .name("microphone".to_owned())
                .spawn(move || match open_microphone(channels, sample_rate, &recorded) {
                    Ok(stream) => {
                        let _ = started.send(Ok(()));
                        while !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        drop(stream);
                        debug!("🎤 microphone released");
                    }
                    Err(error) => {
                        let _ = started.send(Err(error));
                    }
                })
                .map_err(|e| e.to_string())?;
        }

        wait.recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "the microphone did not answer".to_owned())??;

        info!(
            channels,
            sample_rate, frames_per_packet, "🎤 recording for the session"
        );
        self.capture = Some(Capture {
            stop,
            since: std::time::Instant::now(),
            reported: std::time::Instant::now(),
            produced: 0,
            format_rate: sample_rate,
            format_channels: channels,
            recorded,
            packet_bytes,
        });
        Ok(())
    }
}

impl ironrdp_core::AsAny for AudioInput {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for AudioInput {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel_id, "🎤 the session asked for the microphone");
        self.channel_id = Some(channel_id);
        // The server speaks first, with its version.
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let Some((&message, body)) = payload.split_first() else {
            return Ok(Vec::new());
        };

        match message {
            MSG_VERSION => {
                debug!("🎤 version from the server");
                Ok(vec![Pdu::word(MSG_VERSION, VERSION)])
            }
            MSG_FORMATS => self.on_formats(body),
            MSG_OPEN => self.on_open(body),
            MSG_FORMAT_CHANGE => self.on_format_change(body),
            // Out-of-sequence and unrecognised PDUs are to be ignored ([MS-RDPEAI] 3.1.5).
            other => {
                debug!(message = other, "🎤 ignoring an audio input PDU");
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel_id, "🎤 the session let the microphone go");
        self.channel_id = None;
        self.capture = None;
    }
}

/// Whether this client is willing to record in a format the server offered.
///
/// Only uncompressed 16-bit PCM: it is the one format the protocol requires everyone to
/// support, and encoding into anything else would mean carrying a codec for the sake of a
/// microphone.
fn recordable(format: &AudioFormat) -> bool {
    format.format == WaveFormat::PCM
        && format.bits_per_sample == 16
        && (1..=2).contains(&format.n_channels)
}

/// Reads a Sound Formats PDU body: how many formats, a size to ignore, then the formats.
fn decode_formats(body: &[u8]) -> Result<Vec<AudioFormat>, String> {
    if body.len() < 8 {
        return Err("shorter than its own header".to_owned());
    }
    let count = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    // cbSizeFormatsPacket is reserved in this direction and is to be ignored.
    let mut src = ReadCursor::new(&body[8..]);

    let mut formats = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        match decode_cursor::<AudioFormat>(&mut src) {
            Ok(format) => formats.push(format),
            // The list may be followed by ExtraData, and a short read is where it starts.
            Err(_) => break,
        }
    }
    Ok(formats)
}

/// Finds the microphone to record from: the one the preferences name, or this machine's default.
///
/// A name usually matches more than one device -- ALSA offers the same card raw, through its
/// converting plug layer, and through whatever sound server is running -- and the raw one often
/// records at one rate only. So a match that can record at the rate agreed with the server is
/// preferred over a match that merely comes first. The channel count is not part of the
/// question: that is mixed to suit.
fn choose_microphone(sample_rate: u32) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    let wanted = crate::preferences::Preferences::load().microphone;
    let wanted = wanted.trim().to_lowercase();

    if !wanted.is_empty() {
        let mut names = Vec::new();
        let mut exact = None;
        let mut fallback = None;
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let name = device.name().unwrap_or_default();
                let lowered = name.to_lowercase();
                if !lowered.contains(&wanted) {
                    names.push(name);
                    continue;
                }
                // A name written out in full means that device and no other. ALSA lists the
                // same card several ways and only some of them convert, so a person who has
                // worked out which one they want must be able to say so exactly.
                if lowered == wanted {
                    info!(device = %name, "🎤 using the microphone the preferences name");
                    return Ok(device);
                }
                if records_at(&device, sample_rate) {
                    exact.get_or_insert(device);
                } else {
                    fallback.get_or_insert(device);
                }
                names.push(name);
            }
        }
        if let Some(device) = exact {
            info!(device = %device.name().unwrap_or_default(), "🎤 using the microphone the preferences name");
            return Ok(device);
        }
        if let Some(device) = fallback {
            info!(
                device = %device.name().unwrap_or_default(),
                sample_rate,
                "🎤 the named microphone may not manage this rate, trying it anyway"
            );
            return Ok(device);
        }
        warn!(
            wanted,
            available = ?names,
            "🎤 no microphone matches, falling back to the default"
        );
    }

    let device = host
        .default_input_device()
        .ok_or_else(|| "this machine has no microphone".to_owned())?;
    debug!(device = %device.name().unwrap_or_default(), "🎤 using the default microphone");
    Ok(device)
}

/// Opens the microphone at one exact format, writing what it hears into `recorded`.
fn open_microphone(
    channels: u16,
    sample_rate: u32,
    recorded: &Arc<Mutex<Vec<u8>>>,
) -> Result<cpal::Stream, String> {
    let device = choose_microphone(sample_rate)?;

    // A device advertises the same rate in several sample formats, and the order it lists them
    // in means nothing. Signed 16-bit is what the session is promised, so take that when it is
    // on offer and convert from the next best thing when it is not.
    let preference = [
        cpal::SampleFormat::I16,
        cpal::SampleFormat::F32,
        cpal::SampleFormat::I32,
        cpal::SampleFormat::U16,
        cpal::SampleFormat::U8,
    ];

    // The channel count is the device's to decide. A headset records one channel and the
    // session may have asked for two; that is a question of mixing, not a reason to refuse.
    // The sample rate is not negotiable in the same way -- resampling is a different job -- so
    // only formats at exactly the rate agreed are considered.
    let at_rate: Vec<_> = device
        .supported_input_configs()
        .map_err(|e| e.to_string())?
        .filter(|range| {
            range.min_sample_rate().0 <= sample_rate && sample_rate <= range.max_sample_rate().0
        })
        .collect();
    // What a device advertises and what it will do are not always the same -- ALSA's
    // converting plug layer is exactly the case where they differ -- so an empty list is a
    // reason to try rather than to refuse.
    if at_rate.is_empty() {
        debug!(sample_rate, "🎤 the microphone advertises no such rate; trying anyway");
        return build_stream(
            &device,
            &cpal::StreamConfig {
                channels,
                sample_rate: cpal::SampleRate(sample_rate),
                buffer_size: cpal::BufferSize::Default,
            },
            cpal::SampleFormat::I16,
            (channels, channels),
            recorded,
        );
    }

    let device_channels = if at_rate.iter().any(|range| range.channels() == channels) {
        channels
    } else {
        let closest = at_rate
            .iter()
            .map(|range| range.channels())
            .min_by_key(|have| have.abs_diff(channels))
            .unwrap_or(channels);
        info!(
            wanted = channels,
            recording = closest,
            "🎤 the microphone has a different number of channels, mixing to suit"
        );
        closest
    };

    let sample_format = preference
        .into_iter()
        .find(|wanted| {
            at_rate
                .iter()
                .any(|range| range.channels() == device_channels && range.sample_format() == *wanted)
        })
        .ok_or_else(|| "the microphone offers no sample format this can read".to_owned())?;

    let config = cpal::StreamConfig {
        channels: device_channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let mix = (device_channels, channels);

    build_stream(&device, &config, sample_format, mix, recorded)
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mix: (u16, u16),
    recorded: &Arc<Mutex<Vec<u8>>>,
) -> Result<cpal::Stream, String> {
    let on_error = |error| warn!(%error, "🎤 microphone stream error");

    // Whatever the device gives, the session is promised signed 16-bit samples.
    let stream = match sample_format {
        cpal::SampleFormat::I16 => {
            let recorded = Arc::clone(recorded);
            device.build_input_stream(
                config,
                move |samples: &[i16], _| append(&recorded, samples.iter().copied(), mix),
                on_error,
                None,
            )
        }
        cpal::SampleFormat::I32 => {
            let recorded = Arc::clone(recorded);
            device.build_input_stream(
                config,
                move |samples: &[i32], _| {
                    append(&recorded, samples.iter().map(|s| (*s >> 16) as i16), mix)
                },
                on_error,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let recorded = Arc::clone(recorded);
            device.build_input_stream(
                config,
                move |samples: &[u16], _| {
                    append(
                        &recorded,
                        samples.iter().map(|s| (*s as i32 - 32768) as i16),
                        mix,
                    )
                },
                on_error,
                None,
            )
        }
        cpal::SampleFormat::U8 => {
            let recorded = Arc::clone(recorded);
            device.build_input_stream(
                config,
                move |samples: &[u8], _| {
                    append(
                        &recorded,
                        samples.iter().map(|s| ((*s as i16 - 128) << 8)),
                        mix,
                    )
                },
                on_error,
                None,
            )
        }
        cpal::SampleFormat::F32 => {
            let recorded = Arc::clone(recorded);
            device.build_input_stream(
                config,
                move |samples: &[f32], _| {
                    append(
                        &recorded,
                        samples
                            .iter()
                            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16),
                        mix,
                    )
                },
                on_error,
                None,
            )
        }
        other => return Err(format!("the microphone records {other:?}, which is not 16-bit")),
    }
    .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Whether a device can record at exactly this rate.
fn records_at(device: &cpal::Device, sample_rate: u32) -> bool {
    device.supported_input_configs().is_ok_and(|mut configs| {
        configs.any(|range| {
            range.min_sample_rate().0 <= sample_rate && sample_rate <= range.max_sample_rate().0
        })
    })
}

/// How much recorded audio may wait before the oldest is dropped: about two seconds at the
/// highest format offered. A session that stops reading is not a reason to grow without bound.
const RECORDED_LIMIT: usize = 44_100 * 2 * BYTES_PER_SAMPLE * 2;

/// Appends what was heard, in the channel count that was promised.
///
/// `mix` is what the device gives and what the session expects. When they differ the frame is
/// averaged to one value and written out as many times as the session wants -- a headset's one
/// channel arriving as two identical ones, or a stereo pair arriving as their average.
fn append(recorded: &Arc<Mutex<Vec<u8>>>, samples: impl Iterator<Item = i16>, mix: (u16, u16)) {
    let Ok(mut recorded) = recorded.lock() else {
        return;
    };

    let (from, to) = mix;
    if from == to {
        for sample in samples {
            recorded.extend_from_slice(&sample.to_le_bytes());
        }
    } else {
        let from = usize::from(from.max(1));
        let mut frame = 0i32;
        let mut seen = 0usize;
        for sample in samples {
            frame += i32::from(sample);
            seen += 1;
            if seen == from {
                let averaged = (frame / from as i32) as i16;
                for _ in 0..to {
                    recorded.extend_from_slice(&averaged.to_le_bytes());
                }
                frame = 0;
                seen = 0;
            }
        }
    }
    if recorded.len() > RECORDED_LIMIT {
        let excess = recorded.len() - RECORDED_LIMIT;
        recorded.drain(..excess);
    }
}
