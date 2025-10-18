//! GFX Dynamic Virtual Channel integration
//!
//! This module bridges IronRDP's DVC system with our RDPEGFX implementation.

use anyhow::{Context as _, Result};
use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_gfx::GfxClient;
use ironrdp_pdu::PduResult;
use tracing::{debug, trace};

use crate::gfx::GfxState;

/// RDPEGFX dynamic channel name
pub const GFX_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Graphics";

/// GFX message wrapper for DVC
struct GfxDvcMessage {
    data: Vec<u8>,
}

impl ironrdp_core::Encode for GfxDvcMessage {
    fn encode(&self, dst: &mut ironrdp_core::WriteCursor<'_>) -> ironrdp_core::EncodeResult<()> {
        dst.write_slice(&self.data);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "GfxDvcMessage"
    }

    fn size(&self) -> usize {
        self.data.len()
    }
}

impl ironrdp_dvc::DvcEncode for GfxDvcMessage {}

/// GFX DVC Processor
pub struct GfxDvcProcessor {
    /// GFX client
    client: GfxClient<GfxState>,
    /// Current channel ID (set when channel opens)
    channel_id: Option<u32>,
}

impl GfxDvcProcessor {
    /// Create a new GFX DVC processor
    pub fn new(gfx_state: GfxState) -> Self {
        let client = GfxClient::new(gfx_state, false, false);

        Self {
            client,
            channel_id: None,
        }
    }

    /// Wrap raw RDPEGFX payload in a zGFX segmented packet
    fn wrap_zgfx_packet(payload: &[u8]) -> Vec<u8> {
        const DESCRIPTOR_SINGLE: u8 = 0xE0; // ZGFX_SEGMENTED_SINGLE
        // FreedRDP sets the packet header to ZGFX_PACKET_COMPR_TYPE_RDP8 (0x04)
        // even when the payload is left uncompressed. Mirror that behaviour so
        // the server recognizes the stream as RDPEGFX/RDP8 encoded data.
        const HEADER_RDP8: u8 = 0x04;

        let mut packet = Vec::with_capacity(payload.len() + 2);
        packet.push(DESCRIPTOR_SINGLE);
        packet.push(HEADER_RDP8);
        packet.extend_from_slice(payload);
        packet
    }
}

impl AsAny for GfxDvcProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for GfxDvcProcessor {
    fn channel_name(&self) -> &str {
        GFX_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        use tracing::info;
        info!("🎨 RDPEGFX channel opened! channel_id={}", channel_id);
        self.channel_id = Some(channel_id);

        // Send capabilities advertisement
        if let Err(_e) = self.client.send_caps_advertise() {
            info!("⚠️ Failed to send CAPS_ADVERTISE");
            return Ok(Vec::new());
        }

        // Get outgoing messages
        let messages = self.client.ctx.take_outgoing_messages();
        info!(
            "📤 Sending CAPS_ADVERTISE ({} bytes)",
            messages.iter().map(|m| m.len()).sum::<usize>()
        );
        Ok(messages
            .into_iter()
            .map(|data| {
                let packet = Self::wrap_zgfx_packet(&data);
                Box::new(GfxDvcMessage { data: packet }) as DvcMessage
            })
            .collect())
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        use tracing::info;
        info!(
            "📥 RDPEGFX: Received {} bytes on channel {}",
            payload.len(),
            channel_id
        );

        // Decompress with zGFX
        let decompressed = zgfx::decompress(payload).map_err(|_e| {
            info!("❌ RDPEGFX: zGFX decompression failed");
            ironrdp_pdu::pdu_other_err!("GFX decompress failed")
        })?;

        info!(
            "📦 RDPEGFX: Decompressed {} -> {} bytes",
            payload.len(),
            decompressed.len()
        );

        // Process PDUs
        self.client.process_pdu_stream(&decompressed).map_err(|e| {
            info!("❌ RDPEGFX: PDU processing failed: {:?}", e);
            ironrdp_pdu::pdu_other_err!("GFX PDU processing failed")
        })?;

        // Get any outgoing messages (acknowledgements, etc.)
        let messages = self.client.ctx.take_outgoing_messages();
        if !messages.is_empty() {
            info!(
                "📤 RDPEGFX: Sending {} response messages ({} bytes)",
                messages.len(),
                messages.iter().map(|m| m.len()).sum::<usize>()
            );
        }
        Ok(messages
            .into_iter()
            .map(|data| {
                let packet = Self::wrap_zgfx_packet(&data);
                Box::new(GfxDvcMessage { data: packet }) as DvcMessage
            })
            .collect())
    }

    fn close(&mut self, channel_id: u32) {
        use tracing::info;
        info!("🔌 RDPEGFX channel closed (ID: {})", channel_id);
        self.channel_id = None;
    }
}
