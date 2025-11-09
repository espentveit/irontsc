// MouseCursor DVC channel processor
// Microsoft::Windows::RDS::MouseCursor channel
//
// This channel is created by the server but typically receives no data.
// We implement it as a pass-through that acknowledges creation.

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use std::fmt;
use tracing::info;

pub const CHANNEL_NAME: &str = "Microsoft::Windows::RDS::MouseCursor";

#[derive(Debug)]
pub struct MouseCursorProcessor;

impl MouseCursorProcessor {
    pub fn new() -> Self {
        Self
    }
}

impl AsAny for MouseCursorProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for MouseCursorProcessor {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(
            "🖱️  MouseCursor channel opened! channel_id={}",
            channel_id
        );
        // No initial messages to send
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        info!("MouseCursor: Received {} bytes (ignored)", payload.len());
        // This channel typically receives no data - acknowledge and ignore
        Ok(Vec::new())
    }
}

impl fmt::Display for MouseCursorProcessor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MouseCursorProcessor")
    }
}
