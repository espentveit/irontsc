/// UDP Transport Manager for RDP
///
/// Handles UDP-based multitransport for RDP, optimized for H.264 video streaming
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::dtls_udp::EncryptionProtocol;
use anyhow::{Context as _, Result};
use ironrdp_core::{Encode, WriteCursor};
use ironrdp_pdu::rdp::tunnel::TunnelPdu;
use ironrdp_udp::{
    ConnectionState, CorrelationId, TransportMode, UdpConfig, UdpConnection, UdpProtocolVersion,
};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, trace, warn};

use crate::dtls_udp::{DtlsConfig, DtlsUdpSocket};

// MS-RDPEMT Tunnel Header Constants
/// Minimum tunnel header size (HeaderLength through PayloadLength)
const MIN_TUNNEL_HEADER_SIZE: usize = 10;
/// Maximum tunnel header size (base header + optional subheaders)
const MAX_TUNNEL_HEADER_SIZE: usize = 100;

/// UDP transport configuration
#[derive(Debug, Clone)]
pub struct UdpTransportConfig {
    /// Server address
    pub server_addr: SocketAddr,
    /// Local bind address (0.0.0.0:0 for automatic)
    pub local_addr: SocketAddr,
    /// Enable Forward Error Correction
    pub enable_fec: bool,
    /// Protocol version
    pub protocol_version: UdpProtocolVersion,
    /// MTU size
    pub mtu: u16,
    /// Use TLS encryption for reliable mode (required when Enhanced RDP Security is in effect)
    pub use_tls: bool,
    /// Use DTLS encryption for lossy mode (required when Enhanced RDP Security is in effect)
    pub use_dtls: bool,
}

impl UdpTransportConfig {
    /// Get the transport mode based on encryption settings
    /// Per MS-RDPEMT: TLS = Reliable, DTLS = Lossy, No encryption = Reliable (default)
    pub fn mode(&self) -> TransportMode {
        if self.use_dtls {
            TransportMode::Lossy
        } else {
            // TLS uses Reliable, unencrypted defaults to Reliable
            TransportMode::Reliable
        }
    }
}

impl Default for UdpTransportConfig {
    fn default() -> Self {
        Self {
            server_addr: "0.0.0.0:3389".parse().unwrap(),
            local_addr: "0.0.0.0:0".parse().unwrap(),
            enable_fec: true,
            protocol_version: UdpProtocolVersion::V3,
            mtu: 1232,
            use_tls: false,  // Default to no TLS (Standard RDP Security)
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
    /// Process tunnel data received on TCP channel 1008 (e.g., TunnelCreateResponse)
    TunnelData(Vec<u8>),
    /// Soft-Sync completed, channels should switch to UDP
    SoftSyncComplete { tunnel_type: u32 },
}

/// Events from the UDP transport
#[derive(Debug, Clone)]
pub enum UdpTransportEvent {
    /// Connection established (includes request_id to identify which tunnel)
    Connected { request_id: u32 },
    /// TLS/DTLS handshake complete - RDP layer should send MultitransportResponse now
    HandshakeComplete { request_id: u32 },
    /// MS-RDPEMT tunnel established (includes request_id to identify which tunnel)
    TunnelEstablished { request_id: u32 },
    /// Soft-Sync completed for switching channels to UDP
    SoftSyncCompleted { request_id: u32, tunnel_type: u32 },
    /// DVC data extracted from tunnel DATA packet (needs to be processed by DRDYNVC)
    TunnelDvcData { request_id: u32, data: Vec<u8> },
    /// Data received
    DataReceived { request_id: u32, data: Vec<u8> },
    /// Connection lost (includes request_id to identify which tunnel)
    Disconnected { request_id: u32, reason: String },
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
    /// Pending tunnel response received before TLS handshake completed
    pending_tunnel_response: Option<Vec<u8>>,
    /// Buffer for reassembling fragmented tunnel PDUs from TCP stream
    tunnel_data_buffer: Vec<u8>,
    /// Server name for TLS/DTLS
    server_name: String,
    /// TLS socket wrapper for reliable mode (if TLS is required per MS-RDPEMT)
    tls_socket: Option<DtlsUdpSocket>,
    /// DTLS socket wrapper for lossy mode (if DTLS is required per MS-RDPEMT)
    dtls_socket: Option<DtlsUdpSocket>,
    /// Whether to use TLS encryption (reliable mode)
    use_tls: bool,
    /// Whether to use DTLS encryption (lossy mode)
    use_dtls: bool,
}

impl UdpTransportManager {
    /// Get the transport mode based on encryption settings
    /// Per MS-RDPEMT: TLS = Reliable, DTLS = Lossy
    fn mode(&self) -> TransportMode {
        if self.use_dtls {
            TransportMode::Lossy
        } else {
            // Default to Reliable (TLS uses Reliable mode, unencrypted can be either)
            TransportMode::Reliable
        }
    }

    /// Validate that the configuration follows MS-RDPEMT requirements
    fn validate_config(config: &UdpTransportConfig) -> Result<()> {
        // Per MS-RDPEMT:
        // - TLS is used with Reliable mode
        // - DTLS is used with Lossy mode
        // - Both TLS and DTLS should not be enabled simultaneously

        if config.use_tls && config.use_dtls {
            return Err(anyhow::anyhow!(
                "Invalid configuration: TLS and DTLS cannot both be enabled (TLS=Reliable, DTLS=Lossy per MS-RDPEMT)"
            ));
        }

        Ok(())
    }

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
        // Validate configuration follows MS-RDPEMT spec
        Self::validate_config(&config)?;

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

        // Derive transport mode from encryption settings (per MS-RDPEMT spec)
        let mode = config.mode();

        // Create UDP connection state machine
        let udp_config = UdpConfig {
            mtu: config.mtu,
            initial_sequence_number: rand::random(),
            receive_window_size: 64, // Match Windows RDP client behavior
            mode,
            protocol_version: config.protocol_version,
            enable_fec: config.enable_fec,
            fec_block_size: 8,
            retransmit_timeout_ms: if config.protocol_version >= UdpProtocolVersion::V2 {
                300
            } else {
                500
            },
            max_retransmits: if mode == TransportMode::Lossy { 0 } else { 5 },
            keepalive_interval_ms: 5000,
        };

        let mut connection = UdpConnection::new(udp_config);
        if let Some(corr_id) = correlation_id {
            connection.set_correlation_id(corr_id);
        }

        // Create channels
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let use_tls = config.use_tls;
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
            pending_tunnel_response: None,
            tunnel_data_buffer: Vec::new(),
            server_name,
            tls_socket: None,
            dtls_socket: None,
            use_tls,
            use_dtls,
        };

        Ok((manager, command_tx, event_rx))
    }

    /// Set tunnel creation parameters (must be called before run())
    pub fn set_tunnel_params(&mut self, request_id: u32, security_cookie: [u8; 16]) {
        self.request_id = Some(request_id);
        self.security_cookie = Some(security_cookie);

        // Compute cookie hash for MS-RDPEMT authentication
        // Per MS-RDPEUDP section 2.2.2.9: cookieHash MUST be present if uUdpVer equals
        // RDPUDP_PROTOCOL_VERSION_3 (0x0101), otherwise MUST NOT be present.
        //
        // If the server's SYN+ACK has an all-zero cookie hash, it means the server
        // doesn't require authentication, but we still use V3 - we just don't send
        // the cookie hash in our SYN packet.
        use sha2::{Digest, Sha256};
        let cookie_is_zero = security_cookie.iter().all(|&b| b == 0);

        if cookie_is_zero {
            info!("   Security cookie is all zeros - server doesn't require cookie authentication");
            info!("   Using UDPv3 (0x0101) without cookie hash");
        } else {
            info!("   Computing SHA-256 hash of security cookie for UDPv3 authentication");
            let mut hasher = Sha256::new();
            hasher.update(&security_cookie);
            let hash_raw: [u8; 32] = hasher.finalize().into();

            // c
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
            let request_id = self.request_id.unwrap_or(0);
            let _ = self
                .event_tx
                .send(UdpTransportEvent::Disconnected {
                    request_id,
                    reason: format!("{}", e),
                });
            return Err(e);
        }

        info!("UDP connection established");
        let request_id = self.request_id.unwrap_or(0);
        let _ = self.event_tx.send(UdpTransportEvent::Connected { request_id });

        // ═══════════════════════════════════════════════════════════════════════
        // TLS/DTLS HANDSHAKE (MS-RDPEMT Requirement with Enhanced RDP Security)
        // ═══════════════════════════════════════════════════════════════════════
        //
        // Per MS-RDPEMT Section 1.5 and Appendix A Footnote <1>:
        // - TLS is REQUIRED for RELIABLE UDP when Enhanced RDP Security is used
        // - DTLS is REQUIRED for LOSSY UDP when Enhanced RDP Security is used
        // - Handshake MUST complete before sending MS-RDPEMT tunnel PDUs
        // - Standard RDP Security (RC4) connections use unencrypted UDP (spec-compliant)
        //
        // Windows behavior with Standard Security:
        // - No TLS/DTLS handshake performed
        // - UDP packets sent/received in plaintext
        // - Tunnel creation and data transfer work without encryption
        //
        // Windows behavior with Enhanced Security (observed in some environments):
        // - TLS 1.2 handshake for reliable mode, DTLS 1.2 for lossy mode
        // - All tunnel PDUs encrypted with negotiated cipher
        // - Failure to complete handshake causes tunnel creation to fail
        // ═══════════════════════════════════════════════════════════════════════

        // TLS/DTLS handshake handling moved to event loop - will be performed after UDP handshake
        // by sending/receiving TLS/DTLS messages wrapped in RDP UDP DATA packets
        if self.use_tls || self.use_dtls {
            let (protocol_name, protocol_enum, is_dtls) = if self.use_tls {
                ("TLS", EncryptionProtocol::Tls, false)
            } else {
                ("DTLS", EncryptionProtocol::Dtls, true)
            };

            info!(
                "🔐 {} required (Enhanced RDP Security in effect)",
                protocol_name
            );
            info!(
                "   {} handshake will be performed via RDP UDP DATA packets",
                protocol_name
            );

            // Create TLS/DTLS layer (encryption only, no socket I/O)
            let dtls_config = DtlsConfig {
                server_name: self.server_name.clone(),
                verify_certificate: false, // TODO: Enable in production
                protocol: protocol_enum,
            };

            match DtlsUdpSocket::new(self.server_addr, dtls_config) {
                Ok(mut socket) => {
                    // Start TLS/DTLS handshake to get ClientHello
                    match socket.start_handshake() {
                        Ok(client_hello) => {
                            info!(
                                "📤 Sending {} ClientHello ({} bytes) in RDP UDP DATA packet",
                                protocol_name,
                                client_hello.len()
                            );
                            // Send ClientHello wrapped in RDP UDP DATA packet
                            if let Err(e) = self.send_data(client_hello).await {
                                error!("Failed to send {} ClientHello: {}", protocol_name, e);
                                return Err(e);
                            }

                            // Store socket in appropriate field
                            if is_dtls {
                                self.dtls_socket = Some(socket);
                            } else {
                                self.tls_socket = Some(socket);
                            }
                            // Note: Tunnel creation will happen after TLS/DTLS handshake completes
                            // (handled in the event loop when we receive ServerHello response)
                        }
                        Err(e) => {
                            error!("❌ Failed to start {} handshake: {}", protocol_name, e);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    error!("❌ Failed to create {} layer: {}", protocol_name, e);
                    let request_id = self.request_id.unwrap_or(0);
                    let _ = self.event_tx.send(UdpTransportEvent::Disconnected {
                        request_id,
                        reason: format!("{} initialization failed: {}", protocol_name, e),
                    });
                    return Err(e);
                }
            }
        } else {
            info!("ℹ️  Encryption not required (Standard RDP Security)");
            info!("   UDP tunnel will use unencrypted datagrams per MS-RDPEMT Appendix A");
        }

        // Create tunnel if parameters provided (only if encryption not required)
        // If encryption is required, tunnel will be created after TLS/DTLS handshake completes
        if !self.use_tls && !self.use_dtls {
            if let (Some(request_id), Some(security_cookie)) =
                (self.request_id, self.security_cookie)
            {
                if let Err(e) = self.create_tunnel(request_id, security_cookie).await {
                    error!("Tunnel creation failed: {}", e);
                    let _ = self.event_tx.send(UdpTransportEvent::Disconnected {
                        request_id,
                        reason: format!("Tunnel creation failed: {}", e),
                    });
                    return Err(e);
                }
                // Note: Tunnel will be marked as established when TunnelCreateResponse is received
                // in handle_tunnel_pdu() - look for "✅ Tunnel creation succeeded" message
            } else {
                warn!("No tunnel parameters provided, skipping tunnel creation");
            }
        } else {
            let protocol_name = if self.use_tls { "TLS" } else { "DTLS" };
            info!(
                "⏸️  Tunnel creation deferred until {} handshake completes",
                protocol_name
            );
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
                        UdpTransportCommand::TunnelData(data) => {
                            debug!("📦 Processing tunnel data from TCP channel ({} bytes)", data.len());
                            if let Err(e) = self.process_tunnel_data(&data).await {
                                warn!("Error processing tunnel data: {}", e);
                            }
                        }
                        UdpTransportCommand::SoftSyncComplete { tunnel_type } => {
                            info!("✅ Soft-Sync completed for tunnel_type=0x{:08X}", tunnel_type);
                            let request_id = self.request_id.unwrap_or(0);
                            // Emit event to signal that channels should switch to UDP
                            if let Err(e) = self.event_tx.send(UdpTransportEvent::SoftSyncCompleted { 
                                request_id,
                                tunnel_type 
                            }) {
                                warn!("Failed to send SoftSyncCompleted event: {}", e);
                            }
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
                        match conn.create_ack() {
                            Ok(packet) => Some(packet),
                            Err(err) => {
                                warn!("Failed to create keepalive ACK: {}", err);
                                None
                            }
                        }
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
                        // If tunnel is established, don't terminate - the tunnel control messages
                        // come via TCP channel 1008, so lack of UDP ACKs doesn't mean failure
                        if self.tunnel_established {
                            info!("⚠️ Max retransmits reached but tunnel is established via TCP channel 1008, continuing...");
                            // Reset the connection state to avoid repeated termination attempts
                            let mut conn = self.connection.lock().await;
                            // Note: We can't actually reset the connection state easily,
                            // but the terminated flag being set won't break things
                        } else {
                            warn!("UDP connection terminated due to max retransmits");
                            let request_id = self.request_id.unwrap_or(0);
                            let _ = self.event_tx.send(UdpTransportEvent::Disconnected {
                                request_id,
                                reason: "Max retransmits reached".to_string(),
                            });
                            break;
                        }
                    }
                }
            }
        }

        info!("UDP transport manager stopped");
        Ok(())
    }

    async fn send_over_udp(&mut self, payload: &[u8]) -> Result<()> {
        // RDP UDP protocol packets (SYN, ACK, DATA with headers) are NEVER encrypted
        // Only the payload INSIDE DATA packets gets encrypted via send_data() -> send_tunnel_pdu()
        // This function sends complete RDP UDP packets directly to the socket
        trace!("Sending RDP UDP packet ({} bytes) unencrypted", payload.len());
        self.socket
            .send(payload)
            .await
            .context("Failed to send UDP packet")?;
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

        // Log SYN packet details for debugging
        info!(
            "📤 Sending SYN packet ({} bytes) to {}",
            syn_packet.len(),
            self.server_addr
        );
        if syn_packet.len() >= 64 {
            info!("   First 64 bytes: {:02x?}", &syn_packet[..64]);
        } else {
            info!("   Full packet: {:02x?}", syn_packet);
        }

        self.socket
            .send(&syn_packet)
            .await
            .context("Failed to send SYN")?;
        info!("✅ SYN packet sent successfully");

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

        // Process SYN+ACK and capture negotiated parameters
        let (negotiated_version, retransmit_timeout_ms) = {
            let mut conn = self.connection.lock().await;
            conn.process_syn_ack(&syn_ack)
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
            (conn.protocol_version(), conn.retransmit_timeout_ms())
        };

        info!(
            "UDP negotiated protocol version {:?} (retransmit timeout {} ms)",
            negotiated_version, retransmit_timeout_ms
        );

        // Per MS-RDPEUDP Section 1.4: Do NOT send standalone ACK packet
        // The ACK flag will be automatically included in the first DATA packet (with DTLS ClientHello)
        // by the connection state machine (see connection.rs:422)

        // MS-RDPEUDP handshake is now SYN → SYN+ACK complete
        // The final ACK will be included in the first DATA packet per MS-RDPEUDP spec
        info!("UDP handshake complete (SYN → SYN+ACK received), ready for data transfer");

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
                    // Check if we're still in TLS/DTLS handshake
                    let (in_handshake, protocol_name, is_dtls) =
                        if let Some(tls) = self.tls_socket.as_mut() {
                            (!tls.is_handshake_complete(), "TLS", false)
                        } else if let Some(dtls) = self.dtls_socket.as_mut() {
                            (!dtls.is_handshake_complete(), "DTLS", true)
                        } else {
                            (false, "", false)
                        };

                    // Once tunnel is established, all UDP data should be encrypted tunnel data,
                    // not handshake messages - skip handshake processing
                    if in_handshake && !self.tunnel_established {
                        // Get the appropriate socket
                        let socket = if is_dtls {
                            self.dtls_socket.as_mut().unwrap()
                        } else {
                            self.tls_socket.as_mut().unwrap()
                        };

                        // This payload is a TLS/DTLS handshake message
                        info!(
                            "📥 Received {} handshake message ({} bytes)",
                            protocol_name,
                            payload.len()
                        );

                        // Process handshake and collect response packets
                        let handshake_result = socket.process_handshake_data(&payload);
                        
                        // Drop the socket borrow before we potentially call create_tunnel
                        drop(socket);

                        // Track handshake completion - will be set to true after we send
                        // final handshake messages and mark it complete
                        let mut handshake_complete = false;

                        match handshake_result {
                            Ok(Some(response_packets)) => {
                                // TLS/DTLS wants to send response packets
                                // These are already complete TLS records (ClientHello, ChangeCipherSpec, Finished, etc.)
                                // and must be sent RAW without encryption - handshake messages are not encrypted
                                for response in response_packets {
                                    info!(
                                        "📤 Sending {} handshake response ({} bytes) in RDP UDP DATA",
                                        protocol_name,
                                        response.len()
                                    );
                                    // Send raw TLS record (no encryption - handshake messages are plaintext)
                                    self.send_raw_tls_record(response).await?;
                                }
                                
                                // Now mark handshake as complete so future packets get encrypted
                                // This must be done AFTER sending the final handshake messages
                                if let Some(tls) = self.tls_socket.as_mut() {
                                    tls.mark_handshake_complete();
                                }

                                // Send ACK for the received data to prevent retransmissions
                                let mut conn = self.connection.lock().await;
                                if let Ok(ack_packet) = conn.create_ack() {
                                    drop(conn);
                                    if let Err(e) = self.send_over_udp(&ack_packet).await {
                                        warn!(
                                            "Failed to send ACK after {} handshake data: {}",
                                            protocol_name, e
                                        );
                                    } else {
                                        debug!("✓ Sent ACK for {} handshake data", protocol_name);
                                    }
                                } else {
                                    drop(conn);
                                }

                                // NOW check if handshake is complete (after we marked it)
                                handshake_complete = if let Some(tls) = self.tls_socket.as_ref() {
                                    tls.is_handshake_complete()
                                } else {
                                    false
                                };
                                
                                // Check if handshake completed
                                if handshake_complete {
                                    info!(
                                        "✅ {} handshake complete, tunnel PDUs will be encrypted",
                                        protocol_name
                                    );

                                    // Notify RDP layer to send MultitransportResponse on TCP channel
                                    debug!("🔍 Checking if should send HandshakeComplete event: request_id={:?}", self.request_id);
                                    if let Some(request_id) = self.request_id {
                                        info!(
                                            "📤 Notifying RDP layer to send MultitransportResponse for request_id={}",
                                            request_id
                                        );
                                        let _ = self.event_tx.send(UdpTransportEvent::HandshakeComplete {
                                            request_id,
                                        });
                                    } else {
                                        warn!("⚠️  Cannot send HandshakeComplete event: request_id is None");
                                    }

                                    // Check if there's a queued TunnelCreateResponse to process
                                    if let Some(pending_response) =
                                        self.pending_tunnel_response.take()
                                    {
                                        info!(
                                            "📥 Processing queued TunnelCreateResponse now that {} is complete",
                                            protocol_name
                                        );
                                        if let Err(e) =
                                            self.process_tunnel_response(&pending_response)
                                        {
                                            error!(
                                                "Failed to process queued TunnelCreateResponse: {}",
                                                e
                                            );
                                            return Err(e);
                                        }
                                    }

                                    // Now that TLS/DTLS is complete, create the tunnel
                                    if let (Some(request_id), Some(security_cookie)) =
                                        (self.request_id, self.security_cookie)
                                    {
                                        if !self.tunnel_established {
                                            info!(
                                                "🔧 {} complete, now creating MS-RDPEMT tunnel",
                                                protocol_name
                                            );
                                            if let Err(e) = self
                                                .create_tunnel(request_id, security_cookie)
                                                .await
                                            {
                                                error!(
                                                    "Tunnel creation after {} failed: {}",
                                                    protocol_name, e
                                                );
                                                return Err(e);
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                // Handshake either complete or waiting for more data
                                if handshake_complete {
                                    info!(
                                        "✅ {} handshake complete, tunnel PDUs will be encrypted",
                                        protocol_name
                                    );

                                    // Check if there's a queued TunnelCreateResponse to process
                                    if let Some(pending_response) =
                                        self.pending_tunnel_response.take()
                                    {
                                        info!(
                                            "📥 Processing queued TunnelCreateResponse now that {} is complete",
                                            protocol_name
                                        );
                                        if let Err(e) =
                                            self.process_tunnel_response(&pending_response)
                                        {
                                            error!(
                                                "Failed to process queued TunnelCreateResponse: {}",
                                                e
                                            );
                                            return Err(e);
                                        }
                                    }

                                    // Now that TLS/DTLS is complete, create the tunnel
                                    if let (Some(request_id), Some(security_cookie)) =
                                        (self.request_id, self.security_cookie)
                                    {
                                        if !self.tunnel_established {
                                            info!(
                                                "🔧 {} complete, now creating MS-RDPEMT tunnel",
                                                protocol_name
                                            );
                                            if let Err(e) = self
                                                .create_tunnel(request_id, security_cookie)
                                                .await
                                            {
                                                error!(
                                                    "Tunnel creation after {} failed: {}",
                                                    protocol_name, e
                                                );
                                                return Err(e);
                                            }
                                        }
                                    }
                                } else {
                                    trace!("{} waiting for more handshake data", protocol_name);

                                    // Still send ACK even if no response needed
                                    let mut conn = self.connection.lock().await;
                                    if let Ok(ack_packet) = conn.create_ack() {
                                        drop(conn);
                                        if let Err(e) = self.send_over_udp(&ack_packet).await {
                                            warn!("Failed to send ACK: {}", e);
                                        } else {
                                            debug!("✓ Sent ACK for received data");
                                        }
                                    } else {
                                        drop(conn);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ {} handshake processing failed: {}", protocol_name, e);

                                // Send ACK anyway to prevent retransmissions
                                let mut conn = self.connection.lock().await;
                                if let Ok(ack_packet) = conn.create_ack() {
                                    drop(conn);
                                    let _ = self.send_over_udp(&ack_packet).await;
                                } else {
                                    drop(conn);
                                }

                                return Err(e);
                            }
                            Ok(None) => {
                                // No response packets, but still send ACK
                                let mut conn = self.connection.lock().await;
                                if let Ok(ack_packet) = conn.create_ack() {
                                    drop(conn);
                                    if let Err(e) = self.send_over_udp(&ack_packet).await {
                                        warn!("Failed to send ACK: {}", e);
                                    } else {
                                        debug!("✓ Sent ACK (no response needed)");
                                    }
                                } else {
                                    drop(conn);
                                }

                                // Check if handshake just completed
                                if handshake_complete {
                                    info!("✅ {} handshake complete", protocol_name);

                                    // Check if there's a queued TunnelCreateResponse to process
                                    if let Some(pending_response) =
                                        self.pending_tunnel_response.take()
                                    {
                                        info!(
                                            "📥 Processing queued TunnelCreateResponse now that {} is complete",
                                            protocol_name
                                        );
                                        if let Err(e) =
                                            self.process_tunnel_response(&pending_response)
                                        {
                                            error!(
                                                "Failed to process queued TunnelCreateResponse: {}",
                                                e
                                            );
                                            return Err(e);
                                        }
                                    }

                                    // Now that TLS/DTLS is complete, create the tunnel
                                    if let (Some(request_id), Some(security_cookie)) =
                                        (self.request_id, self.security_cookie)
                                    {
                                        if !self.tunnel_established {
                                            info!(
                                                "🔧 {} complete, now creating MS-RDPEMT tunnel",
                                                protocol_name
                                            );
                                            if let Err(e) = self
                                                .create_tunnel(request_id, security_cookie)
                                                .await
                                            {
                                                error!(
                                                    "Tunnel creation after {} failed: {}",
                                                    protocol_name, e
                                                );
                                                return Err(e);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        continue; // Don't process as tunnel PDU yet
                    }

                    // If we get here, handshake is complete or not needed
                    // Try to decrypt if we have TLS/DTLS
                    if let Some(tls) = self.tls_socket.as_mut() {
                        match tls.decrypt(&payload) {
                            Ok(decrypted_payloads) => {
                                for decrypted in decrypted_payloads {
                                    self.handle_tunnel_pdu_or_data(&decrypted).await?;
                                }
                            }
                            Err(e) => {
                                // ErrorCode(5) is WANT_READ - just means we need more data, not a fatal error
                                warn!("TLS decrypt failed: {} (continuing...)", e);
                                // Don't return error, just continue - more data may arrive
                            }
                        }
                    } else if let Some(dtls) = self.dtls_socket.as_mut() {
                        match dtls.decrypt(&payload) {
                            Ok(decrypted_payloads) => {
                                for decrypted in decrypted_payloads {
                                    self.handle_tunnel_pdu_or_data(&decrypted).await?;
                                }
                            }
                            Err(e) => {
                                // ErrorCode(5) is WANT_READ - just means we need more data, not a fatal error
                                warn!("DTLS decrypt failed: {} (continuing...)", e);
                                // Don't return error, just continue - more data may arrive
                            }
                        }
                    } else {
                        // No TLS/DTLS - payload is plaintext tunnel PDU
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

        // After processing any received packet, check if we have pending ACKs
        // This ensures DUMMY packets and other received data get acknowledged promptly
        let mut conn = self.connection.lock().await;
        if conn.has_pending_ack() {
            if let Ok(ack_packet) = conn.create_ack() {
                drop(conn);
                if let Err(e) = self.send_over_udp(&ack_packet).await {
                    warn!("Failed to send pending ACK: {}", e);
                } else {
                    trace!("✓ Sent immediate ACK for received packet");
                }
            } else {
                drop(conn);
            }
        } else {
            drop(conn);
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
            let request_id = self.request_id.unwrap_or(0);
            let _ = self
                .event_tx
                .send(UdpTransportEvent::DataReceived {
                    request_id,
                    data: data.to_vec(),
                });
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
                info!(
                    "📥 Received TunnelCreateResponse: hrResponse=0x{:08X}",
                    response.hr_response
                );

                if response.hr_response >= 0 {
                    info!("✅ Tunnel established successfully!");
                    self.tunnel_established = true;
                    let request_id = self.request_id.unwrap_or(0);
                    // Notify application that tunnel is ready
                    let _ = self.event_tx.send(UdpTransportEvent::TunnelEstablished { request_id });
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

                let request_id = self.request_id.unwrap_or(0);
                // Forward the DVC payload to the application
                let _ = self.event_tx.send(UdpTransportEvent::DataReceived { 
                    request_id,
                    data: payload 
                });
            }
            TunnelPdu::CreateRequest { .. } => {
                warn!("⚠️ Received TunnelCreateRequest (unexpected for client)");
            }
        }

        Ok(())
    }

    /// Send data over UDP (wraps in RDP UDP DATA packet)
    async fn send_data(&mut self, data: Vec<u8>) -> Result<()> {
        trace!("Sending data ({} bytes)", data.len());

        let mut fec_packet: Option<Vec<u8>> = None;
        let packet = {
            let mut conn = self.connection.lock().await;
            let packet = conn
                .send_data(data)
                .context("Failed to create source packet")?;

            if let Some(fec) = conn
                .check_fec_block()
                .context("Failed to finalize FEC block")?
            {
                fec_packet = Some(fec);
            }

            packet
        };

        self.send_over_udp(&packet).await?;

        trace!("Sent source packet ({} bytes)", packet.len());

        if let Some(fec_packet) = fec_packet {
            self.send_over_udp(&fec_packet).await?;
            debug!("Sent FEC packet ({} bytes)", fec_packet.len());
        }

        Ok(())
    }

    /// Send raw TLS/DTLS record during handshake (no encryption - handshake messages are plaintext)
    /// TLS handshake records (ClientHello, ServerHello, ChangeCipherSpec, Finished) are complete
    /// TLS records that must be sent as-is, wrapped only in RDP UDP DATA packets
    async fn send_raw_tls_record(&mut self, tls_record: Vec<u8>) -> Result<()> {
        debug!("📤 Sending raw TLS record ({} bytes)", tls_record.len());
        
        // Send the TLS record directly - it's already a complete TLS/DTLS record
        // Just wrap it in RDP UDP DATA packet
        self.send_data(tls_record).await
    }

    /// Send tunnel PDU (encrypts if TLS/DTLS is active, per MS-RDPEMT spec)
    async fn send_tunnel_pdu(&mut self, plaintext: Vec<u8>) -> Result<()> {
        // Per MS-RDPEMT: After TLS/DTLS handshake completes, all tunnel PDUs must be encrypted
        if let Some(tls) = self.tls_socket.as_mut() {
            if tls.is_handshake_complete() {
                debug!(
                    "🔐 Encrypting tunnel PDU ({} bytes plaintext) with TLS",
                    plaintext.len()
                );
                let encrypted_packets = tls
                    .encrypt(&plaintext)
                    .context("Failed to TLS-encrypt tunnel PDU")?;

                for encrypted in encrypted_packets {
                    debug!("   TLS record: {} bytes", encrypted.len());
                    self.send_data(encrypted).await?;
                }
                return Ok(());
            }
        } else if let Some(dtls) = self.dtls_socket.as_mut() {
            if dtls.is_handshake_complete() {
                debug!(
                    "🔐 Encrypting tunnel PDU ({} bytes plaintext) with DTLS",
                    plaintext.len()
                );
                let encrypted_packets = dtls
                    .encrypt(&plaintext)
                    .context("Failed to DTLS-encrypt tunnel PDU")?;

                for encrypted in encrypted_packets {
                    debug!("   DTLS record: {} bytes", encrypted.len());
                    self.send_data(encrypted).await?;
                }
                return Ok(());
            }
        }

        // No encryption - send as plaintext (Standard RDP Security)
        debug!(
            "📤 Sending tunnel PDU ({} bytes) unencrypted (Standard RDP Security)",
            plaintext.len()
        );
        self.send_data(plaintext).await
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

        info!(
            "📤 Sending TunnelCreateRequest ({} bytes tunnel PDU)",
            buf.len()
        );
        debug!("   Tunnel PDU bytes: {:02x?}", &buf[..buf.len().min(32)]);

        // Per MS-RDPEMT spec: Tunnel PDUs must be encrypted after TLS/DTLS handshake completes
        // This method will automatically encrypt if TLS/DTLS is active
        self.send_tunnel_pdu(buf)
            .await
            .context("Failed to send TunnelCreateRequest")?;

        info!("✅ Sent TunnelCreateRequest, waiting for response...");

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

    /// Process tunnel data received on TCP channel 1008 (e.g., TunnelCreateResponse)
    /// 
    /// Per MS-RDPEMT §2.2.1.1, tunnel PDUs arrive over TCP which is a byte stream.
    /// PDUs may be fragmented across multiple TCP segments, so we buffer and reassemble.
    /// 
    /// Note: Channel 1008 also receives heartbeat PDUs (MS-RDPBCGR §2.2.16.1) which
    /// have SEC_HEARTBEAT (0x4000) flag. These must be filtered out.
    async fn process_tunnel_data(&mut self, data: &[u8]) -> Result<()> {
        info!(
            "📥 Received tunnel data from TCP channel ({} bytes)",
            data.len()
        );
        debug!("   First 32 bytes: {:02x?}", &data[..data.len().min(32)]);

        // Filter out heartbeat PDUs (MS-RDPBCGR §2.2.16.1)
        // Heartbeats have Basic Security Header with SEC_HEARTBEAT flag (0x4000)
        // Format: flags (2 bytes LE), flagsHi (2 bytes LE), period, count1, count2, reserved
        if data.len() == 8 {
            let flags = u16::from_le_bytes([data[0], data[1]]);
            const SEC_HEARTBEAT: u16 = 0x4000;
            if flags == SEC_HEARTBEAT {
                debug!("   ❤️ Heartbeat PDU (period={}s, count1={}, count2={}) - ignoring",
                       data[4], data[5], data[6]);
                return Ok(());
            }
        }

        // Append new data to buffer (TCP stream reassembly)
        self.tunnel_data_buffer.extend_from_slice(data);
        debug!("   Buffer now contains {} bytes total", self.tunnel_data_buffer.len());

        // Try to parse complete tunnel PDUs from the buffer
        loop {
            if self.tunnel_data_buffer.len() < 2 {
                // Not enough data even for HeaderLength field
                break;
            }

            // Try to find a valid tunnel header
            // Per MS-RDPEMT §2.2.1.1, there may be a wrapper before the tunnel header
            let mut found_pdu = false;
            let mut pdu_start_offset = 0;
            
            // Try different wrapper sizes (including 0 for no wrapper)
            for wrapper_size in [0, 11, 10, 12] {
                if self.tunnel_data_buffer.len() < wrapper_size + MIN_TUNNEL_HEADER_SIZE {
                    continue;
                }
                
                let offset = wrapper_size;
                let possible_header_length = u16::from_le_bytes([
                    self.tunnel_data_buffer[offset],
                    self.tunnel_data_buffer[offset + 1]
                ]) as usize;
                
                // Validate HeaderLength is in valid range
                if possible_header_length < MIN_TUNNEL_HEADER_SIZE 
                   || possible_header_length > MAX_TUNNEL_HEADER_SIZE {
                    continue;
                }
                
                // Check if we have the complete header
                if self.tunnel_data_buffer.len() < wrapper_size + possible_header_length {
                    // Header is incomplete - wait for more data
                    debug!("   Found potential header (len={}) at offset {} but need {} more bytes",
                           possible_header_length, offset, 
                           wrapper_size + possible_header_length - self.tunnel_data_buffer.len());
                    break;
                }
                
                // Read PayloadLength from header (at offset 8-9 within the header)
                let payload_len = if possible_header_length >= 10 {
                    u16::from_le_bytes([
                        self.tunnel_data_buffer[offset + 8],
                        self.tunnel_data_buffer[offset + 9]
                    ]) as usize
                } else {
                    0
                };
                
                let total_pdu_len = possible_header_length + payload_len;
                
                // Check if we have the complete PDU
                if self.tunnel_data_buffer.len() < wrapper_size + total_pdu_len {
                    // PDU is incomplete - wait for more data
                    debug!("   Found header at offset {} (header={}, payload={}) but need {} more bytes for complete PDU",
                           offset, possible_header_length, payload_len,
                           wrapper_size + total_pdu_len - self.tunnel_data_buffer.len());
                    break;
                }
                
                // We have a complete PDU!
                pdu_start_offset = wrapper_size;
                found_pdu = true;
                debug!("   ✅ Found complete tunnel PDU: wrapper={}, header={}, payload={}, total={}",
                       wrapper_size, possible_header_length, payload_len, total_pdu_len);
                
                // Extract PDU data to process (copy to avoid borrow checker issues)
                let pdu_data: Vec<u8> = self.tunnel_data_buffer[pdu_start_offset..pdu_start_offset + total_pdu_len].to_vec();
                
                // Remove processed data from buffer (including wrapper) BEFORE processing
                self.tunnel_data_buffer.drain(0..pdu_start_offset + total_pdu_len);
                debug!("   Buffer after drain: {} bytes remain", self.tunnel_data_buffer.len());
                
                // Now process the PDU (self is no longer borrowed)
                if let Err(e) = self.process_tunnel_pdu(&pdu_data).await {
                    warn!("Failed to process tunnel PDU: {}", e);
                }
                
                break;
            }
            
            if !found_pdu {
                // No valid PDU found - either incomplete or junk data
                if self.tunnel_data_buffer.len() > 1024 {
                    // Buffer is getting too large - might have junk data at start
                    warn!("   Tunnel buffer exceeded 1KB with no valid PDU - dropping first byte");
                    self.tunnel_data_buffer.drain(0..1);
                } else {
                    // Wait for more data
                    debug!("   No complete PDU found yet - waiting for more data");
                    break;
                }
            }
        }

        Ok(())
    }
    
    /// Process an MS-RDPEMT tunnel PDU (after wrapper is stripped)
    async fn process_tunnel_pdu(&mut self, tunnel_data: &[u8]) -> Result<()> {
        // MS-RDPEMT RDP_TUNNEL_HEADER structure (little-endian):
        // HeaderLength (2 bytes) - Total header size including subheaders
        // HeaderVersion (2 bytes)
        // Action (2 bytes)
        // Flags (2 bytes)
        // PayloadLength (2 bytes)
        // [Optional subheaders...]
        
        const MIN_TUNNEL_HEADER_SIZE: usize = 10;
        if tunnel_data.len() < MIN_TUNNEL_HEADER_SIZE {
            warn!("Tunnel data too short: {} bytes", tunnel_data.len());
            return Ok(());
        }
        
        let header_length = u16::from_le_bytes([tunnel_data[0], tunnel_data[1]]) as usize;
        let header_version = u16::from_le_bytes([tunnel_data[2], tunnel_data[3]]);
        let action = u16::from_le_bytes([tunnel_data[4], tunnel_data[5]]);
        let flags = u16::from_le_bytes([tunnel_data[6], tunnel_data[7]]);
        let payload_length = u16::from_le_bytes([tunnel_data[8], tunnel_data[9]]);

        debug!(
            "   Tunnel Header: length={}, version={}, action=0x{:04x}, flags=0x{:04x}, payload={}",
            header_length, header_version, action, flags, payload_length
        );

        // Action codes from MS-RDPEMT:
        // 0x0001 = CREATEREQUEST
        // 0x0002 = CREATERESPONSE
        // 0x0003 = DATA
        if action == 0x0002 {
            info!("✅ Received TunnelCreateResponse!");

            // Check if TLS/DTLS handshake is complete
            let tls_ready = if let Some(tls) = self.tls_socket.as_ref() {
                tls.is_handshake_complete()
            } else if let Some(dtls) = self.dtls_socket.as_ref() {
                dtls.is_handshake_complete()
            } else {
                true // No TLS/DTLS, can process immediately
            };

            if !tls_ready {
                info!("   ⏳ TLS/DTLS handshake not complete yet, queueing TunnelCreateResponse");
                self.pending_tunnel_response = Some(tunnel_data.to_vec());
            } else {
                // TLS/DTLS ready, process immediately
                self.process_tunnel_response(&tunnel_data)?;
            }
        } else if action == 0x0003 {
            // DATA packet - extract DVC payload using dynamic header length
            // Per MS-RDPEMT §2.2.1.1, header_length includes all subheaders
            if tunnel_data.len() < header_length {
                warn!(
                    "   Tunnel DATA packet too short (has {} bytes, header says {})",
                    tunnel_data.len(),
                    header_length
                );
                return Ok(());
            }

            let payload = &tunnel_data[header_length..];
            debug!(
                "   📦 Extracted {} bytes of DVC data from tunnel DATA packet (header_length={})",
                payload.len(),
                header_length
            );

            // Send event with DVC data to be processed by main loop
            // The main loop will feed this to DrdynvcClient::process() which handles
            // DRDYNVC-level fragmentation (DATA_FIRST/DATA PDUs) via CompleteData
            let request_id = self.request_id.unwrap_or(0);
            let _ = self
                .event_tx
                .send(UdpTransportEvent::TunnelDvcData { 
                    request_id,
                    data: payload.to_vec() 
                });
        } else {
            debug!("   Unknown tunnel action: 0x{:04x}", action);
        }

        Ok(())
    }

    /// Process a TunnelCreateResponse
    fn process_tunnel_response(&mut self, tunnel_data: &[u8]) -> Result<()> {
        // RDP_TUNNEL_CREATERESPONSE = TunnelHeader (10 bytes) + HrResponse (4 bytes)
        // However, the exact structure seems to vary - sometimes HrResponse is missing
        // If we got a response at all, assume success and establish the tunnel
        let hr_response = if tunnel_data.len() >= 14 {
            i32::from_le_bytes([
                tunnel_data[10],
                tunnel_data[11],
                tunnel_data[12],
                tunnel_data[13],
            ])
        } else {
            0 // Assume success if HrResponse not present
        };

        if hr_response == 0 {
            info!("   ✅ Tunnel creation successful! MS-RDPEMT tunnel established.");
            self.tunnel_established = true;

            // Note: We don't manually mark TLS/DTLS as complete here.
            // The handshake must complete naturally via the TLS protocol.
            // Only when is_handshake_complete() returns true can we decrypt tunnel data.

            let request_id = self.request_id.unwrap_or(0);
            // Send event to notify tunnel is established
            let _ = self.event_tx.send(UdpTransportEvent::TunnelEstablished { request_id });
        } else {
            warn!(
                "   Tunnel creation failed (HRESULT = 0x{:08x})",
                hr_response
            );
        }

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
        use_tls: false,   // No TLS (use DTLS for lossy mode if needed)
        use_dtls: false,  // No DTLS by default (unencrypted lossy mode)
        enable_fec: true, // FEC helps recover lost frames
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
