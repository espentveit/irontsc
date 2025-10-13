//! RDPEGFX (Graphics Virtual Channel Extension) integration
//!
//! This module implements RDPEGFX support with H.264 decoding, allowing
//! the client to receive and render high-quality graphics from the server.

use anyhow::{Result, Context as _};
use ironrdp_gfx::{GfxClient, GfxContext, codec};
#[cfg(feature = "h264")]
use ironrdp_h264::{H264Decoder, FfmpegDecoder, AvcKind};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

use crate::rdp::{RdpOutputEvent, RdpEventSender};
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

/// GFX client state
pub struct GfxState {
    /// H.264 decoder (optional, requires h264 feature)
    #[cfg(feature = "h264")]
    h264_decoder: FfmpegDecoder,
    /// Active surfaces
    surfaces: HashMap<u16, GfxSurface>,
    /// Event sender for UI updates
    event_sender: Box<dyn RdpEventSender>,
    /// Outgoing message buffer
    outgoing_buffer: Vec<Vec<u8>>,
}

impl GfxState {
    pub fn new(event_sender: Box<dyn RdpEventSender>) -> Result<Self> {
        #[cfg(feature = "h264")]
        let h264_decoder = FfmpegDecoder::new()
            .context("Failed to initialize H.264 decoder - ensure FFmpeg libraries are installed")?;

        #[cfg(feature = "h264")]
        debug!("Initialized RDPEGFX client with H.264 support");

        #[cfg(not(feature = "h264"))]
        debug!("Initialized RDPEGFX client without H.264 support");

        Ok(Self {
            #[cfg(feature = "h264")]
            h264_decoder,
            surfaces: HashMap::new(),
            event_sender,
            outgoing_buffer: Vec::new(),
        })
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
        debug!(
            surface_id,
            width,
            height,
            pixel_format,
            "Creating GFX surface"
        );

        // Delete old surface if it exists (protocol allows reuse)
        self.surfaces.remove(&surface_id);

        // Create new surface with BGRA buffer
        let buffer_size = (width as usize) * (height as usize) * 4; // BGRA = 4 bytes per pixel
        let surface = GfxSurface {
            id: surface_id,
            width,
            height,
            pixel_format,
            buffer: vec![0; buffer_size],
        };

        self.surfaces.insert(surface_id, surface);
        Ok(())
    }

    fn on_delete_surface(&mut self, surface_id: u16) -> Result<()> {
        debug!(surface_id, "Deleting GFX surface");
        self.surfaces.remove(&surface_id);
        Ok(())
    }

    fn on_start_frame(&mut self, frame_id: u32, timestamp: u32) -> Result<()> {
        trace!(frame_id, timestamp, "GFX frame start");
        Ok(())
    }

    fn on_end_frame(&mut self, frame_id: u32) -> Result<()> {
        trace!(frame_id, "GFX frame end");

        // Send all modified surfaces to UI
        // For now, we'll send the primary surface (typically surface 0)
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
        dest_rect: ironrdp_gfx::pdu::Rectangle,
        bitmap_data: &[u8],
    ) -> Result<()> {
        trace!(
            surface_id,
            codec_id,
            "GFX surface command: {}x{} at ({}, {})",
            dest_rect.width(),
            dest_rect.height(),
            dest_rect.left,
            dest_rect.top
        );

        let surface = self.surfaces.get_mut(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        match codec_id {
            #[cfg(feature = "h264")]
            codec::codec_id::AVC420 => {
                // Decode H.264/AVC420
                let frame = self.h264_decoder.decode_gfx_stream(
                    AvcKind::Avc420,
                    bitmap_data,
                ).context("Failed to decode AVC420 frame")?;

                Self::blit_frame_to_surface(surface, &dest_rect, &frame)?;
            }
            #[cfg(feature = "h264")]
            codec::codec_id::AVC444 | codec::codec_id::AVC444V2 => {
                // Decode H.264/AVC444
                let kind = if codec_id == codec::codec_id::AVC444 {
                    AvcKind::Avc444
                } else {
                    AvcKind::Avc444v2
                };

                let frame = self.h264_decoder.decode_gfx_stream(kind, bitmap_data)
                    .context("Failed to decode AVC444 frame")?;

                Self::blit_frame_to_surface(surface, &dest_rect, &frame)?;
            }
            codec::codec_id::UNCOMPRESSED => {
                // Raw BGRA bitmap
                Self::blit_raw_to_surface(surface, &dest_rect, bitmap_data)?;
            }
            _ => {
                #[cfg(not(feature = "h264"))]
                if matches!(codec_id, codec::codec_id::AVC420 | codec::codec_id::AVC444 | codec::codec_id::AVC444V2) {
                    warn!("H.264 codec not supported - rebuild with 'h264' feature enabled");
                } else {
                    warn!("Unsupported codec: 0x{:04X} ({})", codec_id, codec::codec_name(codec_id));
                }

                #[cfg(feature = "h264")]
                warn!("Unsupported codec: 0x{:04X} ({})", codec_id, codec::codec_name(codec_id));
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
        trace!(surface_id, codec_id, codec_context_id, "GFX full surface command");

        // Full surface update (typically for progressive codecs like RFX)
        let surface = self.surfaces.get(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        // Create full-surface rect
        let dest_rect = ironrdp_gfx::pdu::Rectangle {
            left: 0,
            top: 0,
            right: surface.width,
            bottom: surface.height,
        };

        self.on_surface_command(surface_id, codec_id, pixel_format, dest_rect, bitmap_data)
    }

    fn on_solid_fill(
        &mut self,
        surface_id: u16,
        fill_pixel: ironrdp_gfx::pdu::Color32,
        fill_rects: &[ironrdp_gfx::pdu::Rectangle],
    ) -> Result<()> {
        trace!(surface_id, "GFX solid fill: {} rects", fill_rects.len());

        let surface = self.surfaces.get_mut(&surface_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown surface: {}", surface_id))?;

        // Fill each rectangle with the solid color
        for rect in fill_rects {
            Self::fill_rect(surface, rect, fill_pixel)?;
        }

        Ok(())
    }
}

impl GfxState {
    /// Blit a decoded H.264 frame to a surface
    #[cfg(feature = "h264")]
    fn blit_frame_to_surface(
        surface: &mut GfxSurface,
        dest_rect: &ironrdp_gfx::pdu::Rectangle,
        frame: &ironrdp_h264::DecodedFrame,
    ) -> Result<()> {
        // Ensure frame format is BGRA
        if !matches!(frame.format, ironrdp_h264::PixelFormat::Bgra) {
            anyhow::bail!("Expected BGRA frame format, got {:?}", frame.format);
        }

        if frame.planes.is_empty() {
            anyhow::bail!("Decoded frame has no data");
        }

        let frame_data = &frame.planes[0];
        let frame_stride = frame.line_sizes[0];

        // Blit to surface buffer
        let rect_width = dest_rect.width() as usize;
        let rect_height = dest_rect.height() as usize;
        let surface_width = surface.width as usize;

        for y in 0..rect_height {
            let src_offset = y * frame_stride;
            let dst_y = dest_rect.top as usize + y;
            let dst_x = dest_rect.left as usize;
            let dst_offset = (dst_y * surface_width + dst_x) * 4;

            if dst_offset + rect_width * 4 <= surface.buffer.len()
                && src_offset + rect_width * 4 <= frame_data.len()
            {
                surface.buffer[dst_offset..dst_offset + rect_width * 4]
                    .copy_from_slice(&frame_data[src_offset..src_offset + rect_width * 4]);
            }
        }

        Ok(())
    }

    /// Blit raw BGRA data to a surface
    fn blit_raw_to_surface(
        surface: &mut GfxSurface,
        dest_rect: &ironrdp_gfx::pdu::Rectangle,
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

            if dst_offset + rect_width * 4 <= surface.buffer.len() {
                surface.buffer[dst_offset..dst_offset + rect_width * 4]
                    .copy_from_slice(&data[src_offset..src_offset + rect_width * 4]);
            }
        }

        Ok(())
    }

    /// Fill a rectangle with a solid color
    fn fill_rect(
        surface: &mut GfxSurface,
        rect: &ironrdp_gfx::pdu::Rectangle,
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

    /// Send a surface to the UI for rendering
    fn send_surface_to_ui(&self, surface: &GfxSurface) -> Result<()> {
        let width = NonZeroU16::new(surface.width)
            .ok_or_else(|| anyhow::anyhow!("Surface width is zero"))?;
        let height = NonZeroU16::new(surface.height)
            .ok_or_else(|| anyhow::anyhow!("Surface height is zero"))?;

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
}
