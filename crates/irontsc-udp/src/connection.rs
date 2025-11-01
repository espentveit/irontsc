use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::error::{Result, UdpError};
use crate::v1::fec::{self, SourceBlock as FecSourceBlock};
use crate::v1::{
    AckSection, AckVector, AckVectorElement, CorrelationIdPayload, FecPayload, FecPayloadHeader,
    HeaderFlags as V1HeaderFlags, Packet as V1Packet, RdpUdpFecHeader, SourcePayload,
    SourcePayloadHeader, SynDataExPayload, SynDataPayload, TransportMode, UdpVersionFlags,
    VectorElementState,
};
use crate::v2::{
    AckPayload as V2AckPayload, DataBodyPayload, DataHeaderPayload, HeaderFlags as V2HeaderFlags,
    Packet as V2Packet, PacketHeader as V2PacketHeader, PacketPrefixByte,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Idle,
    SynSent,
    Connected,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CorrelationId {
    value: [u8; 16],
}

impl CorrelationId {
    pub const fn new(value: [u8; 16]) -> Self {
        Self { value }
    }

    pub(crate) fn payload(&self) -> CorrelationIdPayload {
        CorrelationIdPayload {
            correlation_id: self.value,
        }
    }

    pub fn value(&self) -> &[u8; 16] {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UdpProtocolVersion {
    V1,
    V2,
    V3,
}

impl UdpProtocolVersion {
    pub fn min_retransmit_timeout_ms(self) -> u32 {
        match self {
            Self::V1 => 500,
            Self::V2 | Self::V3 => 300,
        }
    }

    fn to_version_flags(self) -> UdpVersionFlags {
        let mut flags = UdpVersionFlags::VERSION_1;
        if self >= Self::V2 {
            flags |= UdpVersionFlags::VERSION_2;
        }
        if self >= Self::V3 {
            flags |= UdpVersionFlags::VERSION_3;
        }
        flags
    }

    fn to_synex_version(self) -> UdpVersionFlags {
        self.to_version_flags()
    }
}

#[derive(Debug, Clone)]
pub struct UdpConfig {
    pub mtu: u16,
    pub initial_sequence_number: u32,
    pub receive_window_size: u16,
    pub mode: TransportMode,
    pub protocol_version: UdpProtocolVersion,
    pub enable_fec: bool,
    pub fec_block_size: u8,
    pub retransmit_timeout_ms: u32,
    pub max_retransmits: u8,
    pub keepalive_interval_ms: u32,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            mtu: 1232,
            initial_sequence_number: 0,
            receive_window_size: 256,
            mode: TransportMode::Reliable,
            protocol_version: UdpProtocolVersion::V2,
            enable_fec: true,
            fec_block_size: 8,
            retransmit_timeout_ms: 300,
            max_retransmits: 5,
            keepalive_interval_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone)]
struct PendingPacket {
    data: Vec<u8>,
    last_sent: Instant,
    retransmit_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PendingKey {
    V1(u32),
    V3(u16),
}

#[derive(Debug, Clone)]
struct SourceRecord {
    sequence: u32,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum PendingAck {
    V1 { run_length: u8 },
    V3 { sequence: u16 },
}

pub struct UdpConnection {
    config: UdpConfig,
    state: ConnectionState,
    correlation_id: Option<CorrelationId>,
    cookie_hash: Option<[u8; 32]>,
    negotiated_version: UdpProtocolVersion,

    // Sequence state for UDP v1/v2
    next_source_sequence: u32,
    next_coded_sequence: u32,
    expected_remote_sequence: u32,
    remote_acked_sequence: u32,

    // Sequence state for UDP v2 (version 3)
    v3_next_data_sequence: u16,
    v3_next_channel_sequence: u16,
    v3_expected_sequence: u16,

    pending_packets: HashMap<PendingKey, PendingPacket>,
    receive_buffer: HashMap<u32, Vec<u8>>,
    v3_receive_buffer: HashMap<u16, Vec<u8>>,
    source_block: Vec<SourceRecord>,
    fec_index: u8,

    pending_ack: Option<PendingAck>,

    last_activity: Instant,
    last_keepalive: Instant,
}

impl UdpConnection {
    pub fn new(config: UdpConfig) -> Self {
        let now = Instant::now();
        Self {
            negotiated_version: config.protocol_version,
            config,
            state: ConnectionState::Idle,
            correlation_id: None,
            cookie_hash: None,
            next_source_sequence: 0,
            next_coded_sequence: 0,
            expected_remote_sequence: 0,
            remote_acked_sequence: 0,
            v3_next_data_sequence: 0,
            v3_next_channel_sequence: 0,
            v3_expected_sequence: 0,
            pending_packets: HashMap::new(),
            receive_buffer: HashMap::new(),
            v3_receive_buffer: HashMap::new(),
            source_block: Vec::new(),
            fec_index: 0,
            pending_ack: None,
            last_activity: now,
            last_keepalive: now,
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state
    }

    pub fn set_correlation_id(&mut self, correlation_id: CorrelationId) {
        self.correlation_id = Some(correlation_id);
    }

    pub fn set_protocol_version(&mut self, version: UdpProtocolVersion) {
        self.negotiated_version = version;
        self.config.protocol_version = version;
        self.config.retransmit_timeout_ms = version.min_retransmit_timeout_ms();
    }

    pub fn set_cookie_hash(&mut self, cookie_hash: [u8; 32]) {
        self.cookie_hash = Some(cookie_hash);
    }

    pub fn protocol_version(&self) -> UdpProtocolVersion {
        self.negotiated_version
    }

    pub fn retransmit_timeout_ms(&self) -> u32 {
        self.config.retransmit_timeout_ms
    }

    pub fn create_syn(&mut self) -> Result<Vec<u8>> {
        if self.state != ConnectionState::Idle {
            return Err(UdpError::Protocol("connection not idle"));
        }

        if self.negotiated_version == UdpProtocolVersion::V3 && self.cookie_hash.is_none() {
            // Cannot negotiate UDPv3 without cookie hash; fall back to v2.
            self.negotiated_version = UdpProtocolVersion::V2;
            self.config.protocol_version = UdpProtocolVersion::V2;
        }

        self.next_source_sequence = self.config.initial_sequence_number;
        self.next_coded_sequence = self.config.initial_sequence_number;

        let syn_data = SynDataPayload {
            initial_sequence_number: self.next_source_sequence,
            upstream_mtu: self.config.mtu,
            downstream_mtu: self.config.mtu,
        };

        let mut packet = V1Packet::default();
        packet.header = RdpUdpFecHeader::syn(
            self.config.receive_window_size,
            self.config.mode == TransportMode::Lossy,
        );
        packet.syn_data = Some(syn_data);

        if let Some(correlation) = self.correlation_id {
            packet.correlation_id = Some(correlation.payload());
            packet.header.flags |= V1HeaderFlags::CORRELATION_ID;
        }

        if self.negotiated_version != UdpProtocolVersion::V1 {
            let mut syn_ex = SynDataExPayload {
                flags: crate::v1::SynExFlags::VERSION_INFO_VALID,
                udp_version: Some(self.negotiated_version.to_synex_version()),
                cookie_hash: None,
            };
            if self.negotiated_version == UdpProtocolVersion::V3 {
                if let Some(cookie) = self.cookie_hash {
                    syn_ex.cookie_hash = Some(cookie);
                }
            }
            packet.syn_data_ex = Some(syn_ex);
            packet.header.flags |= V1HeaderFlags::SYN_EX;
        }

        packet.header.flags |= if self.config.mode == TransportMode::Lossy {
            V1HeaderFlags::SYN_LOSSY
        } else {
            V1HeaderFlags::empty()
        };

        self.state = ConnectionState::SynSent;
        packet.encode()
    }

    pub fn process_syn_ack(&mut self, bytes: &[u8]) -> Result<()> {
        if self.state != ConnectionState::SynSent {
            return Err(UdpError::Protocol("unexpected SYN+ACK"));
        }

        let packet = V1Packet::decode(bytes)?;
        if !packet.header.flags.contains(V1HeaderFlags::SYN) {
            return Err(UdpError::Protocol("SYN+ACK missing SYN flag"));
        }

        let syn = packet
            .syn_data
            .ok_or(UdpError::Protocol("SYN+ACK missing syndata"))?;

        self.expected_remote_sequence = syn.initial_sequence_number;
        self.remote_acked_sequence = syn.initial_sequence_number;

        match self.negotiated_version {
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => {
                self.pending_ack = Some(PendingAck::V1 { run_length: 1 });
            }
            UdpProtocolVersion::V3 => {
                let seq16 = syn.initial_sequence_number as u16;
                self.v3_expected_sequence = seq16;
                self.v3_next_data_sequence = seq16.wrapping_add(1);
                self.v3_next_channel_sequence = seq16.wrapping_add(1);
                self.pending_ack = Some(PendingAck::V3 { sequence: seq16 });
            }
        }

        if let Some(syn_ex) = packet.syn_data_ex {
            if let Some(remote_versions) = syn_ex.udp_version {
                if remote_versions.contains(UdpVersionFlags::VERSION_3)
                    && self.negotiated_version == UdpProtocolVersion::V3
                    && syn_ex.cookie_hash.is_none()
                {
                    // Remote downgraded us
                    self.negotiated_version = UdpProtocolVersion::V2;
                } else if remote_versions.contains(UdpVersionFlags::VERSION_2)
                    && self.negotiated_version == UdpProtocolVersion::V3
                    && !remote_versions.contains(UdpVersionFlags::VERSION_3)
                {
                    self.negotiated_version = UdpProtocolVersion::V2;
                } else if !remote_versions.contains(UdpVersionFlags::VERSION_2) {
                    self.negotiated_version = UdpProtocolVersion::V1;
                }
            }
        }

        self.config.retransmit_timeout_ms = self.negotiated_version.min_retransmit_timeout_ms();
        self.state = ConnectionState::Connected;
        self.last_activity = Instant::now();
        self.last_keepalive = self.last_activity;
        Ok(())
    }

    pub fn send_data(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
        if self.state != ConnectionState::Connected {
            return Err(UdpError::Protocol("connection not established"));
        }

        self.last_activity = Instant::now();
        self.last_keepalive = self.last_activity;

        match self.negotiated_version {
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => self.send_data_v1(data),
            UdpProtocolVersion::V3 => self.send_data_v3(data),
        }
    }

    pub fn process_source_packet(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        if self.state != ConnectionState::Connected {
            return Ok(Vec::new());
        }

        self.last_activity = Instant::now();

        match self.negotiated_version {
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => self.process_v1_source(bytes),
            UdpProtocolVersion::V3 => self.process_v3_source(bytes),
        }
    }

    pub fn process_ack_packet(&mut self, bytes: &[u8]) -> Result<()> {
        if self.state != ConnectionState::Connected {
            return Ok(());
        }

        self.last_activity = Instant::now();

        match self.negotiated_version {
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => {
                let packet = V1Packet::decode(bytes)?;
                self.update_remote_ack(packet.header.sn_source_ack);
                Ok(())
            }
            UdpProtocolVersion::V3 => {
                let packet = V2Packet::decode_on_wire(bytes)?;
                if let Some(ack) = packet.ack {
                    self.update_remote_ack_v3(ack.sequence_number);
                }
                Ok(())
            }
        }
    }

    pub fn check_retransmits(&mut self) -> Vec<Vec<u8>> {
        if self.config.mode != TransportMode::Reliable {
            return Vec::new();
        }

        let mut retransmits = Vec::new();
        let now = Instant::now();
        let timeout = Duration::from_millis(self.config.retransmit_timeout_ms as u64);

        for pending in self.pending_packets.values_mut() {
            if now.duration_since(pending.last_sent) >= timeout {
                if pending.retransmit_count < self.config.max_retransmits {
                    pending.retransmit_count += 1;
                    pending.last_sent = now;
                    retransmits.push(pending.data.clone());
                } else {
                    self.state = ConnectionState::Terminated;
                }
            }
        }

        retransmits
    }

    pub fn check_fec_block(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.config.enable_fec {
            return Ok(None);
        }
        if self.negotiated_version == UdpProtocolVersion::V3 {
            return Ok(None);
        }
        if self.source_block.len() < self.config.fec_block_size as usize {
            return Ok(None);
        }

        let mut fec_index = self.fec_index;
        let blocks: Vec<FecSourceBlock<'_>> = self
            .source_block
            .iter()
            .map(|record| FecSourceBlock {
                sequence_number: record.sequence,
                payload: &record.payload,
            })
            .collect();

        let encoded = fec::encode_block(&blocks, &mut fec_index)?;
        self.fec_index = fec_index;

        let sn_coded = self.next_coded_sequence;
        self.next_coded_sequence = self.next_coded_sequence.wrapping_add(1);

        let fec_payload = FecPayload {
            header: FecPayloadHeader {
                sn_coded,
                sn_source_start: encoded.start_sequence,
                range: encoded.range,
                fec_index: encoded.fec_index,
            },
            data: encoded.payload,
        };

        let mut packet = V1Packet::default();
        packet.header = self.base_header(V1HeaderFlags::DATA | V1HeaderFlags::FEC);
        packet.fec_payload = Some(fec_payload);

        if let Some(PendingAck::V1 { run_length }) = self.pending_ack.take() {
            packet.header.flags |= V1HeaderFlags::ACK;
            packet.ack = Some(self.build_ack_section(run_length)?);
        }

        self.source_block.clear();
        Ok(Some(packet.encode()?))
    }

    pub fn needs_keepalive(&self) -> bool {
        Instant::now().duration_since(self.last_keepalive)
            >= Duration::from_millis(self.config.keepalive_interval_ms as u64)
    }

    pub fn create_ack(&mut self) -> Result<Vec<u8>> {
        self.last_keepalive = Instant::now();
        match self.negotiated_version {
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => {
                let ack_section = self.build_ack_section(1)?;
                let mut packet = V1Packet::default();
                packet.header = self.base_header(V1HeaderFlags::ACK);
                packet.ack = Some(ack_section);
                packet.encode()
            }
            UdpProtocolVersion::V3 => {
                let header = V2PacketHeader::new(V2HeaderFlags::ACK, 0)?;
                let ack_payload = V2AckPayload {
                    sequence_number: self.v3_expected_sequence.wrapping_sub(1),
                    received_timestamp: 0,
                    send_ack_time_gap_ms: 0,
                    num_delayed_acks: 0,
                    delay_ack_time_scale: 0,
                    delay_ack_time_additions: Vec::new(),
                };
                let packet = V2Packet {
                    header,
                    ack: Some(ack_payload),
                    overhead_size: None,
                    delay_ack_info: None,
                    ack_of_acks: None,
                    ack_vector: None,
                    data_header: None,
                    data_body: None,
                };
                packet.encode_on_wire(PacketPrefixByte::TYPE_STANDARD)
            }
        }
    }

    fn send_data_v1(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
        let sn_coded = self.next_coded_sequence;
        let sn_source = self.next_source_sequence;
        self.next_coded_sequence = self.next_coded_sequence.wrapping_add(1);
        self.next_source_sequence = self.next_source_sequence.wrapping_add(1);

        let mut packet = V1Packet::default();
        packet.header = self.base_header(V1HeaderFlags::DATA);

        if let Some(PendingAck::V1 { run_length }) = self.pending_ack.take() {
            packet.header.flags |= V1HeaderFlags::ACK;
            packet.ack = Some(self.build_ack_section(run_length)?);
        }

        packet.source_payload = Some(SourcePayload {
            header: SourcePayloadHeader {
                sn_coded,
                sn_source_start: sn_source,
            },
            data: data.clone(),
        });

        let encoded = packet.encode()?;
        self.track_pending(PendingKey::V1(sn_coded), encoded.clone());
        if self.config.enable_fec {
            self.source_block.push(SourceRecord {
                sequence: sn_source,
                payload: data,
            });
        }
        Ok(encoded)
    }

    fn send_data_v3(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
        let data_sequence = self.v3_next_data_sequence;
        let channel_sequence = self.v3_next_channel_sequence;
        self.v3_next_data_sequence = self.v3_next_data_sequence.wrapping_add(1);
        self.v3_next_channel_sequence = self.v3_next_channel_sequence.wrapping_add(1);

        let mut flags = V2HeaderFlags::DATA;
        let mut ack_payload = None;
        if let Some(PendingAck::V3 { sequence }) = self.pending_ack.take() {
            flags |= V2HeaderFlags::ACK;
            ack_payload = Some(V2AckPayload {
                sequence_number: sequence,
                received_timestamp: 0,
                send_ack_time_gap_ms: 0,
                num_delayed_acks: 0,
                delay_ack_time_scale: 0,
                delay_ack_time_additions: Vec::new(),
            });
        }

        let header = V2PacketHeader::new(flags, 0)?;
        let packet = V2Packet {
            header,
            ack: ack_payload,
            overhead_size: None,
            delay_ack_info: None,
            ack_of_acks: None,
            ack_vector: None,
            data_header: Some(DataHeaderPayload {
                data_sequence_number: data_sequence,
            }),
            data_body: Some(DataBodyPayload {
                channel_sequence_number: channel_sequence,
                data: data.clone(),
            }),
        };

        let encoded = packet.encode_on_wire(PacketPrefixByte::TYPE_STANDARD)?;
        self.track_pending(PendingKey::V3(data_sequence), encoded.clone());
        Ok(encoded)
    }

    fn process_v1_source(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let packet = V1Packet::decode(bytes)?;
        self.update_remote_ack(packet.header.sn_source_ack);

        let mut delivered = Vec::new();
        if let Some(source) = packet.source_payload {
            let sequence = source.header.sn_source_start;
            self.receive_buffer.insert(sequence, source.data);
            delivered = self.collect_ready_packets();
        }

        if let Some(fec) = packet.fec_payload {
            // Basic implementation does not attempt FEC recovery yet.
            drop(fec);
        }

        Ok(delivered)
    }

    fn process_v3_source(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let packet = V2Packet::decode_on_wire(bytes)?;
        if let Some(ack) = &packet.ack {
            self.update_remote_ack_v3(ack.sequence_number);
        }

        if let Some(body) = packet.data_body {
            let sequence = packet
                .data_header
                .map(|h| h.data_sequence_number)
                .unwrap_or(0);
            self.v3_receive_buffer.insert(sequence, body.data);
            return Ok(self.collect_ready_packets_v3());
        }

        Ok(Vec::new())
    }

    fn collect_ready_packets(&mut self) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let mut current = self.expected_remote_sequence;
        let mut run = 0u8;

        while let Some(data) = self.receive_buffer.remove(&current) {
            result.push(data);
            current = current.wrapping_add(1);
            run = run.saturating_add(1);
            if run == 63 {
                break;
            }
        }

        if run > 0 {
            self.expected_remote_sequence = current;
            self.remote_acked_sequence = current.wrapping_sub(1);
            self.pending_ack = Some(PendingAck::V1 { run_length: run });
        }

        result
    }

    fn collect_ready_packets_v3(&mut self) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let mut current = self.v3_expected_sequence;
        let mut delivered = false;

        while let Some(data) = self.v3_receive_buffer.remove(&current) {
            result.push(data);
            current = current.wrapping_add(1);
            delivered = true;
        }

        if delivered {
            self.v3_expected_sequence = current;
            self.pending_ack = Some(PendingAck::V3 {
                sequence: current.wrapping_sub(1),
            });
        }

        result
    }

    fn build_ack_section(&self, run_length: u8) -> Result<AckSection> {
        let count = run_length.max(1);
        let element = AckVectorElement::new(VectorElementState::DatagramReceived, count)?;
        Ok(AckSection {
            ack_vector: AckVector {
                elements: vec![element],
            },
        })
    }

    fn base_header(&self, flags: V1HeaderFlags) -> RdpUdpFecHeader {
        RdpUdpFecHeader {
            sn_source_ack: self.remote_acked_sequence,
            receive_window_size: self.config.receive_window_size,
            flags,
        }
    }

    fn track_pending(&mut self, key: PendingKey, data: Vec<u8>) {
        if self.config.mode != TransportMode::Reliable {
            return;
        }
        let now = Instant::now();
        self.pending_packets.insert(
            key,
            PendingPacket {
                data,
                last_sent: now,
                retransmit_count: 0,
            },
        );
    }

    fn update_remote_ack(&mut self, ack_sequence: u32) {
        self.remote_acked_sequence = ack_sequence;
        self.pending_packets.retain(|key, _| match key {
            PendingKey::V1(seq) => *seq > ack_sequence,
            PendingKey::V3(_) => true,
        });
    }

    fn update_remote_ack_v3(&mut self, ack_sequence: u16) {
        self.pending_packets.retain(|key, _| match key {
            PendingKey::V3(seq) => *seq > ack_sequence,
            PendingKey::V1(_) => true,
        });
    }
}
