//! RDPEGFX (Graphics Virtual Channel Extension) integration
//!
//! This module implements RDPEGFX support with H.264 decoding, allowing
//! the client to receive and render high-quality graphics from the server.

use anyhow::{Context as _, Result, ensure};
use clearcodec::ClearCodec;
use ironrdp_gfx::pdu::{MonitorDefinition, Point16, Rectangle};
use ironrdp_gfx::{GfxContext, codec};
use ironrdp_graphics::progressive::ProgressiveDecoder;
#[cfg(feature = "h264")]
use ironrdp_h264::{AvcKind, FfmpegDecoder, H264Decoder};
use ironrdp_pdu::codecs::rfx::EntropyAlgorithm;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use tracing::{debug, trace, warn};

/// Maximum surface dimension (8K resolution)
const MAX_SURFACE_DIM: u16 = 8192;
/// Maximum total surface pixels (8K × 8K)
const MAX_SURFACE_PIXELS: usize = 8192 * 8192;

use crate::rdp::{RdpEventSender, RdpOutputEvent};
use core::num::NonZeroU16;

/// GFX surface information
#[derive(Debug, Clone)]
struct GfxSurface {
    id: u16,
    width: u16,
    height: u16,
    pixel_format: u8,
    /// Buffer for decoded frames (BGRA format)
    buffer: Vec<u8>,
}

#[derive(Debug, Clone)]
struct CachedBitmap {
    key: u64,
    width: u16,
    height: u16,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct GraphicsOutput {
    width: u16,
    height: u16,
    buffer: Vec<u8>,
    monitors: Vec<MonitorDefinition>,
}

impl GraphicsOutput {
    fn new(width: u16, height: u16, monitors: &[MonitorDefinition]) -> Self {
        let mut output = Self {
            width,
            height,
            buffer: vec![0; (width as usize) * (height as usize) * 4],
            monitors: monitors.to_vec(),
        };
        output.clear();
        output
    }

    fn resize(&mut self, width: u16, height: u16, monitors: &[MonitorDefinition]) {
        self.width = width;
        self.height = height;
        self.monitors = monitors.to_vec();
        self.buffer
            .resize((width as usize) * (height as usize) * 4, 0);
        self.clear();
    }

    fn clear(&mut self) {
        self.buffer.fill(0);
    }
}

#[derive(Debug, Clone)]
struct SurfaceOutputMapping {
    surface_id: u16,
    output_origin_x: u32,
    output_origin_y: u32,
    target_width: u32,
    target_height: u32,
}

#[derive(Debug, Clone)]
struct WindowMapping {
    surface_id: u16,
    mapped_width: u32,
    mapped_height: u32,
    target_size: Option<(u32, u32)>,
}

/// Unsafe Send wrapper for FFmpeg decoder (it's not actually sent between threads)
#[cfg(feature = "h264")]
struct SendFfmpegDecoder(FfmpegDecoder);

#[cfg(feature = "h264")]
unsafe impl Send for SendFfmpegDecoder {}

/// GFX client state
pub struct GfxState {
    /// ClearCodec decoder
    clearcodec_decoder: ClearCodec,
    /// Progressive codec decoder
    progressive_decoder: ProgressiveDecoder,
    /// H.264 decoder (optional, requires h264 feature)
    #[cfg(feature = "h264")]
    h264_decoder: SendFfmpegDecoder,
    /// Active surfaces
    surfaces: HashMap<u16, GfxSurface>,
    /// Graphics output buffer (ResetGraphics)
    graphics_output: Option<GraphicsOutput>,
    /// Surface to graphics output mappings
    surface_output_mappings: Vec<SurfaceOutputMapping>,
    /// Surface to window mappings
    window_mappings: HashMap<u64, WindowMapping>,
    /// Active progressive codec contexts
    active_codec_contexts: HashSet<(u16, u32)>,
    /// Bitmap cache entries indexed by slot
    bitmap_cache: HashMap<u16, CachedBitmap>,
    /// Event sender for UI updates
    event_sender: Box<dyn RdpEventSender>,
    /// Outgoing message buffer
    outgoing_buffer: Vec<Vec<u8>>,
}

impl GfxState {
    pub fn new(event_sender: Box<dyn RdpEventSender>) -> Result<Self> {
        use tracing::info;

        #[cfg(feature = "h264")]
        let h264_decoder = {
            info!("🎬 Initializing FFmpeg H.264 decoder...");
            let decoder = FfmpegDecoder::new().context(
                "Failed to initialize H.264 decoder - ensure FFmpeg libraries are installed",
            )?;
            info!("✅ FFmpeg H.264 decoder initialized successfully");
            SendFfmpegDecoder(decoder)
        };

        #[cfg(feature = "h264")]
        info!("✅ RDPEGFX GfxState initialized with H.264 support");

        #[cfg(not(feature = "h264"))]
        info!("⚠️ RDPEGFX GfxState initialized WITHOUT H.264 support");

        info!("🎨 Initializing RFX Progressive decoder with RLGR1 entropy...");

        Ok(Self {
            clearcodec_decoder: ClearCodec::new(),
            progressive_decoder: ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1),
            #[cfg(feature = "h264")]
            h264_decoder,
            surfaces: HashMap::new(),
            graphics_output: None,
            surface_output_mappings: Vec::new(),
            window_mappings: HashMap::new(),
            active_codec_contexts: HashSet::new(),
            bitmap_cache: HashMap::new(),
            event_sender,
            outgoing_buffer: Vec::new(),
        })
    }

    /// Replace the event sender (useful for switching from dummy to real sender after connection)
    pub fn set_event_sender(&mut self, event_sender: Box<dyn RdpEventSender>) {
        self.event_sender = event_sender;
    }

    /// Get pending outgoing messages and clear the buffer
    pub fn take_outgoing_messages(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outgoing_buffer)
    }
}

impl GfxContext for GfxState {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        // Buffer the outgoing message
        // The caller will retrieve these via take_outgoing_messages() and send them
        self.outgoing_buffer.push(data.to_vec());
        trace!("Buffered {} bytes for sending via GFX channel", data.len());
        Ok(())
    }

    fn on_create_surface(
        &mut self,
        surface_id: u16,
        width: u16,
        height: u16,
        pixel_format: u8,
    ) -> Result<()> {
        use tracing::info;

        info!(
            "🖼️ RDPEGFX: CREATE_SURFACE id={} size={}x{} format=0x{:02X}",
            surface_id, width, height, pixel_format
        );

        // Validate surface dimensions to prevent DoS via huge allocations
        if width > MAX_SURFACE_DIM || height > MAX_SURFACE_DIM {
            anyhow::bail!(
                "Surface dimensions too large: {}x{} (max: {}x{})",
                width,
                height,
                MAX_SURFACE_DIM,
                MAX_SURFACE_DIM
            );
        }

        let pixels = (width as usize) * (height as usize);
        if pixels > MAX_SURFACE_PIXELS {
            anyhow::bail!(
                "Surface area too large: {} pixels (max: {})",
                pixels,
                MAX_SURFACE_PIXELS
            );
        }

        // Delete old surface if it exists (protocol allows reuse)
        self.surfaces.remove(&surface_id);

        // Create new surface with BGRA buffer
        let buffer_size = pixels * 4; // BGRA = 4 bytes per pixel
        let surface = GfxSurface {
            id: surface_id,
            width,
            height,
            pixel_format,
            buffer: vec![0; buffer_size],
        };

        self.surfaces.insert(surface_id, surface);

        // Register surface with progressive decoder
        self.progressive_decoder
            .reset_surface(surface_id, width as u32, height as u32);

        self.refresh_output_mapping_dimensions(surface_id, width, height);
        Ok(())
    }

    fn on_delete_surface(&mut self, surface_id: u16) -> Result<()> {
        debug!("Deleting GFX surface {}", surface_id);
        if self.surfaces.remove(&surface_id).is_some() {
            self.surface_output_mappings
                .retain(|mapping| mapping.surface_id != surface_id);
            self.window_mappings
                .retain(|_, mapping| mapping.surface_id != surface_id);
            self.active_codec_contexts
                .retain(|(surf, _)| *surf != surface_id);

            // Remove surface from progressive decoder
            self.progressive_decoder.remove_surface(surface_id);
        }
        Ok(())
    }

    fn on_start_frame(&mut self, frame_id: u32, timestamp: u32) -> Result<()> {
        trace!(
            "GFX frame start frame_id={} timestamp={}",
            frame_id, timestamp
        );
        Ok(())
    }

    fn on_end_frame(&mut self, frame_id: u32) -> Result<()> {
        trace!("GFX frame end frame_id={}", frame_id);

        if let Some(output) = self.graphics_output.as_mut() {
            if self.surface_output_mappings.is_empty() {
                if let Some(surface) = self.surfaces.get(&0) {
                    self.send_surface_to_ui(surface)?;
                }
            } else {
                output.clear();

                for mapping in &self.surface_output_mappings {
                    match self.surfaces.get(&mapping.surface_id) {
                        Some(surface) => {
                            Self::blit_surface_to_output(output, surface, mapping)?;
                        }
                        None => {
                            warn!(
                                "Output mapping references unknown surface {}",
                                mapping.surface_id
                            );
                        }
                    }
                }
            }

            if !self.surface_output_mappings.is_empty() {
                if let Some(output) = self.graphics_output.as_ref() {
                    self.send_graphics_output_to_ui(output)?;
                }
            }

            return Ok(());
        }

        // Fallback: send the primary surface (typically surface 0)
        if let Some(surface) = self.surfaces.get(&0) {
            self.send_surface_to_ui(surface)?;
        }

        Ok(())
    }

    fn on_surface_command(
        &mut self,
        surface_id: u16,
        codec_id: u16,
        pixel_format: u8,
        dest_rect: Rectangle,
        bitmap_data: &[u8],
    ) -> Result<()> {
        use tracing::debug;

        debug!(
            "🎨 RDPEGFX: WIRE_TO_SURFACE surface={} codec=0x{:04X} ({}) rect={}x{} at ({},{}) data={} bytes (first 32: {:02X?})",
            surface_id,
            codec_id,
            codec::codec_name(codec_id),
            dest_rect.width(),
            dest_rect.height(),
            dest_rect.left,
            dest_rect.top,
            bitmap_data.len(),
            &bitmap_data[..bitmap_data.len().min(32)]
        );

        let surface = self
            .surfaces
            .get_mut(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        match codec_id {
            codec::codec_id::CLEARCODEC => {
                debug!(
                    "🧹 Decoding ClearCodec frame ({} bytes)...",
                    bitmap_data.len()
                );

                // Decode ClearCodec
                let width = dest_rect.width() as u32;
                let height = dest_rect.height() as u32;
                let mut decoded_buffer = vec![0u8; (width * height * 4) as usize];

                self.clearcodec_decoder
                    .decompress(bitmap_data, width, height, &mut decoded_buffer)
                    .context("Failed to decode ClearCodec frame")?;

                debug!("✅ ClearCodec decode complete, blitting to surface");
                Self::blit_raw_to_surface(surface, &dest_rect, &decoded_buffer)?;
            }
            #[cfg(feature = "h264")]
            codec::codec_id::AVC420 => {
                debug!(
                    "🎬 Decoding H.264/AVC420 frame ({} bytes)...",
                    bitmap_data.len()
                );
                // Decode H.264/AVC420
                let frame = self
                    .h264_decoder
                    .0
                    .decode_gfx_stream(AvcKind::Avc420, bitmap_data)
                    .context("Failed to decode AVC420 frame")?;

                debug!("✅ H.264 decode complete, blitting to surface");
                Self::blit_frame_to_surface(surface, &dest_rect, frame)?;
            }
            #[cfg(feature = "h264")]
            codec::codec_id::AVC444 | codec::codec_id::AVC444V2 => {
                // Decode H.264/AVC444
                let kind = if codec_id == codec::codec_id::AVC444 {
                    AvcKind::Avc444
                } else {
                    AvcKind::Avc444v2
                };

                match self.h264_decoder.0.decode_gfx_stream(kind, bitmap_data) {
                    Ok(frame) => {
                        Self::blit_frame_to_surface(surface, &dest_rect, frame)?;
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        // Skip frames that can't be decoded (empty, corrupted, or unsupported)
                        if err_msg.contains("zero length")
                            || err_msg.contains("empty frame")
                            || err_msg.contains("Failed to receive frame")
                            || err_msg.contains("Failed to send packet")
                            || err_msg.contains("End of file")
                            || err_msg.contains("Invalid data found")
                        {
                            debug!("⏭️ Skipping AVC444 frame that can't be decoded: {}", e);
                        } else {
                            // Other errors are still fatal
                            return Err(e).context("Failed to decode AVC444 frame");
                        }
                    }
                }
            }
            codec::codec_id::UNCOMPRESSED => {
                // Raw BGRA bitmap
                Self::blit_raw_to_surface(surface, &dest_rect, bitmap_data)?;
            }
            codec::codec_id::RFX_PROGRESSIVE | codec::codec_id::RFX_PROGRESSIVE_V2 => {
                // RFX Progressive codec - use the progressive decoder
                use tracing::debug;

                debug!(
                    "🎨 RFX Progressive codec (0x{:04X}) decoding {} bytes for surface {}",
                    codec_id,
                    bitmap_data.len(),
                    surface_id
                );

                // Decode the progressive stream
                match self.progressive_decoder.decode_surface_update(
                    surface_id,
                    0, // frame_index_hint
                    bitmap_data,
                ) {
                    Ok(update) => {
                        debug!(
                            "✅ Progressive decode successful: {} tiles decoded for frame {}",
                            update.tiles.len(),
                            update.frame_index
                        );

                        // Blit each tile to the surface
                        for tile in &update.tiles {
                            Self::blit_tile_to_surface(surface, tile)
                                .context("Failed to blit progressive tile")?;
                        }
                    }
                    Err(e) => {
                        warn!("❌ Progressive decode failed: {:?}", e);
                        // Fill with error indicator color (dark red)
                        Self::fill_rect(
                            surface,
                            &dest_rect,
                            ironrdp_gfx::pdu::Color32 {
                                b: 0,
                                g: 0,
                                r: 64,
                                xa: 255,
                            },
                        )?;
                    }
                }
            }
            _ => {
                #[cfg(not(feature = "h264"))]
                if matches!(
                    codec_id,
                    codec::codec_id::AVC420 | codec::codec_id::AVC444 | codec::codec_id::AVC444V2
                ) {
                    warn!("H.264 codec not supported - rebuild with 'h264' feature enabled");
                } else {
                    warn!(
                        "Unsupported codec: 0x{:04X} ({})",
                        codec_id,
                        codec::codec_name(codec_id)
                    );
                }

                #[cfg(feature = "h264")]
                warn!(
                    "Unsupported codec: 0x{:04X} ({})",
                    codec_id,
                    codec::codec_name(codec_id)
                );
            }
        }

        // Send update to UI if we have surface mappings and graphics output
        // This handles cases where END_FRAME might not be sent immediately
        if self.graphics_output.is_some() && !self.surface_output_mappings.is_empty() {
            // Check if this surface is mapped to output
            if self
                .surface_output_mappings
                .iter()
                .any(|m| m.surface_id == surface_id)
            {
                debug!(
                    "📺 Updating graphics output with decoded surface {}",
                    surface_id
                );
                let output = self.graphics_output.as_mut().unwrap();
                output.clear();
                for mapping in &self.surface_output_mappings {
                    if let Some(surf) = self.surfaces.get(&mapping.surface_id) {
                        Self::blit_surface_to_output(output, surf, mapping)?;
                    }
                }
                // Send output to UI after blitting is complete
                let output_ref = self.graphics_output.as_ref().unwrap();
                self.send_graphics_output_to_ui(output_ref)?;
            }
        } else {
            // Fallback: send surface directly if no graphics output composition
            debug!("📺 Sending surface {} directly to UI", surface_id);
            if let Some(surf) = self.surfaces.get(&surface_id) {
                self.send_surface_to_ui(surf)?;
            }
        }

        Ok(())
    }

    fn on_surface_command_full(
        &mut self,
        surface_id: u16,
        codec_id: u16,
        codec_context_id: u32,
        pixel_format: u8,
        bitmap_data: &[u8],
    ) -> Result<()> {
        trace!(
            "GFX full surface command surface={} codec={} codec_context={}",
            surface_id, codec_id, codec_context_id
        );

        // Full surface update (typically for progressive codecs like RFX)
        let (right, bottom) = {
            let surface = self
                .surfaces
                .get(&surface_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;
            (surface.width, surface.height)
        };

        let dest_rect = Rectangle {
            left: 0,
            top: 0,
            right,
            bottom,
        };

        self.on_surface_command(surface_id, codec_id, pixel_format, dest_rect, bitmap_data)?;
        self.track_codec_context(surface_id, codec_context_id);

        Ok(())
    }

    fn on_solid_fill(
        &mut self,
        surface_id: u16,
        fill_pixel: ironrdp_gfx::pdu::Color32,
        fill_rects: &[Rectangle],
    ) -> Result<()> {
        trace!(
            "GFX solid fill: {} rects (surface={})",
            fill_rects.len(),
            surface_id
        );

        let surface = self
            .surfaces
            .get_mut(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        // Fill each rectangle with the solid color
        for rect in fill_rects {
            Self::fill_rect(surface, rect, fill_pixel)?;
        }

        Ok(())
    }

    fn on_surface_to_surface(
        &mut self,
        source_surface_id: u16,
        destination_surface_id: u16,
        source_rect: Rectangle,
        dest_points: &[Point16],
    ) -> Result<()> {
        trace!(
            "GFX surface to surface: src={} dst={} points={}",
            source_surface_id,
            destination_surface_id,
            dest_points.len()
        );

        if dest_points.is_empty() {
            return Ok(());
        }

        let region_width = usize::from(source_rect.width());
        let region_height = usize::from(source_rect.height());

        if region_width == 0 || region_height == 0 {
            anyhow::bail!("SurfaceToSurface with zero-sized region");
        }

        if source_surface_id == destination_surface_id {
            let surface = self
                .surfaces
                .get_mut(&source_surface_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", source_surface_id))?;

            Self::validate_rect_within_surface(surface, &source_rect)?;
            let region = Self::extract_surface_region(surface, &source_rect)?;

            for point in dest_points {
                let (dest_x, dest_y) =
                    Self::ensure_region_fits(surface, *point, region_width, region_height)?;
                Self::write_region_to_surface(
                    surface,
                    dest_x,
                    dest_y,
                    region_width,
                    region_height,
                    &region,
                )?;
            }

            return Ok(());
        }

        let region = {
            let source_surface = self
                .surfaces
                .get(&source_surface_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown source surface: {}", source_surface_id))?;
            Self::validate_rect_within_surface(source_surface, &source_rect)?;
            Self::extract_surface_region(source_surface, &source_rect)?
        };
        let dest_surface = self
            .surfaces
            .get_mut(&destination_surface_id)
            .ok_or_else(|| {
                anyhow::anyhow!("Unknown destination surface: {}", destination_surface_id)
            })?;

        for point in dest_points {
            let (dest_x, dest_y) =
                Self::ensure_region_fits(dest_surface, *point, region_width, region_height)?;
            Self::write_region_to_surface(
                dest_surface,
                dest_x,
                dest_y,
                region_width,
                region_height,
                &region,
            )?;
        }

        Ok(())
    }

    fn on_surface_to_cache(
        &mut self,
        surface_id: u16,
        cache_key: u64,
        cache_slot: u16,
        source_rect: Rectangle,
    ) -> Result<()> {
        trace!(
            "GFX surface to cache: surface={} slot={} key=0x{:016X}",
            surface_id, cache_slot, cache_key
        );

        let surface = self
            .surfaces
            .get(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        Self::validate_rect_within_surface(surface, &source_rect)?;
        let data = Self::extract_surface_region(surface, &source_rect)?;

        let cached = CachedBitmap {
            key: cache_key,
            width: source_rect.width(),
            height: source_rect.height(),
            data,
        };

        self.bitmap_cache.insert(cache_slot, cached);
        Ok(())
    }

    fn on_cache_to_surface(
        &mut self,
        cache_slot: u16,
        surface_id: u16,
        dest_points: &[Point16],
    ) -> Result<()> {
        trace!(
            "GFX cache to surface: slot={} surface={} points={}",
            cache_slot,
            surface_id,
            dest_points.len()
        );

        if dest_points.is_empty() {
            return Ok(());
        }

        let cached = self
            .bitmap_cache
            .get(&cache_slot)
            .ok_or_else(|| anyhow::anyhow!("Unknown bitmap cache slot: {}", cache_slot))?;

        let surface = self
            .surfaces
            .get_mut(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        let region_width = usize::from(cached.width);
        let region_height = usize::from(cached.height);

        if cached.data.len() < region_width * region_height * 4 {
            anyhow::bail!(
                "Cached bitmap data too small: have {}, expected {}",
                cached.data.len(),
                region_width * region_height * 4
            );
        }

        for point in dest_points {
            let (dest_x, dest_y) =
                Self::ensure_region_fits(surface, *point, region_width, region_height)?;
            Self::write_region_to_surface(
                surface,
                dest_x,
                dest_y,
                region_width,
                region_height,
                &cached.data,
            )?;
        }

        Ok(())
    }

    fn on_evict_cache_entry(&mut self, cache_slot: u16) -> Result<()> {
        trace!("GFX evict cache slot {}", cache_slot);
        self.bitmap_cache.remove(&cache_slot);
        Ok(())
    }

    fn on_cache_import_reply(&mut self, imported_slots: &[u16]) -> Result<()> {
        trace!(
            "GFX cache import reply received: {} slots",
            imported_slots.len()
        );
        Ok(())
    }

    fn on_delete_encoding_context(&mut self, surface_id: u16, codec_context_id: u32) -> Result<()> {
        if self
            .active_codec_contexts
            .remove(&(surface_id, codec_context_id))
        {
            trace!(
                "Removed codec context surface={} context={}",
                surface_id, codec_context_id
            );
        } else {
            debug!(
                "DeleteEncodingContext for unknown codec context surface={} context={}",
                surface_id, codec_context_id
            );
        }
        Ok(())
    }

    fn on_reset_graphics(
        &mut self,
        width: u32,
        height: u32,
        monitors: &[MonitorDefinition],
    ) -> Result<()> {
        use tracing::info;

        let width_u16 = u16::try_from(width).context("RESET_GRAPHICS width exceeds u16 range")?;
        let height_u16 =
            u16::try_from(height).context("RESET_GRAPHICS height exceeds u16 range")?;

        ensure!(
            width_u16 != 0 && height_u16 != 0,
            "RESET_GRAPHICS with zero dimensions"
        );

        info!(
            "🖥️ RDPEGFX: RESET_GRAPHICS width={} height={} monitors={}",
            width,
            height,
            monitors.len()
        );

        match self.graphics_output.as_mut() {
            Some(output) => output.resize(width_u16, height_u16, monitors),
            None => {
                self.graphics_output = Some(GraphicsOutput::new(width_u16, height_u16, monitors));
            }
        }

        self.surface_output_mappings.clear();
        self.window_mappings.clear();
        Ok(())
    }

    fn on_map_surface_to_output(
        &mut self,
        surface_id: u16,
        output_origin_x: u32,
        output_origin_y: u32,
    ) -> Result<()> {
        use tracing::info;

        let (target_width, target_height) = self
            .surfaces
            .get(&surface_id)
            .map(|surface| (u32::from(surface.width), u32::from(surface.height)))
            .unwrap_or_else(|| {
                warn!(
                    "MAP_SURFACE_TO_OUTPUT before surface creation; deferring dimensions (surface={})",
                    surface_id
                );
                (0, 0)
            });

        info!(
            "🧭 RDPEGFX: MAP_SURFACE_TO_OUTPUT surface={} origin=({}, {}) target={}x{}",
            surface_id, output_origin_x, output_origin_y, target_width, target_height
        );

        let mapping = SurfaceOutputMapping {
            surface_id,
            output_origin_x,
            output_origin_y,
            target_width,
            target_height,
        };
        self.upsert_output_mapping(mapping);
        Ok(())
    }

    fn on_map_surface_to_scaled_output(
        &mut self,
        surface_id: u16,
        output_origin_x: u32,
        output_origin_y: u32,
        target_width: u32,
        target_height: u32,
    ) -> Result<()> {
        use tracing::info;

        if target_width == 0 || target_height == 0 {
            warn!(
                "MAP_SURFACE_TO_SCALED_OUTPUT with zero-sized target ({}x{}) for surface={}",
                target_width, target_height, surface_id
            );
            return Ok(());
        }

        info!(
            "🧭 RDPEGFX: MAP_SURFACE_TO_SCALED_OUTPUT surface={} origin=({}, {}) target={}x{}",
            surface_id, output_origin_x, output_origin_y, target_width, target_height
        );

        let mapping = SurfaceOutputMapping {
            surface_id,
            output_origin_x,
            output_origin_y,
            target_width,
            target_height,
        };
        self.upsert_output_mapping(mapping);
        Ok(())
    }

    fn on_map_surface_to_window(
        &mut self,
        surface_id: u16,
        window_id: u64,
        mapped_width: u32,
        mapped_height: u32,
    ) -> Result<()> {
        use tracing::info;

        info!(
            "🪟 RDPEGFX: MAP_SURFACE_TO_WINDOW surface={} window=0x{:016X} mapped={}x{}",
            surface_id, window_id, mapped_width, mapped_height
        );

        let mapping = WindowMapping {
            surface_id,
            mapped_width,
            mapped_height,
            target_size: None,
        };
        self.window_mappings.insert(window_id, mapping);
        Ok(())
    }

    fn on_map_surface_to_scaled_window(
        &mut self,
        surface_id: u16,
        window_id: u64,
        mapped_width: u32,
        mapped_height: u32,
        target_width: u32,
        target_height: u32,
    ) -> Result<()> {
        use tracing::info;

        if target_width == 0 || target_height == 0 {
            warn!(
                "MAP_SURFACE_TO_SCALED_WINDOW with zero-sized target ({}x{}) for surface={} window=0x{:016X}",
                target_width, target_height, surface_id, window_id
            );
            return Ok(());
        }

        info!(
            "🪟 RDPEGFX: MAP_SURFACE_TO_SCALED_WINDOW surface={} window=0x{:016X} mapped={}x{} target={}x{}",
            surface_id, window_id, mapped_width, mapped_height, target_width, target_height
        );

        let mapping = WindowMapping {
            surface_id,
            mapped_width,
            mapped_height,
            target_size: Some((target_width, target_height)),
        };
        self.window_mappings.insert(window_id, mapping);
        Ok(())
    }
}

impl GfxState {
    /// Blit a decoded H.264 frame to a surface
    #[cfg(feature = "h264")]
    fn blit_frame_to_surface(
        surface: &mut GfxSurface,
        dest_rect: &Rectangle,
        frame: ironrdp_h264::DecodedFrame,
    ) -> Result<()> {
        // Ensure frame format is BGRA
        let ironrdp_h264::DecodedFrame {
            width: frame_width,
            height: frame_height,
            format,
            planes,
            line_sizes,
        } = frame;

        if !matches!(format, ironrdp_h264::PixelFormat::Bgra) {
            anyhow::bail!("Expected BGRA frame format, got {:?}", format);
        }

        if planes.is_empty() {
            anyhow::bail!("Decoded frame has no data");
        }

        let frame_stride = *line_sizes
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("Decoded frame missing stride information"))?;
        let rect_width = dest_rect.width() as usize;
        let rect_height = dest_rect.height() as usize;
        let surface_width = surface.width as usize;
        let frame_width_usize = frame_width as usize;
        let frame_height_usize = frame_height as usize;

        // Fast path: full-surface update with tightly packed BGRA buffer.
        let is_full_surface = dest_rect.left == 0
            && dest_rect.top == 0
            && rect_width == surface_width
            && rect_height == surface.height as usize
            && frame_width_usize == rect_width
            && frame_height_usize == rect_height
            && frame_stride == rect_width * 4;

        if is_full_surface {
            if planes.len() != 1 {
                anyhow::bail!(
                    "Expected single-plane BGRA data for fast blit, got {} planes",
                    planes.len()
                );
            }
            surface.buffer = planes.into_iter().next().unwrap_or_default();
            return Ok(());
        }

        let frame_data = planes
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("Decoded frame missing BGRA data plane"))?;

        // Blit to surface buffer
        for y in 0..rect_height {
            let src_offset = y * frame_stride;
            let dst_y = dest_rect.top as usize + y;
            let dst_x = dest_rect.left as usize;
            let dst_offset = (dst_y * surface_width + dst_x) * 4;

            // Fail on bounds violations instead of silently skipping
            if dst_offset + rect_width * 4 > surface.buffer.len() {
                anyhow::bail!(
                    "Blit destination out of bounds: line={}, offset={}, size={}, buffer={}",
                    y,
                    dst_offset,
                    rect_width * 4,
                    surface.buffer.len()
                );
            }
            if src_offset + rect_width * 4 > frame_data.len() {
                anyhow::bail!(
                    "Blit source out of bounds: line={}, offset={}, size={}, frame={}",
                    y,
                    src_offset,
                    rect_width * 4,
                    frame_data.len()
                );
            }

            surface.buffer[dst_offset..dst_offset + rect_width * 4]
                .copy_from_slice(&frame_data[src_offset..src_offset + rect_width * 4]);
        }

        Ok(())
    }

    /// Blit raw BGRA data to a surface
    fn blit_raw_to_surface(
        surface: &mut GfxSurface,
        dest_rect: &Rectangle,
        data: &[u8],
    ) -> Result<()> {
        let rect_width = dest_rect.width() as usize;
        let rect_height = dest_rect.height() as usize;
        let surface_width = surface.width as usize;
        let expected_size = rect_width * rect_height * 4;

        if data.len() < expected_size {
            anyhow::bail!(
                "Not enough raw bitmap data: expected {}, got {}",
                expected_size,
                data.len()
            );
        }

        for y in 0..rect_height {
            let src_offset = y * rect_width * 4;
            let dst_y = dest_rect.top as usize + y;
            let dst_x = dest_rect.left as usize;
            let dst_offset = (dst_y * surface_width + dst_x) * 4;

            // Fail on bounds violations instead of silently skipping
            if dst_offset + rect_width * 4 > surface.buffer.len() {
                anyhow::bail!(
                    "Blit destination out of bounds: line={}, offset={}, size={}, buffer={}",
                    y,
                    dst_offset,
                    rect_width * 4,
                    surface.buffer.len()
                );
            }

            surface.buffer[dst_offset..dst_offset + rect_width * 4]
                .copy_from_slice(&data[src_offset..src_offset + rect_width * 4]);
        }

        Ok(())
    }

    /// Blit a progressive tile to a surface
    fn blit_tile_to_surface(
        surface: &mut GfxSurface,
        tile: &ironrdp_graphics::progressive::TileUpdate,
    ) -> Result<()> {
        let tile_width = tile.rect.width as usize;
        let tile_height = tile.rect.height as usize;
        let surface_width = surface.width as usize;
        let expected_size = tile_width * tile_height * 4;

        if tile.pixels.len() < expected_size {
            anyhow::bail!(
                "Not enough progressive tile data: expected {}, got {}",
                expected_size,
                tile.pixels.len()
            );
        }

        for y in 0..tile_height {
            let src_offset = y * tile_width * 4;
            let dst_y = tile.rect.y as usize + y;
            let dst_x = tile.rect.x as usize;
            let dst_offset = (dst_y * surface_width + dst_x) * 4;

            // Check bounds
            if dst_offset + tile_width * 4 > surface.buffer.len() {
                anyhow::bail!(
                    "Progressive tile blit out of bounds: tile={}x{} at ({},{}), line={}, offset={}, size={}, buffer={}",
                    tile_width,
                    tile_height,
                    tile.rect.x,
                    tile.rect.y,
                    y,
                    dst_offset,
                    tile_width * 4,
                    surface.buffer.len()
                );
            }

            surface.buffer[dst_offset..dst_offset + tile_width * 4]
                .copy_from_slice(&tile.pixels[src_offset..src_offset + tile_width * 4]);
        }

        Ok(())
    }

    /// Fill a rectangle with a solid color
    fn fill_rect(
        surface: &mut GfxSurface,
        rect: &Rectangle,
        color: ironrdp_gfx::pdu::Color32,
    ) -> Result<()> {
        let rect_width = rect.width() as usize;
        let rect_height = rect.height() as usize;
        let surface_width = surface.width as usize;

        // Create BGRA pixel
        let pixel = [color.b, color.g, color.r, color.xa];

        for y in 0..rect_height {
            let dst_y = rect.top as usize + y;
            let dst_x = rect.left as usize;
            let dst_offset = (dst_y * surface_width + dst_x) * 4;

            for x in 0..rect_width {
                let offset = dst_offset + x * 4;
                if offset + 4 <= surface.buffer.len() {
                    surface.buffer[offset..offset + 4].copy_from_slice(&pixel);
                }
            }
        }

        Ok(())
    }

    fn validate_rect_within_surface(surface: &GfxSurface, rect: &Rectangle) -> Result<()> {
        ensure!(
            rect.right <= surface.width,
            "Rectangle right bound {} exceeds surface width {}",
            rect.right,
            surface.width
        );
        ensure!(
            rect.bottom <= surface.height,
            "Rectangle bottom bound {} exceeds surface height {}",
            rect.bottom,
            surface.height
        );
        Ok(())
    }

    fn extract_surface_region(surface: &GfxSurface, rect: &Rectangle) -> Result<Vec<u8>> {
        let width = usize::from(rect.width());
        let height = usize::from(rect.height());

        if width == 0 || height == 0 {
            anyhow::bail!("Cannot extract zero-sized surface region");
        }

        let surface_width = usize::from(surface.width);
        let mut region = Vec::with_capacity(width * height * 4);

        for y in 0..height {
            let src_y = usize::from(rect.top) + y;
            let src_x = usize::from(rect.left);
            let offset = (src_y * surface_width + src_x) * 4;
            let end = offset + width * 4;

            if end > surface.buffer.len() {
                anyhow::bail!(
                    "Surface region out of bounds: line={}, offset={}, size={}, buffer={}",
                    y,
                    offset,
                    width * 4,
                    surface.buffer.len()
                );
            }

            region.extend_from_slice(&surface.buffer[offset..end]);
        }

        Ok(region)
    }

    fn ensure_region_fits(
        surface: &GfxSurface,
        point: Point16,
        width: usize,
        height: usize,
    ) -> Result<(usize, usize)> {
        ensure!(point.x >= 0 && point.y >= 0, "Negative destination point");

        let x = point.x as usize;
        let y = point.y as usize;
        let surface_width = usize::from(surface.width);
        let surface_height = usize::from(surface.height);

        ensure!(
            x + width <= surface_width,
            "Destination region exceeds surface width (x={} width={} surface={})",
            x,
            width,
            surface_width
        );
        ensure!(
            y + height <= surface_height,
            "Destination region exceeds surface height (y={} height={} surface={})",
            y,
            height,
            surface_height
        );

        Ok((x, y))
    }

    fn write_region_to_surface(
        surface: &mut GfxSurface,
        dest_x: usize,
        dest_y: usize,
        width: usize,
        height: usize,
        data: &[u8],
    ) -> Result<()> {
        let row_bytes = width * 4;
        let expected_size = row_bytes * height;

        ensure!(
            data.len() >= expected_size,
            "Not enough data to write region: expected {}, got {}",
            expected_size,
            data.len()
        );

        let surface_width = usize::from(surface.width);

        for y in 0..height {
            let dst_offset = ((dest_y + y) * surface_width + dest_x) * 4;
            let src_offset = y * row_bytes;

            if dst_offset + row_bytes > surface.buffer.len() {
                anyhow::bail!(
                    "Destination write out of bounds: line={}, offset={}, size={}, buffer={}",
                    y,
                    dst_offset,
                    row_bytes,
                    surface.buffer.len()
                );
            }

            surface.buffer[dst_offset..dst_offset + row_bytes]
                .copy_from_slice(&data[src_offset..src_offset + row_bytes]);
        }

        Ok(())
    }

    /// Send a surface to the UI for rendering
    fn send_surface_to_ui(&self, surface: &GfxSurface) -> Result<()> {
        let width = NonZeroU16::new(surface.width)
            .ok_or_else(|| anyhow::anyhow!("Surface width is zero"))?;
        let height = NonZeroU16::new(surface.height)
            .ok_or_else(|| anyhow::anyhow!("Surface height is zero"))?;

        use tracing::info;
        info!(
            "🖼️ Sending surface to UI: {}x{} ({} bytes)",
            width,
            height,
            surface.buffer.len()
        );

        self.event_sender
            .send_event(RdpOutputEvent::Image {
                buffer: surface.buffer.clone(),
                width,
                height,
                region: None,
            })
            .map_err(|_| anyhow::anyhow!("Failed to send image event to UI"))?;

        Ok(())
    }

    fn send_graphics_output_to_ui(&self, output: &GraphicsOutput) -> Result<()> {
        let width = NonZeroU16::new(output.width)
            .ok_or_else(|| anyhow::anyhow!("Graphics output width is zero"))?;
        let height = NonZeroU16::new(output.height)
            .ok_or_else(|| anyhow::anyhow!("Graphics output height is zero"))?;

        use tracing::debug;
        debug!(
            "🖼️ Sending graphics output to UI: {}x{} ({} bytes)",
            width,
            height,
            output.buffer.len()
        );

        self.event_sender
            .send_event(RdpOutputEvent::Image {
                buffer: output.buffer.clone(),
                width,
                height,
                region: None,
            })
            .map_err(|_| anyhow::anyhow!("Failed to send graphics output to UI"))
    }

    fn track_codec_context(&mut self, surface_id: u16, codec_context_id: u32) {
        if codec_context_id != 0 {
            self.active_codec_contexts
                .insert((surface_id, codec_context_id));
        }
    }

    fn upsert_output_mapping(&mut self, mapping: SurfaceOutputMapping) {
        if let Some(existing) = self
            .surface_output_mappings
            .iter_mut()
            .find(|entry| entry.surface_id == mapping.surface_id)
        {
            *existing = mapping;
        } else {
            self.surface_output_mappings.push(mapping);
        }
    }

    fn refresh_output_mapping_dimensions(&mut self, surface_id: u16, width: u16, height: u16) {
        if let Some(mapping) = self
            .surface_output_mappings
            .iter_mut()
            .find(|entry| entry.surface_id == surface_id)
        {
            if mapping.target_width == 0 {
                mapping.target_width = u32::from(width);
            }
            if mapping.target_height == 0 {
                mapping.target_height = u32::from(height);
            }
        }
    }

    fn blit_surface_to_output(
        output: &mut GraphicsOutput,
        surface: &GfxSurface,
        mapping: &SurfaceOutputMapping,
    ) -> Result<()> {
        if mapping.target_width == 0 || mapping.target_height == 0 {
            warn!(
                "Skipping output mapping with zero-sized target for surface={}",
                mapping.surface_id
            );
            return Ok(());
        }

        let origin_x = match usize::try_from(mapping.output_origin_x) {
            Ok(value) => value,
            Err(_) => {
                warn!(
                    "Output origin X ({}) does not fit in usize; skipping surface {}",
                    mapping.output_origin_x, mapping.surface_id
                );
                return Ok(());
            }
        };
        let origin_y = match usize::try_from(mapping.output_origin_y) {
            Ok(value) => value,
            Err(_) => {
                warn!(
                    "Output origin Y ({}) does not fit in usize; skipping surface {}",
                    mapping.output_origin_y, mapping.surface_id
                );
                return Ok(());
            }
        };

        let output_width = usize::from(output.width);
        let output_height = usize::from(output.height);

        if origin_x >= output_width || origin_y >= output_height {
            warn!(
                "Output origin ({}, {}) outside graphics buffer {}x{} for surface {}",
                origin_x, origin_y, output_width, output_height, mapping.surface_id
            );
            return Ok(());
        }

        let target_width_full = match usize::try_from(mapping.target_width) {
            Ok(value) if value > 0 => value,
            _ => {
                warn!(
                    "Target width ({}) invalid for surface {}",
                    mapping.target_width, mapping.surface_id
                );
                return Ok(());
            }
        };
        let target_height_full = match usize::try_from(mapping.target_height) {
            Ok(value) if value > 0 => value,
            _ => {
                warn!(
                    "Target height ({}) invalid for surface {}",
                    mapping.target_height, mapping.surface_id
                );
                return Ok(());
            }
        };

        let mut dest_width = target_width_full;
        let mut dest_height = target_height_full;

        if origin_x + dest_width > output_width {
            dest_width = output_width - origin_x;
        }
        if origin_y + dest_height > output_height {
            dest_height = output_height - origin_y;
        }

        if dest_width == 0 || dest_height == 0 {
            warn!(
                "Output mapping for surface {} results in empty destination area",
                mapping.surface_id
            );
            return Ok(());
        }

        let surface_width = usize::from(surface.width);
        let surface_height = usize::from(surface.height);

        if surface_width == 0 || surface_height == 0 {
            warn!(
                "Surface {} has zero dimensions; skipping output composite",
                mapping.surface_id
            );
            return Ok(());
        }

        let surface_len = surface.buffer.len();
        let output_len = output.buffer.len();

        let is_identity_blit =
            target_width_full == surface_width && target_height_full == surface_height;

        if is_identity_blit {
            for dy in 0..dest_height {
                let src_y = dy.min(surface_height.saturating_sub(1));
                let dst_row = origin_y + dy;
                let src_index = src_y * surface_width * 4;
                let dst_index = (dst_row * output_width + origin_x) * 4;
                let copy_len = dest_width * 4;

                if src_index + copy_len <= surface_len && dst_index + copy_len <= output_len {
                    output.buffer[dst_index..dst_index + copy_len]
                        .copy_from_slice(&surface.buffer[src_index..src_index + copy_len]);
                }
            }
            return Ok(());
        }

        for dy in 0..dest_height {
            let src_y =
                ((dy * surface_height) / target_height_full).min(surface_height.saturating_sub(1));

            for dx in 0..dest_width {
                let src_x =
                    ((dx * surface_width) / target_width_full).min(surface_width.saturating_sub(1));

                let src_index = (src_y * surface_width + src_x) * 4;
                let dst_index = ((origin_y + dy) * output_width + (origin_x + dx)) * 4;

                if src_index + 4 <= surface_len && dst_index + 4 <= output_len {
                    output.buffer[dst_index..dst_index + 4]
                        .copy_from_slice(&surface.buffer[src_index..src_index + 4]);
                }
            }
        }

        Ok(())
    }
}
