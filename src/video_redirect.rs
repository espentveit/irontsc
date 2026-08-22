//! Video Redirection Manager
//!
//! Coordinates the three Video Redirection channels:
//! - Control: Manages video presentations
//! - Data: Receives H.264 video samples
//! - Geometry: Tracks video region positions

use anyhow::{Context, Result};
use ironrdp_geometry::{MappedGeometry, Rectangle};
use ironrdp_video::{PresentationRequest, VideoData};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

#[cfg(feature = "video-redirection")]
use ironrdp_h264::{AvcKind, FfmpegDecoder, H264Decoder};

use core::num::NonZeroU16;

use crate::rdp::{ImageRegion, RdpEventSender, RdpOutputEvent};

/// Presentation context for an active video stream
#[derive(Debug)]
struct PresentationContext {
    pub presentation_id: u8,
    pub geometry_mapping_id: u64,
    pub source_width: u32,
    pub source_height: u32,
    pub scaled_width: u32,
    pub scaled_height: u32,
    pub frame_rate: u8,
    pub is_h264: bool,
    pub extra_data: Vec<u8>, // SPS/PPS for H.264
    pub geometry: Option<MappedGeometry>,

    // Sample reassembly for fragmented packets
    pub pending_sample: Option<PendingSample>,
}

#[derive(Debug)]
struct PendingSample {
    pub sample_number: u32,
    pub packets_expected: u16,
    pub packets_received: u16,
    pub fragments: Vec<Vec<u8>>,
    pub timestamp: u64,
    pub duration: u64,
    pub is_keyframe: bool,
}

/// Thread-safe wrapper for FFmpeg decoder
#[cfg(feature = "video-redirection")]
struct SendFfmpegDecoder(FfmpegDecoder);

#[cfg(feature = "video-redirection")]
unsafe impl Send for SendFfmpegDecoder {}

/// Video Redirection Manager
pub struct VideoRedirectionManager {
    /// Active presentations indexed by presentation ID
    presentations: HashMap<u8, PresentationContext>,

    /// Geometry mappings indexed by mapping ID
    geometries: HashMap<u64, MappedGeometry>,

    /// H.264 decoder (optional, requires feature)
    #[cfg(feature = "video-redirection")]
    h264_decoder: Option<SendFfmpegDecoder>,

    /// Event sender for UI updates
    event_sender: Box<dyn RdpEventSender>,

    /// How big the desktop is, which a decoded frame needs to know: a video is delivered as a
    /// rectangle to paint somewhere on that desktop, and the size of the whole is part of
    /// saying where.
    surface: (u16, u16),
}

impl VideoRedirectionManager {
    pub fn new(event_sender: Box<dyn RdpEventSender>, h264_hw_accel: bool) -> Result<Self> {
        #[cfg(feature = "video-redirection")]
        let h264_decoder = {
            match FfmpegDecoder::new(h264_hw_accel) {
                Ok(decoder) => {
                    info!("✅ Video Redirection: H.264 decoder initialized");
                    Some(SendFfmpegDecoder(decoder))
                }
                Err(e) => {
                    warn!(
                        "⚠️ Video Redirection: H.264 decoder initialization failed: {}",
                        e
                    );
                    None
                }
            }
        };

        Ok(Self {
            presentations: HashMap::new(),
            geometries: HashMap::new(),
            #[cfg(feature = "video-redirection")]
            h264_decoder,
            event_sender,
            surface: (0, 0),
        })
    }

    /// Points the manager at the real event loop, once there is one.
    ///
    /// Until then the channels are registered and answer the server, but a decoded frame has
    /// nowhere to go.
    pub fn set_event_sender(&mut self, sender: Box<dyn RdpEventSender>) {
        self.event_sender = sender;
    }

    /// Tells the manager how big the desktop is, and again whenever it changes.
    pub fn set_surface_size(&mut self, width: u16, height: u16) {
        self.surface = (width, height);
    }

    /// Handle a presentation request from the control channel
    pub fn handle_presentation_request(&mut self, request: PresentationRequest) -> Result<()> {
        info!(
            "🎬 Video Redirection: Presentation {} command={:?} resolution={}x{} fps={} geometry_id={}",
            request.presentation_id,
            request.command,
            request.source_width,
            request.source_height,
            request.frame_rate,
            request.geometry_mapping_id
        );

        match request.command {
            ironrdp_video::PresentationCommand::Start => {
                self.start_presentation(request)?;
            }
            ironrdp_video::PresentationCommand::Stop => {
                self.stop_presentation(request.presentation_id)?;
            }
        }

        Ok(())
    }

    /// Start a new video presentation
    fn start_presentation(&mut self, request: PresentationRequest) -> Result<()> {
        // Check if it's H.264
        let is_h264 = request.video_subtype_id == ironrdp_video::H264_GUID;

        if !is_h264 {
            warn!(
                "⚠️ Video Redirection: Unsupported video format for presentation {}",
                request.presentation_id
            );
            return Ok(());
        }

        // Look up geometry if available
        let geometry = self.geometries.get(&request.geometry_mapping_id).cloned();

        if geometry.is_none() {
            warn!(
                "⚠️ Video Redirection: Geometry {} not found for presentation {}",
                request.geometry_mapping_id, request.presentation_id
            );
        }

        let context = PresentationContext {
            presentation_id: request.presentation_id,
            geometry_mapping_id: request.geometry_mapping_id,
            source_width: request.source_width,
            source_height: request.source_height,
            scaled_width: request.scaled_width,
            scaled_height: request.scaled_height,
            frame_rate: request.frame_rate,
            is_h264,
            extra_data: request.extra_data.clone(),
            geometry,
            pending_sample: None,
        };

        self.presentations.insert(request.presentation_id, context);

        debug!(
            "📹 Video Redirection: Presentation {} started (H.264, SPS/PPS: {} bytes)",
            request.presentation_id,
            request.extra_data.len()
        );

        Ok(())
    }

    /// Stop a video presentation
    fn stop_presentation(&mut self, presentation_id: u8) -> Result<()> {
        if self.presentations.remove(&presentation_id).is_some() {
            info!(
                "🛑 Video Redirection: Presentation {} stopped",
                presentation_id
            );
        }
        Ok(())
    }

    /// Handle geometry update from geometry channel
    pub fn handle_geometry(&mut self, geometry: MappedGeometry) -> Result<()> {
        info!(
            "📐 Video Redirection: Geometry {} update type={:?} bounds=({},{} {}x{})",
            geometry.mapping_id,
            geometry.update_type,
            geometry.left,
            geometry.top,
            geometry.bounds().width(),
            geometry.bounds().height()
        );

        match geometry.update_type {
            ironrdp_geometry::UpdateType::Update => {
                // Update geometry
                self.geometries
                    .insert(geometry.mapping_id, geometry.clone());

                // Update any presentations using this geometry
                for presentation in self.presentations.values_mut() {
                    if presentation.geometry_mapping_id == geometry.mapping_id {
                        presentation.geometry = Some(geometry.clone());
                        debug!(
                            "📹 Video Redirection: Updated geometry for presentation {}",
                            presentation.presentation_id
                        );
                    }
                }
            }
            ironrdp_geometry::UpdateType::Clear => {
                // Remove geometry
                self.geometries.remove(&geometry.mapping_id);

                // Clear geometry from presentations
                for presentation in self.presentations.values_mut() {
                    if presentation.geometry_mapping_id == geometry.mapping_id {
                        presentation.geometry = None;
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle video data from data channel
    pub fn handle_video_data(&mut self, data: VideoData) -> Result<()> {
        debug!(
            "📥 Video Redirection: Video data presentation={} sample={} fragment={}/{} keyframe={} bytes={}",
            data.presentation_id,
            data.sample_number,
            data.current_packet_index + 1,
            data.packets_in_sample,
            data.flags.is_keyframe(),
            data.sample_data.len()
        );

        // Handle sample assembly (fragmentation)
        let complete_sample = {
            let presentation = match self.presentations.get_mut(&data.presentation_id) {
                Some(p) => p,
                None => {
                    warn!(
                        "⚠️ Video Redirection: Unknown presentation {} for video data",
                        data.presentation_id
                    );
                    return Ok(());
                }
            };

            if data.is_complete() {
                // Single-packet sample
                Some((
                    data.sample_data.clone(),
                    data.timestamp,
                    data.duration,
                    data.flags.is_keyframe(),
                ))
            } else {
                // Multi-packet sample - reassemble
                Self::reassemble_sample(presentation, &data)?
            }
        };

        // Decode and render if we have a complete sample
        if let Some((sample_data, timestamp, duration, is_keyframe)) = complete_sample {
            // Extract geometry before calling decode_and_render to avoid borrowing issues
            let geometry = self
                .presentations
                .get(&data.presentation_id)
                .and_then(|p| p.geometry.clone());

            let presentation_id = data.presentation_id;

            self.decode_and_render(
                presentation_id,
                geometry,
                &sample_data,
                timestamp,
                duration,
                is_keyframe,
            )?;
        }

        Ok(())
    }

    /// Reassemble fragmented video samples
    fn reassemble_sample(
        presentation: &mut PresentationContext,
        data: &VideoData,
    ) -> Result<Option<(Vec<u8>, u64, u64, bool)>> {
        // Initialize pending sample if this is the first fragment
        if data.is_first_fragment() {
            presentation.pending_sample = Some(PendingSample {
                sample_number: data.sample_number,
                packets_expected: data.packets_in_sample,
                packets_received: 0,
                fragments: vec![Vec::new(); data.packets_in_sample as usize],
                timestamp: data.timestamp,
                duration: data.duration,
                is_keyframe: data.flags.is_keyframe(),
            });
        }

        let pending = match &mut presentation.pending_sample {
            Some(p) if p.sample_number == data.sample_number => p,
            _ => {
                warn!(
                    "⚠️ Video Redirection: Fragment mismatch for presentation {}",
                    presentation.presentation_id
                );
                return Ok(None);
            }
        };

        // Store fragment
        pending.fragments[data.current_packet_index as usize] = data.sample_data.clone();
        pending.packets_received += 1;

        // Check if complete
        if pending.packets_received == pending.packets_expected {
            // Extract values before clearing pending_sample
            let packets_expected = pending.packets_expected;
            let complete_sample: Vec<u8> = pending.fragments.iter().flatten().copied().collect();
            let timestamp = pending.timestamp;
            let duration = pending.duration;
            let is_keyframe = pending.is_keyframe;

            // Now clear pending_sample after we're done borrowing it
            presentation.pending_sample = None;

            debug!(
                "✅ Video Redirection: Assembled sample {} ({} bytes from {} fragments)",
                data.sample_number,
                complete_sample.len(),
                packets_expected
            );

            return Ok(Some((complete_sample, timestamp, duration, is_keyframe)));
        }

        Ok(None)
    }

    /// Decode H.264 sample and render to screen
    #[cfg(feature = "video-redirection")]
    fn decode_and_render(
        &mut self,
        presentation_id: u8,
        geometry: Option<MappedGeometry>,
        sample_data: &[u8],
        _timestamp: u64,
        _duration: u64,
        is_keyframe: bool,
    ) -> Result<()> {
        let decoder = match &mut self.h264_decoder {
            Some(d) => d,
            None => {
                warn!("⚠️ Video Redirection: H.264 decoder not available");
                return Ok(());
            }
        };

        let geometry = match &geometry {
            Some(g) => g,
            None => {
                warn!(
                    "⚠️ Video Redirection: No geometry for presentation {}",
                    presentation_id
                );
                return Ok(());
            }
        };

        debug!(
            "🎬 Video Redirection: Decoding H.264 sample ({} bytes, keyframe={})",
            sample_data.len(),
            is_keyframe
        );

        // Decode H.264 frame
        let frame = decoder
            .0
            .decode_gfx_stream(AvcKind::Avc420, sample_data, None)
            .context("Failed to decode H.264 frame")?;

        debug!(
            "✅ Video Redirection: Decoded frame {}x{} format={:?}",
            frame.width, frame.height, frame.format
        );

        let bounds = geometry.bounds();
        let (surface_width, surface_height) = self.surface;
        if surface_width == 0 || surface_height == 0 {
            warn!("⚠️ Video Redirection: desktop size not known yet, dropping a frame");
            return Ok(());
        }

        // The video window can hang off the edge of the desktop, or be scrolled partly out of
        // it. Only the part that is on the desktop can be painted.
        let left = bounds.left.max(0);
        let top = bounds.top.max(0);
        let right = bounds.right.min(i32::from(surface_width));
        let bottom = bounds.bottom.min(i32::from(surface_height));
        if right <= left || bottom <= top {
            debug!("🎬 Video Redirection: the video is entirely off the desktop");
            return Ok(());
        }

        let region_width = (right - left) as usize;
        let region_height = (bottom - top) as usize;

        let stride = *frame
            .line_sizes
            .first()
            .ok_or_else(|| anyhow::anyhow!("decoded frame has no stride"))?;
        let plane = frame
            .planes
            .first()
            .ok_or_else(|| anyhow::anyhow!("decoded frame has no data"))?;
        if !matches!(frame.format, ironrdp_h264::PixelFormat::Bgra) {
            anyhow::bail!("expected a BGRA frame, got {:?}", frame.format);
        }

        // The decoder gives the video at its own size; the geometry says how big it is on the
        // desktop. Nearest neighbour is enough here -- the scale is usually 1:1, and when it is
        // not, the alternative is carrying a resampler for a picture that is already lossy.
        let mut region = vec![0u8; region_width * region_height * 4];
        let frame_width = frame.width as usize;
        let frame_height = frame.height as usize;
        let scaled_width = bounds.width().max(1) as usize;
        let scaled_height = bounds.height().max(1) as usize;
        let skipped_x = (left - bounds.left) as usize;
        let skipped_y = (top - bounds.top) as usize;

        for y in 0..region_height {
            let source_y = ((y + skipped_y) * frame_height / scaled_height).min(frame_height - 1);
            for x in 0..region_width {
                let source_x = ((x + skipped_x) * frame_width / scaled_width).min(frame_width - 1);
                let from = source_y * stride + source_x * 4;
                let to = (y * region_width + x) * 4;
                if let (Some(pixel), Some(slot)) =
                    (plane.get(from..from + 4), region.get_mut(to..to + 4))
                {
                    slot.copy_from_slice(pixel);
                }
            }
        }

        let (Some(width), Some(height)) = (
            NonZeroU16::new(surface_width),
            NonZeroU16::new(surface_height),
        ) else {
            return Ok(());
        };
        let (Some(region_w), Some(region_h)) = (
            NonZeroU16::new(region_width as u16),
            NonZeroU16::new(region_height as u16),
        ) else {
            return Ok(());
        };

        debug!(
            "🖼️ Video Redirection: painting {}x{} at ({},{})",
            region_width, region_height, left, top
        );

        self.event_sender
            .send_event(RdpOutputEvent::Image {
                buffer: Arc::new(region),
                width,
                height,
                region: Some(ImageRegion {
                    x: left as u16,
                    y: top as u16,
                    width: region_w,
                    height: region_h,
                }),
            })
            .map_err(|_| anyhow::anyhow!("could not hand a video frame to the window"))?;

        Ok(())
    }

    #[cfg(not(feature = "video-redirection"))]
    fn decode_and_render(
        &mut self,
        _presentation_id: u8,
        _geometry: Option<MappedGeometry>,
        _sample_data: &[u8],
        _timestamp: u64,
        _duration: u64,
        _is_keyframe: bool,
    ) -> Result<()> {
        warn!("⚠️ Video Redirection: H.264 decoder not compiled in");
        Ok(())
    }
}

/// Thread-safe shared manager
pub type SharedVideoRedirectionManager = Arc<Mutex<VideoRedirectionManager>>;
