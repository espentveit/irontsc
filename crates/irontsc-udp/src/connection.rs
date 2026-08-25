use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::error::{Result, UdpError};
use crate::rdpudp_v1_packet;
use crate::rdpudp_v1_packet_bytes;
use crate::rdpudp_v1_syn_ex;
use crate::rdpudp_v1_syn_packet_bytes;
use crate::rdpudp_v2_flags;
use crate::rdpudp_v2_packet;
use crate::rdpudp_v2_packet_bytes;
use crate::v1::fec::{self, SourceBlock as FecSourceBlock};
use crate::v1::{
    AckSection, AckVector, AckVectorElement, CorrelationIdPayload, FecPayload, FecPayloadHeader,
    HeaderFlags as V1HeaderFlags, Packet as V1Packet, RdpUdpFecHeader, SourcePayload,
    SourcePayloadHeader, SynDataPayload, TransportMode, UdpVersionFlags, VectorElementState,
};
use crate::v2::{
    AckPayload as V2AckPayload, AckVecEntry, AckVectorPayload, DataBodyPayload, DataHeaderPayload,
    Packet as V2Packet, PacketHeader as V2PacketHeader, TimestampInfo,
};

/// Sequence number both Windows endpoints use to start the RDP-UDP2 data sequence space.
const V3_INITIAL_SEQUENCE: u16 = 100;

/// Number of sequence numbers we are willing to hold while waiting for a gap to be filled.
/// Anything further ahead than this is treated as bogus rather than buffered.
const V3_RECEIVE_WINDOW: u16 = 4096;

/// How long chunks may pile up behind a missing channel sequence before it is reported. The
/// transport keeps acknowledging normally while this happens, so without a warning the stream
/// simply stops and the session looks frozen for no visible reason.
const CHANNEL_STALL_WARN: Duration = Duration::from_millis(1500);

/// How long a stalled stream is given before the transport is written off.
///
/// [MS-RDPEUDP2] 3.1.5.1.3 puts reliability entirely on the sender: a lost packet is "resent
/// with a new sequence number ... the ChannelSeqNum remains the same", and the receiver's only
/// move is to buffer and wait -- there is no way to ask for a channel sequence again. So once
/// the sender has written a packet off (an AckOfAcks moved our window past it) and no
/// retransmission carries its ChannelSeqNum, the stream cannot recover, and waiting longer only
/// keeps a dead session on screen. The transport is dropped instead, and the dynamic channels it
/// carried go back over TCP, which is where they were before Soft-Sync moved them.
const CHANNEL_STALL_FATAL: Duration = Duration::from_secs(4);

/// Largest ACK vector we will build, in bytes (the wire field is 7 bits wide).
const V3_MAX_ACKVEC_BYTES: usize = 0x7f;

/// [MS-RDPEUDP2] 2.2.1.1 LogWindowSize. Windows always advertises the maximum (2^15 * MTU);
/// advertising a small window here throttles the server's sender window for no benefit.
const V3_LOG_WINDOW_SIZE: u8 = 15;

/// Advances a channel sequence number. These are 1-based: the peer wraps 65535 -> 1 and never
/// sends 0, so a plain wrapping add leaves the receiver waiting for a chunk that will never
/// arrive, stalling the stream permanently.
#[inline]
fn next_channel_sequence(sequence: u16) -> u16 {
    match sequence.checked_add(1) {
        Some(next) => next,
        None => 1,
    }
}

/// True when `a` is strictly newer than `b` in a wrapping 16-bit sequence space.
#[inline]
fn seq_gt(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// True when `a` is strictly newer than `b` in a wrapping 32-bit sequence space.
#[inline]
fn seq_gt32(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

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
        match self {
            Self::V1 => UdpVersionFlags::VERSION_1,
            Self::V2 => UdpVersionFlags::VERSION_2,
            Self::V3 => UdpVersionFlags::VERSION_3,
        }
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
            max_retransmits: 60, // Increased to handle slow server responses (can take 3-4 seconds)
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
    /// A v3 acknowledgement is owed; the sequence to report is derived from the receive window
    /// when the packet is actually built.
    V3,
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
    /// Data sequence numbers received ahead of the window base. This drives acknowledgement and
    /// loss reporting only -- it says nothing about where the bytes belong in the stream.
    v3_received_dseq: HashSet<u16>,
    /// Channel stream chunks keyed by channel sequence number, which is the only field that says
    /// where a chunk belongs. A retransmission is sent under a *new* data sequence number
    /// carrying the original channel sequence, so it arrives after the chunks that follow it and
    /// has to be put back in its place before being handed up.
    v3_channel_buffer: HashMap<u16, Vec<u8>>,
    /// Next channel sequence number to hand upwards.
    v3_next_channel_delivery: Option<u16>,
    /// When chunks first started piling up behind a channel sequence that has not arrived.
    v3_channel_stalled_since: Option<Instant>,
    /// Whether the current stall has already been reported, so it is logged once and not once
    /// per packet.
    v3_channel_stall_reported: bool,
    /// False until the first v3 DATA packet tells us where the peer's sequence space starts.
    v3_base_established: bool,
    /// Highest v3 sequence number seen so far, used to size the ACK vector.
    v3_highest_received: u16,
    /// Lowest v3 sequence number of ours that the peer has not acknowledged yet.
    v3_sender_base: u16,
    /// Last AckOfAcks value we put on the wire, so we only repeat it when it changes.
    v3_announced_aoa: Option<u16>,
    /// Highest cumulative acknowledgement received from the peer for our own v3 data.
    v3_remote_acked: Option<u16>,
    source_block: Vec<SourceRecord>,
    fec_index: u8,

    pending_ack: Option<PendingAck>,

    /// Smoothed round trip time, sampled from acknowledgements of our own data packets.
    smoothed_rtt: Option<Duration>,
    /// Reference point for the 24-bit, 4-microsecond ACK timestamps of [MS-RDPEUDP2] 3.1.1.1.4.
    epoch: Instant,
    /// Arrival time of the most recent DATA packet, for SendAckTimeGapInMs.
    last_data_arrival: Option<Instant>,
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
            v3_received_dseq: HashSet::new(),
            v3_channel_buffer: HashMap::new(),
            v3_next_channel_delivery: None,
            v3_channel_stalled_since: None,
            v3_channel_stall_reported: false,
            v3_base_established: false,
            v3_highest_received: 0,
            v3_sender_base: 0,
            v3_announced_aoa: None,
            v3_remote_acked: None,
            source_block: Vec::new(),
            fec_index: 0,
            pending_ack: None,
            smoothed_rtt: None,
            epoch: now,
            last_data_arrival: None,
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

        let correlation_payload = self.correlation_id.map(|id| id.payload());
        let syn_data_ex_payload = if self.negotiated_version != UdpProtocolVersion::V1 {
            let version_flags = self.negotiated_version.to_synex_version();
            let syn_ex = if self.negotiated_version == UdpProtocolVersion::V3 {
                match self.cookie_hash {
                    Some(cookie) => {
                        rdpudp_v1_syn_ex!(udp_version = version_flags, cookie_hash = cookie)
                    }
                    None => rdpudp_v1_syn_ex!(udp_version = version_flags),
                }
            } else {
                rdpudp_v1_syn_ex!(udp_version = version_flags)
            };
            Some(syn_ex)
        } else {
            None
        };

        let mut syn_bytes = rdpudp_v1_syn_packet_bytes!(
            self.config.receive_window_size,
            self.config.mode == TransportMode::Lossy,
            syn_data,
            correlation = correlation_payload,
            syn_ex = syn_data_ex_payload
        )?;

        // [MS-RDPEUDP] 3.1.5.1.1: the SYN datagram MUST be zero-padded up to the smaller of the
        // upstream and downstream MTU, which is how the path MTU gets validated.
        let padded = usize::from(self.config.mtu);
        if syn_bytes.len() < padded {
            syn_bytes.resize(padded, 0);
        }

        self.state = ConnectionState::SynSent;
        Ok(syn_bytes)
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
                debug!(
                    "📊 V1/V2: expected_remote={}, next_source={}, next_coded={}",
                    self.expected_remote_sequence,
                    self.next_source_sequence,
                    self.next_coded_sequence
                );
            }
            UdpProtocolVersion::V3 => {
                // For V3, the 32-bit Initial SequenceNumber in SYN/SYN+ACK is for V1/V2
                // compatibility only. The 16-bit RDP-UDP2 sequence space is separate and is
                // learned from the first DATA packet the peer sends.
                self.v3_expected_sequence = 0;
                self.v3_base_established = false;
                self.v3_received_dseq.clear();
                self.v3_channel_buffer.clear();
                self.v3_next_channel_delivery = None;
                self.v3_channel_stalled_since = None;
                self.v3_channel_stall_reported = false;

                // Windows starts both directions of the RDP-UDP2 sequence space at 100 and
                // announces that base in the AckOfAcks payload of its first DATA packet.
                let our_seq16 = V3_INITIAL_SEQUENCE;
                self.v3_next_data_sequence = our_seq16;
                self.v3_sender_base = our_seq16;
                self.v3_announced_aoa = None;
                self.v3_remote_acked = None;

                // Channel sequence is independent and starts from 1 (matches FreeRDP behavior)
                self.v3_next_channel_sequence = 1;
                // V3 does not send ACK payload in first DATA packet (only AckOfAcks)
                self.pending_ack = None;
                debug!(
                    "📊 V3: Server initial_seq={} (0x{:08X}) [for V1/V2 compat only]",
                    syn.initial_sequence_number, syn.initial_sequence_number
                );
                debug!("📊 V3: Will learn server's DATA sequence from first packet");
                debug!(
                    "📊 V3: Our initial_seq={} (0x{:08X}), lower 16-bit={} (0x{:04X})",
                    self.config.initial_sequence_number,
                    self.config.initial_sequence_number,
                    our_seq16,
                    our_seq16
                );
                debug!("📊 V3: We send to server: next_data_seq={} (0x{:04X}), next_channel_seq={} (0x{:04X})", 
                    self.v3_next_data_sequence, self.v3_next_data_sequence, 
                    self.v3_next_channel_sequence, self.v3_next_channel_sequence);
                if let Some(hash) = &self.cookie_hash {
                    debug!("📊 V3: Cookie hash = {:02X?}", hash);
                }
            }
        }

        if let Some(syn_ex) = packet.syn_data_ex {
            if let Some(remote_flags) = syn_ex.udp_version {
                let remote_version = udp_version_from_flags(remote_flags);
                match remote_version {
                    // The version in the SYN+ACK is the one both endpoints must use
                    // ([MS-RDPEUDP] 3.1.5.1.2). A server that rejected our cookie hash answers
                    // with version 2, so the advertised value is authoritative on its own and
                    // must not be second-guessed by looking for a cookie the server never sends.
                    Some(UdpProtocolVersion::V3) => {
                        self.negotiated_version = self.negotiated_version.min(UdpProtocolVersion::V3);
                    }
                    Some(UdpProtocolVersion::V2) => {
                        self.negotiated_version =
                            self.negotiated_version.min(UdpProtocolVersion::V2);
                    }
                    Some(UdpProtocolVersion::V1) => {
                        self.negotiated_version = UdpProtocolVersion::V1;
                    }
                    None => {
                        // Unknown value: fall back to the most compatible mode.
                        self.negotiated_version = UdpProtocolVersion::V1;
                    }
                }
            } else {
                // Version not advertised, assume V1.
                self.negotiated_version = UdpProtocolVersion::V1;
            }
        }

        self.config.protocol_version = self.negotiated_version;
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
        let mut to_remove = Vec::new();
        let now = Instant::now();
        let timeout = Duration::from_millis(self.config.retransmit_timeout_ms as u64);

        for (key, pending) in self.pending_packets.iter_mut() {
            if now.duration_since(pending.last_sent) >= timeout {
                if pending.retransmit_count < self.config.max_retransmits {
                    pending.retransmit_count += 1;
                    pending.last_sent = now;
                    retransmits.push(pending.data.clone());
                } else {
                    // Don't terminate connection - just stop retransmitting this packet
                    // The higher layer (TLS/DTLS) will handle timeouts if needed
                    to_remove.push(*key);
                }
            }
        }

        // Remove packets that hit max retransmits. Giving up on one moves our sender window
        // forward, which the peer learns from the next AckOfAcks we send.
        for key in to_remove {
            self.pending_packets.remove(&key);
            if let PendingKey::V3(seq) = key {
                let base = seq.wrapping_add(1);
                if seq_gt(base, self.v3_sender_base) {
                    self.v3_sender_base = base;
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

        let ack_section = if let Some(PendingAck::V1 { run_length }) = self.pending_ack.take() {
            Some(self.build_ack_section(run_length)?)
        } else {
            None
        };

        let packet_bytes = rdpudp_v1_packet_bytes!(
            header: self.base_header(V1HeaderFlags::DATA | V1HeaderFlags::FEC),
            fec_payload: Some(fec_payload),
            ack: ack_section
        )?;

        self.source_block.clear();
        Ok(Some(packet_bytes))
    }

    pub fn needs_keepalive(&self) -> bool {
        Instant::now().duration_since(self.last_keepalive)
            >= Duration::from_millis(self.config.keepalive_interval_ms as u64)
    }

    /// Returns true if there's a pending acknowledgement that should be sent
    pub fn has_pending_ack(&self) -> bool {
        self.pending_ack.is_some()
    }

    pub fn create_ack(&mut self) -> Result<Vec<u8>> {
        self.last_keepalive = Instant::now();
        match self.negotiated_version {
            UdpProtocolVersion::V1 | UdpProtocolVersion::V2 => {
                let ack_section = self.build_ack_section(1)?;
                rdpudp_v1_packet_bytes!(
                    header: self.base_header(V1HeaderFlags::ACK),
                    ack: Some(ack_section)
                )
            }
            UdpProtocolVersion::V3 => {
                use crate::rdpudp_v2_overhead;

                self.pending_ack = None;

                // OverheadSize: bytes this layer adds on top of the payload
                // (prefix byte + header + ACK payload).
                let overhead_size = Some(rdpudp_v2_overhead!(10));

                if self.v3_has_gap() {
                    // A cumulative ACK cannot describe a hole, so report the exact receive
                    // window state and let the peer retransmit only what is missing.
                    // ACKVEC and ACK are mutually exclusive ([MS-RDPEUDP2] 2.2.1.1).
                    let vector = self.build_v3_ack_vector();
                    let flags = rdpudp_v2_flags!(ACKVEC | OVERHEADSIZE);
                    let header = V2PacketHeader::new(flags, V3_LOG_WINDOW_SIZE)?;
                    debug!(
                        "📤 V3 ACKVEC: base={} entries={}",
                        vector.base_sequence_number,
                        vector.entries.len()
                    );
                    return rdpudp_v2_packet_bytes!(
                        header = header,
                        overhead_size = overhead_size,
                        ack_vector = Some(vector)
                    );
                }

                let flags = rdpudp_v2_flags!(ACK | OVERHEADSIZE);
                let header = V2PacketHeader::new(flags, V3_LOG_WINDOW_SIZE)?;
                let ack_payload = V2AckPayload {
                    sequence_number: self.v3_expected_sequence.wrapping_sub(1),
                    // A zero timestamp makes the peer's RTT and loss-timeout estimates
                    // meaningless, so a real one is always supplied.
                    received_timestamp: self.v3_timestamp(),
                    send_ack_time_gap_ms: self.v3_send_ack_gap_ms(),
                    num_delayed_acks: 0,
                    delay_ack_time_scale: 0,
                    delay_ack_time_additions: Vec::new(),
                };

                rdpudp_v2_packet_bytes!(
                    header = header,
                    ack = Some(ack_payload),
                    overhead_size = overhead_size
                )
            }
        }
    }

    fn send_data_v1(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
        let sn_coded = self.next_coded_sequence;
        let sn_source = self.next_source_sequence;
        self.next_coded_sequence = self.next_coded_sequence.wrapping_add(1);
        self.next_source_sequence = self.next_source_sequence.wrapping_add(1);

        let ack_section = if let Some(PendingAck::V1 { run_length }) = self.pending_ack.take() {
            Some(self.build_ack_section(run_length)?)
        } else {
            None
        };

        let encoded = rdpudp_v1_packet_bytes!(
            header: self.base_header(V1HeaderFlags::DATA),
            ack: ack_section,
            source_payload: Some(SourcePayload {
                header: SourcePayloadHeader {
                    sn_coded,
                    sn_source_start: sn_source,
                },
                data: data.clone(),
            })
        )?;
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

        debug!(
            "📤 V3 DATA: data_seq={} (0x{:04X}), channel_seq={} (0x{:04X}), payload_len={}",
            data_sequence,
            data_sequence,
            channel_sequence,
            channel_sequence,
            data.len()
        );

        let mut flags = rdpudp_v2_flags!(DATA);
        let mut ack_payload = None;
        if self.pending_ack.take().is_some() {
            flags |= rdpudp_v2_flags!(ACK);
            let sequence = self.v3_expected_sequence.wrapping_sub(1);
            ack_payload = Some(V2AckPayload {
                sequence_number: sequence,
                received_timestamp: self.v3_timestamp(),
                send_ack_time_gap_ms: self.v3_send_ack_gap_ms(),
                num_delayed_acks: 0,
                delay_ack_time_scale: 0,
                delay_ack_time_additions: Vec::new(),
            });
            debug!(
                "📤 V3 DATA: Including ACK payload for seq={} (0x{:04X})",
                sequence, sequence
            );
        } else {
            // Both Windows endpoints set this on every DATA packet that carries no ACK.
            flags |= rdpudp_v2_flags!(NOACK);
        }

        // Per MS-RDPEUDP2, include DelayAckInfo for reliable mode
        // This tells the peer our delayed ACK parameters
        use crate::{rdpudp_v2_ack_of_acks, rdpudp_v2_delay_ack_info};
        let delay_ack_info = if self.config.mode == TransportMode::Reliable {
            Some(rdpudp_v2_delay_ack_info!(1, 20)) // max_delayed_acks=1, timeout=20ms
        } else {
            None
        };

        // AckOfAcks is the *lowest* sequence number of ours still awaiting acknowledgement
        // ([MS-RDPEUDP2] 2.2.1.2.4), which tells the peer where our sender window starts.
        // Sending the sequence being transmitted instead would ask the peer to abandon every
        // packet still in flight. It only needs to go out when the value changes.
        let ack_of_acks = if self.v3_announced_aoa != Some(self.v3_sender_base) {
            self.v3_announced_aoa = Some(self.v3_sender_base);
            Some(rdpudp_v2_ack_of_acks!(self.v3_sender_base))
        } else {
            None
        };

        let header = V2PacketHeader::new(flags, V3_LOG_WINDOW_SIZE)?;
        let encoded = rdpudp_v2_packet_bytes!(
            header = header,
            ack = ack_payload,
            delay_ack_info = delay_ack_info,
            ack_of_acks = ack_of_acks,
            data_header = Some(DataHeaderPayload {
                data_sequence_number: data_sequence,
            }),
            data_body = Some(DataBodyPayload {
                channel_sequence_number: channel_sequence,
                data: data.clone(),
            })
        )?;
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

        // Feedback about our own sender window, regardless of whether this packet carries data.
        if let Some(ack) = &packet.ack {
            self.update_remote_ack_v3(ack.sequence_number);
        }
        if let Some(vector) = &packet.ack_vector {
            self.process_remote_ack_vector(vector);
        }
        // AckOfAcks moves the lower bound of *our* receive window ([MS-RDPEUDP2] 3.1.1.2.2).
        // This is the only way to get past a lost dummy packet, because dummies are never
        // retransmitted -- without honouring it the receive stream stalls forever.
        let mut delivered = Vec::new();
        if let Some(aoa) = &packet.ack_of_acks {
            self.advance_receive_base(aoa.sequence_number);
        }

        // The AckOfAcks above is usually piggybacked on a DATA packet, so this packet's own
        // payload still has to be processed and appended to whatever the base advance released.
        let Some(data_header) = packet.data_header else {
            return Ok(delivered);
        };
        let sequence = data_header.data_sequence_number;
        let is_dummy = packet.data_body.is_none();
        self.last_data_arrival = Some(Instant::now());

        if !self.v3_base_established {
            debug!("📥 V3: first DATA packet, sequence space starts at {sequence}");
            self.v3_base_established = true;
            self.v3_expected_sequence = sequence;
            self.v3_highest_received = sequence.wrapping_sub(1);
        }

        let ahead = sequence.wrapping_sub(self.v3_expected_sequence);
        if ahead >= 0x8000 {
            // Older than our window: the peer retransmitted because it believes our
            // acknowledgement was lost, so we must acknowledge again rather than stay silent.
            debug!(
                "⏭️  V3: re-acknowledging already-delivered packet seq={sequence} (expected={}) \
                 flags=0x{:03x} len={} raw={:02x?}",
                self.v3_expected_sequence,
                packet.header.flags.bits(),
                bytes.len(),
                &bytes[..bytes.len().min(24)]
            );
            self.arm_pending_ack();
            return Ok(delivered);
        }
        if ahead > V3_RECEIVE_WINDOW {
            debug!(
                "⚠️  V3: dropping packet seq={sequence}, {ahead} past expected={}",
                self.v3_expected_sequence
            );
            return Ok(delivered);
        }

        if seq_gt(sequence, self.v3_highest_received) {
            self.v3_highest_received = sequence;
        }

        self.v3_received_dseq.insert(sequence);

        // A dummy packet carries a body that higher layers must ignore. It still occupies a data
        // sequence number and is acknowledged, but it contributes nothing to the stream.
        match packet.data_body {
            Some(body) => {
                debug!(
                    "📥 V3 DATA packet: seq={sequence}, chan_seq={}, data_len={} flags=0x{:03x}",
                    body.channel_sequence_number,
                    body.data.len(),
                    packet.header.flags.bits()
                );
                self.buffer_channel_data(body.channel_sequence_number, body.data);
            }
            None => debug!("📥 V3 DUMMY packet: seq={sequence}"),
        }
        let _ = is_dummy;

        self.arm_pending_ack();
        self.advance_dseq_window();
        delivered.extend(self.take_ready_channel_data());
        self.check_channel_stall();
        Ok(delivered)
    }

    /// Places a chunk in the stream buffer, discarding one that has already been handed up.
    fn buffer_channel_data(&mut self, channel_sequence: u16, data: Vec<u8>) {
        match self.v3_next_channel_delivery {
            None => self.v3_next_channel_delivery = Some(channel_sequence),
            Some(next) if channel_sequence != next && !seq_gt(channel_sequence, next) => {
                debug!(
                    "⏭️  V3: dropping channel sequence {channel_sequence} as already delivered \
                     (awaiting {next}, {} buffered)",
                    self.v3_channel_buffer.len()
                );
                return;
            }
            Some(_) => {}
        }
        self.v3_channel_buffer.insert(channel_sequence, data);
    }

    /// Moves the transport window past every data sequence number that has arrived.
    fn advance_dseq_window(&mut self) {
        while self.v3_received_dseq.remove(&self.v3_expected_sequence) {
            self.v3_expected_sequence = self.v3_expected_sequence.wrapping_add(1);
        }
    }

    /// Hands up stream chunks in channel-sequence order, stopping at the first hole. A hole is
    /// filled by a retransmission arriving later under a different data sequence number.
    fn take_ready_channel_data(&mut self) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let Some(mut next) = self.v3_next_channel_delivery else {
            return result;
        };
        let started_at = next;
        while let Some(data) = self.v3_channel_buffer.remove(&next) {
            if !data.is_empty() {
                result.push(data);
            }
            next = next_channel_sequence(next);
        }
        self.v3_next_channel_delivery = Some(next);

        if next != started_at {
            // The stream moved, so any pile-up has cleared.
            self.v3_channel_stalled_since = None;
            self.v3_channel_stall_reported = false;
        }
        result
    }

    /// Reports, once, that the stream has stopped moving while chunks pile up behind a channel
    /// sequence that never arrives. Everything else stays healthy in that state -- the transport
    /// keeps acknowledging and the peer keeps sending -- so the session just appears to freeze.
    /// True once a stalled stream has been given up on, so the transport can be torn down.
    pub fn is_beyond_recovery(&self) -> bool {
        self.v3_channel_stalled_since
            .is_some_and(|since| Instant::now().duration_since(since) >= CHANNEL_STALL_FATAL)
            && !self.v3_channel_buffer.is_empty()
    }

    fn check_channel_stall(&mut self) {
        if self.v3_channel_buffer.is_empty() {
            self.v3_channel_stalled_since = None;
            self.v3_channel_stall_reported = false;
            return;
        }

        let now = Instant::now();
        let since = *self.v3_channel_stalled_since.get_or_insert(now);
        if self.v3_channel_stall_reported || now.duration_since(since) < CHANNEL_STALL_WARN {
            return;
        }
        self.v3_channel_stall_reported = true;

        let awaited = self.v3_next_channel_delivery;
        // Describe the buffered chunks relative to what is awaited, so the log says how far the
        // stream has run ahead of the hole rather than just printing wrapped raw numbers.
        let (nearest, furthest) = awaited.map_or((None, None), |next| {
            let mut nearest: Option<u16> = None;
            let mut furthest: Option<u16> = None;
            for &cs in self.v3_channel_buffer.keys() {
                let ahead = cs.wrapping_sub(next);
                if nearest.is_none_or(|best| ahead < best.wrapping_sub(next)) {
                    nearest = Some(cs);
                }
                if furthest.is_none_or(|worst| ahead > worst.wrapping_sub(next)) {
                    furthest = Some(cs);
                }
            }
            (nearest, furthest)
        });

        warn!(
            "🧊 V3 stream stalled for {:?}: awaiting channel sequence {:?}, {} chunks buffered \
             (nearest {:?}, furthest {:?}); transport is healthy at dseq {} with {} out of order, \
             so nothing will unstick this on its own -- giving it until {:?} before the \
             transport is dropped and its channels go back over TCP",
            now.duration_since(since),
            awaited,
            self.v3_channel_buffer.len(),
            nearest,
            furthest,
            self.v3_expected_sequence,
            self.v3_received_dseq.len(),
            CHANNEL_STALL_FATAL
        );
    }

    /// Records that an acknowledgement is owed for everything received so far.
    fn arm_pending_ack(&mut self) {
        self.pending_ack = Some(PendingAck::V3);
    }

    /// Moves the lower bound of the receive window forward, abandoning any gap below it.
    /// Returns true when the base actually moved.
    fn advance_receive_base(&mut self, base: u16) -> bool {
        if !self.v3_base_established || !seq_gt(base, self.v3_expected_sequence) {
            return false;
        }
        if base.wrapping_sub(self.v3_expected_sequence) > V3_RECEIVE_WINDOW {
            debug!("⚠️  V3: ignoring implausible AckOfAcks {base}");
            return false;
        }
        debug!(
            "🔀 V3: AckOfAcks moves receive base {} → {base}, abandoning the gap",
            self.v3_expected_sequence
        );
        // This only writes off transport sequence numbers, which the peer has given up on
        // retransmitting under that number. It must never drop stream data: a chunk the peer
        // abandoned here is resent under a later data sequence number carrying its original
        // channel sequence, and the stream buffer puts it back in place when it arrives.
        let mut seq = self.v3_expected_sequence;
        while seq != base {
            self.v3_received_dseq.remove(&seq);
            seq = seq.wrapping_add(1);
        }
        self.v3_expected_sequence = base;
        if seq_gt(base.wrapping_sub(1), self.v3_highest_received) {
            self.v3_highest_received = base.wrapping_sub(1);
        }
        self.advance_dseq_window();
        self.arm_pending_ack();
        true
    }

    /// Applies an ACK vector sent by the peer to our own outstanding packets.
    fn process_remote_ack_vector(&mut self, vector: &AckVectorPayload) {
        let mut sequence = vector.base_sequence_number;
        for entry in &vector.entries {
            match *entry {
                AckVecEntry::StateMap(bits) => {
                    for bit in 0..7u8 {
                        if bits & (1 << bit) != 0 {
                            self.pending_packets.remove(&PendingKey::V3(sequence));
                        }
                        sequence = sequence.wrapping_add(1);
                    }
                }
                AckVecEntry::RunLength { received, length } => {
                    for _ in 0..length {
                        if received {
                            self.pending_packets.remove(&PendingKey::V3(sequence));
                        }
                        sequence = sequence.wrapping_add(1);
                    }
                }
            }
        }
        // Everything below the vector base has already been acknowledged.
        let base = vector.base_sequence_number;
        self.pending_packets
            .retain(|key, _| !matches!(key, PendingKey::V3(seq) if !seq_gt(*seq, base.wrapping_sub(1))));
        if self.v3_sender_base != base && !seq_gt(self.v3_sender_base, base) {
            self.v3_sender_base = base;
        }
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

    /// True when packets have arrived out of order and a gap is still outstanding, in which case
    /// a cumulative ACK cannot describe our state and an ACK vector must be sent instead.
    fn v3_has_gap(&self) -> bool {
        !self.v3_received_dseq.is_empty()
    }

    /// 24-bit timestamp in units of 4 microseconds ([MS-RDPEUDP2] 3.1.1.1.4).
    fn v3_timestamp(&self) -> u32 {
        ((Instant::now().duration_since(self.epoch).as_micros() / 4) as u32) & 0x00ff_ffff
    }

    /// Milliseconds between the arrival of the newest data packet and now, saturating at 254
    /// because 255 means "invalid" on the wire.
    fn v3_send_ack_gap_ms(&self) -> u8 {
        self.last_data_arrival
            .map(|at| {
                Instant::now()
                    .duration_since(at)
                    .as_millis()
                    .min(254) as u8
            })
            .unwrap_or(0)
    }

    /// Builds an ACK vector describing the receive window from the first missing sequence up to
    /// the highest one seen, using state-map entries (bit 0 = base, increasing towards bit 6).
    fn build_v3_ack_vector(&self) -> AckVectorPayload {
        let base = self.v3_expected_sequence;
        let span = self.v3_highest_received.wrapping_sub(base).wrapping_add(1) as usize;
        let bytes = span.div_ceil(7).min(V3_MAX_ACKVEC_BYTES);
        let mut entries = Vec::with_capacity(bytes);
        for chunk in 0..bytes {
            let mut bits = 0u8;
            for bit in 0..7u8 {
                let seq = base.wrapping_add((chunk * 7) as u16 + bit as u16);
                if self.v3_received_dseq.contains(&seq) {
                    bits |= 1 << bit;
                }
            }
            entries.push(AckVecEntry::StateMap(bits));
        }
        AckVectorPayload {
            base_sequence_number: base,
            timestamp_info: Some(TimestampInfo {
                timestamp: self.v3_timestamp(),
                send_ack_time_gap_ms: self.v3_send_ack_gap_ms(),
            }),
            entries,
        }
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

    /// Handles the peer's `snSourceAck`, which reports the newest packet *of ours* it has
    /// received. It must not touch `remote_acked_sequence`, which is what we report about the
    /// peer's packets in our own header.
    fn update_remote_ack(&mut self, ack_sequence: u32) {
        self.pending_packets.retain(|key, _| match key {
            PendingKey::V1(seq) => seq_gt32(*seq, ack_sequence),
            PendingKey::V3(_) => true,
        });
    }

    /// Round trip time as most recently measured, if any has been.
    pub fn smoothed_rtt(&self) -> Option<Duration> {
        self.smoothed_rtt
    }

    /// Takes a round trip sample from an acknowledged packet.
    ///
    /// Retransmitted packets are skipped: there is no way to tell which transmission the
    /// acknowledgement refers to, so sampling them would corrupt the estimate (Karn's
    /// algorithm). Samples are smoothed the usual way, 7/8 old to 1/8 new.
    fn sample_rtt(&mut self, sequence: u16) {
        let Some(pending) = self.pending_packets.get(&PendingKey::V3(sequence)) else {
            return;
        };
        if pending.retransmit_count > 0 {
            return;
        }

        let sample = Instant::now().duration_since(pending.last_sent);
        self.smoothed_rtt = Some(match self.smoothed_rtt {
            Some(previous) => (previous * 7 + sample) / 8,
            None => sample,
        });
        debug!(
            "📶 V3 RTT sample {:.1}ms from seq={sequence}, smoothed {:.1}ms",
            sample.as_secs_f64() * 1000.0,
            self.smoothed_rtt.unwrap_or_default().as_secs_f64() * 1000.0
        );
    }

    fn update_remote_ack_v3(&mut self, ack_sequence: u16) {
        if self
            .v3_remote_acked
            .is_some_and(|previous| !seq_gt(ack_sequence, previous))
        {
            return;
        }
        self.sample_rtt(ack_sequence);
        self.v3_remote_acked = Some(ack_sequence);
        // Comparisons have to wrap: once the 16-bit space rolls over, a plain `>` keeps every
        // outstanding packet forever and turns into a retransmit storm.
        self.pending_packets.retain(|key, _| match key {
            PendingKey::V3(seq) => seq_gt(*seq, ack_sequence),
            PendingKey::V1(_) => true,
        });
        let base = ack_sequence.wrapping_add(1);
        if seq_gt(base, self.v3_sender_base) {
            self.v3_sender_base = base;
        }
    }
}

fn udp_version_from_flags(flags: UdpVersionFlags) -> Option<UdpProtocolVersion> {
    let bits = flags.bits();
    if bits == UdpVersionFlags::VERSION_1.bits() {
        Some(UdpProtocolVersion::V1)
    } else if bits == UdpVersionFlags::VERSION_2.bits() {
        Some(UdpProtocolVersion::V2)
    } else if bits == UdpVersionFlags::VERSION_3.bits() {
        Some(UdpProtocolVersion::V3)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config(protocol_version: UdpProtocolVersion) -> UdpConfig {
        UdpConfig {
            protocol_version,
            ..UdpConfig::default()
        }
    }

    #[test]
    fn syn_advertises_requested_version_with_cookie() {
        let mut conn = UdpConnection::new(default_config(UdpProtocolVersion::V3));
        conn.set_cookie_hash([0xAA; 32]);
        let syn = conn.create_syn().expect("syn encode");
        let packet = V1Packet::decode(&syn).expect("decode syn");
        let syn_ex = packet.syn_data_ex.expect("syn ex present");
        let version = syn_ex.udp_version.expect("version present");
        assert_eq!(version.bits(), UdpVersionFlags::VERSION_3.bits());
    }

    fn connected_v3() -> UdpConnection {
        let mut conn = UdpConnection::new(default_config(UdpProtocolVersion::V3));
        conn.set_cookie_hash([0xAA; 32]);
        conn.create_syn().expect("syn");
        let syn_ack = crate::rdpudp_v1_syn_ack_packet_bytes!(
            0,
            64,
            SynDataPayload {
                initial_sequence_number: 0,
                upstream_mtu: 1232,
                downstream_mtu: 1232,
            },
            syn_ex = Some(rdpudp_v1_syn_ex!(
                udp_version = UdpVersionFlags::VERSION_3,
                cookie_hash = [0u8; 32]
            ))
        )
        .expect("syn ack");
        conn.process_syn_ack(&syn_ack).expect("process syn ack");
        assert_eq!(conn.protocol_version(), UdpProtocolVersion::V3);
        conn
    }

    /// Builds an on-wire v3 DATA packet. `body` of `None` produces a dummy packet, which on the
    /// wire still carries a body that the receiver must ignore. The channel sequence tracks the
    /// data sequence, which is what a first transmission looks like.
    fn v3_data(sequence: u16, body: Option<&[u8]>) -> Vec<u8> {
        v3_data_with_channel(sequence, sequence, body)
    }

    /// Builds a v3 DATA packet with an explicit channel sequence, so a retransmission -- a fresh
    /// data sequence carrying an earlier channel sequence -- can be constructed.
    fn v3_data_with_channel(sequence: u16, channel_sequence: u16, body: Option<&[u8]>) -> Vec<u8> {
        let is_dummy = body.is_none();
        let packet = crate::rdpudp_v2_packet!(
            header = V2PacketHeader::new(rdpudp_v2_flags!(DATA), 15).unwrap(),
            data_header = Some(DataHeaderPayload {
                data_sequence_number: sequence,
            }),
            data_body = Some(DataBodyPayload {
                channel_sequence_number: channel_sequence,
                data: body.unwrap_or(&[0u8; 16]).to_vec(),
            })
        );
        let index = if is_dummy {
            crate::v2::PacketPrefixByte::TYPE_DUMMY
        } else {
            crate::v2::PacketPrefixByte::TYPE_STANDARD
        };
        packet.encode_on_wire(index).expect("encode")
    }

    fn v3_ack_of_acks(sequence: u16) -> Vec<u8> {
        crate::rdpudp_v2_packet!(
            header = V2PacketHeader::new(rdpudp_v2_flags!(AOA), 15).unwrap(),
            ack_of_acks = Some(crate::rdpudp_v2_ack_of_acks!(sequence))
        )
        .encode_on_wire(crate::v2::PacketPrefixByte::TYPE_STANDARD)
        .expect("encode")
    }

    #[test]
    fn syn_is_padded_to_the_mtu() {
        let mut conn = UdpConnection::new(default_config(UdpProtocolVersion::V2));
        let syn = conn.create_syn().expect("syn");
        assert_eq!(syn.len(), 1232);
    }

    #[test]
    fn dummy_packets_are_acknowledged_but_not_delivered() {
        let mut conn = connected_v3();
        let delivered = conn.process_source_packet(&v3_data(100, None)).expect("dummy");
        assert!(delivered.is_empty(), "a dummy packet carries nothing upwards");
        assert!(
            conn.has_pending_ack(),
            "a dummy still occupies a sequence number and must be acknowledged"
        );
        // The sequence space advanced, so real data behind it is delivered immediately.
        let delivered = conn
            .process_source_packet(&v3_data(101, Some(b"payload")))
            .expect("data");
        assert_eq!(delivered, vec![b"payload".to_vec()]);
    }

    /// Regression for the real shape of a retransmission, taken from a live capture: a lost
    /// chunk is *not* resent under its original data sequence number. It comes back later under
    /// a new one, still carrying its original channel sequence, so it arrives after the chunks
    /// that follow it. Handing the stream up in arrival order corrupts the TLS record layer.
    #[test]
    fn retransmission_is_reordered_back_into_the_stream() {
        let mut conn = connected_v3();
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(223, 23, Some(b"a")))
                .expect("223"),
            vec![b"a".to_vec()]
        );

        // Channel sequence 24 is lost. 25 and 26 arrive and must wait for it.
        assert!(conn
            .process_source_packet(&v3_data_with_channel(224, 25, Some(b"c")))
            .expect("224")
            .is_empty());
        assert!(conn
            .process_source_packet(&v3_data_with_channel(225, 26, Some(b"d")))
            .expect("225")
            .is_empty());

        // The peer resends chunk 24 under a fresh data sequence number.
        let delivered = conn
            .process_source_packet(&v3_data_with_channel(226, 24, Some(b"b")))
            .expect("retransmit");
        assert_eq!(
            delivered,
            vec![b"b".to_vec(), b"c".to_vec(), b"d".to_vec()],
            "the stream must be handed up in channel-sequence order, not arrival order"
        );
    }

    /// An AckOfAcks writes off a transport sequence number the peer stopped retransmitting under
    /// (a dummy). It must not disturb the stream, whose chunks are ordered by channel sequence.
    #[test]
    fn ack_of_acks_advances_the_window_without_touching_the_stream() {
        let mut conn = connected_v3();
        conn.process_source_packet(&v3_data_with_channel(100, 10, Some(b"a")))
            .expect("100");
        // 101 is a dummy that never arrives, so 102 waits behind it at the transport layer only.
        conn.process_source_packet(&v3_data_with_channel(102, 11, Some(b"b")))
            .expect("102");
        assert_eq!(conn.v3_expected_sequence, 101, "the window stops at the hole");

        conn.process_source_packet(&v3_ack_of_acks(102)).expect("aoa");
        assert_eq!(conn.v3_expected_sequence, 103, "the window moves past the dummy");
        assert!(
            conn.v3_channel_buffer.is_empty(),
            "chunk 11 was already handed up in order and nothing was discarded"
        );
    }

    #[test]
    fn retransmitted_packets_are_acknowledged_again() {
        let mut conn = connected_v3();
        conn.process_source_packet(&v3_data(100, Some(b"first"))).expect("first");
        conn.create_ack().expect("ack");
        assert!(!conn.has_pending_ack());

        // The peer resends 100 because it never saw our ACK. Staying silent would make it
        // retransmit until it gives up.
        let delivered = conn.process_source_packet(&v3_data(100, Some(b"first"))).expect("dup");
        assert!(delivered.is_empty(), "already-delivered data is not delivered twice");
        assert!(conn.has_pending_ack(), "a retransmit must be re-acknowledged");
    }

    /// The channel sequence space is 1-based and wraps 65535 -> 1, skipping 0. Advancing with a
    /// plain wrapping add leaves the receiver waiting for a chunk 0 that the peer never sends,
    /// which stalls the stream for the rest of the connection while the transport stays healthy.
    #[test]
    fn channel_sequence_wraps_past_zero() {
        let mut conn = connected_v3();
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(100, 65535, Some(b"a")))
                .expect("65535"),
            vec![b"a".to_vec()]
        );
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(101, 1, Some(b"b")))
                .expect("1"),
            vec![b"b".to_vec()],
            "the chunk after 65535 is 1, not 0"
        );
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(102, 2, Some(b"c")))
                .expect("2"),
            vec![b"c".to_vec()]
        );
        assert!(
            conn.v3_channel_buffer.is_empty(),
            "nothing should be left waiting behind a sequence that is never sent"
        );
    }

    /// A retransmission carries a *new* data sequence number but repeats an earlier channel
    /// sequence. Delivering it again duplicates bytes in the tunnel's byte stream, which
    /// permanently desynchronises the TLS record layer, so it must be dropped.
    #[test]
    fn retransmitted_channel_data_is_not_delivered_twice() {
        let mut conn = connected_v3();
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(100, 10, Some(b"a")))
                .expect("100"),
            vec![b"a".to_vec()]
        );
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(101, 11, Some(b"b")))
                .expect("101"),
            vec![b"b".to_vec()]
        );

        // The peer never saw our ACK and resends channel sequence 11 as data sequence 102.
        let delivered = conn
            .process_source_packet(&v3_data_with_channel(102, 11, Some(b"b")))
            .expect("retransmit");
        assert!(
            delivered.is_empty(),
            "channel sequence 11 has already been handed up"
        );
        assert!(conn.has_pending_ack(), "the retransmit is still acknowledged");

        // New data still flows.
        assert_eq!(
            conn.process_source_packet(&v3_data_with_channel(103, 12, Some(b"c")))
                .expect("103"),
            vec![b"c".to_vec()]
        );
    }

    #[test]
    fn gap_produces_an_ack_vector_rather_than_a_cumulative_ack() {
        let mut conn = connected_v3();
        conn.process_source_packet(&v3_data(100, Some(b"a"))).expect("100");
        conn.process_source_packet(&v3_data(102, Some(b"c"))).expect("102");

        let ack = conn.create_ack().expect("ack");
        let packet = V2Packet::decode_on_wire(&ack).expect("decode");
        let vector = packet.ack_vector.expect("a hole must be reported as an ACK vector");
        assert!(packet.ack.is_none(), "ACK and ACKVEC are mutually exclusive");
        assert_eq!(vector.base_sequence_number, 101, "base is the first missing sequence");
        // Bit 0 is the base (101, missing), bit 1 is 102 (received).
        assert_eq!(vector.entries, vec![AckVecEntry::StateMap(0b10)]);
    }

    #[test]
    fn ack_carries_a_real_timestamp() {
        let mut conn = connected_v3();
        conn.process_source_packet(&v3_data(100, Some(b"a"))).expect("100");
        let ack = conn.create_ack().expect("ack");
        let packet = V2Packet::decode_on_wire(&ack).expect("decode");
        let ack = packet.ack.expect("cumulative ack");
        assert_eq!(ack.sequence_number, 100);
        assert_eq!(packet.header.log_window_size, 15);
    }

    #[test]
    fn acknowledgements_survive_sequence_wraparound() {
        let mut conn = connected_v3();
        conn.v3_next_data_sequence = 0xfffe;
        conn.v3_sender_base = 0xfffe;
        for _ in 0..4 {
            conn.send_data(vec![0u8; 8]).expect("send");
        }
        assert_eq!(conn.pending_packets.len(), 4);
        // The peer acknowledges past the wrap point.
        conn.update_remote_ack_v3(0x0000);
        assert_eq!(
            conn.pending_packets.len(),
            1,
            "a cumulative ACK of 0x0000 covers 0xfffe, 0xffff and 0x0000, leaving only 0x0001"
        );
    }

    #[test]
    fn ack_of_acks_reports_the_oldest_unacknowledged_packet() {
        let mut conn = connected_v3();
        let first = conn.send_data(vec![1u8; 8]).expect("send");
        let packet = V2Packet::decode_on_wire(&first).expect("decode");
        let aoa = packet.ack_of_acks.expect("the first packet announces our base");
        assert_eq!(aoa.sequence_number, V3_INITIAL_SEQUENCE);

        // Nothing acknowledged yet, so the base has not moved and is not repeated.
        let second = conn.send_data(vec![2u8; 8]).expect("send");
        let packet = V2Packet::decode_on_wire(&second).expect("decode");
        assert!(packet.ack_of_acks.is_none());

        // Once the peer acknowledges the first packet the base advances and is re-announced.
        conn.update_remote_ack_v3(V3_INITIAL_SEQUENCE);
        let third = conn.send_data(vec![3u8; 8]).expect("send");
        let packet = V2Packet::decode_on_wire(&third).expect("decode");
        assert_eq!(
            packet.ack_of_acks.expect("base moved").sequence_number,
            V3_INITIAL_SEQUENCE + 1
        );
    }

    #[test]
    fn syn_falls_back_to_v2_without_cookie_hash() {
        let mut conn = UdpConnection::new(default_config(UdpProtocolVersion::V3));
        let syn = conn.create_syn().expect("syn encode");
        let packet = V1Packet::decode(&syn).expect("decode syn");
        let syn_ex = packet.syn_data_ex.expect("syn ex present");
        let version = syn_ex.udp_version.expect("version present");
        assert_eq!(version.bits(), UdpVersionFlags::VERSION_2.bits());
    }
}
