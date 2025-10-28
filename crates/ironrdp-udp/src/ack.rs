use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::syndataex::UdpProtocolVersion;

/// VECTOR_ELEMENT_STATE Enumeration (section 2.2.1.1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VectorElementState {
    /// Datagram was received
    DatagramReceived = 0,
    /// Reserved (not used)
    DatagramReserved1 = 1,
    /// Reserved (not used)
    DatagramReserved2 = 2,
    /// Datagram has not been received yet
    DatagramNotYetReceived = 3,
}

impl VectorElementState {
    pub fn from_bits(value: u8) -> UdpResult<Self> {
        match value {
            0 => Ok(Self::DatagramReceived),
            1 => Ok(Self::DatagramReserved1),
            2 => Ok(Self::DatagramReserved2),
            3 => Ok(Self::DatagramNotYetReceived),
            _ => Err(UdpError::invalid_field(
                "VECTOR_ELEMENT_STATE",
                "state",
                "invalid state value",
            )),
        }
    }

    pub fn to_bits(self) -> u8 {
        self as u8
    }
}

/// ACK Vector Element (section 2.2.2.7.1)
///
/// Uses run-length encoding to represent a sequence of datagram states.
/// Each element contains:
/// - state: 2 bits (VectorElementState)
/// - length: 6 bits (run length, 1-64 datagrams)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckVectorElement {
    pub state: VectorElementState,
    /// Run length (1-64 datagrams). Stored value is actual count - 1.
    pub length: u8,
}

impl AckVectorElement {
    pub const NAME: &'static str = "ACK_VECTOR_ELEMENT";

    /// Create a new ACK vector element.
    ///
    /// # Arguments
    /// * `state` - The state of the datagrams
    /// * `count` - The number of consecutive datagrams with this state (1-64)
    pub fn new(state: VectorElementState, count: u8) -> UdpResult<Self> {
        if count == 0 || count > 64 {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "length",
                "count must be between 1 and 64",
            ));
        }
        Ok(Self {
            state,
            length: count - 1, // Store as 0-63
        })
    }

    /// Get the actual count of datagrams represented by this element
    pub fn count(&self) -> u8 {
        self.length + 1
    }

    /// Decode from a single byte
    pub fn from_byte(byte: u8) -> UdpResult<Self> {
        let state_bits = (byte >> 6) & 0b11;
        let length = byte & 0b00111111;
        let state = VectorElementState::from_bits(state_bits)?;
        Ok(Self { state, length })
    }

    /// Encode to a single byte
    pub fn to_byte(self) -> u8 {
        (self.state.to_bits() << 6) | (self.length & 0b00111111)
    }
}

/// RDPUDP_ACK_VECTOR_HEADER Structure (section 2.2.2.7)
#[derive(Debug, Clone)]
pub struct AckVectorHeader {
    /// Base sequence number being acknowledged
    pub base_sequence_number: u32,
    /// Timestamp when the ACK was created (in milliseconds)
    pub ack_timestamp: Option<u32>,
    /// Optional gap between last received packet and ACK emission (RDPUDP2)
    pub send_ack_time_gap_ms: Option<u8>,
    /// Vector of acknowledgment elements (RLE encoded)
    pub ack_vectors: Vec<AckVectorElement>,
}

impl AckVectorHeader {
    pub const NAME: &'static str = "RDPUDP_ACK_VECTOR_HEADER";

    pub fn new(
        base_sequence_number: u32,
        ack_timestamp: Option<u32>,
        ack_vectors: Vec<AckVectorElement>,
    ) -> UdpResult<Self> {
        if ack_vectors.is_empty() {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uNumVectors",
                "must have at least one ACK vector element",
            ));
        }
        if ack_vectors.len() > u16::MAX as usize {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uNumVectors",
                "too many ACK vector elements",
            ));
        }
        Ok(Self {
            base_sequence_number,
            ack_timestamp,
            send_ack_time_gap_ms: None,
            ack_vectors,
        })
    }

    pub fn decode(cursor: &mut ReadCursor<'_>, version: UdpProtocolVersion) -> UdpResult<Self> {
        match version {
            // V1 and V2 use the same ACK vector format per MS-RDPEUDP Section 1.3.2.2
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => Self::decode_v1(cursor),
            // V3+ uses the coded ACK vector format per MS-RDPEUDP2
            UdpProtocolVersion::V3 => Self::decode_v2(cursor),
            _ => Err(UdpError::invalid_state(
                Self::NAME,
                "unsupported protocol version for ACK vector",
            )),
        }
    }

    pub fn encode_into(&self, output: &mut Vec<u8>, version: UdpProtocolVersion) -> UdpResult<()> {
        match version {
            // V1 and V2 use the same ACK vector format per MS-RDPEUDP Section 1.3.2.2
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => {
                Self::encode_v1(output, &self.ack_vectors)?;
                Ok(())
            }
            // V3+ uses the coded ACK vector format per MS-RDPEUDP2
            UdpProtocolVersion::V3 => Self::encode_v2(
                output,
                self.base_sequence_number,
                self.ack_timestamp,
                self.send_ack_time_gap_ms,
                &self.ack_vectors,
            ),
            _ => Err(UdpError::invalid_state(
                Self::NAME,
                "unsupported protocol version for ACK vector",
            )),
        }
    }
}

/// RDPUDP_ACK_OF_ACKVECTOR_HEADER Structure (section 2.2.2.6)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckOfAckVectorHeader {
    /// Sequence number of the source packet being acknowledged
    pub sequence_number: u32,
}

impl AckOfAckVectorHeader {
    pub const NAME: &'static str = "RDPUDP_ACK_OF_ACKVECTOR_HEADER";
    pub const SIZE_V1: usize = 4;

    pub fn new(sequence_number: u32) -> Self {
        Self { sequence_number }
    }

    pub fn decode(cursor: &mut ReadCursor<'_>, version: UdpProtocolVersion) -> UdpResult<Self> {
        match version {
            UdpProtocolVersion::V1
            | UdpProtocolVersion::V2
            | UdpProtocolVersion::V3 => {
                let sequence_number = cursor
                    .try_read_u32_be()
                    .map_err(|e| UdpError::decode(Self::NAME, e))?;
                Ok(Self { sequence_number })
            }
            _ => Err(UdpError::invalid_state(
                Self::NAME,
                "unsupported protocol version for ACK-of-ACK",
            )),
        }
    }

    pub fn encode_into(&self, output: &mut Vec<u8>, version: UdpProtocolVersion) -> UdpResult<()> {
        match version {
            UdpProtocolVersion::V1
            | UdpProtocolVersion::V2
            | UdpProtocolVersion::V3 => {
                output.extend_from_slice(&self.sequence_number.to_be_bytes());
                Ok(())
            }
            _ => Err(UdpError::invalid_state(
                Self::NAME,
                "unsupported protocol version for ACK-of-ACK",
            )),
        }
    }
}

impl AckVectorHeader {
    fn decode_v1(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        use ironrdp_core::NotEnoughBytesError;

        let ack_vector_size = cursor
            .try_read_u16_be()
            .map_err(|e: NotEnoughBytesError| UdpError::decode(Self::NAME, e))?;

        let mut ack_vectors = Vec::with_capacity(ack_vector_size as usize);
        for _ in 0..ack_vector_size {
            let byte = cursor
                .try_read_u8()
                .map_err(|e| UdpError::decode(Self::NAME, e))?;
            ack_vectors.push(AckVectorElement::from_byte(byte)?);
        }

        // V1 structures are padded to a DWORD boundary
        let consumed = 2 + ack_vector_size as usize;
        let padding = (4 - (consumed % 4)) % 4;
        for _ in 0..padding {
            // Ignore padding bytes if present
            if cursor.len() > 0 {
                cursor
                    .try_read_u8()
                    .map_err(|e| UdpError::decode(Self::NAME, e))?;
            }
        }

        Ok(Self {
            base_sequence_number: 0,
            ack_timestamp: None,
            send_ack_time_gap_ms: None,
            ack_vectors,
        })
    }

    fn decode_v2(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        use ironrdp_core::NotEnoughBytesError;

        let base_sequence_low = cursor
            .try_read_u16_be()
            .map_err(|e: NotEnoughBytesError| UdpError::decode(Self::NAME, e))?;

        let size_and_flag = cursor
            .try_read_u8()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let timestamp_present = (size_and_flag & 0x80) != 0;
        let coded_size = (size_and_flag & 0x7F) as usize;

        let ack_timestamp = if timestamp_present {
            if cursor.len() < 3 {
                return Err(UdpError::invalid_field(
                    Self::NAME,
                    "timestamp",
                    "not enough bytes",
                ));
            }
            let mut ts_bytes = [0u8; 4];
            let ts_slice = cursor.read_slice(3);
            ts_bytes[1..].copy_from_slice(ts_slice);
            Some(u32::from_be_bytes(ts_bytes))
        } else {
            None
        };

        let send_ack_time_gap_ms = if timestamp_present {
            Some(
                cursor
                    .try_read_u8()
                    .map_err(|e| UdpError::decode(Self::NAME, e))?,
            )
        } else {
            None
        };

        if cursor.len() < coded_size {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "AckVector",
                "not enough bytes",
            ));
        }

        let coded_bytes = cursor.read_slice(coded_size);

        let ack_vectors = decode_coded_ack_vector(coded_bytes)?;

        Ok(Self {
            base_sequence_number: base_sequence_low as u32,
            ack_timestamp,
            send_ack_time_gap_ms,
            ack_vectors,
        })
    }

    fn encode_v1(output: &mut Vec<u8>, elements: &[AckVectorElement]) -> UdpResult<()> {
        let size = elements.len();
        if size > u16::MAX as usize {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uAckVectorSize",
                "too many ACK vector elements",
            ));
        }

        output.extend_from_slice(&(size as u16).to_be_bytes());
        for element in elements {
            output.push(element.to_byte());
        }

        let consumed = 2 + size;
        let padding = (4 - (consumed % 4)) % 4;
        output.extend(std::iter::repeat(0u8).take(padding));

        Ok(())
    }

    fn encode_v2(
        output: &mut Vec<u8>,
        base_sequence_number: u32,
        ack_timestamp: Option<u32>,
        send_ack_time_gap_ms: Option<u8>,
        elements: &[AckVectorElement],
    ) -> UdpResult<()> {
        let coded_vector = encode_coded_ack_vector(elements)?;
        if coded_vector.len() > 0x7F {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "codedAckVecSize",
                "coded ACK vector too large",
            ));
        }

        output.extend_from_slice(&((base_sequence_number & 0xFFFF) as u16).to_be_bytes());

        if coded_vector.len() > u8::MAX as usize {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "codedAckVecSize",
                "coded ACK vector too large",
            ));
        }

        let mut size_and_flag = (coded_vector.len() as u8) & 0x7F;
        if ack_timestamp.is_some() {
            size_and_flag |= 0x80;
        }
        output.push(size_and_flag);

        if let Some(ts) = ack_timestamp {
            let ts_bytes = ts.to_be_bytes();
            output.extend_from_slice(&ts_bytes[1..]);
            output.push(send_ack_time_gap_ms.unwrap_or(0));
        }

        output.extend_from_slice(&coded_vector);
        Ok(())
    }
}

fn decode_coded_ack_vector(bytes: &[u8]) -> UdpResult<Vec<AckVectorElement>> {
    let mut elements = Vec::new();

    for &byte in bytes {
        if (byte & 0x80) == 0 {
            // State map mode (7 bits)
            let mut run_state = None;
            let mut run_length = 0u8;

            for bit in (0..7).rev() {
                let received = ((byte >> bit) & 0x01) != 0;
                let state = if received {
                    VectorElementState::DatagramReceived
                } else {
                    VectorElementState::DatagramNotYetReceived
                };

                if run_state == Some(state) && run_length < 64 {
                    run_length += 1;
                } else {
                    if let Some(prev_state) = run_state.take() {
                        elements.push(AckVectorElement::new(prev_state, run_length)?);
                    }
                    run_state = Some(state);
                    run_length = 1;
                }
            }

            if let Some(prev_state) = run_state {
                elements.push(AckVectorElement::new(prev_state, run_length)?);
            }
        } else {
            // Run-length mode
            let state = if (byte & 0x40) != 0 {
                VectorElementState::DatagramReceived
            } else {
                VectorElementState::DatagramNotYetReceived
            };

            let length = (byte & 0x3F) + 1;
            elements.push(AckVectorElement::new(state, length)?);
        }
    }

    Ok(elements)
}

fn encode_coded_ack_vector(elements: &[AckVectorElement]) -> UdpResult<Vec<u8>> {
    let mut bytes = Vec::new();

    for element in elements {
        let mut remaining = element.count();
        let state_bit = match element.state {
            VectorElementState::DatagramReceived => 0x40,
            VectorElementState::DatagramNotYetReceived => 0x00,
            _ => {
                return Err(UdpError::invalid_state(
                    AckVectorHeader::NAME,
                    "unsupported ACK vector state for RDPUDP2",
                ))
            }
        };

        while remaining > 0 {
            let chunk = remaining.min(64);
            let byte = 0x80 | state_bit | ((chunk - 1) & 0x3F);
            bytes.push(byte);
            remaining -= chunk;
        }
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_element_encoding() {
        // Test DatagramReceived with count of 1
        let elem = AckVectorElement::new(VectorElementState::DatagramReceived, 1).unwrap();
        assert_eq!(elem.to_byte(), 0b00000000);
        assert_eq!(elem.count(), 1);

        // Test DatagramNotYetReceived with count of 64
        let elem = AckVectorElement::new(VectorElementState::DatagramNotYetReceived, 64).unwrap();
        assert_eq!(elem.to_byte(), 0b11111111);
        assert_eq!(elem.count(), 64);

        // Test DatagramReceived with count of 32
        let elem = AckVectorElement::new(VectorElementState::DatagramReceived, 32).unwrap();
        assert_eq!(elem.to_byte(), 0b00011111); // state=00, length=31
        assert_eq!(elem.count(), 32);
    }

    #[test]
    fn test_vector_element_decoding() {
        let elem = AckVectorElement::from_byte(0b11111111).unwrap();
        assert_eq!(elem.state, VectorElementState::DatagramNotYetReceived);
        assert_eq!(elem.count(), 64);

        let elem = AckVectorElement::from_byte(0b00000000).unwrap();
        assert_eq!(elem.state, VectorElementState::DatagramReceived);
        assert_eq!(elem.count(), 1);
    }

    #[test]
    fn test_ack_vector_header_encoding_decoding_v1() {
        let vectors = vec![
            AckVectorElement::new(VectorElementState::DatagramReceived, 5).unwrap(),
            AckVectorElement::new(VectorElementState::DatagramNotYetReceived, 2).unwrap(),
            AckVectorElement::new(VectorElementState::DatagramReceived, 10).unwrap(),
        ];

        let header = AckVectorHeader::new(0, None, vectors.clone()).unwrap();

        let mut encoded = Vec::new();
        header
            .encode_into(&mut encoded, UdpProtocolVersion::V1)
            .unwrap();

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = AckVectorHeader::decode(&mut cursor, UdpProtocolVersion::V1).unwrap();

        assert_eq!(decoded.base_sequence_number, 0);
        assert_eq!(decoded.ack_timestamp, None);
        assert_eq!(decoded.ack_vectors.len(), 3);
        assert_eq!(
            decoded.ack_vectors[0].state,
            VectorElementState::DatagramReceived
        );
        assert_eq!(decoded.ack_vectors[0].count(), 5);
        assert_eq!(
            decoded.ack_vectors[1].state,
            VectorElementState::DatagramNotYetReceived
        );
        assert_eq!(decoded.ack_vectors[1].count(), 2);
    }

    #[test]
    fn test_ack_vector_header_encoding_decoding_v2() {
        let vectors = vec![
            AckVectorElement::new(VectorElementState::DatagramReceived, 3).unwrap(),
            AckVectorElement::new(VectorElementState::DatagramNotYetReceived, 1).unwrap(),
        ];

        let header = AckVectorHeader::new(1234, Some(0x00FF_FFEE), vectors.clone()).unwrap();

        let mut encoded = Vec::new();
        header
            .encode_into(&mut encoded, UdpProtocolVersion::V2)
            .unwrap();

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = AckVectorHeader::decode(&mut cursor, UdpProtocolVersion::V2).unwrap();

        // V2/V1 format does not transmit the base sequence number explicitly.
        assert_eq!(decoded.base_sequence_number, 0);
        assert_eq!(decoded.ack_timestamp, None);
        assert_eq!(decoded.ack_vectors.len(), vectors.len());
    }

    #[test]
    fn test_ack_of_ack_vector_v1() {
        let header = AckOfAckVectorHeader::new(12345);

        let mut encoded = Vec::new();
        header
            .encode_into(&mut encoded, UdpProtocolVersion::V1)
            .unwrap();

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = AckOfAckVectorHeader::decode(&mut cursor, UdpProtocolVersion::V1).unwrap();

        assert_eq!(decoded.sequence_number, 12345);
    }

    #[test]
    fn test_ack_of_ack_vector_v2() {
        let header = AckOfAckVectorHeader::new(0xABCD_1234);

        let mut encoded = Vec::new();
        header
            .encode_into(&mut encoded, UdpProtocolVersion::V2)
            .unwrap();

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = AckOfAckVectorHeader::decode(&mut cursor, UdpProtocolVersion::V2).unwrap();

        assert_eq!(decoded.sequence_number, 0xABCD_1234);
    }
}
