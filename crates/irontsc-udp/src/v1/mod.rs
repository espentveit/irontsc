//! MS-RDPEUDP (versions 1 and 2) helpers.
//!
//! This module provides typed representations of the packet headers and
//! payloads defined in [MS-RDPEUDP], together with encoder/decoder helpers and
//! a lightweight Forward Error Correction (FEC) helper.

use std::convert::TryFrom;

use bitflags::bitflags;

use crate::error::{ensure_min_length, DecodeFrom, EncodeInto, Result, UdpError};

pub mod fec;

bitflags! {
    /// Flags carried in the [`RdpUdpFecHeader`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HeaderFlags: u16 {
        const SYN = 0x0001;
        const FIN = 0x0002;
        const ACK = 0x0004;
        const DATA = 0x0008;
        const FEC = 0x0010;
        const CN = 0x0020;
        const CWR = 0x0040;
        const SACK_OPTION = 0x0080;
        const ACK_OF_ACKS = 0x0100;
        const SYN_LOSSY = 0x0200;
        const ACK_DELAYED = 0x0400;
        const CORRELATION_ID = 0x0800;
        const SYN_EX = 0x1000;
    }
}

/// RDPUDP header common to all packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RdpUdpFecHeader {
    pub sn_source_ack: u32,
    pub receive_window_size: u16,
    pub flags: HeaderFlags,
}

impl RdpUdpFecHeader {
    /// Creates a header configured for a SYN packet.
    pub fn syn(initial_window: u16, lossy: bool) -> Self {
        let mut flags = HeaderFlags::SYN;
        if lossy {
            flags |= HeaderFlags::SYN_LOSSY;
        }
        Self {
            sn_source_ack: u32::MAX,
            receive_window_size: initial_window,
            flags,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.receive_window_size == 0 {
            return Err(UdpError::InvalidField("receive_window_size"));
        }
        Ok(())
    }
}

impl Default for RdpUdpFecHeader {
    fn default() -> Self {
        Self {
            sn_source_ack: 0,
            receive_window_size: 1,
            flags: HeaderFlags::empty(),
        }
    }
}

impl EncodeInto for RdpUdpFecHeader {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sn_source_ack.to_be_bytes());
        out.extend_from_slice(&self.receive_window_size.to_be_bytes());
        out.extend_from_slice(&self.flags.bits().to_be_bytes());
    }
}

impl<'a> DecodeFrom<'a> for RdpUdpFecHeader {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 8)?;
        let sn_source_ack = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
        let receive_window_size = u16::from_be_bytes([input[4], input[5]]);
        let raw_flags = u16::from_be_bytes([input[6], input[7]]);
        let flags = HeaderFlags::from_bits(raw_flags).ok_or(UdpError::InvalidFlags(raw_flags))?;
        let header = Self {
            sn_source_ack,
            receive_window_size,
            flags,
        };
        header.validate()?;
        Ok((header, &input[8..]))
    }
}

/// Transport mode negotiated during the SYN exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    Reliable,
    Lossy,
}

impl TransportMode {
    pub fn from_flags(flags: HeaderFlags) -> Self {
        if flags.contains(HeaderFlags::SYN_LOSSY) {
            Self::Lossy
        } else {
            Self::Reliable
        }
    }
}

/// Protocol version negotiated via [`SynDataExPayload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtocolVersion {
    V1,
    V2,
}

impl ProtocolVersion {
    pub fn min_retransmit_timeout_ms(self) -> u16 {
        match self {
            Self::V1 => 500,
            Self::V2 => 300,
        }
    }

    pub fn min_delayed_ack_timeout_ms(self) -> u16 {
        match self {
            Self::V1 => 200,
            Self::V2 => 50,
        }
    }
}

/// ACK vector element state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorElementState {
    DatagramReceived = 0,
    Reserved1 = 1,
    Reserved2 = 2,
    DatagramNotYetReceived = 3,
}

impl TryFrom<u8> for VectorElementState {
    type Error = UdpError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::DatagramReceived),
            1 => Ok(Self::Reserved1),
            2 => Ok(Self::Reserved2),
            3 => Ok(Self::DatagramNotYetReceived),
            _ => Err(UdpError::UnknownDiscriminant(value)),
        }
    }
}

impl From<VectorElementState> for u8 {
    fn from(value: VectorElementState) -> Self {
        value as u8
    }
}

/// A single run-length encoded entry inside an ACK vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckVectorElement {
    state: VectorElementState,
    count: u8,
}

impl AckVectorElement {
    pub fn new(state: VectorElementState, count: u8) -> Result<Self> {
        if count == 0 || count > 0x3f {
            return Err(UdpError::InvalidField("ack_vector.count"));
        }
        Ok(Self { state, count })
    }

    pub fn state(&self) -> VectorElementState {
        self.state
    }

    pub fn count(&self) -> u8 {
        self.count
    }

    pub fn to_byte(self) -> u8 {
        ((u8::from(self.state) & 0x03) << 6) | (self.count & 0x3f)
    }

    pub fn from_byte(byte: u8) -> Result<Self> {
        let state = VectorElementState::try_from(byte >> 6)?;
        let count = byte & 0x3f;
        if count == 0 {
            return Err(UdpError::InvalidField("ack_vector.count"));
        }
        Ok(Self { state, count })
    }
}

/// Complete ACK vector (RLE encoded).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AckVector {
    pub elements: Vec<AckVectorElement>,
}

impl AckVector {
    pub fn total_datagrams(&self) -> usize {
        self.elements.iter().map(|e| e.count as usize).sum()
    }

    pub fn encode_bytes(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.elements.len());
        for element in &self.elements {
            buf.push(element.to_byte());
        }
        Ok(buf)
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        let mut elements = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            elements.push(AckVectorElement::from_byte(byte)?);
        }
        Ok(Self { elements })
    }
}

/// ACK section, comprising the vector and necessary padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckSection {
    pub ack_vector: AckVector,
}

impl AckSection {
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<()> {
        let encoded = self.ack_vector.encode_bytes()?;
        if encoded.len() > u16::MAX as usize {
            return Err(UdpError::InvalidField("ack_vector.size"));
        }
        out.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
        out.extend_from_slice(&encoded);
        let padding = (4 - ((2 + encoded.len()) % 4)) % 4;
        out.extend(std::iter::repeat(0).take(padding));
        Ok(())
    }

    pub fn decode(input: &[u8]) -> Result<(Self, &[u8])> {
        ensure_min_length(input, 2)?;
        let size = u16::from_be_bytes([input[0], input[1]]) as usize;
        ensure_min_length(&input[2..], size)?;
        let ack_bytes = &input[2..2 + size];
        let ack_vector = AckVector::decode_bytes(ack_bytes)?;
        let consumed = 2 + size;
        let padding = (4 - (consumed % 4)) % 4;
        ensure_min_length(&input[consumed..], padding)?;
        Ok((Self { ack_vector }, &input[consumed + padding..]))
    }
}

/// ACK of ACKs structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckOfAcks {
    pub sequence_number: u32,
}

impl EncodeInto for AckOfAcks {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sequence_number.to_be_bytes());
    }
}

impl<'a> DecodeFrom<'a> for AckOfAcks {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 4)?;
        let seq = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
        Ok((
            Self {
                sequence_number: seq,
            },
            &input[4..],
        ))
    }
}

/// Correlation identifier exchanged during the SYN handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelationIdPayload {
    pub correlation_id: [u8; 16],
}

impl EncodeInto for CorrelationIdPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.correlation_id);
        out.extend_from_slice(&[0u8; 16]);
    }
}

impl<'a> DecodeFrom<'a> for CorrelationIdPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 32)?;
        let mut correlation_id = [0u8; 16];
        correlation_id.copy_from_slice(&input[..16]);
        if input[16..32].iter().any(|&b| b != 0) {
            return Err(UdpError::InvalidField("correlation_id.reserved"));
        }
        Ok((Self { correlation_id }, &input[32..]))
    }
}

/// SYN payload (RDPUDP_SYNDATA_PAYLOAD).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynDataPayload {
    pub initial_sequence_number: u32,
    pub upstream_mtu: u16,
    pub downstream_mtu: u16,
}

impl SynDataPayload {
    pub fn validate(&self) -> Result<()> {
        if self.upstream_mtu < 1132 || self.upstream_mtu > 1232 {
            return Err(UdpError::InvalidField("syn.upstream_mtu"));
        }
        if self.downstream_mtu < 1132 || self.downstream_mtu > 1232 {
            return Err(UdpError::InvalidField("syn.downstream_mtu"));
        }
        Ok(())
    }
}

impl EncodeInto for SynDataPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.validate().expect("validated before encode");
        out.extend_from_slice(&self.initial_sequence_number.to_be_bytes());
        out.extend_from_slice(&self.upstream_mtu.to_be_bytes());
        out.extend_from_slice(&self.downstream_mtu.to_be_bytes());
    }
}

impl<'a> DecodeFrom<'a> for SynDataPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 8)?;
        let payload = Self {
            initial_sequence_number: u32::from_be_bytes([input[0], input[1], input[2], input[3]]),
            upstream_mtu: u16::from_be_bytes([input[4], input[5]]),
            downstream_mtu: u16::from_be_bytes([input[6], input[7]]),
        };
        payload.validate()?;
        Ok((payload, &input[8..]))
    }
}

bitflags! {
    /// Flags inside the SYNEX payload.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SynExFlags: u16 {
        const VERSION_INFO_VALID = 0x0001;
    }
}

bitflags! {
    /// Version advertisement bits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UdpVersionFlags: u16 {
        const VERSION_1 = 0x0001;
        const VERSION_2 = 0x0002;
        const VERSION_3 = 0x0101;
    }
}

/// SYNEX payload (RDPUDP_SYNDATAEX_PAYLOAD).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynDataExPayload {
    pub flags: SynExFlags,
    pub udp_version: Option<UdpVersionFlags>,
    pub cookie_hash: Option<[u8; 32]>,
}

impl SynDataExPayload {
    pub fn validate(&self) -> Result<()> {
        if self.flags.contains(SynExFlags::VERSION_INFO_VALID) && self.udp_version.is_none() {
            return Err(UdpError::InvalidField("syn_ex.udp_version"));
        }
        if let Some(version) = self.udp_version {
            let bits = version.bits();
            let requires_cookie = bits == UdpVersionFlags::VERSION_3.bits();
            let is_known = bits == UdpVersionFlags::VERSION_1.bits()
                || bits == UdpVersionFlags::VERSION_2.bits()
                || bits == UdpVersionFlags::VERSION_3.bits();
            if !is_known {
                return Err(UdpError::InvalidField("syn_ex.udp_version_value"));
            }
            // The cookie hash is only mandatory in a client SYN ([MS-RDPEUDP] 2.2.2.9); a
            // server's SYN+ACK advertising version 3 never carries one, so its absence must not
            // make an otherwise valid payload unparseable.
            let _ = requires_cookie;
        }
        Ok(())
    }
}

impl EncodeInto for SynDataExPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.validate().expect("validated before encode");
        out.extend_from_slice(&self.flags.bits().to_be_bytes());
        let version_bits = self
            .udp_version
            .unwrap_or_else(|| UdpVersionFlags::empty())
            .bits();
        out.extend_from_slice(&version_bits.to_be_bytes());
        if let Some(cookie) = &self.cookie_hash {
            out.extend_from_slice(cookie);
        }
    }
}

impl<'a> DecodeFrom<'a> for SynDataExPayload {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 4)?;
        let flag_bits = u16::from_be_bytes([input[0], input[1]]);
        let version_bits = u16::from_be_bytes([input[2], input[3]]);
        let flags = SynExFlags::from_bits(flag_bits).ok_or(UdpError::InvalidFlags(flag_bits))?;
        let udp_version = UdpVersionFlags::from_bits(version_bits);
        let mut rest = &input[4..];
        // Only a SYN carries the cookie hash. A SYN+ACK advertising version 3 does not, and it
        // may or may not be zero-padded, so the field is read only when the bytes are actually
        // there instead of failing the whole decode.
        let cookie_hash = if flags.contains(SynExFlags::VERSION_INFO_VALID)
            && udp_version.is_some_and(|v| v.contains(UdpVersionFlags::VERSION_3))
            && rest.len() >= 32
        {
            let mut cookie = [0u8; 32];
            cookie.copy_from_slice(&rest[..32]);
            rest = &rest[32..];
            Some(cookie)
        } else {
            None
        };
        let payload = Self {
            flags,
            udp_version,
            cookie_hash,
        };
        payload.validate()?;
        Ok((payload, rest))
    }
}

/// Source payload header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePayloadHeader {
    pub sn_coded: u32,
    pub sn_source_start: u32,
}

impl EncodeInto for SourcePayloadHeader {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sn_coded.to_be_bytes());
        out.extend_from_slice(&self.sn_source_start.to_be_bytes());
    }
}

impl<'a> DecodeFrom<'a> for SourcePayloadHeader {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 8)?;
        Ok((
            Self {
                sn_coded: u32::from_be_bytes([input[0], input[1], input[2], input[3]]),
                sn_source_start: u32::from_be_bytes([input[4], input[5], input[6], input[7]]),
            },
            &input[8..],
        ))
    }
}

/// Source payload block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePayload {
    pub header: SourcePayloadHeader,
    pub data: Vec<u8>,
}

impl SourcePayload {
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        self.header.encode_into(out);
        out.extend_from_slice(&self.data);
    }

    pub fn decode(input: &[u8]) -> Result<(Self, &[u8])> {
        let (header, rest) = SourcePayloadHeader::decode_from(input)?;
        Ok((
            Self {
                header,
                data: rest.to_vec(),
            },
            &rest[rest.len()..],
        ))
    }
}

/// FEC payload header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecPayloadHeader {
    pub sn_coded: u32,
    pub sn_source_start: u32,
    pub range: u8,
    pub fec_index: u8,
}

impl EncodeInto for FecPayloadHeader {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sn_coded.to_be_bytes());
        out.extend_from_slice(&self.sn_source_start.to_be_bytes());
        out.push(self.range);
        out.push(self.fec_index);
        out.extend_from_slice(&[0u8; 2]);
    }
}

impl<'a> DecodeFrom<'a> for FecPayloadHeader {
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])> {
        ensure_min_length(input, 12)?;
        let header = Self {
            sn_coded: u32::from_be_bytes([input[0], input[1], input[2], input[3]]),
            sn_source_start: u32::from_be_bytes([input[4], input[5], input[6], input[7]]),
            range: input[8],
            fec_index: input[9],
        };
        // padding bytes MUST be ignored but zero by spec
        Ok((header, &input[12..]))
    }
}

/// FEC payload block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FecPayload {
    pub header: FecPayloadHeader,
    pub data: Vec<u8>,
}

impl FecPayload {
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        self.header.encode_into(out);
        out.extend_from_slice(&self.data);
    }

    pub fn decode(input: &[u8]) -> Result<(Self, &[u8])> {
        let (header, rest) = FecPayloadHeader::decode_from(input)?;
        Ok((
            Self {
                header,
                data: rest.to_vec(),
            },
            &rest[rest.len()..],
        ))
    }
}

/// Representation of a full RDPUDP datagram.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    pub header: RdpUdpFecHeader,
    pub ack: Option<AckSection>,
    pub ack_of_acks: Option<AckOfAcks>,
    pub correlation_id: Option<CorrelationIdPayload>,
    pub syn_data: Option<SynDataPayload>,
    pub syn_data_ex: Option<SynDataExPayload>,
    pub source_payload: Option<SourcePayload>,
    pub fec_payload: Option<FecPayload>,
}

impl Packet {
    fn validate(&self) -> Result<()> {
        self.header.validate()?;
        // ACK vector is only present when BOTH ACK and DATA flags are set
        let should_have_ack = self.header.flags.contains(HeaderFlags::ACK)
            && self.header.flags.contains(HeaderFlags::DATA);
        if self.ack.is_some() != should_have_ack {
            return Err(UdpError::InvalidField("ack.flag_mismatch"));
        }
        if self.ack_of_acks.is_some() != self.header.flags.contains(HeaderFlags::ACK_OF_ACKS) {
            return Err(UdpError::InvalidField("ack_of_acks.flag_mismatch"));
        }
        if self.correlation_id.is_some() != self.header.flags.contains(HeaderFlags::CORRELATION_ID)
        {
            return Err(UdpError::InvalidField("correlation_id.flag_mismatch"));
        }
        if self.syn_data.is_some() != self.header.flags.contains(HeaderFlags::SYN) {
            return Err(UdpError::InvalidField("syn.flag_mismatch"));
        }
        if self.syn_data_ex.is_some() != self.header.flags.contains(HeaderFlags::SYN_EX) {
            return Err(UdpError::InvalidField("syn_ex.flag_mismatch"));
        }
        let has_data = self.source_payload.is_some() || self.fec_payload.is_some();
        if has_data != self.header.flags.contains(HeaderFlags::DATA) {
            return Err(UdpError::InvalidField("data.flag_mismatch"));
        }
        if self.source_payload.is_some() && self.fec_payload.is_some() {
            return Err(UdpError::InvalidField("data.source_and_fec"));
        }
        if self.fec_payload.is_some() && !self.header.flags.contains(HeaderFlags::FEC) {
            return Err(UdpError::InvalidField("fec.flag_mismatch"));
        }
        if self.source_payload.is_some() && self.header.flags.contains(HeaderFlags::FEC) {
            return Err(UdpError::InvalidField("fec.unexpected_flag"));
        }
        if self.fec_payload.is_none() && self.header.flags.contains(HeaderFlags::FEC) {
            return Err(UdpError::InvalidField("fec.flag_without_payload"));
        }
        Ok(())
    }

    /// Serialises the packet into a byte vector.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::new();
        self.header.encode_into(&mut out);
        if let Some(ack) = &self.ack {
            ack.encode_into(&mut out)?;
        }
        if let Some(ack_of_acks) = self.ack_of_acks {
            ack_of_acks.encode_into(&mut out);
        }
        if let Some(syn_data) = &self.syn_data {
            syn_data.encode_into(&mut out);
        }
        if let Some(correlation_id) = &self.correlation_id {
            correlation_id.encode_into(&mut out);
        }
        if let Some(syn_data_ex) = &self.syn_data_ex {
            syn_data_ex.encode_into(&mut out);
        }
        if let Some(source_payload) = &self.source_payload {
            source_payload.encode_into(&mut out);
        }
        if let Some(fec_payload) = &self.fec_payload {
            fec_payload.encode_into(&mut out);
        }

        // Per MS-RDPEUDP section 3.1.5.1.1: SYN datagrams MUST be zero-padded
        // to uUpStreamMtu or uDownStreamMtu, whichever is smaller.
        if self.header.flags.contains(HeaderFlags::SYN) {
            if let Some(syn_data) = &self.syn_data {
                let target_mtu = syn_data.upstream_mtu.min(syn_data.downstream_mtu) as usize;
                if out.len() < target_mtu {
                    out.resize(target_mtu, 0);
                }
            }
        }

        Ok(out)
    }

    /// Parses a packet from raw bytes.
    pub fn decode(input: &[u8]) -> Result<Self> {
        let (header, mut rest) = RdpUdpFecHeader::decode_from(input)?;
        eprintln!(
            "🔍 Packet decode: flags={:?}, rest_len={}",
            header.flags,
            rest.len()
        );

        // Per MS-RDPEUDP, the ACK vector section is only present when ACK flag is set
        // AND the packet is a DATA packet (not just SYN+ACK which has ACK flag but no vector)
        let ack = if header.flags.contains(HeaderFlags::ACK)
            && header.flags.contains(HeaderFlags::DATA)
        {
            eprintln!("   Parsing ACK vector...");
            let (ack, tail) = AckSection::decode(rest)?;
            rest = tail;
            Some(ack)
        } else {
            eprintln!("   Skipping ACK vector (no DATA flag or no ACK flag)");
            None
        };
        let ack_of_acks = if header.flags.contains(HeaderFlags::ACK_OF_ACKS) {
            eprintln!("   Parsing ACK_OF_ACKS...");
            let (aoa, tail) = AckOfAcks::decode_from(rest)?;
            rest = tail;
            Some(aoa)
        } else {
            eprintln!("   Skipping ACK_OF_ACKS");
            None
        };
        let syn_data = if header.flags.contains(HeaderFlags::SYN) {
            eprintln!("   Parsing SYN data (rest_len={})...", rest.len());
            let (syn, tail) = SynDataPayload::decode_from(rest)?;
            eprintln!("   SYN data parsed: {:?}, tail_len={}", syn, tail.len());
            rest = tail;
            Some(syn)
        } else {
            eprintln!("   Skipping SYN data");
            None
        };
        let correlation_id = if header.flags.contains(HeaderFlags::CORRELATION_ID) {
            eprintln!("   Parsing CORRELATION_ID...");
            let (cid, tail) = CorrelationIdPayload::decode_from(rest)?;
            rest = tail;
            Some(cid)
        } else {
            eprintln!("   Skipping CORRELATION_ID");
            None
        };
        let syn_data_ex = if header.flags.contains(HeaderFlags::SYN_EX) {
            eprintln!("   Parsing SYN_EX (rest_len={})...", rest.len());
            let (syn_ex, tail) = SynDataExPayload::decode_from(rest)?;
            eprintln!("   SYN_EX parsed successfully, tail_len={}", tail.len());
            rest = tail;
            Some(syn_ex)
        } else {
            eprintln!("   Skipping SYN_EX");
            None
        };
        let (source_payload, fec_payload) = if header.flags.contains(HeaderFlags::DATA) {
            if header.flags.contains(HeaderFlags::FEC) {
                let (fec, tail) = FecPayload::decode(rest)?;
                rest = tail;
                (None, Some(fec))
            } else {
                let (source, tail) = SourcePayload::decode(rest)?;
                rest = tail;
                (Some(source), None)
            }
        } else {
            (None, None)
        };
        // Per MS-RDPEUDP section 3.1.5.1.1: SYN datagrams (including SYN+ACK) are zero-padded
        // to the MTU size, so we should allow trailing bytes for SYN packets
        if !rest.is_empty() && !header.flags.contains(HeaderFlags::SYN) {
            return Err(UdpError::InvalidField("packet.trailing_bytes"));
        }
        let packet = Self {
            header,
            ack,
            ack_of_acks,
            correlation_id,
            syn_data,
            syn_data_ex,
            source_payload,
            fec_payload,
        };
        packet.validate()?;
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::fec::{encode_block, recover_single, SourceBlock};
    use super::*;
    use crate::rdpudp_v1_packet;

    #[test]
    fn syn_packet_round_trip() {
        let mut packet = Packet::default();
        packet.header = RdpUdpFecHeader::syn(1200, false);
        packet.syn_data = Some(SynDataPayload {
            initial_sequence_number: 0x1122_3344,
            upstream_mtu: 1200,
            downstream_mtu: 1200,
        });
        let encoded = packet.encode().expect("encode");
        let decoded = Packet::decode(&encoded).expect("decode");
        assert_eq!(decoded.syn_data.unwrap(), packet.syn_data.unwrap());
        assert_eq!(decoded.header.flags, packet.header.flags);
    }

    #[test]
    fn ack_with_source_payload_round_trip() {
        let header = RdpUdpFecHeader {
            sn_source_ack: 42,
            receive_window_size: 64,
            flags: HeaderFlags::ACK | HeaderFlags::DATA,
        };
        let ack_vector = AckVector {
            elements: vec![
                AckVectorElement::new(VectorElementState::DatagramReceived, 4)
                    .expect("valid element"),
            ],
        };
        let source_payload = SourcePayload {
            header: SourcePayloadHeader {
                sn_coded: 100,
                sn_source_start: 100,
            },
            data: b"payload-data".to_vec(),
        };
        let packet = rdpudp_v1_packet!(
            header: header,
            ack: Some(AckSection {
                ack_vector: ack_vector.clone(),
            }),
            source_payload: Some(source_payload.clone())
        );
        let encoded = packet.encode().expect("encode");
        let decoded = Packet::decode(&encoded).expect("decode");
        assert_eq!(
            decoded.ack.unwrap().ack_vector.elements,
            ack_vector.elements
        );
        assert_eq!(decoded.source_payload.unwrap().data, source_payload.data);
    }

    #[test]
    fn fec_single_loss_recovery() {
        let sources = vec![
            SourceBlock {
                sequence_number: 100,
                payload: b"alpha",
            },
            SourceBlock {
                sequence_number: 101,
                payload: b"bravo",
            },
            SourceBlock {
                sequence_number: 102,
                payload: b"charlie",
            },
        ];

        let mut fec_index = 0x10;
        let encoded = encode_block(&sources, &mut fec_index).expect("encode fec");
        assert_eq!(encoded.range, 2);

        let inputs: Vec<Option<&[u8]>> =
            vec![Some(sources[0].payload), None, Some(sources[2].payload)];
        let recovered = recover_single(
            encoded.fec_index,
            encoded.start_sequence,
            &inputs,
            &encoded.payload,
        )
        .expect("recover");
        assert_eq!(recovered, b"bravo");
    }
}
