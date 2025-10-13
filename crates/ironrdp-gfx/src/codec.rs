//! Codec IDs and constants for RDPEGFX

/// Codec identifiers (MS-RDPEGFX 2.2.3.1)
pub mod codec_id {
    pub const UNCOMPRESSED: u16 = 0x0000;
    pub const CAVIDEO: u16 = 0x0003;
    pub const CLEARCODEC: u16 = 0x0008;
    pub const RFX_PROGRESSIVE: u16 = 0x0009;
    pub const PLANAR: u16 = 0x000A;
    pub const AVC420: u16 = 0x000B; // H.264/AVC YUV420
    pub const ALPHA: u16 = 0x000C;
    pub const RFX_PROGRESSIVE_V2: u16 = 0x000D;
    pub const AVC444: u16 = 0x000E; // H.264/AVC YUV444
    pub const AVC444V2: u16 = 0x000F; // H.264/AVC YUV444 v2
}

/// Pixel format identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PixelFormat {
    Xrgb8888 = 0x20, // Maps to BGRX32
    Argb8888 = 0x21, // Maps to BGRA32
}

impl TryFrom<u8> for PixelFormat {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x20 => Ok(PixelFormat::Xrgb8888),
            0x21 => Ok(PixelFormat::Argb8888),
            _ => anyhow::bail!("Unknown pixel format: 0x{:02X}", value),
        }
    }
}

/// Get codec name for logging
pub fn codec_name(codec_id: u16) -> &'static str {
    match codec_id {
        codec_id::UNCOMPRESSED => "Uncompressed",
        codec_id::CAVIDEO => "CAVideo",
        codec_id::CLEARCODEC => "ClearCodec",
        codec_id::RFX_PROGRESSIVE => "RFX Progressive",
        codec_id::PLANAR => "Planar",
        codec_id::AVC420 => "AVC420",
        codec_id::ALPHA => "Alpha",
        codec_id::RFX_PROGRESSIVE_V2 => "RFX Progressive V2",
        codec_id::AVC444 => "AVC444",
        codec_id::AVC444V2 => "AVC444v2",
        _ => "Unknown",
    }
}
