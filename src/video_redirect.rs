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

use crate::rdp::{RdpEventSender, RdpOutputEvent};

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
        })
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
            .decode_gfx_stream(AvcKind::Avc420, sample_data)
            .context("Failed to decode H.264 frame")?;

        debug!(
            "✅ Video Redirection: Decoded frame {}x{} format={:?}",
            frame.width, frame.height, frame.format
        );

        // TODO: Composite onto RDP bitmap using geometry bounds
        // For now, we'll just log that we decoded successfully
        let bounds = geometry.bounds();
        info!(
            "🖼️ Video Redirection: Would render {}x{} frame to position ({},{}) size={}x{}",
            frame.width,
            frame.height,
            bounds.left,
            bounds.top,
            bounds.width(),
            bounds.height()
        );

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
