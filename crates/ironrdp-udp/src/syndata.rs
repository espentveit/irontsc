use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SynData {
    pub initial_sequence_number: u32,
    pub upstream_mtu: u16,
    pub downstream_mtu: u16,
}

impl SynData {
    pub const NAME: &'static str = "RDPUDP_SYNDATA_PAYLOAD";
    pub const SIZE: usize = 8;

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        use ironrdp_core::NotEnoughBytesError;

        let initial_sequence_number = cursor
            .try_read_u32_be()
            .map_err(|e: NotEnoughBytesError| UdpError::decode(Self::NAME, e))?;
        let upstream_mtu = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;
        let downstream_mtu = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        Ok(Self {
            initial_sequence_number,
            upstream_mtu,
            downstream_mtu,
        })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.initial_sequence_number.to_be_bytes());
        output.extend_from_slice(&self.upstream_mtu.to_be_bytes());
        output.extend_from_slice(&self.downstream_mtu.to_be_bytes());
    }

    pub fn min_padding_size(&self) -> usize {
        usize::from(self.upstream_mtu.min(self.downstream_mtu))
    }
}
