//! GFX Dynamic Virtual Channel integration
//!
//! This module bridges IronRDP's DVC system with our RDPEGFX implementation.

use anyhow::{Result, Context as _};
use ironrdp_gfx::GfxClient;
use ironrdp::svc::SvcMessage;
use tracing::{debug, trace, warn};

use crate::gfx::GfxState;

/// RDPEGFX dynamic channel name
pub const GFX_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Graphics";

/// GFX channel manager
pub struct GfxChannel {
    /// GFX client
    client: GfxClient<GfxState>,
    /// Channel state
    state: ChannelState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelState {
    /// Channel not yet opened
    Closed,
    /// Channel open, waiting for CAPS_CONFIRM
    WaitingForCaps,
    /// Channel fully active
    Active,
}

impl GfxChannel {
    /// Create a new GFX channel
    pub fn new(gfx_state: GfxState) -> Self {
        let client = GfxClient::new(gfx_state, false, false);

        Self {
            client,
            state: ChannelState::Closed,
        }
    }

    /// Called when the channel is opened
    pub fn on_channel_open(&mut self) -> Result<Vec<SvcMessage>> {
        debug!("GFX channel opened");

        // Send capabilities advertisement
        self.client.send_caps_advertise()
            .context("Failed to send GFX capabilities")?;

        self.state = ChannelState::WaitingForCaps;

        // Get outgoing messages
        let messages = self.client.ctx.take_outgoing_messages();
        Ok(messages.into_iter().map(SvcMessage::from).collect())
    }

    /// Process incoming data from the GFX channel
    pub fn process_data(&mut self, data: &[u8]) -> Result<Vec<SvcMessage>> {
        trace!("Processing {} bytes from GFX channel", data.len());

        // Decompress with zGFX
        let decompressed = zgfx::decompress(data)
            .context("Failed to decompress GFX data")?;

        trace!("Decompressed {} -> {} bytes", data.len(), decompressed.len());

        // Process PDUs
        self.client.process_pdu_stream(&decompressed)
            .context("Failed to process GFX PDU stream")?;

        // Check if we transitioned to active state
        if self.state == ChannelState::WaitingForCaps && self.client.cap_version().is_some() {
            debug!("GFX channel now active, negotiated version: {:?}", self.client.cap_version());
            self.state = ChannelState::Active;
        }

        // Get any outgoing messages (acknowledgements, etc.)
        let messages = self.client.ctx.take_outgoing_messages();
        Ok(messages.into_iter().map(SvcMessage::from).collect())
    }

    /// Check if the channel is active
    pub fn is_active(&self) -> bool {
        self.state == ChannelState::Active
    }
}
