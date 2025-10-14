//! MS-RDPEVOR: Remote Desktop Protocol: Video Optimized Remoting Virtual Channel Extension
//!
//! This module implements the Video Redirection protocol for RDP, which allows
//! selective H.264 encoding of video regions while using standard RDP compression
//! for static UI elements.

pub mod pdu;

pub use pdu::*;

/// Video Control DVC channel name
pub const VIDEO_CONTROL_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Video::Control::v08.01";

/// Video Data DVC channel name
pub const VIDEO_DATA_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Video::Data::v08.01";

/// H.264 video format GUID (MFVideoFormat_H264)
pub const H264_GUID: [u8; 16] = [
    0x48, 0x32, 0x36, 0x34, // 'H', '2', '6', '4'
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];
