use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::flags::DatagramFlags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecHeader {
    pub sn_source_ack: u32,
    pub receive_window_size: u16,
    pub flags: DatagramFlags,
}

impl FecHeader {
    pub const NAME: &'static str = "RDPUDP_FEC_HEADER";
    pub const SIZE: usize = 8;

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        use ironrdp_core::NotEnoughBytesError;

        let sn_source_ack = cursor
            .try_read_u32_be()
            .map_err(|e: NotEnoughBytesError| UdpError::decode(Self::NAME, e))?;
        let receive_window_size = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;
        let flags_raw = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let flags = DatagramFlags::from_bits(flags_raw).ok_or_else(|| {
            UdpError::invalid_field(Self::NAME, "uFlags", "unknown bits set in datagram flags")
        })?;

        Ok(Self {
            sn_source_ack,
            receive_window_size,
            flags,
        })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.sn_source_ack.to_be_bytes());
        output.extend_from_slice(&self.receive_window_size.to_be_bytes());
        
        // Per MS-RDPEUDP 2.2.1: flags field is 16 bits with LogWindow in upper 4 bits
        // Bits 0-12: Datagram flags
        // Bits 12-15: LogWindow (receive window size indicator)
        let log_window: u16 = 15; // Maximum window size (0xF = 15)
        let flags_with_logwindow = self.flags.bits() | (log_window << 12);
        output.extend_from_slice(&flags_with_logwindow.to_be_bytes());
    }
}
