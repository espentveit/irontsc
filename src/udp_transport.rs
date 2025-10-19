/// UDP Transport Manager for RDP
///
/// Handles UDP-based multitransport for RDP, optimized for H.264 video streaming
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use ironrdp_udp::{
    ConnectionState, CorrelationId, TransportMode, UdpConfig, UdpConnection, UdpProtocolVersion,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, error, info, trace, warn};

/// UDP transport configuration
#[derive(Debug, Clone)]
pub struct UdpTransportConfig {
    /// Server address
    pub server_addr: SocketAddr,
    /// Local bind address (0.0.0.0:0 for automatic)
    pub local_addr: SocketAddr,
    /// Transport mode (Reliable or Lossy)
    pub mode: TransportMode,
    /// Enable Forward Error Correction
    pub enable_fec: bool,
    /// Protocol version
    pub protocol_version: UdpProtocolVersion,
    /// MTU size
    pub mtu: u16,
}

impl Default for UdpTransportConfig {
    fn default() -> Self {
        Self {
            server_addr: "0.0.0.0:3389".parse().unwrap(),
            local_addr: "0.0.0.0:0".parse().unwrap(),
            mode: TransportMode::Lossy, // Lossy mode better for video
            enable_fec: true,
            protocol_version: UdpProtocolVersion::V2,
            mtu: 1232,
        }
    }
}

/// Commands sent to the UDP transport task
#[derive(Debug)]
pub enum UdpTransportCommand {
    /// Send data over UDP
    SendData(Vec<u8>),
    /// Shutdown the transport
    Shutdown,
}

/// Events from the UDP transport
#[derive(Debug, Clone)]
pub enum UdpTransportEvent {
    /// Connection established
    Connected,
    /// Data received
    DataReceived(Vec<u8>),
    /// Connection lost
    Disconnected(String),
}

/// UDP Transport Manager
pub struct UdpTransportManager {
    /// UDP connection state machine
    connection: UdpConnection,
    /// UDP socket
    socket: Arc<UdpSocket>,
    /// Server address
    server_addr: SocketAddr,
    /// Command receiver
    command_rx: mpsc::UnboundedReceiver<UdpTransportCommand>,
    /// Event sender
    event_tx: mpsc::UnboundedSender<UdpTransportEvent>,
}

impl UdpTransportManager {
    /// Create a new UDP transport manager
    pub async fn new(
        config: UdpTransportConfig,
        correlation_id: Option<CorrelationId>,
    ) -> Result<(
        Self,
        mpsc::UnboundedSender<UdpTransportCommand>,
        mpsc::UnboundedReceiver<UdpTransportEvent>,
    )> {
        // Create UDP socket
        let socket = UdpSocket::bind(config.local_addr)
            .await
            .context("Failed to bind UDP socket")?;

        info!(
            "UDP socket bound to {} for server {}",
            socket.local_addr()?,
            config.server_addr
        );

        // Connect to server
        socket
            .connect(config.server_addr)
            .await
            .context("Failed to connect UDP socket")?;

        // Create UDP connection state machine
        let udp_config = UdpConfig {
            mtu: config.mtu,
            initial_sequence_number: rand::random(),
            receive_window_size: 256,
            mode: config.mode,
            protocol_version: config.protocol_version,
            enable_fec: config.enable_fec,
            fec_block_size: 8,
            retransmit_timeout_ms: if config.protocol_version >= UdpProtocolVersion::V2 {
                300
            } else {
                500
            },
            max_retransmits: if config.mode == TransportMode::Lossy {
                0
            } else {
                5
            },
            keepalive_interval_ms: 5000,
        };

        let mut connection = UdpConnection::new(udp_config);
        if let Some(corr_id) = correlation_id {
            connection.set_correlation_id(corr_id);
        }

        // Create channels
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let manager = Self {
            connection,
            socket: Arc::new(socket),
            server_addr: config.server_addr,
            command_rx,
            event_tx,
        };

        Ok((manager, command_tx, event_rx))
    }

    /// Run the UDP transport manager
    pub async fn run(mut self) -> Result<()> {
        info!("Starting UDP transport manager");

        // Perform three-way handshake
        if let Err(e) = self.handshake().await {
            error!("UDP handshake failed: {}", e);
            let _ = self
                .event_tx
                .send(UdpTransportEvent::Disconnected(format!("{}", e)));
            return Err(e);
        }

        info!("UDP connection established");
        let _ = self.event_tx.send(UdpTransportEvent::Connected);

        // Main event loop
        let mut recv_buffer = vec![0u8; 65536];
        let mut check_retransmit_interval = tokio::time::interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                // Handle incoming UDP packets
                result = self.socket.recv_from(&mut recv_buffer) => {
                    match result {
                        Ok((len, addr)) => {
                            if addr == self.server_addr {
                                if let Err(e) = self.handle_received_packet(&recv_buffer[..len]).await {
                                    warn!("Error handling received packet: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            error!("UDP recv error: {}", e);
                            break;
                        }
                    }
                }

                // Handle commands from application
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        UdpTransportCommand::SendData(data) => {
                            if let Err(e) = self.send_data(data).await {
                                warn!("Error sending data: {}", e);
                            }
                        }
                        UdpTransportCommand::Shutdown => {
                            info!("UDP transport shutdown requested");
                            break;
                        }
                    }
                }

                // Check for retransmits (only in reliable mode)
                _ = check_retransmit_interval.tick() => {
                    let retransmits = self.connection.check_retransmits();
                    for packet in retransmits {
                        if let Err(e) = self.socket.send(&packet).await {
                            error!("Failed to retransmit packet: {}", e);
                        } else {
                            trace!("Retransmitted packet ({} bytes)", packet.len());
                        }
                    }

                    // Check connection state
                    if self.connection.state() == ConnectionState::Terminated {
                        warn!("UDP connection terminated due to max retransmits");
                        let _ = self.event_tx.send(UdpTransportEvent::Disconnected(
                            "Max retransmits reached".to_string()
                        ));
                        break;
                    }
                }
            }
        }

        info!("UDP transport manager stopped");
        Ok(())
    }

    /// Perform UDP handshake
    async fn handshake(&mut self) -> Result<()> {
        info!("Starting UDP handshake");

        // Send SYN
        let syn_packet = self.connection.create_syn()?;
        self.socket
            .send(&syn_packet)
            .await
            .context("Failed to send SYN")?;
        debug!("Sent SYN packet ({} bytes)", syn_packet.len());

        // Wait for SYN+ACK with timeout
        let mut buffer = vec![0u8; 2048];
        let timeout_duration = Duration::from_secs(5);

        let syn_ack = tokio::time::timeout(timeout_duration, async {
            loop {
                let (len, addr) = self
                    .socket
                    .recv_from(&mut buffer)
                    .await
                    .context("Failed to receive SYN+ACK")?;

                if addr == self.server_addr {
                    return Ok::<Vec<u8>, anyhow::Error>(buffer[..len].to_vec());
                }
            }
        })
        .await
        .context("Timeout waiting for SYN+ACK")??;

        debug!("Received SYN+ACK ({} bytes)", syn_ack.len());

        // Process SYN+ACK
        self.connection
            .process_syn_ack(&syn_ack)
            .context("Failed to process SYN+ACK")?;

        // Send ACK (implicit in first data packet, so just mark as connected)
        info!("UDP handshake complete, connection established");

        Ok(())
    }

    /// Handle received UDP packet
    async fn handle_received_packet(&mut self, packet: &[u8]) -> Result<()> {
        trace!("Processing received packet ({} bytes)", packet.len());

        // Try to process as source packet (data)
        match self.connection.process_source_packet(packet) {
            Ok(datas) if !datas.is_empty() => {
                for data in datas {
                    trace!("Received data packet ({} bytes payload)", data.len());
                    let _ = self.event_tx.send(UdpTransportEvent::DataReceived(data));
                }
            }
            Ok(_) => {
                // Packet received but not yet deliverable (out of order)
                trace!("Packet buffered (out of sequence)");
            }
            Err(e) => {
                // Maybe an ACK; try to process
                if let Err(err) = self.connection.process_ack_packet(packet) {
                    trace!("Failed to decode UDP packet: {}; ack error: {}", e, err);
                } else {
                    trace!("Processed UDP ACK packet");
                }
            }
        }

        Ok(())
    }

    /// Send data over UDP
    async fn send_data(&mut self, data: Vec<u8>) -> Result<()> {
        trace!("Sending data ({} bytes)", data.len());

        let packet = self
            .connection
            .send_data(data)
            .context("Failed to create source packet")?;

        self.socket
            .send(&packet)
            .await
            .context("Failed to send packet")?;

        trace!("Sent source packet ({} bytes)", packet.len());
        Ok(())
    }
}

/// Helper to create a UDP transport for video streaming
pub async fn create_video_udp_transport(
    server_addr: SocketAddr,
    correlation_id: Option<CorrelationId>,
) -> Result<(
    mpsc::UnboundedSender<UdpTransportCommand>,
    mpsc::UnboundedReceiver<UdpTransportEvent>,
)> {
    let config = UdpTransportConfig {
        server_addr,
        mode: TransportMode::Lossy, // Best for video
        enable_fec: true,           // FEC helps recover lost frames
        protocol_version: UdpProtocolVersion::V2,
        ..Default::default()
    };

    let (manager, command_tx, event_rx) = UdpTransportManager::new(config, correlation_id).await?;

    // Spawn the transport task
    tokio::spawn(async move {
        if let Err(e) = manager.run().await {
            error!("UDP transport task error: {}", e);
        }
    });

    Ok((command_tx, event_rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_udp_transport_creation() {
        let config = UdpTransportConfig::default();
        let result = UdpTransportManager::new(config, None).await;
        assert!(result.is_ok());
    }
}
