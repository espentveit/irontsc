//! PDU structures and parsing for RDPEGFX

use anyhow::{bail, Result};
use bytes::{Buf, BufMut};

/// PDU command identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum CmdId {
    // Server to Client
    WireToSurface1 = 0x0001,
    WireToSurface2 = 0x0002,
    DeleteEncodingContext = 0x0003,
    SolidFill = 0x0004,
    SurfaceToSurface = 0x0005,
    SurfaceToCache = 0x0006,
    CacheToSurface = 0x0007,
    EvictCacheEntry = 0x0008,
    CreateSurface = 0x0009,
    DeleteSurface = 0x000A,
    StartFrame = 0x000B,
    EndFrame = 0x000C,
    ResetGraphics = 0x000E,
    MapSurfaceToOutput = 0x000F,
    CacheImportOffer = 0x0010,
    CacheImportReply = 0x0011,
    MapSurfaceToWindow = 0x0015,
    MapSurfaceToScaledOutput = 0x0017,
    MapSurfaceToScaledWindow = 0x0018,

    // Client to Server
    FrameAcknowledge = 0x000D,
    CapsAdvertise = 0x0012,
    CapsConfirm = 0x0013,
    QoeFrameAcknowledge = 0x0016,
}

impl TryFrom<u16> for CmdId {
    type Error = anyhow::Error;

    fn try_from(value: u16) -> Result<Self> {
        match value {
            0x0001 => Ok(CmdId::WireToSurface1),
            0x0002 => Ok(CmdId::WireToSurface2),
            0x0003 => Ok(CmdId::DeleteEncodingContext),
            0x0004 => Ok(CmdId::SolidFill),
            0x0005 => Ok(CmdId::SurfaceToSurface),
            0x0006 => Ok(CmdId::SurfaceToCache),
            0x0007 => Ok(CmdId::CacheToSurface),
            0x0008 => Ok(CmdId::EvictCacheEntry),
            0x0009 => Ok(CmdId::CreateSurface),
            0x000A => Ok(CmdId::DeleteSurface),
            0x000B => Ok(CmdId::StartFrame),
            0x000C => Ok(CmdId::EndFrame),
            0x000D => Ok(CmdId::FrameAcknowledge),
            0x000E => Ok(CmdId::ResetGraphics),
            0x000F => Ok(CmdId::MapSurfaceToOutput),
            0x0010 => Ok(CmdId::CacheImportOffer),
            0x0011 => Ok(CmdId::CacheImportReply),
            0x0012 => Ok(CmdId::CapsAdvertise),
            0x0013 => Ok(CmdId::CapsConfirm),
            0x0015 => Ok(CmdId::MapSurfaceToWindow),
            0x0016 => Ok(CmdId::QoeFrameAcknowledge),
            0x0017 => Ok(CmdId::MapSurfaceToScaledOutput),
            0x0018 => Ok(CmdId::MapSurfaceToScaledWindow),
            _ => bail!("Unknown RDPEGFX command ID: 0x{:04X}", value),
        }
    }
}

/// PDU header (8 bytes)
#[derive(Debug, Clone)]
pub struct Header {
    pub cmd_id: CmdId,
    pub flags: u16,
    pub pdu_length: u32,
}

impl Header {
    pub const SIZE: usize = 8;

    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < Self::SIZE {
            bail!("Not enough data for RDPEGFX header");
        }

        let cmd_id = CmdId::try_from(data.get_u16_le())?;
        let flags = data.get_u16_le();
        let pdu_length = data.get_u32_le();

        Ok(Self {
            cmd_id,
            flags,
            pdu_length,
        })
    }

    pub fn write(&self, buf: &mut Vec<u8>) {
        buf.put_u16_le(self.cmd_id as u16);
        buf.put_u16_le(self.flags);
        buf.put_u32_le(self.pdu_length);
    }
}

/// Point16 (4 bytes)
#[derive(Debug, Clone, Copy)]
pub struct Point16 {
    pub x: i16,
    pub y: i16,
}

impl Point16 {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 4 {
            bail!("Not enough data for Point16");
        }

        Ok(Self {
            x: data.get_i16_le(),
            y: data.get_i16_le(),
        })
    }
}

/// Rectangle (8 bytes)
#[derive(Debug, Clone, Copy)]
pub struct Rectangle {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl Rectangle {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 8 {
            bail!("Not enough data for Rectangle");
        }

        let left = data.get_u16_le();
        let top = data.get_u16_le();
        let right = data.get_u16_le();
        let bottom = data.get_u16_le();

        if left >= right || top >= bottom {
            bail!(
                "Invalid rectangle: ({},{}) - ({},{})",
                left,
                top,
                right,
                bottom
            );
        }

        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }

    pub fn width(&self) -> u16 {
        self.right - self.left
    }

    pub fn height(&self) -> u16 {
        self.bottom - self.top
    }
}

/// Color32 (4 bytes, BGRA order)
#[derive(Debug, Clone, Copy)]
pub struct Color32 {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub xa: u8, // X or Alpha
}

impl Color32 {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 4 {
            bail!("Not enough data for Color32");
        }

        Ok(Self {
            b: data.get_u8(),
            g: data.get_u8(),
            r: data.get_u8(),
            xa: data.get_u8(),
        })
    }
}

/// CREATE_SURFACE PDU
#[derive(Debug, Clone)]
pub struct CreateSurface {
    pub surface_id: u16,
    pub width: u16,
    pub height: u16,
    pub pixel_format: u8,
}

impl CreateSurface {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 7 {
            bail!("Not enough data for CreateSurface");
        }

        Ok(Self {
            surface_id: data.get_u16_le(),
            width: data.get_u16_le(),
            height: data.get_u16_le(),
            pixel_format: data.get_u8(),
        })
    }
}

/// DELETE_SURFACE PDU
#[derive(Debug, Clone)]
pub struct DeleteSurface {
    pub surface_id: u16,
}

impl DeleteSurface {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 2 {
            bail!("Not enough data for DeleteSurface");
        }

        Ok(Self {
            surface_id: data.get_u16_le(),
        })
    }
}

/// START_FRAME PDU
#[derive(Debug, Clone)]
pub struct StartFrame {
    pub timestamp: u32,
    pub frame_id: u32,
}

impl StartFrame {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 8 {
            bail!("Not enough data for StartFrame");
        }

        Ok(Self {
            timestamp: data.get_u32_le(),
            frame_id: data.get_u32_le(),
        })
    }
}

/// END_FRAME PDU
#[derive(Debug, Clone)]
pub struct EndFrame {
    pub frame_id: u32,
}

impl EndFrame {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 4 {
            bail!("Not enough data for EndFrame");
        }

        Ok(Self {
            frame_id: data.get_u32_le(),
        })
    }
}

/// WIRE_TO_SURFACE_1 PDU
#[derive(Debug, Clone)]
pub struct WireToSurface1 {
    pub surface_id: u16,
    pub codec_id: u16,
    pub pixel_format: u8,
    pub dest_rect: Rectangle,
    pub bitmap_data: Vec<u8>,
}

impl WireToSurface1 {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 17 {
            bail!("Not enough data for WireToSurface1");
        }

        let surface_id = data.get_u16_le();
        let codec_id = data.get_u16_le();
        let pixel_format = data.get_u8();
        let dest_rect = Rectangle::parse(data)?;
        let bitmap_data_length = data.get_u32_le() as usize;

        if data.len() < bitmap_data_length {
            bail!(
                "Not enough data for bitmap: need {}, have {}",
                bitmap_data_length,
                data.len()
            );
        }

        let bitmap_data = data[..bitmap_data_length].to_vec();
        data.advance(bitmap_data_length);

        Ok(Self {
            surface_id,
            codec_id,
            pixel_format,
            dest_rect,
            bitmap_data,
        })
    }
}

/// WIRE_TO_SURFACE_2 PDU
#[derive(Debug, Clone)]
pub struct WireToSurface2 {
    pub surface_id: u16,
    pub codec_id: u16,
    pub codec_context_id: u32,
    pub pixel_format: u8,
    pub bitmap_data: Vec<u8>,
}

impl WireToSurface2 {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 13 {
            bail!("Not enough data for WireToSurface2");
        }

        let surface_id = data.get_u16_le();
        let codec_id = data.get_u16_le();
        let codec_context_id = data.get_u32_le();
        let pixel_format = data.get_u8();
        let bitmap_data_length = data.get_u32_le() as usize;

        if data.len() < bitmap_data_length {
            bail!(
                "Not enough data for bitmap: need {}, have {}",
                bitmap_data_length,
                data.len()
            );
        }

        let bitmap_data = data[..bitmap_data_length].to_vec();
        data.advance(bitmap_data_length);

        Ok(Self {
            surface_id,
            codec_id,
            codec_context_id,
            pixel_format,
            bitmap_data,
        })
    }
}

/// DELETE_ENCODING_CONTEXT PDU
#[derive(Debug, Clone)]
pub struct DeleteEncodingContext {
    pub surface_id: u16,
    pub codec_context_id: u32,
}

impl DeleteEncodingContext {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 6 {
            bail!("Not enough data for DeleteEncodingContext");
        }

        Ok(Self {
            surface_id: data.get_u16_le(),
            codec_context_id: data.get_u32_le(),
        })
    }
}

/// SOLID_FILL PDU
#[derive(Debug, Clone)]
pub struct SolidFill {
    pub surface_id: u16,
    pub fill_pixel: Color32,
    pub fill_rects: Vec<Rectangle>,
}

impl SolidFill {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 8 {
            bail!("Not enough data for SolidFill header");
        }

        let surface_id = data.get_u16_le();
        let fill_pixel = Color32::parse(data)?;
        let fill_rect_count = data.get_u16_le() as usize;

        if data.len() < fill_rect_count * 8 {
            bail!("Not enough data for fill rectangles");
        }

        let mut fill_rects = Vec::with_capacity(fill_rect_count);
        for _ in 0..fill_rect_count {
            fill_rects.push(Rectangle::parse(data)?);
        }

        Ok(Self {
            surface_id,
            fill_pixel,
            fill_rects,
        })
    }
}

/// CAPS_CONFIRM PDU (Server → Client)
#[derive(Debug, Clone)]
pub struct CapsConfirm {
    pub version: u32,
    pub flags: u32,
    pub extra_data: Vec<u8>,
}

impl CapsConfirm {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 12 {
            bail!("Not enough data for CapsConfirm");
        }

        let version = data.get_u32_le();
        let length = data.get_u32_le() as usize;

        if length < 4 {
            bail!("Invalid CapsConfirm length: {}", length);
        }

        if data.len() < length {
            bail!(
                "CapsConfirm payload truncated: need {}, have {}",
                length,
                data.len()
            );
        }

        let flags = data.get_u32_le();
        let extra_len = length - 4;
        let extra_data = if extra_len > 0 {
            let extra = data[..extra_len].to_vec();
            data.advance(extra_len);
            extra
        } else {
            Vec::new()
        };

        Ok(Self {
            version,
            flags,
            extra_data,
        })
    }
}

/// FRAME_ACKNOWLEDGE PDU (Client → Server)
#[derive(Debug, Clone)]
pub struct FrameAcknowledge {
    pub queue_depth: u32,
    pub frame_id: u32,
    pub total_frames_decoded: u32,
}

impl FrameAcknowledge {
    pub const QUEUE_DEPTH_UNAVAILABLE: u32 = 0x00000000;
    pub const SUSPEND_FRAME_ACKNOWLEDGEMENT: u32 = 0xFFFFFFFF;

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20); // 8 (header) + 12 (body)

        // Header
        let header = Header {
            cmd_id: CmdId::FrameAcknowledge,
            flags: 0,
            pdu_length: 20,
        };
        header.write(&mut buf);

        // Body
        buf.put_u32_le(self.queue_depth);
        buf.put_u32_le(self.frame_id);
        buf.put_u32_le(self.total_frames_decoded);

        buf
    }
}

/// QOE_FRAME_ACKNOWLEDGE PDU (Client → Server)
#[derive(Debug, Clone)]
pub struct QoeFrameAcknowledge {
    pub frame_id: u32,
    pub timestamp: u32,
    pub time_diff_se: u16,  // Start-Encode to Decode-Ready
    pub time_diff_edr: u16, // Encode-Done to Render-Done
}

impl QoeFrameAcknowledge {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20); // 8 (header) + 12 (body)

        // Header
        let header = Header {
            cmd_id: CmdId::QoeFrameAcknowledge,
            flags: 0,
            pdu_length: 20,
        };
        header.write(&mut buf);

        // Body
        buf.put_u32_le(self.frame_id);
        buf.put_u32_le(self.timestamp);
        buf.put_u16_le(self.time_diff_se);
        buf.put_u16_le(self.time_diff_edr);

        buf
    }
}

/// MONITOR_DEF structure used by RESET_GRAPHICS
#[derive(Debug, Clone)]
pub struct MonitorDefinition {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub flags: u32,
}

impl MonitorDefinition {
    fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 20 {
            bail!("Not enough data for MonitorDefinition");
        }

        Ok(Self {
            left: data.get_i32_le(),
            top: data.get_i32_le(),
            right: data.get_i32_le(),
            bottom: data.get_i32_le(),
            flags: data.get_u32_le(),
        })
    }
}

/// RESET_GRAPHICS PDU
#[derive(Debug, Clone)]
pub struct ResetGraphics {
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<MonitorDefinition>,
}

impl ResetGraphics {
    const BODY_SIZE: usize = 332;

    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 12 {
            bail!("Not enough data for ResetGraphics");
        }

        let original_len = data.len();

        if original_len != Self::BODY_SIZE {
            bail!(
                "RESET_GRAPHICS body must be {} bytes, got {}",
                Self::BODY_SIZE,
                original_len
            );
        }

        let width = data.get_u32_le();
        let height = data.get_u32_le();
        let monitor_count = data.get_u32_le() as usize;

        if width == 0 || height == 0 {
            bail!("RESET_GRAPHICS dimensions must be non-zero");
        }

        if width > 32766 || height > 32766 {
            bail!(
                "RESET_GRAPHICS dimensions exceed spec limit: {}x{}",
                width,
                height
            );
        }

        if monitor_count > 16 {
            bail!(
                "RESET_GRAPHICS monitor count exceeds spec limit: {}",
                monitor_count
            );
        }

        if data.len() < monitor_count.saturating_mul(20) {
            bail!("Not enough data for monitor definitions");
        }

        let mut monitors = Vec::with_capacity(monitor_count);
        for _ in 0..monitor_count {
            monitors.push(MonitorDefinition::parse(data)?);
        }

        // Skip padding (RESET_GRAPHICS body is always 332 bytes)
        let consumed = original_len - data.len();
        if consumed > original_len {
            bail!("RESET_GRAPHICS consumed more bytes than available");
        }
        let remaining = original_len - consumed;
        if remaining > 0 {
            data.advance(remaining);
        }

        Ok(Self {
            width,
            height,
            monitors,
        })
    }
}

/// SURFACE_TO_SURFACE PDU
#[derive(Debug, Clone)]
pub struct SurfaceToSurface {
    pub source_surface_id: u16,
    pub destination_surface_id: u16,
    pub source_rect: Rectangle,
    pub dest_points: Vec<Point16>,
}

impl SurfaceToSurface {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 14 {
            bail!("Not enough data for SurfaceToSurface");
        }

        let source_surface_id = data.get_u16_le();
        let destination_surface_id = data.get_u16_le();
        let source_rect = Rectangle::parse(data)?;
        let dest_points_count = data.get_u16_le() as usize;

        if data.len() < dest_points_count.saturating_mul(4) {
            bail!("Not enough data for destination points");
        }

        let mut dest_points = Vec::with_capacity(dest_points_count);
        for _ in 0..dest_points_count {
            dest_points.push(Point16::parse(data)?);
        }

        Ok(Self {
            source_surface_id,
            destination_surface_id,
            source_rect,
            dest_points,
        })
    }
}

/// SURFACE_TO_CACHE PDU
#[derive(Debug, Clone)]
pub struct SurfaceToCache {
    pub surface_id: u16,
    pub cache_key: u64,
    pub cache_slot: u16,
    pub source_rect: Rectangle,
}

impl SurfaceToCache {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 20 {
            bail!("Not enough data for SurfaceToCache");
        }

        let surface_id = data.get_u16_le();
        let cache_key = data.get_u64_le();
        let cache_slot = data.get_u16_le();
        let source_rect = Rectangle::parse(data)?;

        Ok(Self {
            surface_id,
            cache_key,
            cache_slot,
            source_rect,
        })
    }
}

/// CACHE_TO_SURFACE PDU
#[derive(Debug, Clone)]
pub struct CacheToSurface {
    pub cache_slot: u16,
    pub surface_id: u16,
    pub dest_points: Vec<Point16>,
}

impl CacheToSurface {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 6 {
            bail!("Not enough data for CacheToSurface");
        }

        let cache_slot = data.get_u16_le();
        let surface_id = data.get_u16_le();
        let dest_points_count = data.get_u16_le() as usize;

        if data.len() < dest_points_count.saturating_mul(4) {
            bail!("Not enough data for destination points");
        }

        let mut dest_points = Vec::with_capacity(dest_points_count);
        for _ in 0..dest_points_count {
            dest_points.push(Point16::parse(data)?);
        }

        Ok(Self {
            cache_slot,
            surface_id,
            dest_points,
        })
    }
}

/// EVICT_CACHE_ENTRY PDU
#[derive(Debug, Clone)]
pub struct EvictCacheEntry {
    pub cache_slot: u16,
}

impl EvictCacheEntry {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 2 {
            bail!("Not enough data for EvictCacheEntry");
        }

        Ok(Self {
            cache_slot: data.get_u16_le(),
        })
    }
}

/// CACHE_IMPORT_REPLY PDU (Server → Client)
#[derive(Debug, Clone)]
pub struct CacheImportReply {
    pub imported_slots: Vec<u16>,
}

impl CacheImportReply {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 2 {
            bail!("Not enough data for CacheImportReply");
        }

        let count = data.get_u16_le() as usize;

        if data.len() < count.saturating_mul(2) {
            bail!("Not enough data for cache slots in CacheImportReply");
        }

        let mut imported_slots = Vec::with_capacity(count);
        for _ in 0..count {
            imported_slots.push(data.get_u16_le());
        }

        Ok(Self { imported_slots })
    }
}

/// MAP_SURFACE_TO_OUTPUT PDU
#[derive(Debug, Clone)]
pub struct MapSurfaceToOutput {
    pub surface_id: u16,
    pub output_origin_x: u32,
    pub output_origin_y: u32,
}

impl MapSurfaceToOutput {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 12 {
            bail!("Not enough data for MapSurfaceToOutput");
        }

        let surface_id = data.get_u16_le();
        let _reserved = data.get_u16_le();
        let output_origin_x = data.get_u32_le();
        let output_origin_y = data.get_u32_le();

        Ok(Self {
            surface_id,
            output_origin_x,
            output_origin_y,
        })
    }
}

/// MAP_SURFACE_TO_SCALED_OUTPUT PDU
#[derive(Debug, Clone)]
pub struct MapSurfaceToScaledOutput {
    pub surface_id: u16,
    pub output_origin_x: u32,
    pub output_origin_y: u32,
    pub target_width: u32,
    pub target_height: u32,
}

impl MapSurfaceToScaledOutput {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 20 {
            bail!("Not enough data for MapSurfaceToScaledOutput");
        }

        let surface_id = data.get_u16_le();
        let _reserved = data.get_u16_le();
        let output_origin_x = data.get_u32_le();
        let output_origin_y = data.get_u32_le();
        let target_width = data.get_u32_le();
        let target_height = data.get_u32_le();

        Ok(Self {
            surface_id,
            output_origin_x,
            output_origin_y,
            target_width,
            target_height,
        })
    }
}

/// MAP_SURFACE_TO_WINDOW PDU
#[derive(Debug, Clone)]
pub struct MapSurfaceToWindow {
    pub surface_id: u16,
    pub window_id: u64,
    pub mapped_width: u32,
    pub mapped_height: u32,
}

impl MapSurfaceToWindow {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 18 {
            bail!("Not enough data for MapSurfaceToWindow");
        }

        Ok(Self {
            surface_id: data.get_u16_le(),
            window_id: data.get_u64_le(),
            mapped_width: data.get_u32_le(),
            mapped_height: data.get_u32_le(),
        })
    }
}

/// MAP_SURFACE_TO_SCALED_WINDOW PDU
#[derive(Debug, Clone)]
pub struct MapSurfaceToScaledWindow {
    pub surface_id: u16,
    pub window_id: u64,
    pub mapped_width: u32,
    pub mapped_height: u32,
    pub target_width: u32,
    pub target_height: u32,
}

impl MapSurfaceToScaledWindow {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 26 {
            bail!("Not enough data for MapSurfaceToScaledWindow");
        }

        Ok(Self {
            surface_id: data.get_u16_le(),
            window_id: data.get_u64_le(),
            mapped_width: data.get_u32_le(),
            mapped_height: data.get_u32_le(),
            target_width: data.get_u32_le(),
            target_height: data.get_u32_le(),
        })
    }
}
