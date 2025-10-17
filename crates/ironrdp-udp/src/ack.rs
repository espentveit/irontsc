use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};

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
    pub ack_timestamp: u32,
    /// Vector of acknowledgment elements (RLE encoded)
    pub ack_vectors: Vec<AckVectorElement>,
}

impl AckVectorHeader {
    pub const NAME: &'static str = "RDPUDP_ACK_VECTOR_HEADER";
    pub const FIXED_SIZE: usize = 12; // uBaseSeqNum (4) + uAckTimestamp (4) + uNumVectors (2) + uPadding (2)

    pub fn new(
        base_sequence_number: u32,
        ack_timestamp: u32,
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
            ack_vectors,
        })
    }

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        let base_sequence_number = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let ack_timestamp = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let num_vectors = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        // Skip padding (2 bytes)
        cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let mut ack_vectors = Vec::with_capacity(num_vectors as usize);
        for _ in 0..num_vectors {
            let byte = cursor
                .try_read_u8()
                .map_err(|e| UdpError::decode(Self::NAME, e))?;
            let element = AckVectorElement::from_byte(byte)?;
            ack_vectors.push(element);
        }

        Ok(Self {
            base_sequence_number,
            ack_timestamp,
            ack_vectors,
        })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.base_sequence_number.to_be_bytes());
        output.extend_from_slice(&self.ack_timestamp.to_be_bytes());
        output.extend_from_slice(&(self.ack_vectors.len() as u16).to_be_bytes());
        output.extend_from_slice(&[0u8, 0u8]); // padding

        for element in &self.ack_vectors {
            output.push(element.to_byte());
        }
    }

    pub fn size(&self) -> usize {
        Self::FIXED_SIZE + self.ack_vectors.len()
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
    pub const SIZE: usize = 4;

    pub fn new(sequence_number: u32) -> Self {
        Self { sequence_number }
    }

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        let sequence_number = cursor
            .try_read_u32_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        Ok(Self { sequence_number })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.sequence_number.to_be_bytes());
    }
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
    fn test_ack_vector_header_encoding_decoding() {
        let vectors = vec![
            AckVectorElement::new(VectorElementState::DatagramReceived, 5).unwrap(),
            AckVectorElement::new(VectorElementState::DatagramNotYetReceived, 2).unwrap(),
            AckVectorElement::new(VectorElementState::DatagramReceived, 10).unwrap(),
        ];

        let header = AckVectorHeader::new(42, 1000, vectors.clone()).unwrap();

        let mut encoded = Vec::new();
        header.encode_into(&mut encoded);

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = AckVectorHeader::decode(&mut cursor).unwrap();

        assert_eq!(decoded.base_sequence_number, 42);
        assert_eq!(decoded.ack_timestamp, 1000);
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
    fn test_ack_of_ack_vector() {
        let header = AckOfAckVectorHeader::new(12345);

        let mut encoded = Vec::new();
        header.encode_into(&mut encoded);

        let mut cursor = ReadCursor::new(&encoded);
        let decoded = AckOfAckVectorHeader::decode(&mut cursor).unwrap();

        assert_eq!(decoded.sequence_number, 12345);
    }
}
