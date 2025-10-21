//! RDPEGFX client state machine

use crate::caps::{self, CapabilitySet};
use crate::codec;
use crate::pdu::*;
use anyhow::{Context as _, Result, bail};
use bytes::Buf;
use std::time::Instant;
use tracing::{debug, trace, warn};

/// Maximum number of PDUs to process in a single stream to prevent DoS
const MAX_PDUS_PER_STREAM: usize = 0; // 0 = unlimited

/// Callback interface for GFX events
pub trait GfxContext {
    /// Send data over the GFX dynamic channel (will be ZGFX compressed by caller)
    fn send(&mut self, data: &[u8]) -> Result<()>;

    /// Called when a surface is created
    fn on_create_surface(
        &mut self,
        surface_id: u16,
        width: u16,
        height: u16,
        pixel_format: u8,
    ) -> Result<()>;

    /// Called when a surface is deleted
    fn on_delete_surface(&mut self, surface_id: u16) -> Result<()>;

    /// Called at the start of a frame
    fn on_start_frame(&mut self, frame_id: u32, timestamp: u32) -> Result<()>;

    /// Called at the end of a frame
    fn on_end_frame(&mut self, frame_id: u32) -> Result<()>;

    /// Called when bitmap data arrives (WIRE_TO_SURFACE_1)
    fn on_surface_command(
        &mut self,
        surface_id: u16,
        codec_id: u16,
        pixel_format: u8,
        dest_rect: Rectangle,
        bitmap_data: &[u8],
    ) -> Result<()>;

    /// Called when bitmap data arrives without rect (WIRE_TO_SURFACE_2)
    fn on_surface_command_full(
        &mut self,
        surface_id: u16,
        codec_id: u16,
        codec_context_id: u32,
        pixel_format: u8,
        bitmap_data: &[u8],
    ) -> Result<()>;

    /// Called for solid fill operations
    fn on_solid_fill(
        &mut self,
        surface_id: u16,
        fill_pixel: Color32,
        fill_rects: &[Rectangle],
    ) -> Result<()>;

    /// Called to copy bitmap data between surfaces
    fn on_surface_to_surface(
        &mut self,
        source_surface_id: u16,
        destination_surface_id: u16,
        source_rect: Rectangle,
        dest_points: &[Point16],
    ) -> Result<()>;

    /// Called to store bitmap data from a surface into the cache
    fn on_surface_to_cache(
        &mut self,
        surface_id: u16,
        cache_key: u64,
        cache_slot: u16,
        source_rect: Rectangle,
    ) -> Result<()>;

    /// Called to copy bitmap data from the cache to a surface
    fn on_cache_to_surface(
        &mut self,
        cache_slot: u16,
        surface_id: u16,
        dest_points: &[Point16],
    ) -> Result<()>;

    /// Called when the server evicts a cache entry
    fn on_evict_cache_entry(&mut self, cache_slot: u16) -> Result<()>;

    /// Called when the server confirms persistent cache imports
    fn on_cache_import_reply(&mut self, imported_slots: &[u16]) -> Result<()>;

    /// Called when the server deletes a progressive codec context
    fn on_delete_encoding_context(&mut self, surface_id: u16, codec_context_id: u32) -> Result<()>;

    /// Called when the server resets the graphics output buffer
    fn on_reset_graphics(
        &mut self,
        width: u32,
        height: u32,
        monitors: &[MonitorDefinition],
    ) -> Result<()>;

    /// Called when a surface is mapped to the graphics output buffer
    fn on_map_surface_to_output(
        &mut self,
        surface_id: u16,
        output_origin_x: u32,
        output_origin_y: u32,
    ) -> Result<()>;

    /// Called when a surface is mapped to a scaled region of the graphics output buffer
    fn on_map_surface_to_scaled_output(
        &mut self,
        surface_id: u16,
        output_origin_x: u32,
        output_origin_y: u32,
        target_width: u32,
        target_height: u32,
    ) -> Result<()>;

    /// Called when a surface is mapped to a window
    fn on_map_surface_to_window(
        &mut self,
        surface_id: u16,
        window_id: u64,
        mapped_width: u32,
        mapped_height: u32,
    ) -> Result<()>;

    /// Called when a surface is mapped to a scaled window
    fn on_map_surface_to_scaled_window(
        &mut self,
        surface_id: u16,
        window_id: u64,
        mapped_width: u32,
        mapped_height: u32,
        target_width: u32,
        target_height: u32,
    ) -> Result<()>;
}

/// Frame tracking state
#[derive(Debug)]
struct FrameState {
    frame_id: u32,
    timestamp: u32,
    start_time: Instant,
}

/// RDPEGFX client
pub struct GfxClient<Ctx: GfxContext> {
    /// Context for callbacks and sending
    pub ctx: Ctx,
    /// Negotiated capability version
    cap_version: Option<u32>,
    /// Negotiated capability flags
    cap_flags: u32,
    /// Current frame state
    current_frame: Option<FrameState>,
    /// Total frames decoded
    total_frames_decoded: u32,
    /// Unacknowledged frames
    unacknowledged_frames: u32,
    /// Small cache mode (2560 vs 25600 slots)
    small_cache: bool,
    /// Suspend frame acknowledgements
    suspend_acks: bool,
    /// Send QoE acknowledgements (10.0+)
    send_qoe_acks: bool,
}

impl<Ctx: GfxContext> GfxClient<Ctx> {
    /// Create a new GFX client
    pub fn new(ctx: Ctx, small_cache: bool, suspend_acks: bool) -> Self {
        Self {
            ctx,
            cap_version: None,
            cap_flags: 0,
            current_frame: None,
            total_frames_decoded: 0,
            unacknowledged_frames: 0,
            small_cache,
            suspend_acks,
            send_qoe_acks: true,
        }
    }

    /// Get negotiated capability version
    pub fn cap_version(&self) -> Option<u32> {
        self.cap_version
    }

    /// Build and send CAPS_ADVERTISE message
    pub fn send_caps_advertise(&mut self) -> Result<()> {
        let caps = default_capability_sets(self.small_cache);
        trace!("RDPEGFX CapsAdvertise count={}", caps.len());
        for cap in &caps {
            trace!(
                "  - version=0x{:08X} flags=0x{:08X} extra_len={}",
                cap.version,
                cap.flags,
                cap.extra_data.len()
            );
        }

        let caps_payload_len: usize = caps.iter().map(CapabilitySet::serialized_len).sum();
        let pdu_length = Header::SIZE + 2 + caps_payload_len; // header + count + capsets
        trace!("RDPEGFX CapsAdvertise PDU length={} bytes", pdu_length);
        let mut buf = Vec::with_capacity(pdu_length);

        // Header
        let header = Header {
            cmd_id: CmdId::CapsAdvertise,
            flags: 0,
            pdu_length: pdu_length as u32,
        };
        header.write(&mut buf);

        // Capability set count
        buf.extend_from_slice(&(caps.len() as u16).to_le_bytes());

        // Capability sets
        for cap in &caps {
            buf.extend_from_slice(&cap.to_bytes());
        }

        debug!("Sending CAPS_ADVERTISE with {} capability sets", caps.len());
        self.ctx.send(&buf)
    }

    /// Process a stream of PDUs (already ZGFX decompressed)
    pub fn process_pdu_stream(&mut self, data: &[u8]) -> Result<()> {
        let mut stream = data;
        let mut pdu_count = 0;

        while !stream.is_empty() {
            // Enforce PDU count limit to prevent DoS via many tiny PDUs
            pdu_count += 1;
            if MAX_PDUS_PER_STREAM != 0 && pdu_count > MAX_PDUS_PER_STREAM {
                bail!(
                    "Too many PDUs in stream: {} (max: {})",
                    pdu_count,
                    MAX_PDUS_PER_STREAM
                );
            }

            let start_pos = data.len() - stream.len();

            // Parse header
            let header = Header::parse(&mut stream).context("Failed to parse PDU header")?;

            // Calculate body length
            if header.pdu_length < Header::SIZE as u32 {
                bail!("Invalid PDU length: {}", header.pdu_length);
            }
            let body_length = (header.pdu_length as usize) - Header::SIZE;

            if stream.len() < body_length {
                bail!(
                    "Not enough data for PDU body: need {}, have {}",
                    body_length,
                    stream.len()
                );
            }

            // Extract body
            let body = &stream[..body_length];
            let mut body_stream = body;

            // Dispatch PDU
            match header.cmd_id {
                CmdId::CapsConfirm => self.handle_caps_confirm(&mut body_stream)?,
                CmdId::StartFrame => self.handle_start_frame(&mut body_stream)?,
                CmdId::EndFrame => self.handle_end_frame(&mut body_stream)?,
                CmdId::CreateSurface => self.handle_create_surface(&mut body_stream)?,
                CmdId::DeleteSurface => self.handle_delete_surface(&mut body_stream)?,
                CmdId::WireToSurface1 => self.handle_wire_to_surface_1(&mut body_stream)?,
                CmdId::WireToSurface2 => self.handle_wire_to_surface_2(&mut body_stream)?,
                CmdId::SolidFill => self.handle_solid_fill(&mut body_stream)?,
                CmdId::SurfaceToSurface => self.handle_surface_to_surface(&mut body_stream)?,
                CmdId::SurfaceToCache => self.handle_surface_to_cache(&mut body_stream)?,
                CmdId::CacheToSurface => self.handle_cache_to_surface(&mut body_stream)?,
                CmdId::EvictCacheEntry => self.handle_evict_cache_entry(&mut body_stream)?,
                CmdId::DeleteEncodingContext => {
                    self.handle_delete_encoding_context(&mut body_stream)?
                }
                CmdId::ResetGraphics => self.handle_reset_graphics(&mut body_stream)?,
                CmdId::MapSurfaceToOutput => self.handle_map_surface_to_output(&mut body_stream)?,
                CmdId::MapSurfaceToScaledOutput => {
                    self.handle_map_surface_to_scaled_output(&mut body_stream)?
                }
                CmdId::MapSurfaceToWindow => self.handle_map_surface_to_window(&mut body_stream)?,
                CmdId::MapSurfaceToScaledWindow => {
                    self.handle_map_surface_to_scaled_window(&mut body_stream)?
                }
                CmdId::CacheImportReply => self.handle_cache_import_reply(&mut body_stream)?,
                _ => {
                    warn!("Unhandled RDPEGFX command: {:?}", header.cmd_id);
                }
            }

            // Verify we consumed the entire body
            if !body_stream.is_empty() {
                warn!(
                    "PDU {:?} had {} bytes remaining after parsing",
                    header.cmd_id,
                    body_stream.len()
                );
            }

            // Advance to next PDU
            stream.advance(body_length);

            trace!(
                "Processed {:?} PDU at offset {}, length {}",
                header.cmd_id, start_pos, header.pdu_length
            );
        }

        Ok(())
    }

    fn handle_caps_confirm(&mut self, data: &mut &[u8]) -> Result<()> {
        let caps = CapsConfirm::parse(data)?;

        self.cap_version = Some(caps.version);
        self.cap_flags = caps.flags;

        // Enable QoE for 10.0+
        self.send_qoe_acks = caps.version >= caps::cap_version::V10;

        debug!(
            "CAPS_CONFIRM: version={} ({}), flags=0x{:08X}",
            caps::version_string(caps.version),
            caps.version,
            caps.flags
        );

        Ok(())
    }

    fn handle_start_frame(&mut self, data: &mut &[u8]) -> Result<()> {
        let frame = StartFrame::parse(data)?;

        trace!(
            "START_FRAME: id={}, timestamp={}",
            frame.frame_id, frame.timestamp
        );

        self.current_frame = Some(FrameState {
            frame_id: frame.frame_id,
            timestamp: frame.timestamp,
            start_time: Instant::now(),
        });

        self.unacknowledged_frames += 1;

        self.ctx.on_start_frame(frame.frame_id, frame.timestamp)
    }

    fn handle_end_frame(&mut self, data: &mut &[u8]) -> Result<()> {
        let frame = EndFrame::parse(data)?;

        trace!("END_FRAME: id={}", frame.frame_id);

        // Get frame state
        let frame_state = self
            .current_frame
            .take()
            .ok_or_else(|| anyhow::anyhow!("END_FRAME without START_FRAME"))?;

        if frame_state.frame_id != frame.frame_id {
            bail!(
                "Frame ID mismatch: START={}, END={}",
                frame_state.frame_id,
                frame.frame_id
            );
        }

        // Notify context
        self.ctx.on_end_frame(frame.frame_id)?;

        // Update counters
        self.total_frames_decoded += 1;

        // Send acknowledgements
        self.send_frame_acknowledge(frame.frame_id)?;

        if self.send_qoe_acks {
            self.send_qoe_acknowledge(&frame_state)?;
        }

        if self.unacknowledged_frames > 0 {
            self.unacknowledged_frames -= 1;
        }

        Ok(())
    }

    fn handle_create_surface(&mut self, data: &mut &[u8]) -> Result<()> {
        let surface = CreateSurface::parse(data)?;

        debug!(
            "CREATE_SURFACE: id={}, {}x{}, format=0x{:02X}",
            surface.surface_id, surface.width, surface.height, surface.pixel_format
        );

        self.ctx.on_create_surface(
            surface.surface_id,
            surface.width,
            surface.height,
            surface.pixel_format,
        )
    }

    fn handle_delete_surface(&mut self, data: &mut &[u8]) -> Result<()> {
        let surface = DeleteSurface::parse(data)?;

        debug!("DELETE_SURFACE: id={}", surface.surface_id);

        self.ctx.on_delete_surface(surface.surface_id)
    }

    fn handle_wire_to_surface_1(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = WireToSurface1::parse(data)?;

        trace!(
            "WIRE_TO_SURFACE_1: surface={}, codec={} ({}), rect=({},{} {}x{}), data_len={}",
            cmd.surface_id,
            cmd.codec_id,
            codec::codec_name(cmd.codec_id),
            cmd.dest_rect.left,
            cmd.dest_rect.top,
            cmd.dest_rect.width(),
            cmd.dest_rect.height(),
            cmd.bitmap_data.len()
        );

        self.ctx.on_surface_command(
            cmd.surface_id,
            cmd.codec_id,
            cmd.pixel_format,
            cmd.dest_rect,
            &cmd.bitmap_data,
        )
    }

    fn handle_wire_to_surface_2(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = WireToSurface2::parse(data)?;

        trace!(
            "WIRE_TO_SURFACE_2: surface={}, codec={} ({}), context={}, data_len={}",
            cmd.surface_id,
            cmd.codec_id,
            codec::codec_name(cmd.codec_id),
            cmd.codec_context_id,
            cmd.bitmap_data.len()
        );

        self.ctx.on_surface_command_full(
            cmd.surface_id,
            cmd.codec_id,
            cmd.codec_context_id,
            cmd.pixel_format,
            &cmd.bitmap_data,
        )
    }

    fn handle_solid_fill(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = SolidFill::parse(data)?;

        trace!(
            "SOLID_FILL: surface={}, color=({},{},{},{}), rects={}",
            cmd.surface_id,
            cmd.fill_pixel.r,
            cmd.fill_pixel.g,
            cmd.fill_pixel.b,
            cmd.fill_pixel.xa,
            cmd.fill_rects.len()
        );

        self.ctx
            .on_solid_fill(cmd.surface_id, cmd.fill_pixel, &cmd.fill_rects)
    }

    fn handle_surface_to_surface(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = SurfaceToSurface::parse(data)?;

        trace!(
            "SURFACE_TO_SURFACE: src={} dst={} dest_points={}",
            cmd.source_surface_id,
            cmd.destination_surface_id,
            cmd.dest_points.len()
        );

        self.ctx.on_surface_to_surface(
            cmd.source_surface_id,
            cmd.destination_surface_id,
            cmd.source_rect,
            &cmd.dest_points,
        )
    }

    fn handle_surface_to_cache(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = SurfaceToCache::parse(data)?;

        trace!(
            "SURFACE_TO_CACHE: surface={} cache_slot={} key=0x{:016X}",
            cmd.surface_id, cmd.cache_slot, cmd.cache_key
        );

        self.ctx.on_surface_to_cache(
            cmd.surface_id,
            cmd.cache_key,
            cmd.cache_slot,
            cmd.source_rect,
        )
    }

    fn handle_cache_to_surface(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = CacheToSurface::parse(data)?;

        trace!(
            "CACHE_TO_SURFACE: cache_slot={} surface={} dest_points={}",
            cmd.cache_slot,
            cmd.surface_id,
            cmd.dest_points.len()
        );

        self.ctx
            .on_cache_to_surface(cmd.cache_slot, cmd.surface_id, &cmd.dest_points)
    }

    fn handle_evict_cache_entry(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = EvictCacheEntry::parse(data)?;

        trace!("EVICT_CACHE_ENTRY: slot={}", cmd.cache_slot);

        self.ctx.on_evict_cache_entry(cmd.cache_slot)
    }

    fn handle_cache_import_reply(&mut self, data: &mut &[u8]) -> Result<()> {
        let reply = CacheImportReply::parse(data)?;

        trace!(
            "CACHE_IMPORT_REPLY: imported_slots={:?}",
            reply.imported_slots
        );

        self.ctx.on_cache_import_reply(&reply.imported_slots)
    }

    fn handle_delete_encoding_context(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = DeleteEncodingContext::parse(data)?;

        trace!(
            "DELETE_ENCODING_CONTEXT: surface={}, context={}",
            cmd.surface_id, cmd.codec_context_id
        );

        self.ctx
            .on_delete_encoding_context(cmd.surface_id, cmd.codec_context_id)
    }

    fn handle_reset_graphics(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = ResetGraphics::parse(data)?;

        trace!(
            "RESET_GRAPHICS: width={} height={} monitors={}",
            cmd.width,
            cmd.height,
            cmd.monitors.len()
        );

        self.ctx
            .on_reset_graphics(cmd.width, cmd.height, &cmd.monitors)
    }

    fn handle_map_surface_to_output(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = MapSurfaceToOutput::parse(data)?;

        trace!(
            "MAP_SURFACE_TO_OUTPUT: surface={} origin=({}, {})",
            cmd.surface_id, cmd.output_origin_x, cmd.output_origin_y
        );

        self.ctx
            .on_map_surface_to_output(cmd.surface_id, cmd.output_origin_x, cmd.output_origin_y)
    }

    fn handle_map_surface_to_scaled_output(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = MapSurfaceToScaledOutput::parse(data)?;

        trace!(
            "MAP_SURFACE_TO_SCALED_OUTPUT: surface={} origin=({}, {}) target={}x{}",
            cmd.surface_id,
            cmd.output_origin_x,
            cmd.output_origin_y,
            cmd.target_width,
            cmd.target_height
        );

        self.ctx.on_map_surface_to_scaled_output(
            cmd.surface_id,
            cmd.output_origin_x,
            cmd.output_origin_y,
            cmd.target_width,
            cmd.target_height,
        )
    }

    fn handle_map_surface_to_window(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = MapSurfaceToWindow::parse(data)?;

        trace!(
            "MAP_SURFACE_TO_WINDOW: surface={} window=0x{:016X} mapped={}x{}",
            cmd.surface_id, cmd.window_id, cmd.mapped_width, cmd.mapped_height
        );

        self.ctx.on_map_surface_to_window(
            cmd.surface_id,
            cmd.window_id,
            cmd.mapped_width,
            cmd.mapped_height,
        )
    }

    fn handle_map_surface_to_scaled_window(&mut self, data: &mut &[u8]) -> Result<()> {
        let cmd = MapSurfaceToScaledWindow::parse(data)?;

        trace!(
            "MAP_SURFACE_TO_SCALED_WINDOW: surface={} window=0x{:016X} mapped={}x{} target={}x{}",
            cmd.surface_id,
            cmd.window_id,
            cmd.mapped_width,
            cmd.mapped_height,
            cmd.target_width,
            cmd.target_height
        );

        self.ctx.on_map_surface_to_scaled_window(
            cmd.surface_id,
            cmd.window_id,
            cmd.mapped_width,
            cmd.mapped_height,
            cmd.target_width,
            cmd.target_height,
        )
    }

    fn send_frame_acknowledge(&mut self, frame_id: u32) -> Result<()> {
        let queue_depth = if self.suspend_acks {
            // Only send SUSPEND for first frame
            if self.total_frames_decoded == 1 {
                FrameAcknowledge::SUSPEND_FRAME_ACKNOWLEDGEMENT
            } else {
                return Ok(()); // Don't send ack
            }
        } else {
            FrameAcknowledge::QUEUE_DEPTH_UNAVAILABLE
        };

        let ack = FrameAcknowledge {
            queue_depth,
            frame_id,
            total_frames_decoded: self.total_frames_decoded,
        };

        trace!(
            "Sending FRAME_ACKNOWLEDGE: frame_id={}, total_decoded={}, queue_depth=0x{:08X}",
            frame_id, self.total_frames_decoded, queue_depth
        );

        self.ctx.send(&ack.to_bytes())
    }

    fn send_qoe_acknowledge(&mut self, frame_state: &FrameState) -> Result<()> {
        let elapsed = frame_state.start_time.elapsed();
        let elapsed_ms = elapsed.as_millis() as u64;

        // Cap at 65000 as per protocol
        let time_diff_se = elapsed_ms.min(65000) as u16;
        let time_diff_edr = 0u16; // Render time (can be calculated if needed)

        let qoe = QoeFrameAcknowledge {
            frame_id: frame_state.frame_id,
            timestamp: frame_state.timestamp,
            time_diff_se,
            time_diff_edr,
        };

        trace!(
            "Sending QOE_FRAME_ACKNOWLEDGE: frame_id={}, time_diff={}ms",
            frame_state.frame_id, time_diff_se
        );

        self.ctx.send(&qoe.to_bytes())
    }
}

#[cfg(feature = "h264")]
fn default_capability_sets(small_cache: bool) -> Vec<CapabilitySet> {
    CapabilitySet::default_sets(small_cache, true, true)
}

#[cfg(not(feature = "h264"))]
fn default_capability_sets(small_cache: bool) -> Vec<CapabilitySet> {
    CapabilitySet::default_sets(small_cache, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock GfxContext for testing
    struct MockGfxContext {
        messages: Vec<Vec<u8>>,
    }

    impl MockGfxContext {
        fn new() -> Self {
            Self {
                messages: Vec::new(),
            }
        }

        fn take_outgoing_messages(&mut self) -> Vec<Vec<u8>> {
            std::mem::take(&mut self.messages)
        }
    }

    impl GfxContext for MockGfxContext {
        fn send(&mut self, data: &[u8]) -> Result<()> {
            self.messages.push(data.to_vec());
            Ok(())
        }

        fn on_create_surface(
            &mut self,
            _surface_id: u16,
            _width: u16,
            _height: u16,
            _pixel_format: u8,
        ) -> Result<()> {
            Ok(())
        }

        fn on_delete_surface(&mut self, _surface_id: u16) -> Result<()> {
            Ok(())
        }

        fn on_start_frame(&mut self, _frame_id: u32, _timestamp: u32) -> Result<()> {
            Ok(())
        }

        fn on_end_frame(&mut self, _frame_id: u32) -> Result<()> {
            Ok(())
        }

        fn on_surface_command(
            &mut self,
            _surface_id: u16,
            _codec_id: u16,
            _pixel_format: u8,
            _dest_rect: Rectangle,
            _bitmap_data: &[u8],
        ) -> Result<()> {
            Ok(())
        }

        fn on_surface_command_full(
            &mut self,
            _surface_id: u16,
            _codec_id: u16,
            _codec_context_id: u32,
            _pixel_format: u8,
            _bitmap_data: &[u8],
        ) -> Result<()> {
            Ok(())
        }

        fn on_solid_fill(
            &mut self,
            _surface_id: u16,
            _fill_pixel: Color32,
            _fill_rects: &[Rectangle],
        ) -> Result<()> {
            Ok(())
        }

        fn on_surface_to_surface(
            &mut self,
            _source_surface_id: u16,
            _destination_surface_id: u16,
            _source_rect: Rectangle,
            _dest_points: &[Point16],
        ) -> Result<()> {
            Ok(())
        }

        fn on_surface_to_cache(
            &mut self,
            _surface_id: u16,
            _cache_key: u64,
            _cache_slot: u16,
            _source_rect: Rectangle,
        ) -> Result<()> {
            Ok(())
        }

        fn on_cache_to_surface(
            &mut self,
            _cache_slot: u16,
            _surface_id: u16,
            _dest_points: &[Point16],
        ) -> Result<()> {
            Ok(())
        }

        fn on_evict_cache_entry(&mut self, _cache_slot: u16) -> Result<()> {
            Ok(())
        }

        fn on_cache_import_reply(&mut self, _imported_slots: &[u16]) -> Result<()> {
            Ok(())
        }

        fn on_delete_encoding_context(
            &mut self,
            _surface_id: u16,
            _codec_context_id: u32,
        ) -> Result<()> {
            Ok(())
        }

        fn on_reset_graphics(
            &mut self,
            _width: u32,
            _height: u32,
            _monitors: &[MonitorDefinition],
        ) -> Result<()> {
            Ok(())
        }

        fn on_map_surface_to_output(
            &mut self,
            _surface_id: u16,
            _output_origin_x: u32,
            _output_origin_y: u32,
        ) -> Result<()> {
            Ok(())
        }

        fn on_map_surface_to_scaled_output(
            &mut self,
            _surface_id: u16,
            _output_origin_x: u32,
            _output_origin_y: u32,
            _target_width: u32,
            _target_height: u32,
        ) -> Result<()> {
            Ok(())
        }

        fn on_map_surface_to_window(
            &mut self,
            _surface_id: u16,
            _window_id: u64,
            _mapped_width: u32,
            _mapped_height: u32,
        ) -> Result<()> {
            Ok(())
        }

        fn on_map_surface_to_scaled_window(
            &mut self,
            _surface_id: u16,
            _window_id: u64,
            _mapped_width: u32,
            _mapped_height: u32,
            _target_width: u32,
            _target_height: u32,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_caps_confirm_v8_processing() {
        let ctx = MockGfxContext::new();
        let mut client = GfxClient::new(ctx, false, false);

        // Construct a CAPS_CONFIRM PDU for V8 (0x00080004)
        let mut caps_confirm_data = Vec::new();
        caps_confirm_data.extend_from_slice(&caps::cap_version::V8.to_le_bytes()); // version
        caps_confirm_data.extend_from_slice(&4u32.to_le_bytes()); // capsDataLength
        caps_confirm_data.extend_from_slice(&0u32.to_le_bytes()); // flags

        // Process the CAPS_CONFIRM
        let result = client.handle_caps_confirm(&mut caps_confirm_data.as_slice());
        assert!(result.is_ok(), "CAPS_CONFIRM processing should succeed");

        // Verify capability version was stored
        assert_eq!(
            client.cap_version(),
            Some(caps::cap_version::V8),
            "Capability version should be V8"
        );

        // Verify capability flags were stored
        assert_eq!(client.cap_flags, 0, "Flags should be 0 for V8");

        // Verify QoE acks are disabled for V8 (< V10)
        assert!(
            !client.send_qoe_acks,
            "QoE acknowledgements should be disabled for V8"
        );
    }

    #[test]
    fn test_caps_confirm_v10_processing() {
        let ctx = MockGfxContext::new();
        let mut client = GfxClient::new(ctx, false, false);

        // Construct a CAPS_CONFIRM PDU for V10 (0x000A0002)
        let flags = caps::cap_flags::AVC_DISABLED;
        let mut caps_confirm_data = Vec::new();
        caps_confirm_data.extend_from_slice(&caps::cap_version::V10.to_le_bytes()); // version
        caps_confirm_data.extend_from_slice(&4u32.to_le_bytes()); // capsDataLength
        caps_confirm_data.extend_from_slice(&flags.to_le_bytes()); // flags

        // Process the CAPS_CONFIRM
        let result = client.handle_caps_confirm(&mut caps_confirm_data.as_slice());
        assert!(result.is_ok(), "CAPS_CONFIRM processing should succeed");

        // Verify capability version was stored
        assert_eq!(
            client.cap_version(),
            Some(caps::cap_version::V10),
            "Capability version should be V10"
        );

        // Verify capability flags were stored
        assert_eq!(
            client.cap_flags, flags,
            "Flags should match the AVC_DISABLED flag"
        );

        // Verify QoE acks are enabled for V10+
        assert!(
            client.send_qoe_acks,
            "QoE acknowledgements should be enabled for V10+"
        );
    }

    #[test]
    fn test_caps_confirm_v107_with_flags() {
        let ctx = MockGfxContext::new();
        let mut client = GfxClient::new(ctx, false, false);

        // Construct a CAPS_CONFIRM PDU for V10.7 with multiple flags
        let flags = caps::cap_flags::AVC_DISABLED | caps::cap_flags::SCALEDMAP_DISABLE;
        let mut caps_confirm_data = Vec::new();
        caps_confirm_data.extend_from_slice(&caps::cap_version::V107.to_le_bytes()); // version
        caps_confirm_data.extend_from_slice(&4u32.to_le_bytes()); // capsDataLength
        caps_confirm_data.extend_from_slice(&flags.to_le_bytes()); // flags

        // Process the CAPS_CONFIRM
        let result = client.handle_caps_confirm(&mut caps_confirm_data.as_slice());
        assert!(result.is_ok(), "CAPS_CONFIRM processing should succeed");

        // Verify capability version was stored
        assert_eq!(
            client.cap_version(),
            Some(caps::cap_version::V107),
            "Capability version should be V10.7"
        );

        // Verify all flags were stored correctly
        assert_eq!(
            client.cap_flags, flags,
            "Flags should include both AVC_DISABLED and SCALEDMAP_DISABLE"
        );
        assert_eq!(
            client.cap_flags & caps::cap_flags::AVC_DISABLED,
            caps::cap_flags::AVC_DISABLED,
            "AVC_DISABLED flag should be set"
        );
        assert_eq!(
            client.cap_flags & caps::cap_flags::SCALEDMAP_DISABLE,
            caps::cap_flags::SCALEDMAP_DISABLE,
            "SCALEDMAP_DISABLE flag should be set"
        );

        // Verify QoE acks are enabled for V10.7
        assert!(
            client.send_qoe_acks,
            "QoE acknowledgements should be enabled for V10.7"
        );
    }

    #[test]
    fn test_caps_confirm_qoe_boundary() {
        // Test that QoE is disabled for V8.1 (< V10)
        let ctx = MockGfxContext::new();
        let mut client = GfxClient::new(ctx, false, false);

        let mut caps_confirm_data = Vec::new();
        caps_confirm_data.extend_from_slice(&caps::cap_version::V81.to_le_bytes());
        caps_confirm_data.extend_from_slice(&4u32.to_le_bytes());
        caps_confirm_data.extend_from_slice(&0u32.to_le_bytes());

        client
            .handle_caps_confirm(&mut caps_confirm_data.as_slice())
            .unwrap();

        assert!(!client.send_qoe_acks, "QoE should be disabled for V8.1");

        // Test that QoE is enabled exactly at V10
        let ctx2 = MockGfxContext::new();
        let mut client2 = GfxClient::new(ctx2, false, false);

        let mut caps_confirm_data2 = Vec::new();
        caps_confirm_data2.extend_from_slice(&caps::cap_version::V10.to_le_bytes());
        caps_confirm_data2.extend_from_slice(&4u32.to_le_bytes());
        caps_confirm_data2.extend_from_slice(&0u32.to_le_bytes());

        client2
            .handle_caps_confirm(&mut caps_confirm_data2.as_slice())
            .unwrap();

        assert!(
            client2.send_qoe_acks,
            "QoE should be enabled exactly at V10"
        );
    }
}
