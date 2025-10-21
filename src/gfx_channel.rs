//! GFX Dynamic Virtual Channel integration
//!
//! This module bridges IronRDP's DVC system with our RDPEGFX implementation.

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_gfx::GfxClient;
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

    /// Process UDP data containing H.264 frames
    ///
    /// This method handles RDPEGFX data received via UDP transport (multitransport).
    /// The data may be zGFX compressed and contains GFX PDUs with H.264-encoded frames.
    pub fn process_udp_data(&mut self, data: &[u8]) -> PduResult<Vec<DvcMessage>> {
        use tracing::info;

        info!("📦 RDPEGFX via UDP: Processing {} bytes", data.len());

        // Try to decompress with zGFX first
        // UDP packets may be compressed or uncompressed depending on server settings
        let decompressed = match zgfx::decompress(data) {
            Ok(decompressed) => {
                info!(
                    "📦 RDPEGFX via UDP: zGFX decompressed {} -> {} bytes",
                    data.len(),
                    decompressed.len()
                );
                decompressed
            }
            Err(_) => {
                // If decompression fails, try processing as uncompressed
                info!(
                    "📦 RDPEGFX via UDP: Processing as uncompressed ({} bytes)",
                    data.len()
                );
                data.to_vec()
            }
        };

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
            .map(|data| {
                let packet = Self::wrap_zgfx_packet(&data);
                Box::new(GfxDvcMessage { data: packet }) as DvcMessage
            })
            .collect())
    }

    /// Wrap raw RDPEGFX payload in a zGFX segmented packet
    fn wrap_zgfx_packet(payload: &[u8]) -> Vec<u8> {
        const DESCRIPTOR_SINGLE: u8 = 0xE0; // ZGFX_SEGMENTED_SINGLE
        const HEADER_RDP8_UNCOMPRESSED: u8 = 0x04; // RDP8 stream, uncompressed

        let mut packet = Vec::with_capacity(payload.len() + 2);
        packet.push(DESCRIPTOR_SINGLE);
        packet.push(HEADER_RDP8_UNCOMPRESSED);
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
        if let Some(first) = messages.first() {
            use tracing::debug;
            debug!(
                "RDPEGFX CAPS_ADVERTISE first bytes: {:02X?}",
                &first.get(0..16.min(first.len())).unwrap_or(&[])
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

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        use tracing::info;
        info!(
            "📥 RDPEGFX: Received {} bytes on channel {}",
            payload.len(),
            channel_id
        );

        // Decompress with zGFX (preserving error details in log)
        let decompressed = zgfx::decompress(payload).map_err(|e| {
            // Log detailed error information for debugging
            info!("❌ RDPEGFX: zGFX decompression failed: {:?}", e);
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
            use tracing::debug;
            if let Some(first) = messages.first() {
                debug!(
                    "RDPEGFX response first bytes: {:02X?}",
                    &first.get(0..16.min(first.len())).unwrap_or(&[])
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::GfxState;
    use crate::rdp::{RdpEventSender, RdpOutputEvent};
    use zgfx;

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

        // Outgoing messages must be zGFX wrapped (single segment)
        assert_eq!(original[0], 0xE0, "Missing zGFX segmented header");
        assert_eq!(original[1], 0x04, "Unexpected zGFX flags byte");

        let decompressed = zgfx::decompress(&original).expect("decompress zGFX payload");

        // Validate header parses correctly
        let mut payload = &decompressed[..];
        let header = Header::parse(&mut payload).expect("parse header");
        assert_eq!(header.cmd_id, GfxCmdId::CapsAdvertise);
        assert_eq!(header.flags, 0);
        assert_eq!(header.pdu_length as usize, decompressed.len());

        // Remaining data should start with caps set count
        let count = u16::from_le_bytes([payload[0], payload[1]]);
        assert_eq!(count, 8);
    }
}
