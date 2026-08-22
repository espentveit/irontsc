//! Static virtual channels that the server has moved onto dynamic ones, in one direction.
//!
//! A server that has agreed Soft-Sync ([MS-RDPEDYC] 3.1.5.3) rearranges the session's
//! redirection channels, and does it asymmetrically. It stops writing on the static channel --
//! `cliprdr`, `rdpdr` -- and opens a dynamic channel of the same name to write on, while going
//! on *reading* the static one, and never reading the dynamic one it opened.
//!
//! So a client that keeps its processor on the static channel hears nothing, and a client that
//! moves it to the dynamic channel is not heard. What works is to split the directions: the
//! processor listens on the dynamic channel, where the server is talking, and everything it has
//! to say is collected here and sent back out on the static channel, where the server is
//! listening. [`crate::rdp`] does the sending, because only it knows the static channels.
//!
//! Neither half of that is what MS-RDPECLIP or MS-RDPEFS describes, and neither is a choice this
//! client is free to make differently: it is where a Windows server puts the traffic.

use ironrdp::svc::{SvcMessage, SvcProcessor, TransportContext};
use ironrdp_core::{Encode, EncodeResult, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcMessage, DvcProcessor};
use ironrdp_pdu::PduResult;
use tracing::{debug, info};

/// A channel PDU with nothing wrapped around it, ready for a dynamic channel.
///
/// Only used for the fallback in [`crate::rdp`], where a server turns out not to keep the
/// static channel after all; the ordinary path never sends anything this way.
pub struct RawPdu(pub Vec<u8>);

impl DvcEncode for RawPdu {}

impl Encode for RawPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "RedirectedChannelPdu"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

/// Wraps channel PDUs as the dynamic channel data they would travel as.
pub fn wrap(messages: Vec<SvcMessage>) -> PduResult<Vec<DvcMessage>> {
    messages
        .into_iter()
        .map(|message| {
            message
                .encode_to_vec()
                .map(|bytes| Box::new(RawPdu(bytes)) as DvcMessage)
                .map_err(|error| ironrdp_pdu::encode_err!(error))
        })
        .collect()
}

/// One static channel's processor, listening on the dynamic channel the server opened for it.
pub struct RedirectedChannel<P> {
    name: &'static str,
    processor: P,
    channel_id: Option<u32>,
    outgoing: Vec<SvcMessage>,
}

impl<P: SvcProcessor> RedirectedChannel<P> {
    /// `name` is the dynamic channel's name, which is the static channel's own.
    pub fn new(name: &'static str, processor: P) -> Self {
        Self {
            name,
            processor,
            channel_id: None,
            outgoing: Vec::new(),
        }
    }

    /// The channel the server opened, once it has opened one.
    pub fn channel_id(&self) -> Option<u32> {
        self.channel_id
    }

    /// The processor itself, which is the one that has been through the server's handshake and
    /// so the only one that can build a PDU that makes sense to it.
    pub fn processor(&self) -> &P {
        &self.processor
    }

    /// Everything the processor has said since this was last asked, for the static channel.
    pub fn take_outgoing(&mut self) -> Vec<SvcMessage> {
        core::mem::take(&mut self.outgoing)
    }
}

impl<P: 'static> ironrdp_core::AsAny for RedirectedChannel<P> {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl<P: SvcProcessor + 'static> DvcProcessor for RedirectedChannel<P> {
    fn channel_name(&self) -> &str {
        self.name
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel = self.name, channel_id, "🔀 channel redirected to a dynamic channel");
        self.channel_id = Some(channel_id);
        // Whatever the processor would have said when its static channel opened still has to be
        // said, and still on the static channel.
        self.outgoing.extend(self.processor.start()?);
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // The transport a static channel processor is told to reply on. The reply does not go
        // back the way it came here, so the value is not consulted.
        let replies = self.processor.process(payload, TransportContext::Tcp)?;
        debug!(
            channel = self.name,
            bytes = payload.len(),
            replies = replies.len(),
            "🔀 PDU over the redirected channel"
        );
        self.outgoing.extend(replies);
        Ok(Vec::new())
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel = self.name, channel_id, "🔀 redirected channel closed");
        self.channel_id = None;
    }

    fn supports_udp_transport(&self) -> bool {
        // Nothing is ever sent on this dynamic channel, so there is no reason to ask for it to
        // be moved onto a tunnel.
        false
    }
}
