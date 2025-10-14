//! Video Control DVC Channel
//!
//! Handles the Microsoft::Windows::RDS::Video::Control::v08.01 channel
//! for managing video presentations (start/stop).

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use ironrdp_video::{PresentationRequest, PresentationResponse, PacketType, VIDEO_CONTROL_CHANNEL_NAME};
use tracing::{info, warn};

use crate::video_redirect::SharedVideoRedirectionManager;

/// Video Control DVC message wrapper
struct VideoControlMessage {
    data: Vec<u8>,
}

impl ironrdp_core::Encode for VideoControlMessage {
    fn encode(&self, dst: &mut ironrdp_core::WriteCursor<'_>) -> ironrdp_core::EncodeResult<()> {
        dst.write_slice(&self.data);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "VideoControlMessage"
    }

    fn size(&self) -> usize {
        self.data.len()
    }
}

impl ironrdp_dvc::DvcEncode for VideoControlMessage {}

/// Video Control DVC Processor
pub struct VideoControlProcessor {
    manager: SharedVideoRedirectionManager,
    channel_id: Option<u32>,
}

impl VideoControlProcessor {
    pub fn new(manager: SharedVideoRedirectionManager) -> Self {
        Self {
            manager,
            channel_id: None,
        }
    }
}

impl AsAny for VideoControlProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for VideoControlProcessor {
    fn channel_name(&self) -> &str {
        VIDEO_CONTROL_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!("🎬 Video Control channel opened! channel_id={}", channel_id);
        self.channel_id = Some(channel_id);
        Ok(Vec::new())
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        info!(
            "📥 Video Control: Received {} bytes on channel {}",
            payload.len(),
            channel_id
        );

        if payload.is_empty() {
            return Ok(Vec::new());
        }

        // First byte is packet type
        let packet_type = match PacketType::from_u8(payload[0]) {
            Ok(t) => t,
            Err(e) => {
                warn!("❌ Video Control: Invalid packet type: {}", e);
                return Ok(Vec::new());
            }
        };

        match packet_type {
            PacketType::PresentationRequest => {
                // Parse presentation request
                let request = match PresentationRequest::parse(&payload[1..]) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("❌ Video Control: Failed to parse presentation request: {}", e);
                        return Ok(Vec::new());
                    }
                };

                let presentation_id = request.presentation_id;

                // Handle in manager
                if let Ok(mut manager) = self.manager.lock() {
                    if let Err(e) = manager.handle_presentation_request(request) {
                        warn!("❌ Video Control: Failed to handle presentation request: {}", e);
                    }
                } else {
                    warn!("❌ Video Control: Failed to lock manager");
                }

                // Send response
                let response = PresentationResponse { presentation_id };
                let mut response_data = vec![PacketType::PresentationResponse as u8];
                response_data.extend_from_slice(&response.encode());

                info!(
                    "📤 Video Control: Sending presentation response (id={})",
                    presentation_id
                );

                Ok(vec![
                    Box::new(VideoControlMessage { data: response_data }) as DvcMessage
                ])
            }
            _ => {
                warn!("⚠️ Video Control: Unexpected packet type: {:?}", packet_type);
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!("🔌 Video Control channel closed (ID: {})", channel_id);
        self.channel_id = None;
    }
}
