use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};

/// RDPUDP_PAYLOAD_PREFIX Structure (section 2.2.2.3)
///
/// This structure is present in all coded packets (source and FEC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadPrefix {
    /// Length of the payload data following this prefix
    pub payload_length: u16,
}

impl PayloadPrefix {
    pub const NAME: &'static str = "RDPUDP_PAYLOAD_PREFIX";
    pub const SIZE: usize = 2;

    pub fn new(payload_length: u16) -> Self {
        Self { payload_length }
    }

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        let payload_length = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        Ok(Self { payload_length })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.payload_length.to_be_bytes());
    }
}

/// RDPUDP_SOURCE_PAYLOAD_HEADER Structure (section 2.2.2.4)
///
/// This header is present in source packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePayloadHeader {
    /// Coded sequence number for this packet (snCoded)
    pub sn_coded: u32,
    /// Source sequence number for the payload (snSourceStart)
    pub sn_source_start: u32,
}

impl SourcePayloadHeader {
    pub const NAME: &'static str = "RDPUDP_SOURCE_PAYLOAD_HEADER";
    pub const SIZE: usize = 8;

    pub fn new(sn_coded: u32, sn_source_start: u32) -> Self {
        Self {
            sn_coded,
            sn_source_start,
        }
    }

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        let sn_coded = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;
        let sn_source_start = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        Ok(Self {
            sn_coded,
            sn_source_start,
        })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.sn_coded.to_be_bytes());
        output.extend_from_slice(&self.sn_source_start.to_be_bytes());
    }
}

/// RDPUDP_FEC_PAYLOAD_HEADER Structure (section 2.2.2.2)
///
/// This header is present in FEC packets and contains information about
/// the source packets that were used to generate the FEC packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecPayloadHeader {
    /// Sequence number assigned to this FEC packet (snCoded)
    pub sn_coded: u32,
    /// First source sequence number protected by this FEC packet (snSourceStart)
    pub sn_source_start: u32,
    /// Difference between last and first source sequence number in the block (uRange)
    pub urange: u8,
    /// Index of this FEC packet within the block (uFecIndex)
    pub fec_index: u8,
    /// Reserved padding (MUST be zero)
    pub u_padding: u16,
}

impl FecPayloadHeader {
    pub const NAME: &'static str = "RDPUDP_FEC_PAYLOAD_HEADER";
    pub const SIZE: usize = 12;

    pub fn new(sn_coded: u32, sn_source_start: u32, urange: u8, fec_index: u8) -> Self {
        Self {
            sn_coded,
            sn_source_start,
            urange,
            fec_index,
            u_padding: 0,
        }
    }

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        let sn_coded = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let sn_source_start = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let urange = cursor
            .try_read_u8()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let fec_index = cursor
            .try_read_u8()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let u_padding = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        if u_padding != 0 {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uPadding",
                "padding must be 0",
            ));
        }

        Ok(Self {
            sn_coded,
            sn_source_start,
            urange,
            fec_index,
            u_padding,
        })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.sn_coded.to_be_bytes());
        output.extend_from_slice(&self.sn_source_start.to_be_bytes());
        output.push(self.urange);
        output.push(self.fec_index);
        output.extend_from_slice(&0u16.to_be_bytes());
    }

    /// Get the inclusive sequence number range covered by this FEC packet
    pub fn source_range(&self) -> std::ops::RangeInclusive<u32> {
        let start = self.sn_source_start;
        let end = start.wrapping_add(self.urange as u32);
        start..=end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payload_prefix() {
        let prefix = PayloadPrefix::new(1234);

        let mut encoded = Vec::new();
        prefix.encode_into(&mut encoded);

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = PayloadPrefix::decode(&mut cursor).unwrap();

        assert_eq!(decoded.payload_length, 1234);
    }

    #[test]
    fn test_source_payload_header() {
        let header = SourcePayloadHeader::new(0x11112222, 0x33334444);

        let mut encoded = Vec::new();
        header.encode_into(&mut encoded);

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = SourcePayloadHeader::decode(&mut cursor).unwrap();

        assert_eq!(decoded.sn_coded, 0x11112222);
        assert_eq!(decoded.sn_source_start, 0x33334444);
    }

    #[test]
    fn test_fec_payload_header() {
        let header = FecPayloadHeader::new(200, 100, 9, 2);

        let mut encoded = Vec::new();
        header.encode_into(&mut encoded);

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = FecPayloadHeader::decode(&mut cursor).unwrap();

        assert_eq!(decoded.sn_coded, 200);
        assert_eq!(decoded.sn_source_start, 100);
        assert_eq!(decoded.urange, 9);
        assert_eq!(decoded.fec_index, 2);

        // Test source range
        let range = decoded.source_range();
        assert_eq!(range, 100..=109);
    }

    #[test]
    fn test_fec_payload_header_rejects_invalid_padding() {
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(&100u32.to_be_bytes());
        data[4..8].copy_from_slice(&5u32.to_be_bytes());
        data[8] = 10; // urange
        data[9] = 2; // fec_index
        data[10..12].copy_from_slice(&1u16.to_be_bytes()); // invalid padding (should be 0)

        let mut cursor = ReadCursor::new(&data);
        let result = FecPayloadHeader::decode(&mut cursor);
        assert!(result.is_err());
    }
}
