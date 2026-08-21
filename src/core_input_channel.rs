// CoreInput DVC channel processor
// Microsoft::Windows::RDS::CoreInput channel
//
// This channel is created by the server but typically receives no data.
// We implement it as a pass-through that acknowledges creation.

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use std::fmt;
use tracing::info;

pub const CHANNEL_NAME: &str = "Microsoft::Windows::RDS::CoreInput";

#[derive(Debug)]
pub struct CoreInputProcessor;

impl CoreInputProcessor {
    pub fn new() -> Self {
        Self
    }
}

impl AsAny for CoreInputProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for CoreInputProcessor {
    fn channel_name(&self) -> &str {
        CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!("🎹 CoreInput channel opened! channel_id={}", channel_id);
        // No initial messages to send
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        info!("CoreInput: Received {} bytes (ignored)", payload.len());
        // This channel typically receives no data - acknowledge and ignore
        Ok(Vec::new())
    }
}

impl fmt::Display for CoreInputProcessor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CoreInputProcessor")
    }
}
