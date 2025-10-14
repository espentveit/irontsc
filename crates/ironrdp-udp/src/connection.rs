/// UDP connection state machine and transport implementation
/// Based on MS-RDPEUDP spec sections 3.1.5

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::ack::{AckOfAckVectorHeader, AckVectorElement, AckVectorHeader, VectorElementState};
use crate::correlation::CorrelationId;
use crate::error::{UdpError, UdpErrorExt as _, UdpResult};
use crate::fec::FecCodec;
use crate::handshake::{SynAckPacket, SynPacket};
use crate::packet::{AckPacket, FecPacket, SourcePacket};
use crate::payload::FecPayloadHeader;
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
            protocol_version: UdpProtocolVersion::V2,
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
    
    // FEC state
    fec_codec: FecCodec,
    source_packets_in_block: Vec<Vec<u8>>,
    
    // Timing
    last_keepalive: Instant,
    last_ack_sent: Instant,
    
    // Remote peer info
    remote_mtu: u16,
    remote_window_size: u16,
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
            fec_codec: FecCodec::new(),
            source_packets_in_block: Vec::new(),
            last_keepalive: Instant::now(),
            last_ack_sent: Instant::now(),
            remote_mtu: 1232,
            remote_window_size: 256,
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

        if let Some(ref syn_ex) = inner.syn_data_ex {
            if let Some(their_version) = syn_ex.udp_version {
                let our_version = self.config.protocol_version;
                self.config.protocol_version = our_version.min(their_version);
            }
        }

        self.state = ConnectionState::Connected;
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

        let packet = SourcePacket::new(
            sequence_number,
            self.last_ack_received,
            self.config.receive_window_size,
            data.clone(),
            None,
            None,
        )?;

        let encoded = packet.encode();

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

    /// Process received source packet
    pub fn process_source_packet(&mut self, bytes: &[u8]) -> UdpResult<Option<Vec<u8>>> {
        let packet = SourcePacket::decode(bytes)?;
        
        // Update ACK state
        self.last_ack_received = packet.header.sn_source_ack;
        
        // Remove acknowledged packets from pending
        self.pending_packets.retain(|seq, _| *seq > self.last_ack_received);

        let seq = packet.sequence_number();
        
        // Check if this is the expected packet
        if seq == self.next_receive_sequence {
            self.next_receive_sequence = self.next_receive_sequence.wrapping_add(1);
            Ok(Some(packet.data))
        } else {
            // Store for later processing
            self.receive_buffer.insert(seq, packet.data);
            Ok(None)
        }
    }

    /// Create an ACK packet
    pub fn create_ack(&mut self) -> UdpResult<Vec<u8>> {
        let packet = AckPacket::new(
            self.next_receive_sequence.wrapping_sub(1),
            self.config.receive_window_size,
            None,
            None,
        );

        self.last_ack_sent = Instant::now();
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
        assert_eq!(received, Some(data));
    }
}
