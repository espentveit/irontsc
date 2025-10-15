use ironrdp_core::ReadCursor;

use crate::correlation::CorrelationId;
use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::flags::DatagramFlags;
use crate::header::FecHeader;
use crate::syndata::SynData;
use crate::syndataex::SynDataEx;

#[derive(Debug, Clone)]
pub struct SynPacket(HandshakePacket);

impl SynPacket {
    pub fn new(
        receive_window_size: u16,
        syn_lossy: bool,
        syn_data: SynData,
        correlation_id: Option<CorrelationId>,
        syn_data_ex: Option<SynDataEx>,
    ) -> Self {
        let mut flags = DatagramFlags::SYN;
        if syn_lossy {
            flags |= DatagramFlags::SYNLOSSY;
        }
        if correlation_id.is_some() {
            flags |= DatagramFlags::CORRELATION_ID;
        }
        if syn_data_ex.is_some() {
            flags |= DatagramFlags::SYNEX;
        }

        let header = FecHeader {
            sn_source_ack: u32::MAX,
            receive_window_size,
            flags,
        };

        Self(HandshakePacket {
            header,
            syn_data,
            correlation_id,
            syn_data_ex,
        })
    }

    pub fn decode(bytes: &[u8]) -> UdpResult<Self> {
        let packet = HandshakePacket::decode(bytes)?;
        if !packet.header.flags.contains(DatagramFlags::SYN) {
            return Err(UdpError::invalid_field(
                HandshakePacket::NAME,
                "uFlags",
                "SYN flag not set",
            ));
        }
        if packet.header.flags.contains(DatagramFlags::ACK) {
            return Err(UdpError::invalid_field(
                HandshakePacket::NAME,
                "uFlags",
                "unexpected ACK flag in SYN packet",
            ));
        }
        Ok(Self(packet))
    }

    pub fn inner(&self) -> &HandshakePacket {
        &self.0
    }

    pub fn inner_mut(&mut self) -> &mut HandshakePacket {
        &mut self.0
    }

    pub fn into_inner(self) -> HandshakePacket {
        self.0
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.encode(false)
    }

    pub fn to_padded_bytes(&self) -> Vec<u8> {
        self.0.encode(true)
    }
}

#[derive(Debug, Clone)]
pub struct SynAckPacket(HandshakePacket);

impl SynAckPacket {
    pub fn new(
        sn_source_ack: u32,
        receive_window_size: u16,
        syn_data: SynData,
        correlation_id: Option<CorrelationId>,
        syn_data_ex: Option<SynDataEx>,
    ) -> Self {
        let mut flags = DatagramFlags::SYN | DatagramFlags::ACK;
        if correlation_id.is_some() {
            flags |= DatagramFlags::CORRELATION_ID;
        }
        if syn_data_ex.is_some() {
            flags |= DatagramFlags::SYNEX;
        }

        let header = FecHeader {
            sn_source_ack,
            receive_window_size,
            flags,
        };

        Self(HandshakePacket {
            header,
            syn_data,
            correlation_id,
            syn_data_ex,
        })
    }

    pub fn decode(bytes: &[u8]) -> UdpResult<Self> {
        let packet = HandshakePacket::decode(bytes)?;
        let flags = packet.header.flags;
        if !flags.contains(DatagramFlags::SYN) || !flags.contains(DatagramFlags::ACK) {
            return Err(UdpError::invalid_field(
                HandshakePacket::NAME,
                "uFlags",
                "SYN+ACK packet missing required flags",
            ));
        }
        Ok(Self(packet))
    }

    pub fn inner(&self) -> &HandshakePacket {
        &self.0
    }

    pub fn into_inner(self) -> HandshakePacket {
        self.0
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.encode(false)
    }

    pub fn to_padded_bytes(&self) -> Vec<u8> {
        self.0.encode(true)
    }
}

#[derive(Debug, Clone)]
pub struct HandshakePacket {
    pub header: FecHeader,
    pub syn_data: SynData,
    pub correlation_id: Option<CorrelationId>,
    pub syn_data_ex: Option<SynDataEx>,
}

impl HandshakePacket {
    pub const NAME: &'static str = "RDPUDP_HANDSHAKE";

    fn decode(bytes: &[u8]) -> UdpResult<Self> {
        let mut cursor = ReadCursor::new(bytes);
        let mut header = FecHeader::decode(&mut cursor)?;
        let syn_data = SynData::decode(&mut cursor)?;

        let correlation_id = if header.flags.contains(DatagramFlags::CORRELATION_ID) {
            Some(CorrelationId::decode(&mut cursor)?)
        } else {
            None
        };

        let syn_data_ex = if header.flags.contains(DatagramFlags::SYNEX) {
            Some(SynDataEx::decode(&mut cursor)?)
        } else {
            None
        };

        // Ignore remaining padding bytes
        header.flags = normalize_flags(
            header.flags,
            correlation_id.is_some(),
            syn_data_ex.is_some(),
        );

        Ok(Self {
            header,
            syn_data,
            correlation_id,
            syn_data_ex,
        })
    }

    fn encode(&self, pad_to_mtu: bool) -> Vec<u8> {
        let mut buffer = Vec::new();
        let mut header = self.header;
        header.flags = normalize_flags(
            header.flags,
            self.correlation_id.is_some(),
            self.syn_data_ex.is_some(),
        );
        header.encode_into(&mut buffer);
        self.syn_data.encode_into(&mut buffer);
        if let Some(ref correlation) = self.correlation_id {
            correlation.encode_into(&mut buffer);  // Now 24 bytes: 16 value + 8 reserved
        }
        if let Some(ref syn_data_ex) = self.syn_data_ex {
            syn_data_ex.encode_into(&mut buffer);  // Starts immediately after correlation at byte 48
        }

        if pad_to_mtu {
            let target = self.syn_data.min_padding_size();
            if buffer.len() < target {
                buffer.resize(target, 0);
            }
        }

        buffer
    }
}

fn normalize_flags(
    mut flags: DatagramFlags,
    has_correlation_id: bool,
    has_syn_ex: bool,
) -> DatagramFlags {
    if has_correlation_id {
        flags |= DatagramFlags::CORRELATION_ID;
    } else {
        flags.remove(DatagramFlags::CORRELATION_ID);
    }

    if has_syn_ex {
        flags |= DatagramFlags::SYNEX;
    } else {
        flags.remove(DatagramFlags::SYNEX);
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlation::CorrelationId;

    #[test]
    fn decode_syn_packet_example() {
        let mut data = Vec::new();
        data.extend_from_slice(&[
            0xff, 0xff, 0xff, 0xff, // snSourceAck
            0x04, 0x00, // window
            0x0a, 0x01, // flags (SYN | SYNLOSSY | CORRELATION_ID)
        ]);
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x42, // snInitialSequenceNumber
            0x04, 0xd0, // upstream MTU
            0x04, 0xd0, // downstream MTU
        ]);
        let correlation_bytes: [u8; 16] = [
            0xd2, 0x35, 0xac, 0x43, 0x89, 0x41, 0x42, 0xda, 0xb1, 0x0e, 0xdd, 0x68, 0x87, 0xf7,
            0xf9, 0xfb,
        ];
        data.extend_from_slice(&correlation_bytes);
        data.extend_from_slice(&[0u8; 16]);
        data.resize(1232, 0); // zero padding

        let packet = SynPacket::decode(&data).expect("decode syn example");
        let inner = packet.inner();
        assert_eq!(inner.header.sn_source_ack, u32::MAX);
        assert_eq!(inner.header.receive_window_size, 0x0400);
        assert!(inner.header.flags.contains(DatagramFlags::SYN));
        assert!(inner.header.flags.contains(DatagramFlags::SYNLOSSY));
        assert!(inner.header.flags.contains(DatagramFlags::CORRELATION_ID));
        assert_eq!(inner.syn_data.initial_sequence_number, 0x0000_0042);
        assert_eq!(inner.syn_data.upstream_mtu, 0x04d0);
        assert_eq!(inner.syn_data.downstream_mtu, 0x04d0);
        assert_eq!(
            inner.correlation_id,
            Some(CorrelationId {
                value: correlation_bytes
            })
        );
    }

    #[test]
    fn decode_syn_ack_packet_example() {
        let mut data = Vec::new();
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x42, // snSourceAck
            0x04, 0x00, // window
            0x00, 0x05, // flags (SYN | ACK)
        ]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x42, 0x04, 0xd0, 0x04, 0xd0]);
        data.resize(1232, 0);

        let packet = SynAckPacket::decode(&data).expect("decode syn ack example");
        let inner = packet.inner();
        assert_eq!(inner.header.sn_source_ack, 0x0000_0042);
        assert_eq!(inner.header.receive_window_size, 0x0400);
        assert!(inner.header.flags.contains(DatagramFlags::SYN));
        assert!(inner.header.flags.contains(DatagramFlags::ACK));
        assert!(inner.correlation_id.is_none());
        assert_eq!(inner.syn_data.initial_sequence_number, 0x0000_0042);
    }
}
