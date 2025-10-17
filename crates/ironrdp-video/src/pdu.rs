//! PDU structures for MS-RDPEVOR protocol

use anyhow::{bail, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// TSMM packet types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    PresentationRequest = 1,
    PresentationResponse = 2,
    ClientNotification = 3,
    VideoData = 4,
}

impl PacketType {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::PresentationRequest),
            2 => Ok(Self::PresentationResponse),
            3 => Ok(Self::ClientNotification),
            4 => Ok(Self::VideoData),
            _ => bail!("Invalid packet type: {}", value),
        }
    }
}

/// Presentation commands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PresentationCommand {
    Start = 1,
    Stop = 2,
}

impl PresentationCommand {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Start),
            2 => Ok(Self::Stop),
            _ => bail!("Invalid presentation command: {}", value),
        }
    }
}

/// Video data flags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDataFlags(pub u8);

impl VideoDataFlags {
    pub const HAS_TIMESTAMPS: u8 = 0x01;
    pub const KEYFRAME: u8 = 0x02;
    pub const NEW_FRAMERATE: u8 = 0x04;

    pub fn has_timestamps(&self) -> bool {
        self.0 & Self::HAS_TIMESTAMPS != 0
    }

    pub fn is_keyframe(&self) -> bool {
        self.0 & Self::KEYFRAME != 0
    }

    pub fn has_new_framerate(&self) -> bool {
        self.0 & Self::NEW_FRAMERATE != 0
    }
}

/// TSMM_PRESENTATION_REQUEST (server → client)
#[derive(Debug, Clone)]
pub struct PresentationRequest {
    pub presentation_id: u8,
    pub version: u8,
    pub command: PresentationCommand,
    pub frame_rate: u8,
    pub source_width: u32,
    pub source_height: u32,
    pub scaled_width: u32,
    pub scaled_height: u32,
    pub timestamp_offset: u64,
    pub geometry_mapping_id: u64,
    pub video_subtype_id: [u8; 16],
    pub extra_data: Vec<u8>, // H.264 SPS/PPS NALs
}

impl PresentationRequest {
    const MIN_SIZE: usize = 1 + 1 + 1 + 1 + 4 + 4 + 4 + 4 + 8 + 8 + 16 + 4; // 56 bytes

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < Self::MIN_SIZE {
            bail!(
                "Not enough data for PresentationRequest: need {}, have {}",
                Self::MIN_SIZE,
                data.len()
            );
        }

        let mut buf = Bytes::copy_from_slice(data);

        let presentation_id = buf.get_u8();
        let version = buf.get_u8();
        let command = PresentationCommand::from_u8(buf.get_u8())?;
        let frame_rate = buf.get_u8();
        let source_width = buf.get_u32_le();
        let source_height = buf.get_u32_le();
        let scaled_width = buf.get_u32_le();
        let scaled_height = buf.get_u32_le();
        let timestamp_offset = buf.get_u64_le();
        let geometry_mapping_id = buf.get_u64_le();

        let mut video_subtype_id = [0u8; 16];
        buf.copy_to_slice(&mut video_subtype_id);

        let extra_data_len = buf.get_u32_le() as usize;

        if buf.remaining() < extra_data_len {
            bail!(
                "Not enough data for extra_data: need {}, have {}",
                extra_data_len,
                buf.remaining()
            );
        }

        let extra_data = buf.copy_to_bytes(extra_data_len).to_vec();

        Ok(Self {
            presentation_id,
            version,
            command,
            frame_rate,
            source_width,
            source_height,
            scaled_width,
            scaled_height,
            timestamp_offset,
            geometry_mapping_id,
            video_subtype_id,
            extra_data,
        })
    }
}

/// TSMM_PRESENTATION_RESPONSE (client → server)
#[derive(Debug, Clone)]
pub struct PresentationResponse {
    pub presentation_id: u8,
}

impl PresentationResponse {
    pub fn encode(&self) -> Vec<u8> {
        vec![self.presentation_id]
    }
}

/// TSMM_VIDEO_DATA (server → client)
#[derive(Debug, Clone)]
pub struct VideoData {
    pub presentation_id: u8,
    pub version: u8,
    pub flags: VideoDataFlags,
    pub timestamp: u64,
    pub duration: u64,
    pub current_packet_index: u16,
    pub packets_in_sample: u16,
    pub sample_number: u32,
    pub sample_data: Vec<u8>,
}

impl VideoData {
    const MIN_SIZE: usize = 1 + 1 + 1 + 8 + 8 + 2 + 2 + 4 + 4; // 31 bytes

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < Self::MIN_SIZE {
            bail!(
                "Not enough data for VideoData: need {}, have {}",
                Self::MIN_SIZE,
                data.len()
            );
        }

        let mut buf = Bytes::copy_from_slice(data);

        let presentation_id = buf.get_u8();
        let version = buf.get_u8();
        let flags = VideoDataFlags(buf.get_u8());
        let timestamp = buf.get_u64_le();
        let duration = buf.get_u64_le();
        let current_packet_index = buf.get_u16_le();
        let packets_in_sample = buf.get_u16_le();
        let sample_number = buf.get_u32_le();
        let sample_len = buf.get_u32_le() as usize;

        if buf.remaining() < sample_len {
            bail!(
                "Not enough data for sample: need {}, have {}",
                sample_len,
                buf.remaining()
            );
        }

        let sample_data = buf.copy_to_bytes(sample_len).to_vec();

        Ok(Self {
            presentation_id,
            version,
            flags,
            timestamp,
            duration,
            current_packet_index,
            packets_in_sample,
            sample_number,
            sample_data,
        })
    }

    /// Check if this is a complete sample (not fragmented)
    pub fn is_complete(&self) -> bool {
        self.current_packet_index == 0 && self.packets_in_sample == 1
    }

    /// Check if this is the first fragment
    pub fn is_first_fragment(&self) -> bool {
        self.current_packet_index == 0
    }

    /// Check if this is the last fragment
    pub fn is_last_fragment(&self) -> bool {
        self.current_packet_index + 1 == self.packets_in_sample
    }
}

/// Client notification types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotificationType {
    NetworkError = 1,
    FramerateOverride = 2,
}

impl NotificationType {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::NetworkError),
            2 => Ok(Self::FramerateOverride),
            _ => bail!("Invalid notification type: {}", value),
        }
    }
}

/// TSMM_CLIENT_NOTIFICATION (client → server)
#[derive(Debug, Clone)]
pub struct ClientNotification {
    pub presentation_id: u8,
    pub notification_type: NotificationType,
    pub desired_frame_rate: Option<u32>, // Only for FramerateOverride
}

impl ClientNotification {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = BytesMut::new();
        buf.put_u8(self.presentation_id);
        buf.put_u8(self.notification_type as u8);

        if let Some(rate) = self.desired_frame_rate {
            buf.put_u32_le(0); // Flags (reserved)
            buf.put_u32_le(rate);
        }

        buf.to_vec()
    }

    /// Create a framerate override notification
    pub fn framerate_override(presentation_id: u8, desired_fps: u32) -> Self {
        Self {
            presentation_id,
            notification_type: NotificationType::FramerateOverride,
            desired_frame_rate: Some(desired_fps),
        }
    }

    /// Create a network error notification
    pub fn network_error(presentation_id: u8) -> Self {
        Self {
            presentation_id,
            notification_type: NotificationType::NetworkError,
            desired_frame_rate: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_data_flags() {
        let flags = VideoDataFlags(0x07); // All flags set
        assert!(flags.has_timestamps());
        assert!(flags.is_keyframe());
        assert!(flags.has_new_framerate());

        let flags = VideoDataFlags(0x00);
        assert!(!flags.has_timestamps());
        assert!(!flags.is_keyframe());
        assert!(!flags.has_new_framerate());
    }

    #[test]
    fn test_presentation_response_encode() {
        let response = PresentationResponse {
            presentation_id: 42,
        };
        let encoded = response.encode();
        assert_eq!(encoded, vec![42]);
    }

    #[test]
    fn test_video_data_fragmentation() {
        let data = VideoData {
            presentation_id: 1,
            version: 1,
            flags: VideoDataFlags(0x02),
            timestamp: 0,
            duration: 0,
            current_packet_index: 0,
            packets_in_sample: 1,
            sample_number: 42,
            sample_data: vec![0x00, 0x01, 0x02],
        };

        assert!(data.is_complete());
        assert!(data.is_first_fragment());
        assert!(data.is_last_fragment());

        let fragmented = VideoData {
            current_packet_index: 1,
            packets_in_sample: 3,
            ..data
        };

        assert!(!fragmented.is_complete());
        assert!(!fragmented.is_first_fragment());
        assert!(!fragmented.is_last_fragment());
    }
}
