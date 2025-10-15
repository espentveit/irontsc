use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrelationId {
    pub value: [u8; 16],
}

impl CorrelationId {
    pub const NAME: &'static str = "RDPUDP_CORRELATION_ID_PAYLOAD";
    pub const SIZE: usize = 24;  // 16 bytes value + 8 bytes reserved

    pub const fn new(value: [u8; 16]) -> Self {
        Self { value }
    }

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        if cursor.len() < Self::SIZE {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "payload",
                "not enough bytes to decode correlation id",
            ));
        }

        let value = cursor.read_array::<16>();
        let reserved = cursor.read_array::<8>();  // Only 8 bytes reserved, not 16

        if reserved.iter().any(|&byte| byte != 0) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uReserved",
                "expected all zeros",
            ));
        }

        Ok(Self { value })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.value);
        output.extend_from_slice(&[0; 8]);  // Only 8 bytes reserved, not 16
    }
}
