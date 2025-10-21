/// UDP Transport Manager for RDP
///
/// Handles UDP-based multitransport for RDP, optimized for H.264 video streaming
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use ironrdp_core::{Encode, WriteCursor};
use ironrdp_pdu::rdp::tunnel::TunnelPdu;
use ironrdp_udp::{
    ConnectionState, CorrelationId, TransportMode, UdpConfig, UdpConnection, UdpProtocolVersion,
};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, trace, warn};

use crate::udp_stream_adapter::UdpStreamAdapter;

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
    /// MS-RDPEMT tunnel established
    TunnelEstablished,
    /// Data received
    DataReceived(Vec<u8>),
    /// Connection lost
    Disconnected(String),
}

/// UDP Transport Manager
pub struct UdpTransportManager {
    /// UDP connection state machine
    connection: Arc<Mutex<UdpConnection>>,
    /// UDP socket
    socket: Arc<UdpSocket>,
    /// Server address
    server_addr: SocketAddr,
    /// Command receiver
    command_rx: mpsc::UnboundedReceiver<UdpTransportCommand>,
    /// Event sender
    event_tx: mpsc::UnboundedSender<UdpTransportEvent>,
    /// Request ID from multitransport request (for tunnel creation)
    request_id: Option<u32>,
    /// Security cookie from multitransport request (for tunnel creation)
    security_cookie: Option<[u8; 16]>,
    /// Tunnel established flag
    tunnel_established: bool,
    /// TLS stream (if TLS handshake completed)
    tls_stream: Option<ironrdp_tls::TlsStream<UdpStreamAdapter>>,
    /// TLS adapter (for feeding packets before/during TLS handshake)
    tls_adapter: Option<UdpStreamAdapter>,
    /// Server name for TLS
    server_name: String,
}

impl UdpTransportManager {
    /// Create a new UDP transport manager
    pub async fn new(
        config: UdpTransportConfig,
        correlation_id: Option<CorrelationId>,
        server_name: String,
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
            connection: Arc::new(Mutex::new(connection)),
            socket: Arc::new(socket),
            server_addr: config.server_addr,
            command_rx,
            event_tx,
            request_id: None,
            security_cookie: None,
            tunnel_established: false,
            tls_stream: None,
            tls_adapter: None,
            server_name,
        };

        Ok((manager, command_tx, event_rx))
    }

    /// Set tunnel creation parameters (must be called before run())
    pub fn set_tunnel_params(&mut self, request_id: u32, security_cookie: [u8; 16]) {
        self.request_id = Some(request_id);
        self.security_cookie = Some(security_cookie);
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

        // Perform TLS handshake with packet feeding (required by MS-RDPEMT spec)
        info!("🔐 Starting TLS handshake over UDP...");
        
        // Create adapter
        let adapter = UdpStreamAdapter::new(
            self.socket.clone(),
            self.connection.clone(),
        );
        self.tls_adapter = Some(adapter.clone());
        
        // Spawn task to feed packets to adapter during handshake
        let socket_clone = self.socket.clone();
        let adapter_clone = adapter.clone();
        let server_addr = self.server_addr;
        let local_addr = self.socket.local_addr().ok();
        let feeder_task = tokio::spawn(async move {
            info!("TLS feeder task: started (local={:?}, server={})", local_addr, server_addr);
            let mut buf = vec![0u8; 65536];
            // Feed packets for up to 10 seconds
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut packet_count = 0;
            let mut timeout_count = 0;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(100), socket_clone.recv_from(&mut buf)).await {
                    Ok(Ok((len, addr))) if addr == server_addr => {
                        packet_count += 1;
                        info!("TLS feeder task: received packet #{} ({} bytes from {})", packet_count, len, addr);
                        if let Err(e) = adapter_clone.feed_packet(&buf[..len]).await {
                            warn!("TLS feeder task: feed_packet error: {}", e);
                        }
                    }
                    Ok(Ok((len, addr))) => {
                        info!("TLS feeder task: ignoring packet from wrong address {} ({} bytes, expected {})", addr, len, server_addr);
                    }
                    Ok(Err(e)) => {
                        warn!("TLS feeder task: recv_from error: {}", e);
                    }
                    Err(_) => {
                        // Timeout - continue loop
                        timeout_count += 1;
                        if timeout_count % 10 == 0 {
                            debug!("TLS feeder task: {} timeouts so far", timeout_count);
                        }
                    }
                }
            }
            info!("TLS feeder task: finished, processed {} packets ({} timeouts)", packet_count, timeout_count);
        });
        
        // Perform TLS handshake
        match tokio::time::timeout(
            Duration::from_secs(10),
            ironrdp_tls::upgrade(adapter, &self.server_name)
        ).await {
            Ok(Ok((tls_stream, _public_key))) => {
                info!("✅ TLS handshake complete!");
                self.tls_stream = Some(tls_stream);
                feeder_task.abort(); // Stop feeding packets
            }
            Ok(Err(e)) => {
                error!("❌ TLS handshake failed: {}", e);
                feeder_task.abort();
                let _ = self.event_tx.send(UdpTransportEvent::Disconnected(
                    format!("TLS handshake failed: {}", e)
                ));
                return Err(anyhow::anyhow!("TLS handshake failed: {}", e));
            }
            Err(_) => {
                error!("❌ TLS handshake timeout");
                feeder_task.abort();
                let _ = self.event_tx.send(UdpTransportEvent::Disconnected(
                    "TLS handshake timeout".to_string()
                ));
                return Err(anyhow::anyhow!("TLS handshake timeout"));
            }
        }

        // Create tunnel if parameters provided
        if let (Some(request_id), Some(security_cookie)) = (self.request_id, self.security_cookie) {
            if let Err(e) = self.create_tunnel(request_id, security_cookie).await {
                error!("Tunnel creation failed: {}", e);
                let _ = self
                    .event_tx
                    .send(UdpTransportEvent::Disconnected(format!("Tunnel creation failed: {}", e)));
                return Err(e);
            }
            // Note: Tunnel will be marked as established when TunnelCreateResponse is received
            // in handle_tunnel_pdu() - look for "✅ Tunnel creation succeeded" message
        } else {
            warn!("No tunnel parameters provided, skipping tunnel creation");
        }

        // Main event loop
        let mut recv_buffer = vec![0u8; 65536];
        let mut check_retransmit_interval = tokio::time::interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                // Handle incoming UDP packets
                result = self.socket.recv_from(&mut recv_buffer) => {
                    match result {
                        Ok((len, addr)) => {
                            debug!("📨 UDP: Received {} bytes from {}", len, addr);
                            if addr == self.server_addr {
                                // Feed to TLS adapter if active
                                if let Some(ref adapter) = self.tls_adapter {
                                    if let Err(e) = adapter.feed_packet(&recv_buffer[..len]).await {
                                        debug!("TLS adapter couldn't process packet: {}", e);
                                        // Fall through to regular handling
                                    } else {
                                        // Packet consumed by TLS
                                        continue;
                                    }
                                }
                                
                                // Regular packet handling
                                if let Err(e) = self.handle_received_packet(&recv_buffer[..len]).await {
                                    warn!("Error handling received packet: {}", e);
                                }
                            } else {
                                warn!("📨 UDP: Ignoring packet from unexpected address: {}", addr);
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
                    let mut conn = self.connection.lock().await;
                    let retransmits = conn.check_retransmits();
                    for packet in retransmits {
                        if let Err(e) = self.socket.send(&packet).await {
                            error!("Failed to retransmit packet: {}", e);
                        } else {
                            trace!("Retransmitted packet ({} bytes)", packet.len());
                        }
                    }

                    // Check for completed FEC blocks and send FEC packets
                    if let Ok(Some(fec_packet)) = conn.check_fec_block() {
                        if let Err(e) = self.socket.send(&fec_packet).await {
                            error!("Failed to send FEC packet: {}", e);
                        } else {
                            trace!("Sent FEC packet ({} bytes)", fec_packet.len());
                        }
                    }

                    // Check if keepalive is needed
                    if conn.needs_keepalive() {
                        if let Ok(ack_packet) = conn.create_ack() {
                            if let Err(e) = self.socket.send(&ack_packet).await {
                                warn!("Failed to send keepalive ACK: {}", e);
                            } else {
                                trace!("Sent keepalive ACK ({} bytes)", ack_packet.len());
                            }
                        }
                    }

                    // Check connection state
                    if conn.state() == ConnectionState::Terminated {
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
        let syn_packet = {
            let mut conn = self.connection.lock().await;
            conn.create_syn()?
        };
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
        {
            let mut conn = self.connection.lock().await;
            conn.process_syn_ack(&syn_ack)
                .context("Failed to process SYN+ACK")?;
        }

        // Send ACK (implicit in first data packet, so just mark as connected)
        info!("UDP handshake complete, connection established");

        Ok(())
    }

    /// Begin TLS handshake over UDP (MS-RDPEMT requirement)
    /// Returns the adapter that the main loop should feed packets into
    fn begin_tls_handshake(&mut self) -> UdpStreamAdapter {
        info!("🔐 Preparing TLS handshake over UDP...");

        // Create stream adapter that we'll feed packets into
        let adapter = UdpStreamAdapter::new(
            self.socket.clone(),
            self.connection.clone(),
        );
        
        adapter
    }

    /// Complete TLS handshake with the adapter
    async fn complete_tls_handshake(&mut self, adapter: UdpStreamAdapter) -> Result<()> {
        info!("🔐 Performing TLS handshake...");
        
        // Use ironrdp-tls to upgrade the connection
        // This will perform the TLS handshake by reading/writing through the adapter
        let (tls_stream, _server_public_key) = ironrdp_tls::upgrade(adapter, &self.server_name)
            .await
            .context("TLS handshake failed")?;

        self.tls_stream = Some(tls_stream);

        info!("✅ TLS handshake complete!");
        Ok(())
    }

    /// Handle received UDP packet
    async fn handle_received_packet(&mut self, packet: &[u8]) -> Result<()> {
        trace!("Processing received packet ({} bytes)", packet.len());

        // Try to process as source packet (data)
        let mut conn = self.connection.lock().await;
        match conn.process_source_packet(packet) {
            Ok(datas) if !datas.is_empty() => {
                drop(conn); // Release lock before async operations
                for data in datas {
                    info!("📦 UDP: Received data packet ({} bytes payload)", data.len());
                    
                    // Dump first 32 bytes for debugging
                    if data.len() > 0 {
                        let dump_len = data.len().min(32);
                        debug!("   First {} bytes: {:02x?}", dump_len, &data[..dump_len]);
                    }
                    
                    // Check if this is a tunnel PDU (MS-RDPEMT)
                    if let Err(e) = self.handle_tunnel_pdu(&data).await {
                        // If not a tunnel PDU, send as regular data
                        debug!("Not a tunnel PDU ({}), forwarding as regular data", e);
                        let _ = self.event_tx.send(UdpTransportEvent::DataReceived(data));
                    }
                }
            }
            Ok(_) => {
                // Packet received but not yet deliverable (out of order)
                debug!("📦 UDP packet buffered (out of sequence or no data yet)");
            }
            Err(e) => {
                // Maybe an ACK or FEC; try to process
                if let Err(err) = conn.process_ack_packet(packet) {
                    debug!("⚠️  Failed to decode UDP packet as source or ACK: source_err={}, ack_err={}", e, err);
                    // Dump packet for analysis
                    let dump_len = packet.len().min(32);
                    debug!("   Packet first {} bytes: {:02x?}", dump_len, &packet[..dump_len]);
                } else {
                    debug!("✓ Processed UDP ACK packet");
                }
            }
        }

        Ok(())
    }

    /// Handle MS-RDPEMT tunnel PDU
    async fn handle_tunnel_pdu(&mut self, data: &[u8]) -> Result<()> {
        use ironrdp_core::{Decode, ReadCursor};

        // Try to parse as tunnel PDU
        let mut cursor = ReadCursor::new(data);
        let tunnel_pdu = match TunnelPdu::decode(&mut cursor) {
            Ok(pdu) => pdu,
            Err(e) => {
                debug!("Failed to decode as tunnel PDU: {:?}", e);
                return Err(anyhow::anyhow!("Not a tunnel PDU: {:?}", e));
            }
        };

        match tunnel_pdu {
            TunnelPdu::CreateResponse { header: _, response } => {
                info!("📥 Received TunnelCreateResponse: hrResponse=0x{:08X}", response.hr_response);
                
                if response.hr_response >= 0 {
                    info!("✅ Tunnel established successfully!");
                    self.tunnel_established = true;
                    // Notify application that tunnel is ready
                    let _ = self.event_tx.send(UdpTransportEvent::TunnelEstablished);
                } else {
                    error!("❌ Tunnel creation failed: hrResponse=0x{:08X}", response.hr_response);
                    return Err(anyhow::anyhow!("Tunnel creation failed with hrResponse=0x{:08X}", response.hr_response));
                }
            }
            TunnelPdu::Data { header, payload } => {
                info!("📦 Received TunnelData: {} bytes (header_length={}, payload_length={})", 
                      payload.len(), header.header_length, header.payload_length);
                
                if !self.tunnel_established {
                    warn!("⚠️ Received TunnelData before tunnel established, buffering...");
                }
                
                // Forward the DVC payload to the application
                let _ = self.event_tx.send(UdpTransportEvent::DataReceived(payload));
            }
            TunnelPdu::CreateRequest { .. } => {
                warn!("⚠️ Received TunnelCreateRequest (unexpected for client)");
            }
        }

        Ok(())
    }

    /// Send data over UDP
    async fn send_data(&mut self, data: Vec<u8>) -> Result<()> {
        trace!("Sending data ({} bytes)", data.len());

        let mut conn = self.connection.lock().await;
        let packet = conn
            .send_data(data)
            .context("Failed to create source packet")?;

        self.socket
            .send(&packet)
            .await
            .context("Failed to send packet")?;

        trace!("Sent source packet ({} bytes)", packet.len());

        // Check if FEC block is complete and send FEC packet
        if let Ok(Some(fec_packet)) = conn.check_fec_block() {
            self.socket
                .send(&fec_packet)
                .await
                .context("Failed to send FEC packet")?;
            debug!("Sent FEC packet ({} bytes)", fec_packet.len());
        }

        Ok(())
    }

    /// Create MS-RDPEMT tunnel for binding DVC channels to this UDP transport
    async fn create_tunnel(&mut self, request_id: u32, security_cookie: [u8; 16]) -> Result<()> {
        info!("🔧 Creating MS-RDPEMT tunnel for request_id={}", request_id);

        // Create TunnelCreateRequest PDU using helper
        let tunnel_request = TunnelPdu::create_request(request_id, security_cookie);

        // Encode the request
        let mut buf = vec![0u8; tunnel_request.size()];
        let mut cursor = WriteCursor::new(&mut buf);
        tunnel_request
            .encode(&mut cursor)
            .context("Failed to encode TunnelCreateRequest")?;

        info!("📤 Sending TunnelCreateRequest ({} bytes tunnel PDU)", buf.len());
        debug!("   Tunnel PDU bytes: {:02x?}", &buf[..buf.len().min(32)]);

        // Send the tunnel request wrapped in MS-RDPEUDP packet
        // Use the UDP connection's send_data to properly frame it
        let mut conn = self.connection.lock().await;
        let udp_packet = conn
            .send_data(buf)
            .context("Failed to create UDP packet for tunnel request")?;
        drop(conn); // Release lock

        debug!("   MS-RDPEUDP packet: {} bytes total", udp_packet.len());
        debug!("   First 32 bytes of UDP packet: {:02x?}", &udp_packet[..udp_packet.len().min(32)]);

        self.socket
            .send(&udp_packet)
            .await
            .context("Failed to send TunnelCreateRequest")?;

        info!("✅ Sent TunnelCreateRequest (UDP packet {} bytes), waiting for response...", udp_packet.len());

        // Note: The response will come through the normal packet handling in handle_received_packet
        // For now, we'll mark the tunnel as pending and handle the response asynchronously
        // This is a simplified implementation - in production, you'd want to:
        // 1. Add a channel/oneshot for waiting on the response
        // 2. Store request_id to match responses
        // 3. Handle timeout if no response comes
        
        // For initial implementation, assume tunnel will be established
        // (response handling will be added in next iteration)
        Ok(())
    }
}

/// Helper to create a UDP transport for video streaming
pub async fn create_video_udp_transport(
    server_addr: SocketAddr,
    correlation_id: Option<CorrelationId>,
    server_name: String,
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

    let (manager, command_tx, event_rx) = UdpTransportManager::new(config, correlation_id, server_name).await?;

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
        let result = UdpTransportManager::new(config, None, "testserver".to_string()).await;
        assert!(result.is_ok());
    }
}
