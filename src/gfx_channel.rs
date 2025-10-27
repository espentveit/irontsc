//! GFX Dynamic Virtual Channel integration
//!
//! This module bridges IronRDP's DVC system with our RDPEGFX implementation.

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor, pdu::SoftSyncTunnelType};
use ironrdp_gfx::GfxClient;
use ironrdp_graphics::zgfx::{Decompressor as ZgfxDecompressor, ZgfxError};
use ironrdp_pdu::PduResult;

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
    /// zGFX decompressor (stateful)
    zgfx: ZgfxDecompressor,
    /// UDP transport active (when true, ignore TCP GFX data)
    udp_active: bool,
}

impl GfxDvcProcessor {
    /// Create a new GFX DVC processor
    pub fn new(gfx_state: GfxState) -> Self {
        let client = GfxClient::new(gfx_state, false, false);

        Self {
            client,
            channel_id: None,
            zgfx: ZgfxDecompressor::new(),
            udp_active: false,
        }
    }

    /// Update the event sender (must be called before processing GFX frames)
    pub fn set_event_sender(&mut self, event_sender: Box<dyn crate::rdp::RdpEventSender>) {
        self.client.ctx.set_event_sender(event_sender);
    }

    /// Enable UDP transport mode (TCP GFX data will be ignored)
    pub fn enable_udp_mode(&mut self) {
        use tracing::info;
        info!("🔄 GFX: Switching to UDP transport mode (TCP data will be ignored)");
        self.udp_active = true;
    }

    /// Process UDP data containing H.264 frames
    ///
    /// This method handles RDPEGFX data received via UDP transport (multitransport).
    /// The data may be zGFX compressed and contains GFX PDUs with H.264-encoded frames.
    pub fn process_udp_data(&mut self, data: &[u8]) -> PduResult<Vec<DvcMessage>> {
        use tracing::info;

        info!("📦 RDPEGFX via UDP: Processing {} bytes", data.len());

        // Try to decompress with zGFX first
        // UDP packets may be compressed or uncompressed depending on server settings
        let mut decompressed = Vec::new();
        let zgfx_result = self.zgfx.decompress(data, &mut decompressed);
        if let Err(err) = zgfx_result {
            info!(
                "📦 RDPEGFX via UDP: zGFX decompress failed ({err:?}), treating as raw ({} bytes)",
                data.len()
            );
            decompressed = data.to_vec();
        } else {
            info!(
                "📦 RDPEGFX via UDP: zGFX decompressed {} -> {} bytes",
                data.len(),
                decompressed.len()
            );
        }

        // Process GFX PDUs
        self.client.process_pdu_stream(&decompressed).map_err(|e| {
            info!("❌ RDPEGFX via UDP: PDU processing failed: {:?}", e);
            ironrdp_pdu::pdu_other_err!("GFX UDP PDU processing failed")
        })?;

        // Get any outgoing messages (acknowledgements, etc.)
        let messages = self.client.ctx.take_outgoing_messages();
        if !messages.is_empty() {
            info!(
                "📤 RDPEGFX via UDP: Sending {} response messages ({} bytes)",
                messages.len(),
                messages.iter().map(|m| m.len()).sum::<usize>()
            );
        }

        Ok(messages
            .into_iter()
            .map(|data| Box::new(GfxDvcMessage { data }) as DvcMessage)
            .collect())
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
        if let Some(first) = messages.first() {
            use tracing::debug;
            debug!(
                "RDPEGFX CAPS_ADVERTISE first bytes: {:02X?}",
                &first.get(0..16.min(first.len())).unwrap_or(&[])
            );
        }
        Ok(messages
            .into_iter()
            .map(|data| Box::new(GfxDvcMessage { data }) as DvcMessage)
            .collect())
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        use tracing::{debug, info};

        // If UDP is active, ignore TCP GFX data
        if self.udp_active {
            info!(
                "⏭️ RDPEGFX: Ignoring {} bytes on TCP channel {} (UDP mode active)",
                payload.len(),
                channel_id
            );
            return Ok(Vec::new());
        }

        debug!(
            "📥 RDPEGFX: Received {} bytes on channel {}",
            payload.len(),
            channel_id
        );

        // Decompress with zGFX (preserving error details in log)
        debug!(
            "📦 RDPEGFX: processing payload len={} bytes (first: {:02X?})",
            payload.len(),
            &payload[..payload.len().min(16)]
        );

        let mut decompressed = Vec::new();
        match self.zgfx.decompress(payload, &mut decompressed) {
            Ok(_) => {
                debug!(
                    "📦 RDPEGFX: Decompressed {} -> {} bytes (first: {:02X?})",
                    payload.len(),
                    decompressed.len(),
                    &decompressed[..decompressed.len().min(16)]
                );
            }
            Err(ZgfxError::InvalidSegmentedDescriptor) => {
                debug!(
                    "📦 RDPEGFX: Payload not segmented/compressed, using raw ({} bytes)",
                    payload.len()
                );
                decompressed = payload.to_vec();
            }
            Err(err) => {
                info!("❌ RDPEGFX: zGFX decompression failed: {:?}", err);
                return Err(ironrdp_pdu::pdu_other_err!("GFX decompress failed"));
            }
        }

        // Process PDUs
        self.client.process_pdu_stream(&decompressed).map_err(|e| {
            info!("❌ RDPEGFX: PDU processing failed: {:?}", e);
            ironrdp_pdu::pdu_other_err!("GFX PDU processing failed")
        })?;

        // Get any outgoing messages (acknowledgements, etc.)
        let messages = self.client.ctx.take_outgoing_messages();
        if !messages.is_empty() {
            debug!(
                "📤 RDPEGFX: Sending {} response messages ({} bytes total)",
                messages.len(),
                messages.iter().map(|m| m.len()).sum::<usize>()
            );
            for (i, msg) in messages.iter().enumerate() {
                debug!("  Message {}: {} bytes - {:02X?}", i, msg.len(), &msg[..]);
            }
        }
        Ok(messages
            .into_iter()
            .map(|data| Box::new(GfxDvcMessage { data }) as DvcMessage)
            .collect())
    }

    fn close(&mut self, channel_id: u32) {
        use tracing::info;
        info!("🔌 RDPEGFX channel closed (ID: {})", channel_id);
        self.channel_id = None;
    }

    fn on_soft_sync(&mut self, _channel_id: u32, _tunnel_type: SoftSyncTunnelType) {
        use tracing::info;
        info!("🔄 RDPEGFX: SoftSync request received, enabling UDP mode");
        self.enable_udp_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::GfxState;
    use crate::rdp::{RdpEventSender, RdpOutputEvent};

    struct NoopSender;

    impl RdpEventSender for NoopSender {
        fn send_event(&self, _event: RdpOutputEvent) -> Result<(), ()> {
            Ok(())
        }
    }

    #[test]
    fn caps_advertise_is_raw_rdpgfx_pdu() {
        use ironrdp_gfx::pdu::CmdId;

        let gfx_state = GfxState::new(Box::new(NoopSender)).expect("gfx state");
        let mut processor = GfxDvcProcessor::new(gfx_state);

        processor
            .client
            .send_caps_advertise()
            .expect("caps advertise");

        let messages = processor.client.ctx.take_outgoing_messages();
        assert_eq!(messages.len(), 1);
        let original = &messages[0];

        use ironrdp_gfx::pdu::CmdId as GfxCmdId;
        use ironrdp_gfx::pdu::Header;

        // Validate header parses correctly
        let mut payload = &original[..];
        let header = Header::parse(&mut payload).expect("parse header");
        assert_eq!(header.cmd_id, GfxCmdId::CapsAdvertise);
        assert_eq!(header.flags, 0);
        assert_eq!(header.pdu_length as usize, original.len());

        // Remaining data should start with caps set count (8 capability sets)
        let count = u16::from_le_bytes([payload[0], payload[1]]);
        assert_eq!(count, 8);
    }
}
