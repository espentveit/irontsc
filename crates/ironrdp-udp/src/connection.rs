/// UDP connection state machine and transport implementation
/// Based on MS-RDPEUDP spec sections 3.1.5
use std::collections::{hash_map::Entry, HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::ack::{AckOfAckVectorHeader, AckVectorElement, AckVectorHeader, VectorElementState};
use crate::correlation::CorrelationId;
use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::fec::FecCodec;
use crate::handshake::{SynAckPacket, SynPacket};
use crate::header::FecHeader;
use crate::packet::{AckPacket, SourcePacket};
use crate::syndata::SynData;
use crate::syndataex::{SynDataEx, SynDataExFlags, UdpProtocolVersion};

/// Transport mode (Reliable or Lossy)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Reliable mode - packets are retransmitted if lost
    Reliable,
    /// Lossy mode - no retransmission, best-effort delivery
    Lossy,
}

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state
    Idle,
    /// SYN sent, waiting for SYN+ACK
    SynSent,
    /// SYN received, waiting for ACK
    SynReceived,
    /// Connection established, data transfer active
    Connected,
    /// Connection terminated
    Terminated,
}

/// Configuration for UDP connection
#[derive(Debug, Clone)]
pub struct UdpConfig {
    /// Maximum transmission unit
    pub mtu: u16,
    /// Initial sequence number
    pub initial_sequence_number: u32,
    /// Receive window size
    pub receive_window_size: u16,
    /// Transport mode
    pub mode: TransportMode,
    /// Protocol version to use
    pub protocol_version: UdpProtocolVersion,
    /// Enable FEC
    pub enable_fec: bool,
    /// Number of source packets per FEC block
    pub fec_block_size: u8,
    /// Retransmit timeout in milliseconds
    pub retransmit_timeout_ms: u64,
    /// Maximum number of retransmits before giving up
    pub max_retransmits: u8,
    /// Keepalive interval in milliseconds
    pub keepalive_interval_ms: u64,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            mtu: 1232, // Default MTU from spec
            initial_sequence_number: 0,
            receive_window_size: 256,
            mode: TransportMode::Reliable,
            protocol_version: UdpProtocolVersion::V1,
            enable_fec: true,
            fec_block_size: 8,
            retransmit_timeout_ms: 300, // Version 2 default
            max_retransmits: 5,
            keepalive_interval_ms: 5000,
        }
    }
}

/// Outgoing packet waiting for acknowledgment
#[derive(Debug, Clone)]
struct PendingPacket {
    sequence_number: u32,
    data: Vec<u8>,
    first_sent: Instant,
    last_sent: Instant,
    retransmit_count: u8,
}

/// UDP connection manager
pub struct UdpConnection {
    config: UdpConfig,
    state: ConnectionState,

    // Sequence numbers
    next_send_sequence: u32,
    next_receive_sequence: u32,
    last_ack_received: u32,

    // Correlation ID for multitransport
    correlation_id: Option<CorrelationId>,

    // Pending outgoing packets (for reliable mode)
    pending_packets: HashMap<u32, PendingPacket>,

    // Received packets buffer
    receive_buffer: HashMap<u32, Vec<u8>>,
    /// Tracking state for ACK vector generation (per outstanding sequence)
    ack_states: VecDeque<VectorElementState>,

    // FEC state
    fec_codec: FecCodec,
    source_packets_in_block: Vec<Vec<u8>>,

    // Timing
    last_keepalive: Instant,
    last_ack_sent: Instant,

    // Remote peer info
    remote_mtu: u16,
    remote_window_size: u16,

    // Track if first ACK after handshake has been sent (MS-RDPEUDP 3-way handshake requirement)
    first_ack_sent: bool,
}

impl UdpConnection {
    /// Create a new UDP connection
    pub fn new(config: UdpConfig) -> Self {
        Self {
            config,
            state: ConnectionState::Idle,
            next_send_sequence: 0,
            next_receive_sequence: 0,
            last_ack_received: 0,
            correlation_id: None,
            pending_packets: HashMap::new(),
            receive_buffer: HashMap::new(),
            ack_states: VecDeque::new(),
            fec_codec: FecCodec::new(),
            source_packets_in_block: Vec::new(),
            last_keepalive: Instant::now(),
            last_ack_sent: Instant::now(),
            remote_mtu: 1232,
            remote_window_size: 256,
            first_ack_sent: false,
        }
    }

    /// Get current connection state
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Set correlation ID for multitransport
    pub fn set_correlation_id(&mut self, correlation_id: CorrelationId) {
        self.correlation_id = Some(correlation_id);
    }

    /// Create a SYN packet to initiate connection
    pub fn create_syn(&mut self) -> UdpResult<Vec<u8>> {
        if self.state != ConnectionState::Idle {
            return Err(UdpError::invalid_state(
                "create_syn",
                "Connection not in Idle state",
            ));
        }

        self.next_send_sequence = self.config.initial_sequence_number;

        let syn_data = SynData {
            initial_sequence_number: self.next_send_sequence,
            upstream_mtu: self.config.mtu,
            downstream_mtu: self.config.mtu,
        };

        let syn_data_ex = if self.config.protocol_version != UdpProtocolVersion::V1 {
            Some(SynDataEx {
                flags: SynDataExFlags::VERSION_INFO_VALID,
                udp_version: Some(self.config.protocol_version),
                cookie_hash: None,
            })
        } else {
            None
        };

        let syn_lossy = self.config.mode == TransportMode::Lossy;

        let packet = SynPacket::new(
            self.config.receive_window_size,
            syn_lossy,
            syn_data,
            self.correlation_id,
            syn_data_ex,
        );

        self.state = ConnectionState::SynSent;
        Ok(packet.to_padded_bytes())
    }

    /// Process received SYN packet (server-side)
    pub fn process_syn(&mut self, bytes: &[u8]) -> UdpResult<()> {
        if self.state != ConnectionState::Idle {
            return Err(UdpError::invalid_state(
                "process_syn",
                "Connection not in Idle state",
            ));
        }

        let packet = SynPacket::decode(bytes)?;
        let inner = packet.inner();

        self.next_receive_sequence = inner.syn_data.initial_sequence_number;
        self.remote_mtu = inner.syn_data.upstream_mtu;
        self.remote_window_size = inner.header.receive_window_size;
        self.reset_receive_state(inner.syn_data.initial_sequence_number);

        if let Some(ref syn_ex) = inner.syn_data_ex {
            if let Some(their_version) = syn_ex.udp_version {
                // Negotiate protocol version
                let our_version = self.config.protocol_version;
                self.config.protocol_version = our_version.min(their_version);
            }
        }

        self.state = ConnectionState::SynReceived;
        Ok(())
    }

    /// Create SYN+ACK response (server-side)
    pub fn create_syn_ack(&mut self) -> UdpResult<Vec<u8>> {
        if self.state != ConnectionState::SynReceived {
            return Err(UdpError::invalid_state(
                "create_syn_ack",
                "Connection not in SynReceived state",
            ));
        }

        self.next_send_sequence = self.config.initial_sequence_number;

        let syn_data = SynData {
            initial_sequence_number: self.next_send_sequence,
            upstream_mtu: self.config.mtu,
            downstream_mtu: self.config.mtu,
        };

        let syn_data_ex = if self.config.protocol_version != UdpProtocolVersion::V1 {
            Some(SynDataEx {
                flags: SynDataExFlags::VERSION_INFO_VALID,
                udp_version: Some(self.config.protocol_version),
                cookie_hash: None,
            })
        } else {
            None
        };

        let packet = SynAckPacket::new(
            self.next_receive_sequence,
            self.config.receive_window_size,
            syn_data,
            self.correlation_id,
            syn_data_ex,
        );

        self.state = ConnectionState::Connected;
        Ok(packet.to_padded_bytes())
    }

    /// Process received SYN+ACK packet (client-side)
    pub fn process_syn_ack(&mut self, bytes: &[u8]) -> UdpResult<()> {
        if self.state != ConnectionState::SynSent {
            return Err(UdpError::invalid_state(
                "process_syn_ack",
                "Connection not in SynSent state",
            ));
        }

        let packet = SynAckPacket::decode(bytes)?;
        let inner = packet.inner();

        self.next_receive_sequence = inner.syn_data.initial_sequence_number;
        self.remote_mtu = inner.syn_data.upstream_mtu;
        self.remote_window_size = inner.header.receive_window_size;
        self.last_ack_received = inner.header.sn_source_ack;
        self.reset_receive_state(inner.syn_data.initial_sequence_number);

        if let Some(ref syn_ex) = inner.syn_data_ex {
            if let Some(their_version) = syn_ex.udp_version {
                let our_version = self.config.protocol_version;
                self.config.protocol_version = our_version.min(their_version);
            }
        }

        self.state = ConnectionState::Connected;
        // Next data packet MUST include ACK flag to complete 3-way handshake per MS-RDPEUDP
        self.first_ack_sent = false;
        Ok(())
    }

    /// Send data (creates and returns a source packet)
    pub fn send_data(&mut self, data: Vec<u8>) -> UdpResult<Vec<u8>> {
        if self.state != ConnectionState::Connected {
            return Err(UdpError::invalid_state(
                "send_data",
                "Connection not in Connected state",
            ));
        }

        let sequence_number = self.next_send_sequence;
        self.next_send_sequence = self.next_send_sequence.wrapping_add(1);

        // MS-RDPEUDP 3.1.5.1.2: First DATA packet after SYN+ACK should NOT include ACK_VECTOR
        // because we haven't received any DATA packets yet (only SYN+ACK which is not a DATA packet)
        let ack_vector = if self.first_ack_sent {
            self.build_ack_vector()?
        } else {
            None  // Force no ACK vector for first DATA packet
        };

        // MS-RDPEUDP: First data packet after SYN+ACK MUST have ACK flag set
        // to complete the 3-way handshake (Section 1.4)
        let include_ack = !self.first_ack_sent;

        // snSourceAck should acknowledge the last SOURCE (DATA) packet received
        // For the first DATA packet, we use next_receive_sequence - 1 which is the
        // server's Initial Sequence Number from the SYN+ACK
        // (We decrement because next_receive_sequence was already incremented during reset)
        let sn_source_ack = if self.first_ack_sent {
            // Normal case: acknowledge the last DATA packet we received
            self.next_receive_sequence.wrapping_sub(1)
        } else {
            // No data has been received yet, so acknowledge "prior to" the server's first DATA
            self.next_receive_sequence.wrapping_sub(1)
        };

        let packet = SourcePacket::new(
            sequence_number,
            sn_source_ack,
            self.config.receive_window_size,
            data.clone(),
            ack_vector.clone(),
            None,
            include_ack,
        )?;

        let encoded = packet.encode();

        // Mark first ACK as sent
        if include_ack {
            self.first_ack_sent = true;
        }

        // Store for potential retransmission
        if self.config.mode == TransportMode::Reliable {
            let now = Instant::now();
            self.pending_packets.insert(
                sequence_number,
                PendingPacket {
                    sequence_number,
                    data: encoded.clone(),
                    first_sent: now,
                    last_sent: now,
                    retransmit_count: 0,
                },
            );
        }

        // Add to FEC block if enabled
        if self.config.enable_fec {
            self.source_packets_in_block.push(data);
        }

        Ok(encoded)
    }

    /// Check if FEC block is complete and generate FEC packet if needed
    /// Returns Some(fec_packet_bytes) if a FEC packet should be sent
    pub fn check_fec_block(&mut self) -> UdpResult<Option<Vec<u8>>> {
        if !self.config.enable_fec {
            return Ok(None);
        }

        if self.source_packets_in_block.len() < self.config.fec_block_size as usize {
            return Ok(None);
        }

        // Generate FEC packet for the completed block
        let fec_index = 0u8; // Could support multiple FEC packets per block in future
        let fec_data = self
            .fec_codec
            .encode(&self.source_packets_in_block, fec_index);

        // Create FEC packet header
        let base_sequence = self
            .next_send_sequence
            .wrapping_sub(self.source_packets_in_block.len() as u32);
        let fec_header = crate::payload::FecPayloadHeader::new(
            base_sequence,
            0, // sn_source_start: index of first packet in block (0 for first packet)
            self.source_packets_in_block.len() as u8, // urange: number of source packets
            fec_index,
        );

        let ack_vector = self.build_ack_vector()?;

        let fec_packet = crate::packet::FecPacket::new(
            self.last_ack_received,
            self.config.receive_window_size,
            fec_header,
            fec_data,
            ack_vector,
            None,
        )?;

        // Clear the block for next round
        self.source_packets_in_block.clear();

        Ok(Some(fec_packet.encode()))
    }

    /// Process received source packet
    pub fn process_source_packet(&mut self, bytes: &[u8]) -> UdpResult<Vec<Vec<u8>>> {
        let packet = SourcePacket::decode(bytes)?;

        self.handle_ack_headers(
            &packet.header,
            packet.ack_vector.as_ref(),
            packet.ack_of_ack.as_ref(),
        );

        let seq = packet.sequence_number();

        if seq < self.next_receive_sequence {
            // Already received/acknowledged packet, ignore but update keepalive
            self.last_keepalive = Instant::now();
            return Ok(Vec::new());
        }

        let distance = seq.wrapping_sub(self.next_receive_sequence);
        if distance as u16 > self.remote_window_size {
            return Err(UdpError::invalid_state(
                "process_source_packet",
                "Sequence number outside of receive window",
            ));
        }

        self.ensure_ack_capacity(distance as usize);

        let idx = distance as usize;
        if self.ack_states.len() == idx {
            self.ack_states
                .push_back(VectorElementState::DatagramReceived);
        } else {
            self.ack_states[idx] = VectorElementState::DatagramReceived;
        }

        match self.receive_buffer.entry(seq) {
            Entry::Occupied(_) => {}
            Entry::Vacant(v) => {
                v.insert(packet.data);
            }
        }

        self.last_keepalive = Instant::now();

        Ok(self.flush_receive_buffer())
    }

    /// Create an ACK packet
    pub fn create_ack(&mut self) -> UdpResult<Vec<u8>> {
        let ack_vector = self.build_ack_vector()?;
        let packet = AckPacket::new(
            self.last_ack_received,
            self.config.receive_window_size,
            ack_vector,
            None,
        );

        self.last_ack_sent = Instant::now();
        
        // Mark first ACK as sent (completes 3-way handshake per MS-RDPEUDP)
        if !self.first_ack_sent {
            self.first_ack_sent = true;
        }
        
        Ok(packet.encode())
    }

    /// Check if retransmission is needed and return packets to retransmit
    pub fn check_retransmits(&mut self) -> Vec<Vec<u8>> {
        if self.config.mode != TransportMode::Reliable {
            return Vec::new();
        }

        let now = Instant::now();
        let timeout = Duration::from_millis(self.config.retransmit_timeout_ms);
        let mut to_retransmit = Vec::new();

        for pending in self.pending_packets.values_mut() {
            if now.duration_since(pending.last_sent) >= timeout {
                if pending.retransmit_count < self.config.max_retransmits {
                    pending.last_sent = now;
                    pending.retransmit_count += 1;
                    to_retransmit.push(pending.data.clone());
                } else {
                    // Max retransmits reached, connection failed
                    self.state = ConnectionState::Terminated;
                }
            }
        }

        to_retransmit
    }

    /// Check if keepalive is needed
    pub fn needs_keepalive(&self) -> bool {
        let elapsed = Instant::now().duration_since(self.last_keepalive);
        elapsed >= Duration::from_millis(self.config.keepalive_interval_ms)
    }

    /// Terminate connection
    pub fn terminate(&mut self) {
        self.state = ConnectionState::Terminated;
        self.pending_packets.clear();
        self.receive_buffer.clear();
        self.ack_states.clear();
    }

    /// Process ACK packet from peer
    pub fn process_ack_packet(&mut self, bytes: &[u8]) -> UdpResult<()> {
        let packet = AckPacket::decode(bytes)?;
        self.handle_ack_headers(
            &packet.header,
            packet.ack_vector.as_ref(),
            packet.ack_of_ack.as_ref(),
        );
        Ok(())
    }

    fn reset_receive_state(&mut self, initial_sequence: u32) {
        self.next_receive_sequence = initial_sequence;
        self.receive_buffer.clear();
        self.ack_states.clear();
    }

    fn ensure_ack_capacity(&mut self, offset: usize) {
        while self.ack_states.len() < offset {
            self.ack_states
                .push_back(VectorElementState::DatagramNotYetReceived);
        }
    }

    fn flush_receive_buffer(&mut self) -> Vec<Vec<u8>> {
        let mut delivered = Vec::new();

        loop {
            match self.ack_states.front() {
                Some(VectorElementState::DatagramReceived) => {
                    let seq = self.next_receive_sequence;
                    self.next_receive_sequence = self.next_receive_sequence.wrapping_add(1);
                    self.ack_states.pop_front();
                    if let Some(data) = self.receive_buffer.remove(&seq) {
                        delivered.push(data);
                    }
                }
                Some(VectorElementState::DatagramNotYetReceived)
                | Some(VectorElementState::DatagramReserved1)
                | Some(VectorElementState::DatagramReserved2) => {
                    break;
                }
                None => break,
            }
        }

        delivered
    }

    fn build_ack_vector(&self) -> UdpResult<Option<AckVectorHeader>> {
        if self.ack_states.is_empty() {
            return Ok(None);
        }

        let states: Vec<VectorElementState> = self.ack_states.iter().copied().collect();
        let mut vectors = Vec::new();

        let mut idx = 0;
        while idx < states.len() {
            let current_state = states[idx];
            let mut run = 1usize;

            while idx + run < states.len() && states[idx + run] == current_state {
                run += 1;
                if run == 64 {
                    break;
                }
            }

            vectors.push(AckVectorElement::new(current_state, run as u8)?);

            idx += run;
        }

        let base_sequence_number = self.next_receive_sequence.wrapping_sub(1);
        let ack_timestamp = current_timestamp_ms();

        Ok(Some(AckVectorHeader::new(
            base_sequence_number,
            ack_timestamp,
            vectors,
        )?))
    }

    fn handle_ack_headers(
        &mut self,
        header: &FecHeader,
        ack_vector: Option<&AckVectorHeader>,
        _ack_of_ack: Option<&AckOfAckVectorHeader>,
    ) {
        self.remove_acked_upto(header.sn_source_ack);
        if let Some(vector) = ack_vector {
            self.apply_ack_vector(vector);
        }
        self.last_ack_received = header.sn_source_ack;
        self.last_keepalive = Instant::now();
    }

    fn remove_acked_upto(&mut self, sequence: u32) {
        let keys: Vec<u32> = self
            .pending_packets
            .keys()
            .copied()
            .filter(|seq| sequence_leq(*seq, sequence))
            .collect();

        for key in keys {
            self.pending_packets.remove(&key);
        }
    }

    fn apply_ack_vector(&mut self, vector: &AckVectorHeader) {
        let mut sequence = vector.base_sequence_number;
        let timeout = Duration::from_millis(self.config.retransmit_timeout_ms);
        let now = Instant::now();

        for element in &vector.ack_vectors {
            let count = element.count() as u32;
            match element.state {
                VectorElementState::DatagramReceived => {
                    for offset in 0..count {
                        let seq = sequence.wrapping_add(offset);
                        self.pending_packets.remove(&seq);
                    }
                }
                VectorElementState::DatagramNotYetReceived => {
                    for offset in 0..count {
                        let seq = sequence.wrapping_add(offset);
                        if let Some(pending) = self.pending_packets.get_mut(&seq) {
                            // Mark as ready for immediate retransmission
                            if let Some(adjusted) = now.checked_sub(timeout) {
                                pending.last_sent = adjusted;
                            } else {
                                pending.last_sent = now;
                            }
                        }
                    }
                }
                _ => {}
            }

            sequence = sequence.wrapping_add(count);
        }
    }
}

fn current_timestamp_ms() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_millis() as u64)
        .unwrap_or(0) as u32
}

fn sequence_leq(a: u32, b: u32) -> bool {
    // For now assume no wrap-around over 2^31 window.
    if a == b {
        true
    } else {
        let diff = b.wrapping_sub(a);
        diff < (1u32 << 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_initialization_client() {
        let config = UdpConfig::default();
        let mut conn = UdpConnection::new(config);

        assert_eq!(conn.state(), ConnectionState::Idle);

        let syn_bytes = conn.create_syn().unwrap();
        assert!(syn_bytes.len() > 0);
        assert_eq!(conn.state(), ConnectionState::SynSent);
    }

    #[test]
    fn test_syn_packet_format() {
        let config = UdpConfig {
            mtu: 1232,
            initial_sequence_number: 0x00000042,
            receive_window_size: 0x0100,
            mode: TransportMode::Lossy,
            protocol_version: UdpProtocolVersion::V2,
            enable_fec: true,
            fec_block_size: 8,
            retransmit_timeout_ms: 300,
            max_retransmits: 0,
            keepalive_interval_ms: 5000,
        };

        let mut conn = UdpConnection::new(config);

        // Set correlation ID
        let corr_id = CorrelationId::new([
            0xd2, 0x35, 0xac, 0x43, 0x89, 0x41, 0x42, 0xda, 0xb1, 0x0e, 0xdd, 0x68, 0x87, 0xf7,
            0xf9, 0xfb,
        ]);
        conn.set_correlation_id(corr_id);

        let syn_bytes = conn.create_syn().unwrap();

        println!("\n=== SYN Packet Analysis ===");
        println!("Total length: {} bytes", syn_bytes.len());
        println!("Expected minimum: 1132 bytes (as per spec)");
        println!("Target MTU: 1232 bytes");

        // Header (8 bytes)
        println!("\nFEC Header (8 bytes):");
        println!(
            "  snSourceAck: {:02x} {:02x} {:02x} {:02x} (should be ff ff ff ff)",
            syn_bytes[0], syn_bytes[1], syn_bytes[2], syn_bytes[3]
        );
        println!(
            "  uReceiveWindowSize: {:02x} {:02x}",
            syn_bytes[4], syn_bytes[5]
        );
        println!("  uFlags: {:02x} {:02x}", syn_bytes[6], syn_bytes[7]);

        // SynData (8 bytes)
        println!("\nSYNDATA (8 bytes at offset 8):");
        println!(
            "  snInitialSequenceNumber: {:02x} {:02x} {:02x} {:02x}",
            syn_bytes[8], syn_bytes[9], syn_bytes[10], syn_bytes[11]
        );
        println!(
            "  uUpStreamMtu: {:02x} {:02x}",
            syn_bytes[12], syn_bytes[13]
        );
        println!(
            "  uDownStreamMtu: {:02x} {:02x}",
            syn_bytes[14], syn_bytes[15]
        );

        // Correlation ID (32 bytes at offset 16)
        println!("\nCorrelation ID (32 bytes at offset 16):");
        println!("  Value (16 bytes): {:02x?}", &syn_bytes[16..32]);
        println!("  Reserved (16 bytes): {:02x?}", &syn_bytes[32..48]);

        // SynDataEx (4+ bytes at offset 48)
        println!("\nSYNDATAEX (at offset 48):");
        println!("  uSynExFlags: {:02x} {:02x}", syn_bytes[48], syn_bytes[49]);
        println!("  uUdpVer: {:02x} {:02x}", syn_bytes[50], syn_bytes[51]);

        // Verify spec requirements
        assert_eq!(
            syn_bytes[0..4],
            [0xff, 0xff, 0xff, 0xff],
            "snSourceAck must be -1"
        );
        assert_eq!(
            syn_bytes[8..12],
            [0x00, 0x00, 0x00, 0x42],
            "ISN should be 0x42"
        );
        assert!(
            syn_bytes.len() >= 1132,
            "Packet must be at least 1132 bytes"
        );
        assert!(syn_bytes.len() <= 1232, "Packet must not exceed 1232 bytes");

        // Check flags
        let flags = u16::from_be_bytes([syn_bytes[6], syn_bytes[7]]);
        assert_ne!(flags & 0x0001, 0, "SYN flag must be set");
        assert_ne!(
            flags & 0x0200,
            0,
            "SYNLOSSY flag must be set for lossy mode"
        );
        assert_ne!(flags & 0x0800, 0, "CORRELATION_ID flag must be set");
        assert_ne!(flags & 0x1000, 0, "SYNEX flag must be set");
    }

    #[test]
    fn test_connection_initialization_server() {
        let config = UdpConfig::default();
        let mut server = UdpConnection::new(config.clone());
        let mut client = UdpConnection::new(config);

        // Client sends SYN
        let syn_bytes = client.create_syn().unwrap();

        // Server receives SYN
        server.process_syn(&syn_bytes).unwrap();
        assert_eq!(server.state(), ConnectionState::SynReceived);

        // Server sends SYN+ACK
        let syn_ack_bytes = server.create_syn_ack().unwrap();
        assert_eq!(server.state(), ConnectionState::Connected);

        // Client receives SYN+ACK
        client.process_syn_ack(&syn_ack_bytes).unwrap();
        assert_eq!(client.state(), ConnectionState::Connected);
    }

    #[test]
    fn test_send_receive_data() {
        let config = UdpConfig::default();
        let mut sender = UdpConnection::new(config.clone());
        let mut receiver = UdpConnection::new(config);

        // Establish connection
        sender.state = ConnectionState::Connected;
        receiver.state = ConnectionState::Connected;

        // Send data
        let data = b"Hello, RDP-UDP!".to_vec();
        let packet_bytes = sender.send_data(data.clone()).unwrap();

        // Receive data
        let received = receiver.process_source_packet(&packet_bytes).unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0], data);
    }

    #[test]
    fn test_out_of_order_reassembly() {
        let config = UdpConfig::default();
        let mut receiver = UdpConnection::new(config.clone());
        receiver.state = ConnectionState::Connected;
        receiver.reset_receive_state(0);

        // Simulate receiving sequence 1 before sequence 0
        let packet1 = SourcePacket::new(1, 0, 256, b"two".to_vec(), None, None, false).unwrap();
        let encoded1 = packet1.encode();
        let buffered = receiver.process_source_packet(&encoded1).unwrap();
        assert!(buffered.is_empty());

        // Now receive sequence 0, which should flush both
        let packet0 = SourcePacket::new(0, 0, 256, b"one".to_vec(), None, None, false).unwrap();
        let encoded0 = packet0.encode();
        let delivered = receiver.process_source_packet(&encoded0).unwrap();
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0], b"one".to_vec());
        assert_eq!(delivered[1], b"two".to_vec());
    }
}
