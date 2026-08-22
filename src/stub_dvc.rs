/// Stub DVC processor that accepts a channel but does nothing with the data.
/// This is used for channels we need to acknowledge for protocol compliance
/// but don't actually need to handle (e.g., MouseCursor, CoreInput, etc.)
use ironrdp_core::impl_as_any;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use tracing::debug;

#[derive(Debug)]
pub struct StubDvcProcessor {
    channel_name: String,
}

impl StubDvcProcessor {
    pub fn new(channel_name: impl Into<String>) -> Self {
        Self {
            channel_name: channel_name.into(),
        }
    }
}

impl_as_any!(StubDvcProcessor);

impl DvcProcessor for StubDvcProcessor {
    fn channel_name(&self) -> &str {
        &self.channel_name
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        // No initialization needed for stub
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // Nothing is done with it, but a channel that turns out to carry traffic is worth
        // knowing about: it is the difference between a stub that is right and one that is a
        // hole in the session.
        debug!(
            channel = %self.channel_name,
            bytes = payload.len(),
            "🕳 data on a stubbed dynamic channel"
        );
        Ok(Vec::new())
    }

    fn supports_udp_transport(&self) -> bool {
        // Stubs should stay on TCP to avoid protocol issues
        false
    }
}
