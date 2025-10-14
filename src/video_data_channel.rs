//! Video Data DVC Channel
//!
//! Handles the Microsoft::Windows::RDS::Video::Data::v08.01 channel
//! for receiving H.264 video samples.

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use ironrdp_video::{PacketType, VideoData, VIDEO_DATA_CHANNEL_NAME};
use tracing::{info, warn};

use crate::video_redirect::SharedVideoRedirectionManager;

/// Video Data DVC Processor
pub struct VideoDataProcessor {
    manager: SharedVideoRedirectionManager,
    channel_id: Option<u32>,
}

impl VideoDataProcessor {
    pub fn new(manager: SharedVideoRedirectionManager) -> Self {
        Self {
            manager,
            channel_id: None,
        }
    }
}

impl AsAny for VideoDataProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for VideoDataProcessor {
    fn channel_name(&self) -> &str {
        VIDEO_DATA_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!("📹 Video Data channel opened! channel_id={}", channel_id);
        self.channel_id = Some(channel_id);
        Ok(Vec::new())
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        if payload.is_empty() {
            return Ok(Vec::new());
        }

        // First byte is packet type
        let packet_type = match PacketType::from_u8(payload[0]) {
            Ok(t) => t,
            Err(e) => {
                warn!("❌ Video Data: Invalid packet type: {}", e);
                return Ok(Vec::new());
            }
        };

        match packet_type {
            PacketType::VideoData => {
                // Parse video data
                let data = match VideoData::parse(&payload[1..]) {
                    Ok(d) => d,
                    Err(e) => {
                        warn!("❌ Video Data: Failed to parse video data: {}", e);
                        return Ok(Vec::new());
                    }
                };

                // Handle in manager
                if let Ok(mut manager) = self.manager.lock() {
                    if let Err(e) = manager.handle_video_data(data) {
                        warn!("❌ Video Data: Failed to handle video data: {}", e);
                    }
                } else {
                    warn!("❌ Video Data: Failed to lock manager");
                }

                // No response needed for video data
                Ok(Vec::new())
            }
            _ => {
                warn!("⚠️ Video Data: Unexpected packet type: {:?}", packet_type);
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!("🔌 Video Data channel closed (ID: {})", channel_id);
        self.channel_id = None;
    }
}
