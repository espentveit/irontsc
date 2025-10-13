//! PDU structures and parsing for RDPEGFX

use anyhow::{bail, Context, Result};
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
            bail!("Invalid rectangle: ({},{}) - ({},{})", left, top, right, bottom);
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
}

impl CapsConfirm {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        if data.len() < 12 {
            bail!("Not enough data for CapsConfirm");
        }

        let version = data.get_u32_le();
        let length = data.get_u32_le();

        if length != 4 {
            bail!("Invalid CapsConfirm length: {}", length);
        }

        let flags = data.get_u32_le();

        Ok(Self { version, flags })
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
