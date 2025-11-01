/// UDP Transport Manager for RDP
///
/// Handles UDP-based multitransport for RDP, optimized for H.264 video streaming
use std::io::ErrorKind;
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
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, trace, warn};

use crate::dtls_udp::{DtlsConfig, DtlsUdpSocket, TransportSecurityMode};

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
    /// Use DTLS encryption (required when Enhanced RDP Security is in effect)
    pub use_dtls: bool,
}

impl Default for UdpTransportConfig {
    fn default() -> Self {
        Self {
            server_addr: "0.0.0.0:3389".parse().unwrap(),
            local_addr: "0.0.0.0:0".parse().unwrap(),
            mode: TransportMode::Lossy, // Lossy mode better for video
            enable_fec: true,
            protocol_version: UdpProtocolVersion::V3,
            mtu: 1232,
            use_dtls: false, // Default to no DTLS (Standard RDP Security)
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
    /// Server name for DTLS
    server_name: String,
    /// DTLS socket wrapper (if DTLS is required per MS-RDPEMT)
    dtls_socket: Option<DtlsUdpSocket>,
    /// Whether to use DTLS encryption
    use_dtls: bool,
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
        let desired_addr = config.local_addr;
        let socket = match UdpSocket::bind(desired_addr).await {
            Ok(socket) => {
                info!(
                    "UDP socket bound to {} for server {}",
                    socket.local_addr()?,
                    config.server_addr
                );
                socket
            }
            Err(err) if err.kind() == ErrorKind::AddrInUse && desired_addr.port() != 0 => {
                warn!(
                    "UDP bind failed for {} ({}); falling back to ephemeral port",
                    desired_addr, err
                );
                let fallback_addr = SocketAddr::new(desired_addr.ip(), 0);
                let socket = UdpSocket::bind(fallback_addr)
                    .await
                    .context("Failed to bind UDP socket with ephemeral port")?;
                info!(
                    "UDP socket rebound to {} for server {}",
                    socket.local_addr()?,
                    config.server_addr
                );
                socket
            }
            Err(err) => {
                return Err(err).context("Failed to bind UDP socket");
            }
        };

        // Connect to server
        socket
            .connect(config.server_addr)
            .await
            .context("Failed to connect UDP socket")?;

        // Create UDP connection state machine
        let udp_config = UdpConfig {
            mtu: config.mtu,
            initial_sequence_number: rand::random(),
            receive_window_size: 64, // Match Windows RDP client behavior
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

        let use_dtls = config.use_dtls;

        let manager = Self {
            connection: Arc::new(Mutex::new(connection)),
            socket: Arc::new(socket),
            server_addr: config.server_addr,
            command_rx,
            event_tx,
            request_id: None,
            security_cookie: None,
            tunnel_established: false,
            server_name,
            dtls_socket: None,
            use_dtls,
        };

        Ok((manager, command_tx, event_rx))
    }

    /// Set tunnel creation parameters (must be called before run())
    pub fn set_tunnel_params(&mut self, request_id: u32, security_cookie: [u8; 16]) {
        self.request_id = Some(request_id);
        self.security_cookie = Some(security_cookie);

        // Check if security cookie is all zeros (server doesn't support/require authentication)
        let cookie_is_zero = security_cookie.iter().all(|&b| b == 0);

        if cookie_is_zero {
            // Server doesn't require MS-RDPEMT authentication - use UDPv2 without cookie hash
            warn!(
                "⚠️  Security cookie is all zeros - server doesn't require MS-RDPEMT authentication"
            );
            warn!("   Using UDPv2 without cookie hash for compatibility");

            // Set protocol version to V2 (doesn't require cookie hash)
            if let Ok(mut conn) = self.connection.try_lock() {
                conn.set_protocol_version(UdpProtocolVersion::V2);
                info!("   Protocol version set to UDPv2 (0x0002)");
            }
        } else {
            // Compute cookie hash for MS-RDPEMT authentication (required for UDPv3)
            // This hash authenticates the UDP connection to the RDP session
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&security_cookie);
            let hash_raw: [u8; 32] = hasher.finalize().into();

            // Apply 32-bit little-endian byte swap (per MS-RDPEUDP spec)
            let mut cookie_hash = [0u8; 32];
            for i in 0..8 {
                let offset = i * 4;
                cookie_hash[offset] = hash_raw[offset + 3];
                cookie_hash[offset + 1] = hash_raw[offset + 2];
                cookie_hash[offset + 2] = hash_raw[offset + 1];
                cookie_hash[offset + 3] = hash_raw[offset];
            }

            // Set cookie hash on the UDP connection (will be included in SYN packet for UDPv3)
            if let Ok(mut conn) = self.connection.try_lock() {
                conn.set_cookie_hash(cookie_hash);
                info!("✅ Cookie hash set for MS-RDPEMT authentication (UDPv3)");
                info!("   Hash (first 16 bytes): {:02x?}", &cookie_hash[..16]);
            }
        }
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

        // ═══════════════════════════════════════════════════════════════════════
        // TLS/DTLS HANDSHAKE (MS-RDPEMT Requirement with Enhanced RDP Security)
        // ═══════════════════════════════════════════════════════════════════════
        //
        // Per MS-RDPEMT Section 1.5 and Appendix A Footnote <1>:
        // - TLS/DTLS is REQUIRED when Enhanced RDP Security (TLS/CredSSP/RDSTLS) is used
        // - Handshake MUST complete before sending MS-RDPEMT tunnel PDUs
        // - Standard RDP Security (RC4) connections use unencrypted UDP (spec-compliant)
        //
        // Per MS-RDPEMT Section 1.5:
        // - TLS for reliable UDP transport (RDP-UDP-R mode)
        // - DTLS for lossy UDP transport (RDP-UDP-L mode)
        // ═══════════════════════════════════════════════════════════════════════

        // Flag to track if we need to create tunnel (only for non-TLS connections!)
        // TLS connections send TLS data DIRECTLY in RDPUDP DATA packets (no tunnel)
        let mut tunnel_creation_pending =
            !self.use_dtls && self.request_id.is_some() && self.security_cookie.is_some();

        // Flag to track if we need to initiate TLS/DTLS handshake
        // TLS handshake sends data DIRECTLY in RDPUDP (no tunnel wrapping)
        let mut tls_handshake_pending = self.use_dtls;

        // Main event loop
        let mut recv_buffer = vec![0u8; 65536];
        let mut check_retransmit_interval = tokio::time::interval(Duration::from_millis(100));

        loop {
            // Create tunnel on first iteration ONLY for non-TLS connections
            // TLS connections DO NOT use MS-RDPEMT tunnels - they send TLS directly in RDPUDP
            if tunnel_creation_pending {
                tunnel_creation_pending = false;

                info!("ℹ️  TLS/DTLS not required (Standard RDP Security)");
                info!("   Creating UDP tunnel with unencrypted datagrams per MS-RDPEMT Appendix A");

                if let (Some(request_id), Some(security_cookie)) =
                    (self.request_id, self.security_cookie)
                {
                    if let Err(e) = self.create_tunnel(request_id, security_cookie).await {
                        error!("Tunnel creation failed: {}", e);
                        let _ = self.event_tx.send(UdpTransportEvent::Disconnected(format!(
                            "Tunnel creation failed: {}",
                            e
                        )));
                        break;
                    }
                } else {
                    warn!("No tunnel parameters provided, skipping tunnel creation");
                }
            }

            // Initiate TLS/DTLS handshake if required
            // TLS data is sent DIRECTLY in RDPUDP DATA packets (NOT wrapped in tunnel PDUs)
            // This is per MS-RDPEMT spec - tunnels are only for non-TLS connections
            if tls_handshake_pending {
                tls_handshake_pending = false;

                info!("� TLS Handshake Stage 1/3: Generating ClientHello...");

                // Per MS-RDPEMT Section 1.5: TLS for reliable, DTLS for lossy
                let dtls_config = DtlsConfig {
                    server_name: self.server_name.clone(),
                    verify_certificate: false, // TODO: Enable in production
                    mode: TransportSecurityMode::Tls, // TLS for reliable UDP (RDP-UDP-R)
                };

                match DtlsUdpSocket::new(self.server_addr, dtls_config) {
                    Ok(mut dtls) => {
                        // Start TLS handshake to get ClientHello
                        match dtls.start_handshake() {
                            Ok(client_hello) => {
                                info!(
                                    "✅ TLS Handshake Stage 1/3: Sending ClientHello ({} bytes) in RDPUDP DATA",
                                    client_hello.len()
                                );
                                debug!(
                                    "   TLS ClientHello bytes: {:02x?}",
                                    &client_hello[..client_hello.len().min(32)]
                                );

                                // IMPORTANT: Set dtls_socket BEFORE sending so encryption is active
                                self.dtls_socket = Some(dtls);

                                // Send ClientHello DIRECTLY via send_data (no tunnel wrapping!)
                                // This matches FreeRDP behavior
                                match self.send_data(client_hello.clone()).await {
                                    Ok(()) => {
                                        info!(
                                            "✅ UDP Handshake Stage 3/3: ACK+DATA(ClientHello) sent - UDP handshake complete"
                                        );
                                    }
                                    Err(e) => {
                                        error!("❌ Failed to send TLS ClientHello: {}", e);
                                        self.dtls_socket = None;
                                        break;
                                    }
                                }
                                // Server response will be handled in recv_from branch below
                            }
                            Err(e) => {
                                error!("❌ Failed to start TLS handshake: {}", e);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ Failed to create TLS layer: {}", e);
                        let _ = self.event_tx.send(UdpTransportEvent::Disconnected(format!(
                            "TLS initialization failed: {}",
                            e
                        )));
                        break;
                    }
                }
            }

            tokio::select! {
                // Handle incoming UDP packets
                result = self.socket.recv_from(&mut recv_buffer) => {
                    match result {
                        Ok((len, addr)) => {
                            debug!("📨 UDP: Received {} bytes from {}", len, addr);
                            if addr == self.server_addr {
                                // Handle received packet (decrypt if DTLS is active)
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
                    let fec_packet = match conn.check_fec_block() {
                        Ok(packet) => packet,
                        Err(err) => {
                            warn!("Failed to finalize FEC block: {}", err);
                            None
                        }
                    };
                    let ack_packet = if conn.needs_keepalive() {
                        conn.create_ack().ok()
                    } else {
                        None
                    };
                    let terminated = conn.state() == ConnectionState::Terminated;
                    drop(conn);

                    for packet in retransmits {
                        match self.send_over_udp(&packet).await {
                            Ok(()) => trace!("Retransmitted packet ({} bytes)", packet.len()),
                            Err(e) => error!("Failed to retransmit packet: {}", e),
                        }
                    }

                    if let Some(fec_packet) = fec_packet {
                        match self.send_over_udp(&fec_packet).await {
                            Ok(()) => trace!("Sent FEC packet ({} bytes)", fec_packet.len()),
                            Err(e) => error!("Failed to send FEC packet: {}", e),
                        }
                    }

                    if let Some(ack_packet) = ack_packet {
                        if let Err(e) = self.send_over_udp(&ack_packet).await {
                            warn!("Failed to send keepalive ACK: {}", e);
                        } else {
                            trace!("Sent keepalive ACK ({} bytes)", ack_packet.len());
                        }
                    }

                    if terminated {
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

    async fn send_over_udp(&mut self, payload: &[u8]) -> Result<()> {
        debug!("send_over_udp called with {} bytes", payload.len());
        debug!(
            "   First 32 bytes: {:02x?}",
            &payload[..payload.len().min(32)]
        );

        // Check if DTLS is required and handshake is complete
        if let Some(dtls) = self.dtls_socket.as_mut() {
            debug!(
                "   DTLS socket exists, handshake_complete={}",
                dtls.is_handshake_complete()
            );
            if dtls.is_handshake_complete() {
                // DTLS handshake complete - encrypt the payload
                let packets = dtls.encrypt(payload)?;
                if packets.is_empty() {
                    trace!(
                        "DTLS produced no ciphertext for payload ({} bytes), skipping transmit",
                        payload.len()
                    );
                    return Ok(());
                }

                for packet in packets {
                    self.socket
                        .send(&packet)
                        .await
                        .context("Failed to send DTLS packet")?;
                    trace!("Sent DTLS packet ({} bytes)", packet.len());
                }
            } else {
                // DTLS handshake not complete - send payload unencrypted
                // (This is for DTLS handshake messages wrapped in RDP UDP DATA packets)
                debug!(
                    "Sending unencrypted packet ({} bytes) - DTLS handshake in progress",
                    payload.len()
                );
                let sent_bytes = self
                    .socket
                    .send(payload)
                    .await
                    .context("Failed to send UDP packet")?;
                debug!("   Actually sent {} bytes over socket", sent_bytes);
            }
        } else {
            debug!("   No DTLS socket - sending unencrypted");
            // No DTLS - send payload unencrypted
            let sent_bytes = self
                .socket
                .send(payload)
                .await
                .context("Failed to send UDP packet")?;
            debug!("   Actually sent {} bytes over socket", sent_bytes);
        }

        Ok(())
    }

    /// Perform UDP handshake
    async fn handshake(&mut self) -> Result<()> {
        info!("🔵 UDP Handshake Stage 1/3: Creating SYN packet...");

        // Send SYN
        let syn_packet = {
            let mut conn = self.connection.lock().await;
            conn.create_syn()?
        };

        // Log SYN packet details for debugging
        debug!(
            "📤 Sending SYN packet ({} bytes) to {}",
            syn_packet.len(),
            self.server_addr
        );
        if syn_packet.len() >= 64 {
            debug!("   First 64 bytes: {:02x?}", &syn_packet[..64]);
        } else {
            debug!("   Full packet: {:02x?}", syn_packet);
        }

        self.socket
            .send(&syn_packet)
            .await
            .context("Failed to send SYN")?;
        info!(
            "✅ UDP Handshake Stage 1/3: SYN sent to {}",
            self.server_addr
        );

        // Wait for SYN+ACK with timeout
        let mut buffer = vec![0u8; 2048];
        let timeout_duration = Duration::from_secs(5);

        info!("🔵 UDP Handshake Stage 2/3: Waiting for SYN+ACK...");
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

        info!(
            "✅ UDP Handshake Stage 2/3: SYN+ACK received ({} bytes)",
            syn_ack.len()
        );

        // Process SYN+ACK and get the handshake-completing ACK packet
        let (ack_packet, negotiated_version, retransmit_timeout_ms) = {
            let mut conn = self.connection.lock().await;
            let ack_packet = conn
                .process_syn_ack(&syn_ack)
                .map_err(|e| {
                    error!("❌ Failed to process SYN+ACK: {}", e);
                    // Log the first 64 bytes of the packet for debugging
                    let dump_len = syn_ack.len().min(64);
                    error!(
                        "   Packet first {} bytes: {:02x?}",
                        dump_len,
                        &syn_ack[..dump_len]
                    );
                    e
                })
                .context("Failed to process SYN+ACK")?;
            (
                ack_packet,
                conn.protocol_version(),
                conn.retransmit_timeout_ms(),
            )
        };

        info!(
            "🔵 UDP protocol negotiated: {} (retransmit timeout {} ms)",
            negotiated_version, retransmit_timeout_ms
        );

        // ═══════════════════════════════════════════════════════════════════════
        // TLS HANDSHAKE INTEGRATION WITH UDP HANDSHAKE
        // ═══════════════════════════════════════════════════════════════════════
        // Per MS-RDPEUDP and FreeRDP behavior:
        // - For TLS connections: The third handshake packet (ACK) MUST include
        //   the TLS ClientHello as DATA payload (ACK+DATA combined)
        // - For non-TLS: Send pure ACK packet to complete handshake
        //
        // The working capture shows: SYN → SYN+ACK → ACK+DATA(TLS ClientHello)
        // NOT: SYN → SYN+ACK → ACK, then separate DATA(TLS ClientHello)
        // ═══════════════════════════════════════════════════════════════════════

        if self.use_dtls {
            info!("� UDP Handshake Stage 3/3: Will send ACK+DATA with TLS ClientHello");
            // Re-arm ACK-of-ACK so it can be piggybacked on the TLS ClientHello packet
            let mut conn = self.connection.lock().await;
            conn.prepare_combined_handshake_ack();
            drop(conn);
            // DO NOT send pure ACK! The send_data() call with TLS ClientHello
            // will automatically include ACK information and complete the handshake.
        } else {
            info!("🔵 UDP Handshake Stage 3/3: Sending ACK to complete handshake...");
            // Non-TLS connection - send pure ACK to complete handshake
            self.send_over_udp(&ack_packet)
                .await
                .context("Failed to send handshake ACK")?;
            info!("✅ UDP Handshake Stage 3/3: ACK sent - handshake complete");
        }

        Ok(())
    }

    /// Handle received UDP packet
    async fn handle_received_packet(&mut self, packet: &[u8]) -> Result<()> {
        trace!("Processing received packet ({} bytes)", packet.len());

        // First, try to process as RDP UDP packet to extract payload
        let mut conn = self.connection.lock().await;
        let source_result = conn.process_source_packet(packet);
        drop(conn);

        match source_result {
            Ok(payloads) if !payloads.is_empty() => {
                // Successfully extracted payloads from RDP UDP DATA packet(s)
                for payload in payloads {
                    // Check if we're still in DTLS handshake
                    if let Some(dtls) = self.dtls_socket.as_mut() {
                        if !dtls.is_handshake_complete() {
                            // This payload is a DTLS handshake message
                            info!(
                                "🔵 TLS Handshake Stage 2/3: Received server handshake message ({} bytes)",
                                payload.len()
                            );
                            match dtls.process_handshake_data(&payload) {
                                Ok(Some(response_packets)) => {
                                    // DTLS wants to send response packets
                                    for response in response_packets {
                                        info!(
                                            "✅ TLS Handshake Stage 2/3: Sending handshake response ({} bytes)",
                                            response.len()
                                        );
                                        self.send_data(response).await?;
                                    }
                                }
                                Ok(None) => {
                                    // Handshake either complete or waiting for more data
                                    if dtls.is_handshake_complete() {
                                        info!(
                                            "✅ TLS Handshake Stage 3/3: TLS handshake complete - connection secured"
                                        );

                                        // Now that DTLS is complete, create the tunnel
                                        if let (Some(request_id), Some(security_cookie)) =
                                            (self.request_id, self.security_cookie)
                                        {
                                            if !self.tunnel_established {
                                                info!(
                                                    "� MS-RDPEMT Tunnel: Creating tunnel over TLS connection..."
                                                );
                                                if let Err(e) = self
                                                    .create_tunnel(request_id, security_cookie)
                                                    .await
                                                {
                                                    error!(
                                                        "Tunnel creation after DTLS failed: {}",
                                                        e
                                                    );
                                                    return Err(e);
                                                }
                                            }
                                        }
                                    } else {
                                        trace!("DTLS waiting for more handshake data");
                                    }
                                }
                                Err(e) => {
                                    error!("❌ DTLS handshake processing failed: {}", e);
                                    return Err(e);
                                }
                            }
                            continue; // Don't process as tunnel PDU yet
                        }

                        // DTLS handshake complete - decrypt tunnel PDU
                        match dtls.decrypt(&payload) {
                            Ok(decrypted_payloads) => {
                                for decrypted in decrypted_payloads {
                                    self.handle_tunnel_pdu_or_data(&decrypted).await?;
                                }
                            }
                            Err(e) => {
                                warn!("DTLS decrypt failed: {}", e);
                                return Err(e);
                            }
                        }
                    } else {
                        // No DTLS - payload is plaintext tunnel PDU
                        self.handle_tunnel_pdu_or_data(&payload).await?;
                    }
                }
            }
            Ok(_) => {
                // No payloads yet (buffered or out of sequence)
                debug!("📦 UDP packet buffered (out of sequence or no data yet)");
            }
            Err(e) => {
                // Not a source packet, try as ACK packet
                let mut conn = self.connection.lock().await;
                if let Err(ack_err) = conn.process_ack_packet(packet) {
                    let dump_len = packet.len().min(32);
                    debug!(
                        "⚠️  Failed to decode UDP packet: source_err={}, ack_err={}",
                        e, ack_err
                    );
                    debug!(
                        "   Packet first {} bytes: {:02x?}",
                        dump_len,
                        &packet[..dump_len]
                    );
                } else {
                    debug!("✓ Processed UDP ACK packet");
                }
            }
        }

        Ok(())
    }

    /// Handle tunnel PDU or forward as regular data
    async fn handle_tunnel_pdu_or_data(&mut self, data: &[u8]) -> Result<()> {
        info!(
            "📦 UDP: Received data packet ({} bytes payload)",
            data.len()
        );

        if !data.is_empty() {
            let dump_len = data.len().min(32);
            debug!("   First {} bytes: {:02x?}", dump_len, &data[..dump_len]);
        }

        if let Err(e) = self.handle_tunnel_pdu(data).await {
            debug!("Not a tunnel PDU ({}), forwarding as regular data", e);
            let _ = self
                .event_tx
                .send(UdpTransportEvent::DataReceived(data.to_vec()));
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
            TunnelPdu::CreateResponse {
                header: _,
                response,
            } => {
                debug!(
                    "📥 Received TunnelCreateResponse: hrResponse=0x{:08X}",
                    response.hr_response
                );

                if response.hr_response >= 0 {
                    info!("✅ MS-RDPEMT Tunnel: TunnelCreateResponse OK - tunnel established!");
                    self.tunnel_established = true;
                    // Notify application that tunnel is ready
                    let _ = self.event_tx.send(UdpTransportEvent::TunnelEstablished);
                } else {
                    error!(
                        "❌ Tunnel creation failed: hrResponse=0x{:08X}",
                        response.hr_response
                    );
                    return Err(anyhow::anyhow!(
                        "Tunnel creation failed with hrResponse=0x{:08X}",
                        response.hr_response
                    ));
                }
            }
            TunnelPdu::Data { header, payload } => {
                info!(
                    "📦 Received TunnelData: {} bytes (header_length={}, payload_length={})",
                    payload.len(),
                    header.header_length,
                    header.payload_length
                );

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
        debug!("send_data called with {} bytes", data.len());
        trace!("Sending data ({} bytes)", data.len());

        let mut fec_packet: Option<Vec<u8>> = None;
        let packet = {
            let mut conn = self.connection.lock().await;
            debug!("Connection state: {:?}", conn.state());
            let packet = conn
                .send_data(data)
                .context("Failed to create source packet")?;

            debug!("Created source packet: {} bytes", packet.len());

            if let Some(fec) = conn
                .check_fec_block()
                .context("Failed to finalize FEC block")?
            {
                fec_packet = Some(fec);
            }

            packet
        };

        debug!("Sending UDP packet: {} bytes", packet.len());
        self.send_over_udp(&packet).await?;

        debug!("Sent source packet ({} bytes)", packet.len());

        if let Some(fec_packet) = fec_packet {
            self.send_over_udp(&fec_packet).await?;
            debug!("Sent FEC packet ({} bytes)", fec_packet.len());
        }

        Ok(())
    }

    /// Create MS-RDPEMT tunnel for binding DVC channels to this UDP transport
    async fn create_tunnel(&mut self, request_id: u32, security_cookie: [u8; 16]) -> Result<()> {
        info!(
            "� MS-RDPEMT Tunnel: Sending TunnelCreateRequest for request_id={}",
            request_id
        );

        // Create TunnelCreateRequest PDU using helper
        let tunnel_request = TunnelPdu::create_request(request_id, security_cookie);

        // Encode the request
        let mut buf = vec![0u8; tunnel_request.size()];
        let mut cursor = WriteCursor::new(&mut buf);
        tunnel_request
            .encode(&mut cursor)
            .context("Failed to encode TunnelCreateRequest")?;

        debug!(
            "📤 Sending TunnelCreateRequest ({} bytes tunnel PDU)",
            buf.len()
        );
        debug!("   Tunnel PDU bytes: {:02x?}", &buf[..buf.len().min(32)]);

        // Send the tunnel request wrapped in MS-RDPEUDP packet
        // Use the UDP connection's send_data to properly frame it
        let mut conn = self.connection.lock().await;
        let udp_packet = conn
            .send_data(buf)
            .context("Failed to create UDP packet for tunnel request")?;
        drop(conn); // Release lock

        debug!("   MS-RDPEUDP packet: {} bytes total", udp_packet.len());
        debug!(
            "   First 32 bytes of UDP packet: {:02x?}",
            &udp_packet[..udp_packet.len().min(32)]
        );

        self.send_over_udp(&udp_packet)
            .await
            .context("Failed to send TunnelCreateRequest")?;

        info!("✅ MS-RDPEMT Tunnel: TunnelCreateRequest sent, waiting for server response...");

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

    /// Send data through the MS-RDPEMT tunnel
    /// This wraps the data in a TunnelPdu::Data and sends it over UDP
    async fn send_tunnel_data(&mut self, data: Vec<u8>) -> Result<()> {
        if !self.tunnel_established {
            return Err(anyhow::anyhow!(
                "Cannot send tunnel data: tunnel not established"
            ));
        }

        debug!("📤 Sending {} bytes through MS-RDPEMT tunnel", data.len());

        // Create TunnelPdu::Data with the payload
        let tunnel_pdu = TunnelPdu::data(data);

        // Encode the tunnel PDU
        let mut buf = vec![0u8; tunnel_pdu.size()];
        let mut cursor = WriteCursor::new(&mut buf);
        tunnel_pdu
            .encode(&mut cursor)
            .context("Failed to encode TunnelPdu::Data")?;

        debug!("   Tunnel PDU total: {} bytes", buf.len());
        debug!("   Tunnel PDU bytes: {:02x?}", &buf[..buf.len().min(32)]);

        // Send through UDP connection
        let mut conn = self.connection.lock().await;
        let udp_packet = conn
            .send_data(buf)
            .context("Failed to create UDP packet for tunnel data")?;
        drop(conn); // Release lock

        self.send_over_udp(&udp_packet)
            .await
            .context("Failed to send tunnel data")?;

        debug!("✅ Sent {} bytes through tunnel", udp_packet.len());
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
        protocol_version: UdpProtocolVersion::V3,
        ..Default::default()
    };

    let (manager, command_tx, event_rx) =
        UdpTransportManager::new(config, correlation_id, server_name).await?;

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
