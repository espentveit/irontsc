//! MS-RDPEUDP2 (version 3) helpers.
//!
//! Provides typed representations of packets described in [MS-RDPEUDP2],
//! including encoder/decoder helpers and utilities to work with the on-wire
//! packet prefix transformation.

use bitflags::bitflags;

use crate::error::{ensure_min_length, DecodeFrom, EncodeInto, Result, UdpError};

bitflags! {
    /// Flags carried in the RDP-UDP2 packet header.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HeaderFlags: u16 {
        const ACK = 0x001;
        const DATA = 0x004;
        const ACKVEC = 0x008;
        const AOA = 0x010;
        const OVERHEADSIZE = 0x040;
        const DELAYACKINFO = 0x100;
        /// Not described in [MS-RDPEUDP2] 2.2.1.1, but set by both Windows endpoints on every
        /// DATA packet that does not also carry an ACK payload. It carries no payload of its
        /// own; it is decoded so that it survives a decode/encode round trip.
        const NOACK = 0x200;
    }
}

/// Packet header composed of 12 flag bits and a 4-bit LogWindowSize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub flags: HeaderFlags,
    pub log_window_size: u8,
}

impl PacketHeader {
    pub fn new(flags: HeaderFlags, log_window_size: u8) -> Result<Self> {
        if log_window_size > 0x0f {
            return Err(UdpError::InvalidField("log_window_size"));
        }
        Ok(Self {
            flags,
            log_window_size,
        })
    }
}

impl Default for PacketHeader {
    fn default() -> Self {
        Self {
            flags: HeaderFlags::empty(),
            log_window_size: 0,
        }
    }
}

impl EncodeInto for PacketHeader {
    fn encode_into(&self, out: &mut Vec<u8>) {
        let bits = (self.flags.bits() & 0x0fff) | (((self.log_window_size as u16) & 0x0f) << 12);
        out.extend_from_slice(&bits.to_le_bytes());
    }
}

impl<'a> DecodeFrom<'a> for PacketHeader {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 2)?;
        let raw = u16::from_le_bytes([input[0], input[1]]);
        let flags_bits = raw & 0x0fff;
        let log_window_size = ((raw >> 12) & 0x0f) as u8;
        // Use from_bits_truncate to ignore unknown/reserved flag bits that the server may send
        let flags = HeaderFlags::from_bits_truncate(flags_bits);
        Ok((
            Self {
                flags,
                log_window_size,
            },
            &input[2..],
        ))
    }
}

/// Packet prefix byte inserted into the on-wire payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketPrefixByte {
    pub packet_type_index: u8,
    pub short_length: u8,
}

impl PacketPrefixByte {
    pub const TYPE_STANDARD: u8 = 0;
    pub const TYPE_DUMMY: u8 = 8;

    pub fn with_length(packet_type_index: u8, layout_len: usize) -> Result<(Self, usize)> {
        if packet_type_index != Self::TYPE_STANDARD && packet_type_index != Self::TYPE_DUMMY {
            return Err(UdpError::InvalidField("packet_type_index"));
        }
        // Per MS-RDPEUDP2 spec: If length > 7 bytes, short_length MUST be set to 7
        // If length <= 7 bytes, short_length specifies the actual length
        let short = if layout_len < 7 { layout_len as u8 } else { 7 };
        let padded = if layout_len < 7 { 7 } else { layout_len };
        Ok((
            Self {
                packet_type_index,
                short_length: short,
            },
            padded,
        ))
    }

    pub fn encode(self) -> u8 {
        // Bit layout: [Short_Length:3][Packet_Type_Index:4][Reserved:1]
        // Bits 7-5: Short_Packet_Length (3 bits)
        // Bits 4-1: Packet_Type_Index (4 bits)
        // Bit 0: Reserved (must be 0)
        ((self.short_length & 0x07) << 5) | ((self.packet_type_index & 0x0f) << 1)
    }

    pub fn decode(byte: u8) -> Result<Self> {
        // Bit layout: [Short_Length:3][Packet_Type_Index:4][Reserved:1]
        if byte & 0x01 != 0 {
            return Err(UdpError::InvalidField("packet_prefix.reserved"));
        }
        let short_length = (byte >> 5) & 0x07;
        let packet_type_index = (byte >> 1) & 0x0f;
        if packet_type_index != Self::TYPE_STANDARD && packet_type_index != Self::TYPE_DUMMY {
            return Err(UdpError::InvalidField("packet_type_index"));
        }
        Ok(Self {
            packet_type_index,
            short_length,
        })
    }
}

/// ACK payload carrying timing information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckPayload {
    pub sequence_number: u16,
    pub received_timestamp: u32,
    pub send_ack_time_gap_ms: u8,
    pub num_delayed_acks: u8,
    pub delay_ack_time_scale: u8,
    pub delay_ack_time_additions: Vec<u8>,
}

impl AckPayload {
    pub fn validate(&self) -> Result<()> {
        if self.received_timestamp >= (1 << 24) {
            return Err(UdpError::InvalidField("ack.received_timestamp"));
        }
        if self.num_delayed_acks as usize != self.delay_ack_time_additions.len() {
            return Err(UdpError::InvalidField("ack.delay_additions"));
        }
        Ok(())
    }
}

impl EncodeInto for AckPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.validate().expect("validated before encode");
        out.extend_from_slice(&self.sequence_number.to_le_bytes());
        write_u24_le(self.received_timestamp, out);
        out.push(self.send_ack_time_gap_ms);
        // numDelayedAcks occupies the low nibble and delayAckTimeScale the high one. Getting
        // these the wrong way round makes the payload the wrong length whenever the peer
        // aggregates acknowledgements, which silently shifts every payload that follows.
        out.push(((self.delay_ack_time_scale & 0x0f) << 4) | (self.num_delayed_acks & 0x0f));
        out.extend_from_slice(&self.delay_ack_time_additions);
    }
}

impl<'a> DecodeFrom<'a> for AckPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 7)?;
        let sequence_number = u16::from_le_bytes([input[0], input[1]]);
        let received_timestamp = read_u24_le(&input[2..5]);
        let send_ack_time_gap_ms = input[5];
        let nibble = input[6];
        let num_delayed_acks = nibble & 0x0f;
        let delay_ack_time_scale = (nibble >> 4) & 0x0f;
        let mut rest = &input[7..];
        ensure_min_length(rest, num_delayed_acks as usize)?;
        let delay_ack_time_additions = rest[..num_delayed_acks as usize].to_vec();
        rest = &rest[num_delayed_acks as usize..];
        let payload = Self {
            sequence_number,
            received_timestamp,
            send_ack_time_gap_ms,
            num_delayed_acks,
            delay_ack_time_scale,
            delay_ack_time_additions,
        };
        payload.validate()?;
        Ok((payload, rest))
    }
}

/// Overhead size payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverheadSizePayload {
    pub overhead_size: u8,
}

impl EncodeInto for OverheadSizePayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.overhead_size);
    }
}

impl<'a> DecodeFrom<'a> for OverheadSizePayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 1)?;
        Ok((
            Self {
                overhead_size: input[0],
            },
            &input[1..],
        ))
    }
}

/// Delay ACK information payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayAckInfoPayload {
    pub max_delayed_acks: u8,
    pub delayed_ack_timeout_ms: u16,
}

impl EncodeInto for DelayAckInfoPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.max_delayed_acks);
        out.extend_from_slice(&self.delayed_ack_timeout_ms.to_le_bytes());
    }
}

impl<'a> DecodeFrom<'a> for DelayAckInfoPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 3)?;
        Ok((
            Self {
                max_delayed_acks: input[0],
                delayed_ack_timeout_ms: u16::from_le_bytes([input[1], input[2]]),
            },
            &input[3..],
        ))
    }
}

/// Ack-of-Acks payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckOfAcksPayload {
    pub sequence_number: u16,
}

impl EncodeInto for AckOfAcksPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sequence_number.to_le_bytes());
    }
}

impl<'a> DecodeFrom<'a> for AckOfAcksPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 2)?;
        Ok((
            Self {
                sequence_number: u16::from_le_bytes([input[0], input[1]]),
            },
            &input[2..],
        ))
    }
}

/// Data header payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeaderPayload {
    pub data_sequence_number: u16,
}

impl EncodeInto for DataHeaderPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.data_sequence_number.to_le_bytes());
    }
}

impl<'a> DecodeFrom<'a> for DataHeaderPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 2)?;
        Ok((
            Self {
                data_sequence_number: u16::from_le_bytes([input[0], input[1]]),
            },
            &input[2..],
        ))
    }
}

/// Acknowledgement vector entry as transmitted on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckVecEntry {
    /// State map (7 bits represent state for next 7 sequence numbers).
    StateMap(u8),
    /// Run-length entry covering a continuous range.
    RunLength { received: bool, length: u8 },
}

impl AckVecEntry {
    fn encode(&self) -> Result<u8> {
        let byte = match *self {
            AckVecEntry::StateMap(bits) => bits & 0x7f,
            AckVecEntry::RunLength { received, length } => {
                if length == 0 || length > 0x3f {
                    return Err(UdpError::InvalidField("ackvec.run_length"));
                }
                0x80 | ((received as u8) << 6) | (length & 0x3f)
            }
        };
        Ok(byte)
    }

    fn decode(byte: u8) -> Result<Self> {
        if byte & 0x80 == 0 {
            Ok(AckVecEntry::StateMap(byte & 0x7f))
        } else {
            let received = (byte & 0x40) != 0;
            let length = byte & 0x3f;
            if length == 0 {
                return Err(UdpError::InvalidField("ackvec.run_length"));
            }
            Ok(AckVecEntry::RunLength { received, length })
        }
    }
}

/// Timestamp information (present when [`AckVectorPayload::timestamp_info`] is `Some`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampInfo {
    pub timestamp: u32,
    pub send_ack_time_gap_ms: u8,
}

impl TimestampInfo {
    pub fn validate(&self) -> Result<()> {
        if self.timestamp >= (1 << 24) {
            return Err(UdpError::InvalidField("ackvec.timestamp"));
        }
        Ok(())
    }
}

/// ACK vector payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckVectorPayload {
    pub base_sequence_number: u16,
    pub timestamp_info: Option<TimestampInfo>,
    pub entries: Vec<AckVecEntry>,
}

impl AckVectorPayload {
    pub fn validate(&self) -> Result<()> {
        if self.entries.len() > 0x7f {
            return Err(UdpError::InvalidField("ackvec.size"));
        }
        if let Some(info) = self.timestamp_info {
            info.validate()?;
        }
        Ok(())
    }
}

impl EncodeInto for AckVectorPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.validate().expect("validated before encode");
        out.extend_from_slice(&self.base_sequence_number.to_le_bytes());
        let mut control = (self.entries.len() as u8) & 0x7f;
        if self.timestamp_info.is_some() {
            control |= 0x80;
        }
        out.push(control);
        if let Some(info) = self.timestamp_info {
            write_u24_le(info.timestamp, out);
            out.push(info.send_ack_time_gap_ms);
        }
        for entry in &self.entries {
            out.push(entry.encode().expect("entry validated"));
        }
    }
}

impl<'a> DecodeFrom<'a> for AckVectorPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 3)?;
        let base_sequence_number = u16::from_le_bytes([input[0], input[1]]);
        let mut rest = &input[2..];
        let control = rest[0];
        rest = &rest[1..];
        let size = (control & 0x7f) as usize;
        let timestamp_present = (control & 0x80) != 0;
        let timestamp_info = if timestamp_present {
            ensure_min_length(rest, 4)?;
            let timestamp = read_u24_le(&rest[..3]);
            let gap = rest[3];
            rest = &rest[4..];
            Some(TimestampInfo {
                timestamp,
                send_ack_time_gap_ms: gap,
            })
        } else {
            None
        };
        ensure_min_length(rest, size)?;
        let mut entries = Vec::with_capacity(size);
        for &byte in &rest[..size] {
            entries.push(AckVecEntry::decode(byte)?);
        }
        rest = &rest[size..];
        let payload = Self {
            base_sequence_number,
            timestamp_info,
            entries,
        };
        payload.validate()?;
        Ok((payload, rest))
    }
}

/// Data body payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataBodyPayload {
    pub channel_sequence_number: u16,
    pub data: Vec<u8>,
}

impl EncodeInto for DataBodyPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel_sequence_number.to_le_bytes());
        out.extend_from_slice(&self.data);
    }
}

impl<'a> DecodeFrom<'a> for DataBodyPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 2)?;
        let channel_sequence_number = u16::from_le_bytes([input[0], input[1]]);
        let data = input[2..].to_vec();
        Ok((
            Self {
                channel_sequence_number,
                data,
            },
            &input[input.len()..],
        ))
    }
}

/// Full RDP-UDP2 packet layout (without prefix transformation).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    pub header: PacketHeader,
    pub ack: Option<AckPayload>,
    pub overhead_size: Option<OverheadSizePayload>,
    pub delay_ack_info: Option<DelayAckInfoPayload>,
    pub ack_of_acks: Option<AckOfAcksPayload>,
    pub ack_vector: Option<AckVectorPayload>,
    pub data_header: Option<DataHeaderPayload>,
    pub data_body: Option<DataBodyPayload>,
}

impl Packet {
    fn validate(&self) -> Result<()> {
        if self.ack.is_some() && self.ack_vector.is_some() {
            return Err(UdpError::InvalidField("ack.flags_conflict"));
        }
        // Dummy packets can have data_header without data_body (sequence number only)
        // Standard packets must have both header and body
        if self.data_body.is_some() && self.data_header.is_none() {
            return Err(UdpError::InvalidField("data.body_without_header"));
        }
        let mut expected = HeaderFlags::empty();
        if self.ack.is_some() {
            expected |= HeaderFlags::ACK;
        }
        if self.ack_vector.is_some() {
            expected |= HeaderFlags::ACKVEC;
        }
        if self.ack_of_acks.is_some() {
            expected |= HeaderFlags::AOA;
        }
        if self.overhead_size.is_some() {
            expected |= HeaderFlags::OVERHEADSIZE;
        }
        if self.delay_ack_info.is_some() {
            expected |= HeaderFlags::DELAYACKINFO;
        }
        if self.data_header.is_some() {
            expected |= HeaderFlags::DATA;
        }
        // NOACK is informational and has no associated payload, so it never participates in the
        // payload-presence check.
        if expected != self.header.flags.difference(HeaderFlags::NOACK) {
            return Err(UdpError::InvalidField("header.flags_mismatch"));
        }
        Ok(())
    }

    fn encode_layout(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::new();
        self.header.encode_into(&mut out);
        if let Some(ack) = &self.ack {
            ack.encode_into(&mut out);
        }
        if let Some(overhead) = &self.overhead_size {
            overhead.encode_into(&mut out);
        }
        if let Some(delay) = &self.delay_ack_info {
            delay.encode_into(&mut out);
        }
        if let Some(aoa) = &self.ack_of_acks {
            aoa.encode_into(&mut out);
        }
        if let Some(data_header) = &self.data_header {
            data_header.encode_into(&mut out);
        }
        if let Some(ack_vec) = &self.ack_vector {
            ack_vec.encode_into(&mut out);
        }
        if let Some(data_body) = &self.data_body {
            data_body.encode_into(&mut out);
        }
        Ok(out)
    }

    /// Encode the entire packet including the on-wire prefix byte transformation.
    pub fn encode_on_wire(&self, packet_type_index: u8) -> Result<Vec<u8>> {
        let mut layout = self.encode_layout()?;
        let original_len = layout.len();
        let (prefix, padded_len) = PacketPrefixByte::with_length(packet_type_index, original_len)?;
        if original_len < padded_len {
            layout.resize(padded_len, 0);
        }
        let mut payload = Vec::with_capacity(padded_len + 1);
        payload.push(prefix.encode());
        payload.extend_from_slice(&layout);
        if payload.len() < 8 {
            payload.resize(8, 0);
        }
        payload.swap(0, 7);
        Ok(payload)
    }

    /// Decode an on-wire RDP-UDP2 packet including prefix handling.
    pub fn decode_on_wire(input: &[u8]) -> Result<Self> {
        if input.len() < 8 {
            return Err(UdpError::too_short(8, input.len()));
        }
        let mut buf = input.to_vec();
        buf.swap(0, 7);
        let prefix = PacketPrefixByte::decode(buf[0])?;
        let mut layout = buf[1..].to_vec();
        // short_length: 0 = extended (use full length), 1-6 = actual length, 7 = extended (legacy)
        if prefix.short_length > 0 && prefix.short_length < 7 {
            if layout.len() < 7 {
                return Err(UdpError::InvalidField("packet_prefix.length"));
            }
            layout.truncate(prefix.short_length as usize);
        }
        // For short_length == 0 or 7, use the full layout length (extended length encoding)
        let is_dummy = prefix.packet_type_index == PacketPrefixByte::TYPE_DUMMY;
        Self::decode_layout(&layout, is_dummy)
    }

    fn decode_layout(layout: &[u8], is_dummy: bool) -> Result<Self> {
        let (header, mut rest) = PacketHeader::decode_from(layout)?;
        if header.flags.contains(HeaderFlags::ACK) && header.flags.contains(HeaderFlags::ACKVEC) {
            return Err(UdpError::InvalidField("ack.flags_conflict"));
        }
        let ack = if header.flags.contains(HeaderFlags::ACK) {
            let (ack, tail) = AckPayload::decode_from(rest)?;
            rest = tail;
            Some(ack)
        } else {
            None
        };
        let overhead_size = if header.flags.contains(HeaderFlags::OVERHEADSIZE) {
            let (overhead, tail) = OverheadSizePayload::decode_from(rest)?;
            rest = tail;
            Some(overhead)
        } else {
            None
        };
        let delay_ack_info = if header.flags.contains(HeaderFlags::DELAYACKINFO) {
            let (delay, tail) = DelayAckInfoPayload::decode_from(rest)?;
            rest = tail;
            Some(delay)
        } else {
            None
        };
        let ack_of_acks = if header.flags.contains(HeaderFlags::AOA) {
            let (aoa, tail) = AckOfAcksPayload::decode_from(rest)?;
            rest = tail;
            Some(aoa)
        } else {
            None
        };
        // Per MS-RDPEUDP2 2.2.1, the on-wire payload order is
        //   ACK, OverheadSize, DelayAckInfo, AckOfAcks, DataHeader, ACKVEC, DataBody.
        // DataBody is length-implicit (it runs to the end of the packet), so ACKVEC has to be
        // taken off the wire *before* it -- otherwise a piggybacked DATA+ACKVEC packet is
        // unparseable and the data it carries is lost.
        let (data_header, ack_vector, data_body) = if header.flags.contains(HeaderFlags::DATA) {
            let (dh, is_body_bearing) = if is_dummy {
                // Dummy packet: the DataHeader is followed by a body that higher layers must
                // ignore ([MS-RDPEUDP2] 3.1.1.1.5.1). It still occupies a sequence number.
                ensure_min_length(rest, 2)?;
                let sequence = u16::from_le_bytes([rest[0], rest[1]]);
                rest = &rest[2..];
                (
                    DataHeaderPayload {
                        data_sequence_number: sequence,
                    },
                    false,
                )
            } else {
                let (dh, tail) = DataHeaderPayload::decode_from(rest)?;
                rest = tail;
                (dh, true)
            };
            let ack_vec = if header.flags.contains(HeaderFlags::ACKVEC) {
                let (ack_vec, tail) = AckVectorPayload::decode_from(rest)?;
                rest = tail;
                Some(ack_vec)
            } else {
                None
            };
            let body = if is_body_bearing {
                let (body, tail) = DataBodyPayload::decode_from(rest)?;
                rest = tail;
                Some(body)
            } else {
                None
            };
            (Some(dh), ack_vec, body)
        } else {
            let ack_vec = if header.flags.contains(HeaderFlags::ACKVEC) {
                let (ack_vec, tail) = AckVectorPayload::decode_from(rest)?;
                rest = tail;
                Some(ack_vec)
            } else {
                None
            };
            (None, ack_vec, None)
        };
        // Dummy packets (and potentially all packets) can have trailing padding bytes, and the
        // packet boundary is already enforced by short_length / the datagram length, so trailing
        // bytes are tolerated rather than rejected.
        let _ = rest;
        let packet = Self {
            header,
            ack,
            overhead_size,
            delay_ack_info,
            ack_of_acks,
            ack_vector,
            data_header,
            data_body,
        };
        packet.validate()?;
        Ok(packet)
    }
}

fn read_u24_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}

fn write_u24_le(value: u32, out: &mut Vec<u8>) {
    let bytes = value.to_le_bytes();
    out.extend_from_slice(&bytes[..3]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdpudp_v2_packet;

    #[test]
    fn packet_round_trip_with_ack_and_data() {
        let header = PacketHeader::new(HeaderFlags::ACK | HeaderFlags::DATA, 4).unwrap();
        let ack = AckPayload {
            sequence_number: 0x1234,
            received_timestamp: 0x0055aa,
            send_ack_time_gap_ms: 7,
            num_delayed_acks: 2,
            delay_ack_time_scale: 1,
            delay_ack_time_additions: vec![10, 20],
        };
        let data_header = DataHeaderPayload {
            data_sequence_number: 0x3344,
        };
        let data_body = DataBodyPayload {
            channel_sequence_number: 0x5566,
            data: vec![1, 2, 3, 4, 5],
        };
        let packet = rdpudp_v2_packet!(
            header = header,
            ack = Some(ack.clone()),
            data_header = Some(data_header),
            data_body = Some(data_body.clone())
        );
        let encoded = packet
            .encode_on_wire(PacketPrefixByte::TYPE_STANDARD)
            .expect("encode");
        let decoded = Packet::decode_on_wire(&encoded).expect("decode");
        let decoded_ack = decoded.ack.unwrap();
        assert_eq!(decoded_ack.sequence_number, ack.sequence_number);
        assert_eq!(
            decoded_ack.delay_ack_time_additions,
            ack.delay_ack_time_additions
        );
        assert_eq!(decoded.data_body.unwrap().data, data_body.data);
    }

    /// Bytes captured from a Windows server: an ACK for our sequence 102 that aggregates one
    /// delayed acknowledgement, piggybacked on DATA sequence 224 carrying a TLS record. The
    /// trailing delayAckTimeAdditions byte only appears if numDelayedAcks is read from the low
    /// nibble; reading the high nibble shifts DataHeader by one byte and loses the payload.
    #[test]
    fn real_server_ack_with_delayed_acks_and_data() {
        let raw: [u8; 24] = [
            0x00, 0x45, 0xf0, 0x66, 0x00, 0x03, 0x22, 0xe0, 0x00, 0x01, 0x54, 0x08, 0xe0, 0x00,
            0x02, 0x00, 0x17, 0x03, 0x03, 0x00, 0x62, 0x56, 0x2c, 0x9f,
        ];
        let packet = Packet::decode_on_wire(&raw).expect("decode");
        assert_eq!(packet.header.log_window_size, 15);

        let ack = packet.ack.as_ref().expect("ack payload");
        assert_eq!(ack.sequence_number, 102);
        assert_eq!(ack.num_delayed_acks, 1);
        assert_eq!(ack.delay_ack_time_additions, vec![0x54]);
        assert_eq!(ack.delay_ack_time_scale, 0);

        assert_eq!(
            packet.overhead_size.as_ref().expect("overhead").overhead_size,
            8
        );
        assert_eq!(
            packet
                .data_header
                .as_ref()
                .expect("data header")
                .data_sequence_number,
            224,
            "the data sequence must survive a variable-length ACK payload"
        );
        let body = packet.data_body.as_ref().expect("data body");
        assert_eq!(body.channel_sequence_number, 2);
        assert_eq!(&body.data[..5], &[0x17, 0x03, 0x03, 0x00, 0x62]);

        // And the same bytes come back out.
        let reencoded = packet
            .encode_on_wire(PacketPrefixByte::TYPE_STANDARD)
            .expect("encode");
        assert_eq!(&reencoded[..], &raw[..]);
    }

    #[test]
    fn ack_vector_round_trip() {
        let header = PacketHeader::new(HeaderFlags::ACKVEC, 5).unwrap();
        let ack_vec = AckVectorPayload {
            base_sequence_number: 0x2222,
            timestamp_info: Some(TimestampInfo {
                timestamp: 0x00abcd,
                send_ack_time_gap_ms: 33,
            }),
            entries: vec![
                AckVecEntry::StateMap(0b1010_101),
                AckVecEntry::RunLength {
                    received: true,
                    length: 6,
                },
            ],
        };
        let packet = rdpudp_v2_packet!(header = header, ack_vector = Some(ack_vec.clone()));
        let encoded = packet
            .encode_on_wire(PacketPrefixByte::TYPE_STANDARD)
            .expect("encode");
        let decoded = Packet::decode_on_wire(&encoded).expect("decode");
        let decoded_vec = decoded.ack_vector.unwrap();
        assert_eq!(
            decoded_vec.base_sequence_number,
            ack_vec.base_sequence_number
        );
        assert_eq!(
            decoded_vec.timestamp_info.unwrap().send_ack_time_gap_ms,
            ack_vec.timestamp_info.unwrap().send_ack_time_gap_ms
        );
        assert_eq!(decoded_vec.entries, ack_vec.entries);
    }
}
