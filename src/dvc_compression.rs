//! Undoing the bulk compression a server applies to dynamic channel data.
//!
//! A DVC manager that has agreed version 3 of [MS-RDPEDYC] may send any channel's data as
//! `DYNVC_DATA_COMPRESSED` instead of `DYNVC_DATA`, and a Windows server does exactly that for
//! the redirection channels it moves onto dynamic channels -- the clipboard among them. The
//! payload is an `RDP_SEGMENTED_DATA` structure, the same one the graphics pipeline carries, so
//! the decoder is the one already written for it; only the history is smaller, because a
//! dynamic channel's compressor never reaches back more than 8,192 bytes.

use ironrdp::dvc::{DvcCompression, DvcDecompressor};
use ironrdp_graphics::zgfx::{Decompressor, DVC_HISTORY_SIZE};
use ironrdp_pdu::{custom_err, PduResult};

/// Says that this client can read compressed channel data, which is what asks for version 3.
pub struct Zgfx;

impl DvcCompression for Zgfx {
    fn new_context(&self) -> Box<dyn DvcDecompressor> {
        Box::new(ChannelHistory(Decompressor::with_history_size(
            DVC_HISTORY_SIZE,
        )))
    }
}

/// One channel's compression history, which every block on that channel refers back into.
struct ChannelHistory(Decompressor);

impl DvcDecompressor for ChannelHistory {
    fn decompress(&mut self, input: &[u8], output: &mut Vec<u8>) -> PduResult<()> {
        self.0
            .decompress(input, output)
            .map_err(|error| custom_err!("DVC bulk decompression", error))?;
        Ok(())
    }
}
