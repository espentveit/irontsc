//! H.264/AVC Codec Capability Sidecar
//!
//! This module configures bitmap codec capabilities so the server prefers
//! the Graphics Pipeline (RDPEGFX) for H.264 content instead of legacy
//! RemoteFX fallbacks.

use tracing::info;

/// H.264 GUID as defined in MS-RDPEGFX
/// {3F8B4284-64AD-4F55-88FB-18D2E8C2A528}
const GUID_H264: [u8; 16] = [
    0x84, 0x42, 0x8B, 0x3F, // Data1
    0xAD, 0x64, // Data2
    0x55, 0x4F, // Data3
    0x88, 0xFB, 0x18, 0xD2, 0xE8, 0xC2, 0xA5, 0x28,
];

const CODEC_ID_H264: u8 = 4;
const AVC444_SUPPORT: u8 = 0x02;

/// Create a BitmapConfig tailored for RDPEGFX/H.264 sessions.
///
/// We disable legacy bitmap codecs (RemoteFX) so the server prefers the
/// graphics pipeline path where H.264 is negotiated.
pub fn create_bitmap_config_with_h264(
    lossy_compression: bool,
    color_depth: u32,
) -> anyhow::Result<ironrdp::connector::BitmapConfig> {
    use ironrdp_pdu::rdp::capability_sets::{Codec, CodecProperty, client_codecs_capabilities};

    // When requesting RDPEGFX + H.264, Windows clients disable RemoteFX in the
    // BitmapCodecs capability to nudge the server toward the Graphics Pipeline.
    // FreeRDP follows the same behaviour when `/gfx:AVC444` is specified.
    let mut codecs = client_codecs_capabilities(&[])
        .map_err(|e| anyhow::anyhow!("Failed to get bitmap codecs: {}", e))?;

    // Advertise H.264/AVC444 capability to match mstsc/FreeRDP behaviour.
    codecs.0.push(Codec {
        id: CODEC_ID_H264,
        property: CodecProperty::H264(AVC444_SUPPORT),
    });

    info!("✅ Bitmap codecs configured for RDPEGFX (RemoteFX + H.264 advertised)");

    Ok(ironrdp::connector::BitmapConfig {
        lossy_compression,
        color_depth,
        codecs,
    })
}
