use ironrdp_core::ReadCursor;

use crate::ack::{AckOfAckVectorHeader, AckVectorHeader};
use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::flags::DatagramFlags;
use crate::header::FecHeader;
use crate::payload::{FecPayloadHeader, PayloadPrefix, SourcePayloadHeader};
use crate::syndataex::UdpProtocolVersion;

/// ACK packet sent during the data transfer phase
#[derive(Debug, Clone)]
pub struct AckPacket {
    pub header: FecHeader,
    pub ack_vector: Option<AckVectorHeader>,
    pub ack_of_ack: Option<AckOfAckVectorHeader>,
}

impl AckPacket {
    pub const NAME: &'static str = "RDPUDP_ACK_PACKET";

    pub fn new(
        sn_source_ack: u32,
        receive_window_size: u16,
        ack_vector: Option<AckVectorHeader>,
        ack_of_ack: Option<AckOfAckVectorHeader>,
    ) -> Self {
        let mut flags = DatagramFlags::ACK;
        if ack_vector.is_some() {
            flags |= DatagramFlags::ACK_VECTOR;
        }
        if ack_of_ack.is_some() {
            flags |= DatagramFlags::ACK_OF_ACKS;
        }

        let header = FecHeader {
            sn_source_ack,
            receive_window_size,
            flags,
        };

        Self {
            header,
            ack_vector,
            ack_of_ack,
        }
    }

    pub fn decode(bytes: &[u8], version: UdpProtocolVersion) -> UdpResult<Self> {
        let mut cursor = ReadCursor::new(bytes);
        let header = FecHeader::decode(&mut cursor)?;

        if !header.flags.contains(DatagramFlags::ACK) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "ACK flag not set",
            ));
        }

        let ack_vector = if header.flags.contains(DatagramFlags::ACK_VECTOR) {
            Some(AckVectorHeader::decode(&mut cursor, version)?)
        } else {
            None
        };

        let ack_of_ack = if header.flags.contains(DatagramFlags::ACK_OF_ACKS) {
            Some(AckOfAckVectorHeader::decode(&mut cursor, version)?)
        } else {
            None
        };

        Ok(Self {
            header,
            ack_vector,
            ack_of_ack,
        })
    }

    pub fn encode(&self, version: UdpProtocolVersion) -> UdpResult<Vec<u8>> {
        let mut buffer = Vec::new();
        self.header.encode_into(&mut buffer);

        if let Some(ref ack_vector) = self.ack_vector {
            ack_vector.encode_into(&mut buffer, version)?;
        }

        if let Some(ref ack_of_ack) = self.ack_of_ack {
            ack_of_ack.encode_into(&mut buffer, version)?;
        }

        Ok(buffer)
    }
}

/// Source packet containing user data
#[derive(Debug, Clone)]
pub struct SourcePacket {
    pub header: FecHeader,
    pub ack_vector: Option<AckVectorHeader>,
    pub ack_of_ack: Option<AckOfAckVectorHeader>,
    pub payload_prefix: PayloadPrefix,
    pub source_header: SourcePayloadHeader,
    pub data: Vec<u8>,
}

impl SourcePacket {
    pub const NAME: &'static str = "RDPUDP_SOURCE_PACKET";

    pub fn new(
        sequence_number: u32,
        sn_source_ack: u32,
        receive_window_size: u16,
        data: Vec<u8>,
        ack_vector: Option<AckVectorHeader>,
        ack_of_ack: Option<AckOfAckVectorHeader>,
        include_ack: bool,
    ) -> UdpResult<Self> {
        let mut flags = DatagramFlags::DATA;
        if include_ack {
            flags |= DatagramFlags::ACK;
        }
        if ack_vector.is_some() {
            flags |= DatagramFlags::ACK_VECTOR;
        }
        if ack_of_ack.is_some() {
            flags |= DatagramFlags::ACK_OF_ACKS;
        }

        let header = FecHeader {
            sn_source_ack,
            receive_window_size,
            flags,
        };

        // Payload length includes the source header (4 bytes) + data
        let payload_length = (SourcePayloadHeader::SIZE + data.len()) as u16;
        let payload_prefix = PayloadPrefix::new(payload_length);
        let source_header = SourcePayloadHeader::new(sequence_number);

        Ok(Self {
            header,
            ack_vector,
            ack_of_ack,
            payload_prefix,
            source_header,
            data,
        })
    }

    pub fn decode(bytes: &[u8], version: UdpProtocolVersion) -> UdpResult<Self> {
        let mut cursor = ReadCursor::new(bytes);
        let header = FecHeader::decode(&mut cursor)?;

        if !header.flags.contains(DatagramFlags::DATA) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "DATA flag not set",
            ));
        }

        let ack_vector = if header.flags.contains(DatagramFlags::ACK_VECTOR) {
            Some(AckVectorHeader::decode(&mut cursor, version)?)
        } else {
            None
        };

        let ack_of_ack = if header.flags.contains(DatagramFlags::ACK_OF_ACKS) {
            Some(AckOfAckVectorHeader::decode(&mut cursor, version)?)
        } else {
            None
        };

        let payload_prefix = PayloadPrefix::decode(&mut cursor)?;
        let source_header = SourcePayloadHeader::decode(&mut cursor)?;

        // Remaining data is the payload
        let data_length = payload_prefix
            .payload_length
            .saturating_sub(SourcePayloadHeader::SIZE as u16) as usize;

        let data = cursor.read_slice(data_length).to_vec();

        Ok(Self {
            header,
            ack_vector,
            ack_of_ack,
            payload_prefix,
            source_header,
            data,
        })
    }

    pub fn encode(&self, version: UdpProtocolVersion) -> UdpResult<Vec<u8>> {
        let mut buffer = Vec::new();
        self.header.encode_into(&mut buffer);

        if let Some(ref ack_vector) = self.ack_vector {
            ack_vector.encode_into(&mut buffer, version)?;
        }

        if let Some(ref ack_of_ack) = self.ack_of_ack {
            ack_of_ack.encode_into(&mut buffer, version)?;
        }

        self.payload_prefix.encode_into(&mut buffer);
        self.source_header.encode_into(&mut buffer);
        buffer.extend_from_slice(&self.data);

        Ok(buffer)
    }

    pub fn sequence_number(&self) -> u32 {
        self.source_header.sequence_number
    }
}

/// FEC packet containing redundancy data for error correction
#[derive(Debug, Clone)]
pub struct FecPacket {
    pub header: FecHeader,
    pub ack_vector: Option<AckVectorHeader>,
    pub ack_of_ack: Option<AckOfAckVectorHeader>,
    pub payload_prefix: PayloadPrefix,
    pub fec_header: FecPayloadHeader,
    pub fec_data: Vec<u8>,
}

impl FecPacket {
    pub const NAME: &'static str = "RDPUDP_FEC_PACKET";

    pub fn new(
        sn_source_ack: u32,
        receive_window_size: u16,
        fec_header: FecPayloadHeader,
        fec_data: Vec<u8>,
        ack_vector: Option<AckVectorHeader>,
        ack_of_ack: Option<AckOfAckVectorHeader>,
    ) -> UdpResult<Self> {
        let mut flags = DatagramFlags::DATA | DatagramFlags::FEC;
        if ack_vector.is_some() {
            flags |= DatagramFlags::ACK_VECTOR;
        }
        if ack_of_ack.is_some() {
            flags |= DatagramFlags::ACK_OF_ACKS;
        }

        let header = FecHeader {
            sn_source_ack,
            receive_window_size,
            flags,
        };

        // Payload length includes the FEC header (8 bytes) + FEC data
        let payload_length = (FecPayloadHeader::SIZE + fec_data.len()) as u16;
        let payload_prefix = PayloadPrefix::new(payload_length);

        Ok(Self {
            header,
            ack_vector,
            ack_of_ack,
            payload_prefix,
            fec_header,
            fec_data,
        })
    }

    pub fn decode(bytes: &[u8], version: UdpProtocolVersion) -> UdpResult<Self> {
        let mut cursor = ReadCursor::new(bytes);
        let header = FecHeader::decode(&mut cursor)?;

        if !header.flags.contains(DatagramFlags::DATA) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "DATA flag not set",
            ));
        }

        if !header.flags.contains(DatagramFlags::FEC) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "FEC flag not set",
            ));
        }

        let ack_vector = if header.flags.contains(DatagramFlags::ACK_VECTOR) {
            Some(AckVectorHeader::decode(&mut cursor, version)?)
        } else {
            None
        };

        let ack_of_ack = if header.flags.contains(DatagramFlags::ACK_OF_ACKS) {
            Some(AckOfAckVectorHeader::decode(&mut cursor, version)?)
        } else {
            None
        };

        let payload_prefix = PayloadPrefix::decode(&mut cursor)?;
        let fec_header = FecPayloadHeader::decode(&mut cursor)?;

        // Remaining data is the FEC payload
        let fec_length = payload_prefix
            .payload_length
            .saturating_sub(FecPayloadHeader::SIZE as u16) as usize;

        let fec_data = cursor.read_slice(fec_length).to_vec();

        Ok(Self {
            header,
            ack_vector,
            ack_of_ack,
            payload_prefix,
            fec_header,
            fec_data,
        })
    }

    pub fn encode(&self, version: UdpProtocolVersion) -> UdpResult<Vec<u8>> {
        let mut buffer = Vec::new();
        self.header.encode_into(&mut buffer);

        if let Some(ref ack_vector) = self.ack_vector {
            ack_vector.encode_into(&mut buffer, version)?;
        }

        if let Some(ref ack_of_ack) = self.ack_of_ack {
            ack_of_ack.encode_into(&mut buffer, version)?;
        }

        self.payload_prefix.encode_into(&mut buffer);
        self.fec_header.encode_into(&mut buffer);
        buffer.extend_from_slice(&self.fec_data);

        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ack::{AckVectorElement, VectorElementState};

    #[test]
    fn test_ack_packet_encoding_decoding() {
        let ack_vectors = vec![
            AckVectorElement::new(VectorElementState::DatagramReceived, 5).unwrap(),
            AckVectorElement::new(VectorElementState::DatagramNotYetReceived, 2).unwrap(),
        ];
        let ack_vector = AckVectorHeader::new(100, Some(1000), ack_vectors).unwrap();
        let ack_of_ack = AckOfAckVectorHeader::new(50);

        let packet = AckPacket::new(200, 256, Some(ack_vector), Some(ack_of_ack));
        let encoded = packet
            .encode(UdpProtocolVersion::V2)
            .expect("encode ACK packet");
        let decoded = AckPacket::decode(&encoded, UdpProtocolVersion::V2).unwrap();

        assert_eq!(decoded.header.sn_source_ack, 200);
        assert_eq!(decoded.header.receive_window_size, 256);
        assert!(decoded.ack_vector.is_some());
        assert!(decoded.ack_of_ack.is_some());
        assert_eq!(decoded.ack_of_ack.unwrap().sequence_number, 50);
    }

    #[test]
    fn test_source_packet_encoding_decoding() {
        let data = b"Hello, RDP-UDP!".to_vec();
        let packet = SourcePacket::new(42, 100, 256, data.clone(), None, None, false).unwrap();
        let encoded = packet
            .encode(UdpProtocolVersion::V1)
            .expect("encode SOURCE packet");
        let decoded = SourcePacket::decode(&encoded, UdpProtocolVersion::V1).unwrap();

        assert_eq!(decoded.sequence_number(), 42);
        assert_eq!(decoded.header.sn_source_ack, 100);
        assert_eq!(decoded.data, data);
    }

    #[test]
    fn test_fec_packet_encoding_decoding() {
        let fec_header = FecPayloadHeader::new(100, 5, 10, 2);
        let fec_data = vec![0xAA; 128];

        let packet = FecPacket::new(200, 256, fec_header, fec_data.clone(), None, None).unwrap();
        let encoded = packet
            .encode(UdpProtocolVersion::V1)
            .expect("encode FEC packet");
        let decoded = FecPacket::decode(&encoded, UdpProtocolVersion::V1).unwrap();

        assert_eq!(decoded.header.sn_source_ack, 200);
        assert_eq!(decoded.fec_header.sn_coded, 100);
        assert_eq!(decoded.fec_header.fec_index, 2);
        assert_eq!(decoded.fec_data, fec_data);
    }
}
