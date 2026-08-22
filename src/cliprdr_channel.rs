//! The clipboard, when the server hands it over as a dynamic channel.
//!
//! MS-RDPECLIP describes `cliprdr` as a *static* virtual channel, and that is how it is joined:
//! the channel is requested in the GCC conference create, the server grants it an id, and both
//! ends talk on that id. A server that has agreed Soft-Sync ([MS-RDPEDYC] 3.1.5.3) does
//! something else, and something asymmetric. Having established that the client can carry
//! dynamic channels over the multitransport tunnels, it stops writing on the static id and
//! opens a *dynamic* channel called `cliprdr` to write on instead -- while going on reading the
//! static one, and never reading the dynamic one it opened.
//!
//! That is why the clipboard worked in xfreerdp and not here: xfreerdp implements no UDP, so it
//! never advertises Soft-Sync, so the server leaves the channel where the specification puts it
//! and the one processor sees both directions. Here the two directions are split, and so are the
//! two halves of the protocol: this bridge owns the [`Cliprdr`] the server is talking to -- the
//! one that receives its capabilities and its Monitor Ready and so is the one that reaches the
//! ready state -- and [`crate::rdp`] sends what that clipboard produces back out on the static
//! channel the server is still reading.
//!
//! The data itself arrives as `DYNVC_DATA_COMPRESSED` rather than `DYNVC_DATA`, because a
//! version 3 dynamic channel manager may compress; see [`crate::dvc_compression`].

use ironrdp::cliprdr::backend::CliprdrBackend;
use ironrdp::cliprdr::{Client, Cliprdr};
use ironrdp_core::{Encode, EncodeResult, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcMessage, DvcProcessor};
use ironrdp::svc::{SvcMessage, SvcProcessor, TransportContext};
use ironrdp_pdu::PduResult;
use tracing::{debug, info, warn};

/// The name the server opens the dynamic channel under, which is the static channel's own name.
const CLIPBOARD_CHANNEL_NAME: &str = "cliprdr";

/// A clipboard PDU with nothing wrapped around it, ready for a dynamic channel.
struct RawPdu(Vec<u8>);

impl DvcEncode for RawPdu {}

impl Encode for RawPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "ClipboardDvcPdu"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

/// Carries `cliprdr` on a dynamic channel, for a server that put it there.
pub struct CliprdrDvcProcessor {
    cliprdr: Cliprdr<Client>,
    channel_id: Option<u32>,
}

impl CliprdrDvcProcessor {
    pub fn new(backend: Box<dyn CliprdrBackend>) -> Self {
        Self {
            cliprdr: Cliprdr::new(backend),
            channel_id: None,
        }
    }

    /// The channel the server opened for the clipboard, once it has opened one.
    pub fn channel_id(&self) -> Option<u32> {
        self.channel_id
    }

    /// The clipboard itself, for the side of the conversation this machine starts.
    ///
    /// It is the same [`Cliprdr`] a static channel would own, and it is the one that has been
    /// through the initialisation handshake, so copies and pastes must go through *this* one
    /// rather than through any processor attached to the static channel.
    pub fn clipboard(&self) -> &Cliprdr<Client> {
        &self.cliprdr
    }

    /// Wraps clipboard PDUs as the dynamic channel data they now travel as.
    pub fn wrap(messages: Vec<SvcMessage>) -> PduResult<Vec<DvcMessage>> {
        messages
            .into_iter()
            .map(|message| {
                message
                    .encode_to_vec()
                    .map(|bytes| Box::new(RawPdu(bytes)) as DvcMessage)
                    .map_err(|error| {
                        warn!(%error, "could not re-encode a clipboard PDU for the dynamic channel");
                        ironrdp_pdu::encode_err!(error)
                    })
            })
            .collect()
    }
}

impl ironrdp_core::AsAny for CliprdrDvcProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for CliprdrDvcProcessor {
    fn channel_name(&self) -> &str {
        CLIPBOARD_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel_id, "📋 clipboard arrived as a dynamic channel");
        self.channel_id = Some(channel_id);
        // The server speaks first here as it does on the static channel: capabilities, then
        // Monitor Ready. Nothing to send until it does.
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // The transport a static channel processor is told to reply on. On a dynamic channel
        // the reply goes back the way the request came, and the dynamic channel layer decides
        // which tunnel that is, so the value here is not consulted.
        let responses = self.cliprdr.process(payload, TransportContext::Tcp)?;
        debug!(
            bytes = payload.len(),
            replies = responses.len(),
            "📋 clipboard PDU over the dynamic channel"
        );

        Self::wrap(responses)
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel_id, "📋 clipboard dynamic channel closed");
        self.channel_id = None;
    }
}
