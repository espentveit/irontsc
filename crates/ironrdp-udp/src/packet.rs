use ironrdp_core::ReadCursor;

use crate::ack::{AckOfAckVectorHeader, AckVectorHeader};
use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::flags::DatagramFlags;
use crate::header::FecHeader;
use crate::payload::{FecPayloadHeader, PayloadPrefix, SourcePayloadHeader};
use crate::syndataex::UdpProtocolVersion;

/// PacketPrefixByte for RDPUDP2 (protocol version field 0x0101, aka "v3")
/// 
/// This byte is inserted at position 7 (after byte swapping) in all RDPUDP2 packets.
/// Bit layout: [Reserved:1][PacketType:4][ShortLength:3]
#[derive(Debug, Clone, Copy, PartialEq)]
struct PacketPrefixByte {
    /// Packet type index (0 = normal packet, 8 = dummy packet)
    packet_type: u8,
    /// Short packet length (actual length if <7, otherwise 7)
    short_length: u8,
}

impl PacketPrefixByte {
    /// Create PacketPrefixByte for a normal packet
    fn new(rdpudp_packet_len: usize) -> Self {
        let short_length = if rdpudp_packet_len >= 7 {
            7
        } else {
            rdpudp_packet_len as u8
        };
        Self {
            packet_type: 0, // Normal packet
            short_length,
        }
    }

    /// Encode to a single byte
    fn to_byte(self) -> u8 {
        // Bit layout: [Reserved:1=0][PacketType:4][ShortLength:3]
        (self.packet_type << 3) | (self.short_length & 0x07)
    }

    /// Decode from a single byte
    #[allow(dead_code)]
    fn from_byte(byte: u8) -> Self {
        let packet_type = (byte >> 3) & 0x0F;
        let short_length = byte & 0x07;
        Self {
            packet_type,
            short_length,
        }
    }
}

/// Wrap RDPUDP packet with PacketPrefixByte (MS-RDPEUDP2 section 3.1.1.1.5)
///
/// Steps:
/// 1. Generate PacketPrefixByte
/// 2. Pad packet to 7 bytes if needed
/// 3. Prefix the byte to packet
/// 4. Swap byte 0 with byte 7
fn wrap_with_prefix_byte(mut rdpudp_packet: Vec<u8>) -> Vec<u8> {
    let original_len = rdpudp_packet.len();
    let prefix = PacketPrefixByte::new(original_len);

    // Pad to 7 bytes if needed
    if rdpudp_packet.len() < 7 {
        rdpudp_packet.resize(7, 0);
    }

    // Prefix the PacketPrefixByte
    let mut result = vec![prefix.to_byte()];
    result.extend_from_slice(&rdpudp_packet);

    // Swap byte 0 with byte 7
    result.swap(0, 7);

    result
}

/// Unwrap PacketPrefixByte from received packet (MS-RDPEUDP2 section 3.1.1.1.5)
///
/// Steps:
/// 1. Swap byte 0 with byte 7
/// 2. Remove first byte and parse as PacketPrefixByte
/// 3. Remove padding if short_length < 7
///
/// Returns None if the packet doesn't appear to be wrapped (e.g., handshake packets)
fn unwrap_prefix_byte(mut payload: Vec<u8>) -> UdpResult<Option<Vec<u8>>> {
    // Handshake packets are never wrapped, only data transfer packets
    // If packet is < 8 bytes, it can't be wrapped
    if payload.len() < 8 {
        return Ok(None);
    }

    // Check if byte 7 looks like a valid PacketPrefixByte
    // Valid prefix has: reserved=0, packet_type=0 or 8, short_length=0-7
    let potential_prefix = payload[7];
    let reserved = (potential_prefix >> 7) & 0x01;
    let packet_type = (potential_prefix >> 3) & 0x0F;
    let _short_length = potential_prefix & 0x07;
    
    // If it doesn't look like a valid prefix, assume it's a handshake packet
    if reserved != 0 || (packet_type != 0 && packet_type != 8) {
        return Ok(None);
    }

    // Swap byte 0 with byte 7
    payload.swap(0, 7);

    // Remove and parse PacketPrefixByte
    let prefix_byte = payload.remove(0);
    let prefix = PacketPrefixByte::from_byte(prefix_byte);

    // Remove padding if packet was short
    if prefix.short_length < 7 {
        let padding_to_remove = 7 - prefix.short_length as usize;
        let new_len = payload.len().saturating_sub(padding_to_remove);
        payload.truncate(new_len);
    }

    Ok(Some(payload))
}

fn ack_of_ack_encoded_length(version: UdpProtocolVersion) -> usize {
    match version {
        UdpProtocolVersion::V1
        | UdpProtocolVersion::V2
        | UdpProtocolVersion::V3 => AckOfAckVectorHeader::SIZE_V1,
        _ => AckOfAckVectorHeader::SIZE_V1,
    }
}

fn decode_ack_vector_if_present(
    context: &'static str,
    header: &FecHeader,
    cursor: &mut ReadCursor<'_>,
    version: UdpProtocolVersion,
    require: bool,
    min_remaining: usize,
) -> UdpResult<Option<AckVectorHeader>> {
    if !header.flags.contains(DatagramFlags::ACK) {
        return Ok(None);
    }

    if cursor.len() == 0 {
        return if require {
            Err(UdpError::invalid_field(
                context,
                "AckVector",
                "expected ACK vector bytes",
            ))
        } else {
            Ok(None)
        };
    }

    let mut preview = cursor.clone();
    match AckVectorHeader::decode(&mut preview, version) {
        Ok(vector) => {
            if preview.len() < min_remaining {
                return if require {
                    Err(UdpError::invalid_field(
                        context,
                        "AckVector",
                        "not enough bytes for remaining fields",
                    ))
                } else {
                    Ok(None)
                };
            }
            *cursor = preview;
            Ok(Some(vector))
        }
        Err(err) => {
            if require {
                Err(err)
            } else {
                Ok(None)
            }
        }
    }
}

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
            flags |= DatagramFlags::ACK_VEC;
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
        // Unwrap PacketPrefixByte for RDPUDP v3 only (RDPUDP2 protocol)
        let bytes = if version == UdpProtocolVersion::V3 {
            unwrap_prefix_byte(bytes.to_vec())?.unwrap_or_else(|| bytes.to_vec())
        } else {
            bytes.to_vec()
        };

        let mut cursor = ReadCursor::new(&bytes);
        let header = FecHeader::decode(&mut cursor)?;

        if !header.flags.contains(DatagramFlags::ACK) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "ACK flag not set",
            ));
        }

        let ack_of_ack_len = if header.flags.contains(DatagramFlags::ACK_OF_ACKS) {
            ack_of_ack_encoded_length(version)
        } else {
            0
        };

        let ack_vector = decode_ack_vector_if_present(
            Self::NAME,
            &header,
            &mut cursor,
            version,
            true,
            ack_of_ack_len,
        )?;

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

        // Wrap with PacketPrefixByte for RDPUDP2 (v3 only, per MS-RDPEUDP2 spec)
        if version == UdpProtocolVersion::V3 {
            buffer = wrap_with_prefix_byte(buffer);
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
        coded_sequence_number: u32,
        source_sequence_number: u32,
        sn_source_ack: u32,
        receive_window_size: u16,
        data: Vec<u8>,
        ack_vector: Option<AckVectorHeader>,
        ack_of_ack: Option<AckOfAckVectorHeader>,
        include_ack: bool,
    ) -> UdpResult<Self> {
        debug_assert_eq!(DatagramFlags::DATA.bits(), 0x0004);
        let mut flags = DatagramFlags::DATA;
        if include_ack || ack_vector.is_some() || ack_of_ack.is_some() {
            flags |= DatagramFlags::ACK;
        }
        if ack_vector.is_some() {
            flags |= DatagramFlags::ACK_VEC;
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
        let source_header = SourcePayloadHeader::new(coded_sequence_number, source_sequence_number);

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
        // Unwrap PacketPrefixByte for RDPUDP2 (v3 only, per MS-RDPEUDP2 spec)
        let bytes = if version == UdpProtocolVersion::V3 {
            unwrap_prefix_byte(bytes.to_vec())?.unwrap_or_else(|| bytes.to_vec())
        } else {
            bytes.to_vec()
        };

        let mut cursor = ReadCursor::new(&bytes);
        let header = FecHeader::decode(&mut cursor)?;

        if !header.flags.contains(DatagramFlags::DATA) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "DATA flag not set",
            ));
        }

        if header.flags.contains(DatagramFlags::ACK_OF_ACKS)
            && !header.flags.contains(DatagramFlags::ACK)
        {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "ACK_OF_ACKS flag requires ACK flag",
            ));
        }

        let ack_of_ack_len = if header.flags.contains(DatagramFlags::ACK_OF_ACKS) {
            ack_of_ack_encoded_length(version)
        } else {
            0
        };

        let min_remaining = ack_of_ack_len + PayloadPrefix::SIZE + SourcePayloadHeader::SIZE;

        let ack_vector = decode_ack_vector_if_present(
            Self::NAME,
            &header,
            &mut cursor,
            version,
            false,
            min_remaining,
        )?;

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

        // Wrap with PacketPrefixByte for RDPUDP v3 only (RDPUDP2 protocol)
        if version == UdpProtocolVersion::V3 {
            buffer = wrap_with_prefix_byte(buffer);
        }

        Ok(buffer)
    }

    pub fn sequence_number(&self) -> u32 {
        self.source_header.sn_source_start
    }

    pub fn coded_sequence_number(&self) -> u32 {
        self.source_header.sn_coded
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
        // FEC packets do NOT have the DATA flag - they're identified by absence of DATA
        let mut flags = DatagramFlags::empty();
        if ack_vector.is_some() || ack_of_ack.is_some() {
            flags |= DatagramFlags::ACK;
        }
        if ack_vector.is_some() {
            flags |= DatagramFlags::ACK_VEC;
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
        // Unwrap PacketPrefixByte for RDPUDP2 (v3 only, per MS-RDPEUDP2 spec)
        let bytes = if version == UdpProtocolVersion::V3 {
            unwrap_prefix_byte(bytes.to_vec())?.unwrap_or_else(|| bytes.to_vec())
        } else {
            bytes.to_vec()
        };

        let mut cursor = ReadCursor::new(&bytes);
        let header = FecHeader::decode(&mut cursor)?;

        // FEC packets should NOT have the DATA flag
        // They are identified by the absence of the DATA flag
        if header.flags.contains(DatagramFlags::DATA) {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "FEC packet should not have DATA flag set",
            ));
        }

        if header.flags.contains(DatagramFlags::ACK_OF_ACKS)
            && !header.flags.contains(DatagramFlags::ACK)
        {
            return Err(UdpError::invalid_field(
                Self::NAME,
                "uFlags",
                "ACK_OF_ACKS flag requires ACK flag",
            ));
        }

        let ack_of_ack_len = if header.flags.contains(DatagramFlags::ACK_OF_ACKS) {
            ack_of_ack_encoded_length(version)
        } else {
            0
        };

        let min_remaining = ack_of_ack_len + PayloadPrefix::SIZE + FecPayloadHeader::SIZE;

        let ack_vector = decode_ack_vector_if_present(
            Self::NAME,
            &header,
            &mut cursor,
            version,
            false,
            min_remaining,
        )?;

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

        // Wrap with PacketPrefixByte for RDPUDP2 (v3 only, per MS-RDPEUDP2 spec)
        if version == UdpProtocolVersion::V3 {
            buffer = wrap_with_prefix_byte(buffer);
        }

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
        let packet = SourcePacket::new(42, 42, 100, 256, data.clone(), None, None, false).unwrap();
        let encoded = packet
            .encode(UdpProtocolVersion::V1)
            .expect("encode SOURCE packet");
        let decoded = SourcePacket::decode(&encoded, UdpProtocolVersion::V1).unwrap();

        assert_eq!(decoded.sequence_number(), 42);
        assert_eq!(decoded.coded_sequence_number(), 42);
        assert_eq!(decoded.header.sn_source_ack, 100);
        assert_eq!(decoded.data, data);
    }

    #[test]
    fn source_packet_sets_expected_flags() {
        let packet = SourcePacket::new(
            10,
            10,
            0,
            64,
            b"payload".to_vec(),
            None,
            None,
            true,
        )
        .expect("create source packet");

        let expected = DatagramFlags::DATA | DatagramFlags::ACK;
        assert_eq!(packet.header.flags, expected);

        let encoded = packet
            .encode(UdpProtocolVersion::V3)
            .expect("encode source packet");
        let decoded = SourcePacket::decode(&encoded, UdpProtocolVersion::V3)
            .expect("decode source packet");
        assert_eq!(decoded.header.flags, expected);
    }

    #[test]
    fn test_fec_packet_encoding_decoding() {
        let fec_header = FecPayloadHeader::new(200, 100, 9, 2);
        let fec_data = vec![0xAA; 128];

        let packet = FecPacket::new(200, 256, fec_header, fec_data.clone(), None, None).unwrap();
        let encoded = packet
            .encode(UdpProtocolVersion::V1)
            .expect("encode FEC packet");
        let decoded = FecPacket::decode(&encoded, UdpProtocolVersion::V1).unwrap();

        assert_eq!(decoded.header.sn_source_ack, 200);
        assert_eq!(decoded.fec_header.sn_coded, 200);
        assert_eq!(decoded.fec_header.sn_source_start, 100);
        assert_eq!(decoded.fec_header.fec_index, 2);
        assert_eq!(decoded.fec_data, fec_data);
    }

    #[test]
    fn test_packet_prefix_byte_v3() {
        // Test that v3 packets get wrapped with PacketPrefixByte
        let data = b"Test".to_vec();
        let packet = SourcePacket::new(1, 1, 0, 64, data.clone(), None, None, false).unwrap();
        
        let encoded_v3 = packet.encode(UdpProtocolVersion::V3).expect("encode v3");
        let encoded_v1 = packet.encode(UdpProtocolVersion::V1).expect("encode v1");
        
        // V3 should be longer due to PacketPrefixByte wrapper (adds 1 byte, min 8 bytes total)
        assert!(encoded_v3.len() >= 8, "V3 packet should be at least 8 bytes");
        assert!(encoded_v3.len() > encoded_v1.len(), "V3 should be longer than V1");
        
        // Verify byte 7 contains the PacketPrefixByte (after swap)
        let prefix = PacketPrefixByte::from_byte(encoded_v3[7]);
        assert_eq!(prefix.packet_type, 0, "Should be normal packet type");
        
        // Decode should work
        let decoded = SourcePacket::decode(&encoded_v3, UdpProtocolVersion::V3).unwrap();
        assert_eq!(decoded.data, data);
        assert!(decoded.header.flags.contains(DatagramFlags::DATA));
    }
}
