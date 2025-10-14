//! H.264/AVC Codec Capability Sidecar
//!
//! This module provides a workaround to inject H.264/AVC codec capabilities
//! into BitmapCodecs, since upstream IronRDP doesn't support H.264 in the
//! CodecProperty enum.
//!
//! This is necessary for Windows Server to activate RDPEGFX with hardware
//! encoding (AVC444) instead of falling back to Video Redirection mode.

use tracing::info;

/// H.264 GUID as defined in MS-RDPEGFX
/// {3F8B4284-64AD-4F55-88FB-18D2E8C2A528}
const GUID_H264: [u8; 16] = [
    0x84, 0x42, 0x8B, 0x3F, // Data1: 3F8B4284
    0xAD, 0x64,             // Data2: 64AD
    0x55, 0x4F,             // Data3: 4F55
    0x88, 0xFB, 0x18, 0xD2, 0xE8, 0xC2, 0xA5, 0x28, // Data4
];

/// H.264 Codec ID (we'll use 4, following RemoteFX which uses 3)
const CODEC_ID_H264: u8 = 4;


/// Manually construct H.264 codec capability bytes
///
/// This creates the raw PDU bytes for H.264/AVC444 codec capability:
/// ```text
/// GUID (16 bytes): H.264 GUID
/// CodecID (1 byte): 4
/// CodecPropertiesLength (2 bytes, LE): Length of properties
/// CodecProperties:
///   H264_CAPSET_FLAGS (1 byte):
///     - AVC444_SUPPORT (0x02): Supports AVC444 (hardware encoding)
/// ```
pub fn h264_codec_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();

    // GUID (16 bytes) - in little-endian byte order as sent on wire
    bytes.extend_from_slice(&GUID_H264);

    // Codec ID (1 byte)
    bytes.push(CODEC_ID_H264);

    // Codec Properties Length (2 bytes, little-endian)
    // We'll send a minimal H.264 capability:
    // - 1 byte for H264_CAPSET_FLAGS
    bytes.extend_from_slice(&1u16.to_le_bytes());

    // Codec Properties
    // H264_CAPSET_FLAGS: AVC444_SUPPORT (0x02)
    bytes.push(0x02);

    bytes
}

/// Create a BitmapConfig with H.264 support
///
/// This creates a BitmapCodecs with H.264 codec using the patched IronRDP
/// CodecProperty enum that now supports H.264.
pub fn create_bitmap_config_with_h264(
    lossy_compression: bool,
    color_depth: u32,
) -> anyhow::Result<ironrdp::connector::BitmapConfig> {
    use ironrdp_pdu::rdp::capability_sets::{client_codecs_capabilities, Codec, CodecProperty};

    // Create the default BitmapCodecs (RemoteFX)
    let mut codecs = client_codecs_capabilities(&[])
        .map_err(|e| anyhow::anyhow!("Failed to get default codecs: {}", e))?;

    // Add H.264 codec with AVC444_SUPPORT flag (0x02)
    // This enables hardware-accelerated H.264 encoding on the server (RDP 10.0+)
    const AVC444_SUPPORT: u8 = 0x02;
    codecs.0.push(Codec {
        id: CODEC_ID_H264,
        property: CodecProperty::H264(AVC444_SUPPORT),
    });

    info!("✅ Created BitmapConfig with {} codecs (including H.264/AVC444)", codecs.0.len());
    info!("📋 H.264 GUID: {:02X?}", GUID_H264);
    info!("📋 H.264 Codec ID: {}", CODEC_ID_H264);
    info!("📋 H.264 Flags: 0x{:02X} (AVC444_SUPPORT)", AVC444_SUPPORT);

    Ok(ironrdp::connector::BitmapConfig {
        lossy_compression,
        color_depth,
        codecs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_h264_codec_bytes() {
        let bytes = h264_codec_bytes();

        // Should be 16 (GUID) + 1 (ID) + 2 (len) + 1 (flags) = 20 bytes
        assert_eq!(bytes.len(), 20);

        // Check GUID
        assert_eq!(&bytes[0..16], &GUID_H264);

        // Check Codec ID
        assert_eq!(bytes[16], CODEC_ID_H264);

        // Check properties length (1 byte)
        assert_eq!(&bytes[17..19], &1u16.to_le_bytes());

        // Check AVC444 flag
        assert_eq!(bytes[19], 0x02);
    }

}
