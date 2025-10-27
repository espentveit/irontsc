use core::num::NonZeroU16;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp::cliprdr::pdu::FileDescriptor;
use ironrdp::cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, ClipboardFormatName,
    ClipboardGeneralCapabilityFlags, FileContentsFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse, PackedFileList,
};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{ConnectionResult, ConnectorResult};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::graphics::pointer::DecodedPointer;
use ironrdp::pdu::PduResult;
use ironrdp::pdu::basic_output::fast_path::FastPathUpdate;
use ironrdp::pdu::basic_output::orders::DrawingOrder;
use ironrdp::pdu::geometry::Rectangle;
use ironrdp::pdu::input::fast_path::FastPathInputEvent;
use ironrdp::pdu::rdp::headers::BasicSecurityHeaderFlags;
use ironrdp::pdu::rdp::multitransport::{
    InitiateMultitransportRequest, InitiateMultitransportResponse, MultitransportProtocol,
};
use ironrdp::session::desktop_composition::DesktopCompositionHandler;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{
    ActiveStage, ActiveStageOutput, GracefulDisconnectReason, SessionResult, fast_path,
};
use ironrdp::svc::{ChannelFlags, SvcMessage};
use ironrdp::{cliprdr, connector, rdpdr, rdpsnd, session};
use ironrdp_connector::legacy;
use ironrdp_core::impl_as_any;
use ironrdp_core::{Encode, IntoOwned, WriteBuf, WriteCursor};
use ironrdp_rdpsnd_native::cpal;
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use ironrdp_tokio::{FramedWrite, single_sequence_step_read, split_tokio_framed};
use ironrdp_udp::{
    CorrelationId, SynAckPacket, SynData, SynDataEx, SynDataExFlags, SynPacket, UdpProtocolVersion,
};
use rdpdr::NoopRdpdrBackend;
use smallvec::SmallVec;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, error, info, trace, warn};

use arboard::Clipboard;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Import UDP transport manager for proper integration
use crate::gfx_channel::GfxDvcProcessor;
use crate::udp_transport::{
    UdpTransportCommand, UdpTransportConfig, UdpTransportEvent, UdpTransportManager,
};
use ironrdp_udp::TransportMode;

use crate::config::{Config, Destination, RDCleanPathConfig};
use anyhow::Context as _;
use rand::RngCore;
use rand::SeedableRng;
use sha2::{Digest, Sha256};

fn to_utf16_bytes(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

// Trait for sending RDP output events to the UI
pub trait RdpEventSender: Send + 'static {
    fn send_event(&self, event: RdpOutputEvent) -> Result<(), ()>;
}

#[derive(Debug, Clone, Copy)]
pub struct ImageRegion {
    pub x: u16,
    pub y: u16,
    pub width: NonZeroU16,
    pub height: NonZeroU16,
}

#[derive(Debug)]
pub enum RdpOutputEvent {
    Image {
        buffer: Vec<u8>,
        width: NonZeroU16,
        height: NonZeroU16,
        region: Option<ImageRegion>,
    },
    ConnectionFailure(connector::ConnectorError),
    PointerDefault,
    PointerHidden,
    PointerPosition {
        x: u16,
        y: u16,
    },
    PointerBitmap(Arc<DecodedPointer>),
    Terminated(SessionResult<GracefulDisconnectReason>),
}

#[derive(Debug)]
pub enum RdpInputEvent {
    Resize {
        width: u16,
        height: u16,
        scale_factor: u32,
        /// The physical size of the display in millimeters (width, height).
        physical_size: Option<(u32, u32)>,
    },
    FastPath(SmallVec<[FastPathInputEvent; 2]>),
    Close,
    Clipboard(ClipboardMessage),
    ClipboardFileContents(FileContentsResponse<'static>),
    SendDvcMessages {
        channel_id: u32,
        messages: Vec<SvcMessage>,
    },
}

impl RdpInputEvent {
    pub fn create_channel() -> (mpsc::UnboundedSender<Self>, mpsc::UnboundedReceiver<Self>) {
        mpsc::unbounded_channel()
    }
}

pub struct DvcPipeProxyFactory;

impl DvcPipeProxyFactory {
    pub fn new<T>(_sender: T) -> Self {
        Self
    }

    pub fn create(&self, _channel_name: String, _pipe_name: String) -> Option<()> {
        // Placeholder implementation since DvcNamedPipeProxy is not available
        None
    }
}

pub type WriteDvcMessageFn = Box<dyn Fn(u32, SvcMessage) -> PduResult<()> + Send + 'static>;

pub struct RdpClient<T: RdpEventSender + Clone> {
    pub config: Config,
    pub event_loop_proxy: T,
    pub input_event_receiver: mpsc::UnboundedReceiver<RdpInputEvent>,
    pub cliprdr_factory: Option<Box<dyn CliprdrBackendFactory + Send>>,
    pub dvc_pipe_proxy_factory: DvcPipeProxyFactory,
}

impl<T: RdpEventSender + Clone> RdpClient<T> {
    pub async fn run(mut self) {
        loop {
            let (connection_result, framed, client_addr) =
                if let Some(rdcleanpath) = self.config.rdcleanpath.as_ref() {
                    match connect_ws(
                        &self.config,
                        rdcleanpath,
                        self.cliprdr_factory.as_deref(),
                        &self.dvc_pipe_proxy_factory,
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            let _ = self
                                .event_loop_proxy
                                .send_event(RdpOutputEvent::ConnectionFailure(e));
                            break;
                        }
                    }
                } else {
                    match connect(
                        &self.config,
                        self.cliprdr_factory.as_deref(),
                        &self.dvc_pipe_proxy_factory,
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            let _ = self
                                .event_loop_proxy
                                .send_event(RdpOutputEvent::ConnectionFailure(e));
                            break;
                        }
                    }
                };

            match active_session(
                framed,
                connection_result,
                &self.event_loop_proxy,
                &mut self.input_event_receiver,
                self.config.destination.clone(),
                client_addr,
            )
            .await
            {
                Ok(RdpControlFlow::ReconnectWithNewSize { width, height }) => {
                    info!(
                        width,
                        height, "Fast reconnect requested to apply new desktop size"
                    );
                    self.config.connector.desktop_size.width = width;
                    self.config.connector.desktop_size.height = height;
                }
                Ok(RdpControlFlow::TerminatedGracefully(reason)) => {
                    let _ = self
                        .event_loop_proxy
                        .send_event(RdpOutputEvent::Terminated(Ok(reason)));
                    break;
                }
                Err(e) => {
                    let _ = self
                        .event_loop_proxy
                        .send_event(RdpOutputEvent::Terminated(Err(e)));
                    break;
                }
            }
        }
    }
}

enum RdpControlFlow {
    ReconnectWithNewSize { width: u16, height: u16 },
    TerminatedGracefully(GracefulDisconnectReason),
}

trait AsyncReadWrite: AsyncRead + AsyncWrite {}

impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite {}

type UpgradedFramed = ironrdp_tokio::TokioFramed<Box<dyn AsyncReadWrite + Unpin + Send + Sync>>;

async fn connect(
    config: &Config,
    cliprdr_factory: Option<&(dyn CliprdrBackendFactory + Send)>,
    dvc_pipe_proxy_factory: &DvcPipeProxyFactory,
) -> ConnectorResult<(ConnectionResult, UpgradedFramed, SocketAddr)> {
    let _ = dvc_pipe_proxy_factory;

    let dest = format!(
        "{}:{}",
        config.destination.name(),
        config.destination.port()
    );

    // Gateway connection disabled for now
    if config.gw.is_some() {
        return Err(connector::general_err!(
            "Gateway connections not yet supported"
        ));
    }

    let stream = TcpStream::connect(dest)
        .await
        .map_err(|e| connector::custom_err!("TCP connect", e))?;
    let client_addr = stream
        .local_addr()
        .map_err(|e| connector::custom_err!("get socket local address", e))?;

    let mut framed = ironrdp_tokio::TokioFramed::new(stream);

    #[allow(unused_mut)]
    let mut drdynvc = ironrdp::dvc::DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));

    // Add RDPEGFX channel (falls back to progressive mode when H.264 is absent)
    {
        use crate::gfx::GfxState;
        use crate::gfx_channel::GfxDvcProcessor;

        if cfg!(feature = "h264") {
            info!("Initializing RDPEGFX (H.264) support...");
        } else {
            info!("Initializing RDPEGFX (progressive-only, H.264 disabled)...");
        }

        // Create a simple event sender for GFX (sends to nowhere during init)
        struct DummyEventSender;
        impl RdpEventSender for DummyEventSender {
            fn send_event(&self, _event: RdpOutputEvent) -> Result<(), ()> {
                Ok(()) // Silently ignore during connection phase
            }
        }

        let gfx_state =
            GfxState::new(Box::new(DummyEventSender)).expect("Failed to initialize GFX state");
        let gfx_processor = GfxDvcProcessor::new(gfx_state);

        drdynvc = drdynvc.with_dynamic_channel(gfx_processor);

        if cfg!(feature = "h264") {
            info!("RDPEGFX channel registered with DRDYNVC (H.264 decoder ready)");
        } else {
            info!("RDPEGFX channel registered with DRDYNVC (progressive mode only)");
        }
    }

    // Add Video Redirection channels if feature is enabled
    #[cfg(feature = "video-redirection")]
    {
        use crate::geometry_channel::GeometryProcessor;
        use crate::video_control_channel::VideoControlProcessor;
        use crate::video_data_channel::VideoDataProcessor;
        use crate::video_redirect::VideoRedirectionManager;

        info!("Initializing Video Redirection (MS-RDPEVOR) support...");

        // Create a simple event sender for video (sends to nowhere during init)
        struct DummyEventSender;
        impl RdpEventSender for DummyEventSender {
            fn send_event(&self, _event: RdpOutputEvent) -> Result<(), ()> {
                Ok(()) // Silently ignore during connection phase
            }
        }

        // Create shared video redirection manager
        let manager = Arc::new(Mutex::new(
            VideoRedirectionManager::new(Box::new(DummyEventSender))
                .expect("Failed to initialize Video Redirection manager"),
        ));

        // Register all three channels
        let control_processor = VideoControlProcessor::new(Arc::clone(&manager));
        let data_processor = VideoDataProcessor::new(Arc::clone(&manager));
        let geometry_processor = GeometryProcessor::new(Arc::clone(&manager));

        drdynvc = drdynvc
            .with_dynamic_channel(control_processor)
            .with_dynamic_channel(data_processor)
            .with_dynamic_channel(geometry_processor);

        info!("Video Redirection channels registered with DRDYNVC (Control + Data + Geometry)");
    }

    #[cfg(not(feature = "video-redirection"))]
    {
        info!(
            "Video Redirection support disabled (rebuild with --features video-redirection to enable)"
        );
    }

    // TODO: DVC proxies not yet implemented
    // Instantiate all DVC proxies
    // for proxy in config.dvc_pipe_proxies.iter() {
    //     let channel_name = proxy.channel_name.clone();
    //     let pipe_name = proxy.pipe_name.clone();

    //     trace!(%channel_name, %pipe_name, "Creating DVC proxy");

    //     drdynvc = drdynvc.with_dynamic_channel(dvc_pipe_proxy_factory.create(channel_name, pipe_name));
    // }

    let mut connector = connector::ClientConnector::new(config.connector.clone(), client_addr)
        .with_static_channel(drdynvc)
        .with_static_channel(rdpsnd::client::Rdpsnd::new(Box::new(
            cpal::RdpsndBackend::new(),
        )))
        .with_static_channel(
            rdpdr::Rdpdr::new(Box::new(NoopRdpdrBackend {}), "IronRDP".to_owned())
                .with_smartcard(0),
        );

    if let Some(builder) = cliprdr_factory {
        let backend = builder.build_cliprdr_backend();

        let cliprdr = cliprdr::Cliprdr::new(backend);

        connector.attach_static_channel(cliprdr);
    }

    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector).await?;

    debug!("TLS upgrade");

    // Ensure there is no leftover
    let (initial_stream, leftover_bytes) = framed.into_inner();

    let (upgraded_stream, server_public_key) =
        ironrdp_tls::upgrade(initial_stream, config.destination.name())
            .await
            .map_err(|e| connector::custom_err!("TLS upgrade", e))?;

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);

    let erased_stream = Box::new(upgraded_stream) as Box<dyn AsyncReadWrite + Unpin + Send + Sync>;
    let mut upgraded_framed =
        ironrdp_tokio::TokioFramed::new_with_leftover(erased_stream, leftover_bytes);

    let connection_result = ironrdp_tokio::connect_finalize(
        upgraded,
        &mut upgraded_framed,
        connector,
        (&config.destination).into(),
        server_public_key,
        Some(&mut ReqwestNetworkClient::new()),
        None,
    )
    .await?;

    debug!(?connection_result);

    // ═══════════════════════════════════════════════════════════════════════
    // UDP MULTITRANSPORT ATTEMPT (Optimistic, May Timeout)
    // ═══════════════════════════════════════════════════════════════════════
    //
    // NOTE: This attempts UDP connection immediately after TCP handshake.
    // According to MS-RDPEUDP spec, proper flow requires:
    //
    // 1. Server advertises MULTITRANSPORT support in RDP capabilities
    // 2. Server sends "Initiate Multitransport Request PDU" (MS-RDPBCGR 2.2.15.1)
    // 3. Client uses correlation ID from RDP_NEG_CORRELATION_INFO
    // 4. Client initiates UDP with matching correlation ID
    //
    // We're currently doing "optimistic UDP" - trying immediately with a
    // random correlation ID. This will timeout (~1.5s) if:
    // - Server requires prior multitransport negotiation (most servers)
    // - Server doesn't support UDP (Windows Server 2012 R2 and earlier)
    // - Network/firewall blocks UDP port 3389
    //
    // The timeout is EXPECTED and handled gracefully with TCP fallback.
    // All functionality works via TCP. UDP would provide 20-50ms lower
    // latency for video streaming, but isn't required.
    //
    // TODO: Implement full MS-RDPBCGR multitransport negotiation:
    //       - Wait for server's Initiate Multitransport Request PDU
    //       - Use proper correlation ID from RDP negotiation
    //       - Support RDPUDP_PROTOCOL_VERSION_3 with securityCookie hash
    // ═══════════════════════════════════════════════════════════════════════

    // NOTE: Client-initiated UDP is disabled. According to MS-RDPBCGR Section 1.3.3,
    // the client MUST wait for the server to send an "Initiate Multitransport Request"
    // PDU during the active session. UDP establishment is now handled in active_session()
    // when the server sends SEC_TRANSPORT_REQ.

    info!("✅ Multitransport capability advertised, waiting for server request...");

    // Extract correlation_id from connection_result for later use
    let correlation_id = connection_result.correlation_id;
    if let Some(ref corr_id) = correlation_id {
        debug!(
            "Correlation ID available for multitransport: {:02x?}",
            &corr_id[..8]
        );
    } else {
        warn!("⚠️  No correlation ID - server-initiated UDP multitransport will not be possible");
    }

    Ok((connection_result, upgraded_framed, client_addr))
}

/// Establish UDP transport using the transport manager
/// Returns (command_sender, event_receiver) for controlling the UDP transport
async fn establish_udp_transport(
    destination: Destination,
    client_addr: SocketAddr,
    correlation_id: [u8; 16],
    use_lossy_mode: bool,
    request_id: Option<u32>,
    security_cookie: Option<[u8; 16]>,
    selected_protocol: ironrdp::pdu::nego::SecurityProtocol,
) -> anyhow::Result<(
    mpsc::UnboundedSender<UdpTransportCommand>,
    mpsc::UnboundedReceiver<UdpTransportEvent>,
)> {
    info!("🔌 Establishing UDP transport for {}", destination.name());

    // Determine if DTLS is required based on security protocol
    // Per MS-RDPEMT Appendix A Footnote <1>:
    // - Enhanced RDP Security (TLS/CredSSP/RDSTLS) requires DTLS
    // - Standard RDP Security (RC4) uses unencrypted UDP
    let use_dtls = !selected_protocol.is_standard_rdp_security();

    if use_dtls {
        info!("🔐 Enhanced RDP Security detected: DTLS will be required for UDP");
        info!("   Selected protocol: {}", selected_protocol);
    } else {
        info!("ℹ️  Standard RDP Security: UDP will use unencrypted datagrams");
    }

    let server_addr = SocketAddr::new(destination.name().parse()?, destination.port());
    let server_name = destination.name().to_string();

    let config = UdpTransportConfig {
        server_addr,
        local_addr: SocketAddr::new(client_addr.ip(), client_addr.port()),
        mode: if use_lossy_mode {
            TransportMode::Lossy
        } else {
            TransportMode::Reliable
        },
        enable_fec: true,
        protocol_version: UdpProtocolVersion::V3,
        mtu: 1232,
        use_dtls,
    };

    let corr_id = CorrelationId::new(correlation_id);
    let (mut manager, command_tx, event_rx) =
        UdpTransportManager::new(config, Some(corr_id), server_name).await?;

    // Set MS-RDPEMT tunnel parameters if provided
    if let (Some(req_id), Some(cookie)) = (request_id, security_cookie) {
        info!("🔐 Setting MS-RDPEMT tunnel params: request_id={}", req_id);
        manager.set_tunnel_params(req_id, cookie);
    }

    // Spawn the transport manager task
    tokio::spawn(async move {
        if let Err(e) = manager.run().await {
            error!("UDP transport manager error: {}", e);
        }
    });

    info!("✅ UDP transport manager created and running");

    Ok((command_tx, event_rx))
}

async fn try_establish_rdpudp(
    destination: Destination,
    client_addr: SocketAddr,
    correlation_id: Option<[u8; 16]>,
) {
    // Use reliable mode (false) as default when protocol type not specified
    if let Err(err) = establish_rdpudp(destination, client_addr, correlation_id, None, false).await
    {
        debug!("RDP-UDP handshake failed: {err:?}");
    }
}

async fn establish_rdpudp(
    destination: Destination,
    client_addr: SocketAddr,
    correlation_id: Option<[u8; 16]>,
    cookie_hash: Option<[u8; 32]>,
    use_lossy_mode: bool,
) -> anyhow::Result<UdpSocket> {
    info!("🔌 Starting UDP handshake for {}", destination.name());

    if let Some(ref hash) = cookie_hash {
        debug!(
            "Using security cookie hash for authentication: {:02x?}",
            &hash[..8]
        );
    }

    let desired_bind_addr = SocketAddr::new(client_addr.ip(), client_addr.port());
    debug!(
        "Binding UDP socket to match TCP source: {}",
        desired_bind_addr
    );

    let socket = match UdpSocket::bind(desired_bind_addr).await {
        Ok(socket) => socket,
        Err(err) => {
            warn!(
                "Failed to bind UDP to TCP source {} ({}), retrying with unspecified IP",
                desired_bind_addr, err
            );
            let fallback_ip = match client_addr.ip() {
                IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            };

            match UdpSocket::bind(SocketAddr::new(fallback_ip, client_addr.port())).await {
                Ok(socket) => socket,
                Err(fallback_err) => {
                    warn!(
                        "Fallback bind to {}:{} failed ({}); using ephemeral port",
                        fallback_ip,
                        client_addr.port(),
                        fallback_err
                    );

                    UdpSocket::bind(SocketAddr::new(fallback_ip, 0))
                        .await
                        .with_context(|| {
                            format!("bind fallback UDP socket after error: {fallback_err}")
                        })?
                }
            }
        }
    };

    let local_addr = socket.local_addr()?;
    info!("UDP socket bound to local address: {}", local_addr);

    let server_addr = format!("{}:{}", destination.name(), destination.port());
    debug!("Connecting UDP socket to: {}", server_addr);

    socket
        .connect(&server_addr)
        .await
        .context("connect UDP socket to RDP server")?;

    info!("UDP socket connected to: {}", server_addr);

    // Use correlation_id from X.224 negotiation, or generate a new one if not available
    let correlation_bytes = if let Some(corr_id) = correlation_id {
        debug!(
            "Using correlation ID from X.224 negotiation: {:02x?}",
            &corr_id[..8]
        );
        corr_id
    } else {
        let mut rng = rand::rngs::StdRng::from_entropy();
        let mut correlation_bytes = [0u8; 16];
        loop {
            rng.fill_bytes(&mut correlation_bytes);
            if correlation_bytes[0] != 0x00
                && correlation_bytes[0] != 0xF4
                && !correlation_bytes.iter().any(|&b| b == 0x0D)
            {
                break;
            }
        }
        debug!(
            "Generated new correlation ID: {:02x?}",
            &correlation_bytes[..8]
        );
        correlation_bytes
    };

    let mut rng = rand::rngs::StdRng::from_entropy();

    let syn_data = SynData {
        initial_sequence_number: rng.next_u32(),
        upstream_mtu: 1232,
        downstream_mtu: 1232,
    };

    debug!(
        "Creating SYN packet: seq={}, upstream_mtu={}, downstream_mtu={}",
        syn_data.initial_sequence_number, syn_data.upstream_mtu, syn_data.downstream_mtu
    );

    let syn_packet = SynPacket::new(
        256,            // receive window size (256 packets is the standard per MS-RDPEUDP spec)
        use_lossy_mode, // syn_lossy flag from server's requested protocol type
        syn_data,
        Some(CorrelationId::new(correlation_bytes)),
        // Try UDP v3 with authentication to match working packet captures
        // v3 is what Windows 11 prefers based on pcapng analysis
        cookie_hash.map(|hash| {
            SynDataEx {
                flags: SynDataExFlags::VERSION_INFO_VALID,
                udp_version: Some(UdpProtocolVersion::V3), // V3 matches working captures
                cookie_hash: Some(hash),
            }
        }),
    );

    let payload = syn_packet.to_padded_bytes();
    info!(
        "Sending SYN packet: {} bytes (padded to MTU)",
        payload.len()
    );

    // Log SynDataEx details if present
    if cookie_hash.is_some() {
        info!("📋 SYN packet includes RDP-UDP v3 authentication:");
        info!("   - Protocol Version: 0x0101 (v3 - matches working captures)");
        info!("   - Cookie Hash: {:02x?}", &cookie_hash.unwrap()[..16]);
        info!("   - Correlation ID: {:02x?}", &correlation_bytes[..16]);
    }
    info!("📦 Full SYN packet structure (first 200 bytes):");
    for (i, chunk) in payload[..200.min(payload.len())].chunks(16).enumerate() {
        info!("   0x{:04x}: {:02x?}", i * 16, chunk);
    }

    socket
        .send(&payload)
        .await
        .context("send RDPUDP SYN datagram")?;

    info!("✅ SYN packet sent, waiting for SYN+ACK (timeout: 3000ms)...");
    warn!("⚠️  If this times out, possible causes:");
    warn!("   1. Windows firewall blocking UDP 3389 (run configure_windows_udp.ps1)");
    warn!("   2. Server doesn't support multitransport");
    warn!("   3. Network blocking UDP packets");
    warn!("   Run: sudo ./capture_all_rdp.sh to see network traffic");

    let mut recv_buffer = vec![0u8; 2048]; // Increased buffer size to catch any response
    debug!("Receive buffer size: {} bytes", recv_buffer.len());

    // Log socket details for debugging
    let local = socket.local_addr()?;
    let peer = socket.peer_addr().ok();
    info!("🔍 Socket listening - Local: {}, Peer: {:?}", local, peer);
    info!("🔍 Waiting for SYN+ACK on UDP socket...");

    // Try to receive with recv_from to see the source address
    // Note: Server may not respond until it receives TCP InitiateMultitransportResponse PDU
    let (received, source_addr) = timeout(
        std::time::Duration::from_millis(3000), // Increased from 1500ms
        socket.recv_from(&mut recv_buffer),
    )
    .await
    .context("waiting for SYN-ACK timed out")?
    .context("receive SYN-ACK datagram")?;

    info!("📦 Received {} bytes from {}", received, source_addr);
    debug!(
        "SYN+ACK packet full dump: {:02x?}",
        &recv_buffer[..received.min(100)]
    );

    let syn_ack =
        SynAckPacket::decode(&recv_buffer[..received]).context("decode SYN-ACK datagram")?;

    info!(
        "✅ RDP-UDP handshake completed successfully! ACK seq: {}",
        syn_ack.inner().header.sn_source_ack
    );

    Ok(socket)
}

async fn connect_ws(
    _config: &Config,
    _rdcleanpath: &RDCleanPathConfig,
    _cliprdr_factory: Option<&(dyn CliprdrBackendFactory + Send)>,
    dvc_pipe_proxy_factory: &DvcPipeProxyFactory,
) -> ConnectorResult<(ConnectionResult, UpgradedFramed, SocketAddr)> {
    let _ = dvc_pipe_proxy_factory;

    // TODO: RDCleanPath/WebSocket support requires internal ironrdp modules
    // For now, this functionality is disabled
    return Err(connector::general_err!(
        "RDCleanPath/WebSocket support not yet implemented"
    ))?;

    /*
    let hostname = rdcleanpath
        .url
        .host_str()
        .ok_or_else(|| connector::general_err!("host missing from the URL"))?;

    let port = rdcleanpath.url.port_or_known_default().unwrap_or(443);

    let socket = TcpStream::connect((hostname, port))
        .await
        .map_err(|e| connector::custom_err!("TCP connect", e))?;

    socket
        .set_nodelay(true)
        .map_err(|e| connector::custom_err!("set TCP_NODELAY", e))?;

    let client_addr = socket
        .local_addr()
        .map_err(|e| connector::custom_err!("get socket local address", e))?;

    let (ws, _) = tokio_tungstenite::client_async_tls(rdcleanpath.url.as_str(), socket)
        .await
        .map_err(|e| connector::custom_err!("WS connect", e))?;

    let ws = crate::ws::websocket_compat(ws);

    let mut framed = ironrdp_tokio::TokioFramed::new(ws);

    let mut drdynvc =
        ironrdp::dvc::DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));

    // Instantiate all DVC proxies
    for proxy in config.dvc_pipe_proxies.iter() {
        let channel_name = proxy.channel_name.clone();
        let pipe_name = proxy.pipe_name.clone();

        trace!(%channel_name, %pipe_name, "Creating DVC proxy");

        drdynvc = drdynvc.with_dynamic_channel(dvc_pipe_proxy_factory.create(channel_name, pipe_name));
    }

    let mut connector = connector::ClientConnector::new(config.connector.clone(), client_addr)
        .with_static_channel(drdynvc)
        .with_static_channel(rdpsnd::client::Rdpsnd::new(Box::new(cpal::RdpsndBackend::new())))
        .with_static_channel(rdpdr::Rdpdr::new(Box::new(NoopRdpdrBackend {}), "IronRDP".to_owned()).with_smartcard(0));

    if let Some(builder) = cliprdr_factory {
        let backend = builder.build_cliprdr_backend();

        let cliprdr = cliprdr::Cliprdr::new(backend);

        connector.attach_static_channel(cliprdr);
    }

    let destination = format!("{}:{}", config.destination.name(), config.destination.port());

    let (upgraded, server_public_key) = connect_rdcleanpath(
        &mut framed,
        &mut connector,
        destination,
        rdcleanpath.auth_token.clone(),
        None,
    )
    .await?;

    let connection_result = ironrdp_tokio::connect_finalize(
        upgraded,
        &mut framed,
        connector,
        (&config.destination).into(),
        server_public_key,
        Some(&mut ReqwestNetworkClient::new()),
        None,
    )
    .await?;

    let (ws, leftover_bytes) = framed.into_inner();
    let erased_stream = Box::new(ws) as Box<dyn AsyncReadWrite + Unpin + Send + Sync>;
    let upgraded_framed = ironrdp_tokio::TokioFramed::new_with_leftover(erased_stream, leftover_bytes);

    Ok((connection_result, upgraded_framed, client_addr))
    */
}

async fn connect_rdcleanpath<S>(
    framed: &mut ironrdp_tokio::Framed<S>,
    connector: &mut connector::ClientConnector,
    destination: String,
    proxy_auth_token: String,
    pcb: Option<String>,
) -> ConnectorResult<(ironrdp_tokio::Upgraded, Vec<u8>)>
where
    S: ironrdp_tokio::FramedRead + FramedWrite,
{
    use ironrdp::connector::Sequence as _;
    use x509_cert::der::Decode as _;

    #[derive(Clone, Copy, Debug)]
    struct RDCleanPathHint;

    const RDCLEANPATH_HINT: RDCleanPathHint = RDCleanPathHint;

    impl ironrdp::pdu::PduHint for RDCleanPathHint {
        fn find_size(&self, bytes: &[u8]) -> ironrdp::core::DecodeResult<Option<(bool, usize)>> {
            match ironrdp_rdcleanpath::RDCleanPathPdu::detect(bytes) {
                ironrdp_rdcleanpath::DetectionResult::Detected { total_length, .. } => {
                    Ok(Some((true, total_length)))
                }
                ironrdp_rdcleanpath::DetectionResult::NotEnoughBytes => Ok(None),
                ironrdp_rdcleanpath::DetectionResult::Failed => Err(ironrdp::core::other_err!(
                    "RDCleanPathHint",
                    "detection failed (invalid PDU)"
                )),
            }
        }
    }

    let mut buf = WriteBuf::new();

    info!("Begin connection procedure");

    {
        // RDCleanPath request

        let connector::ClientConnectorState::ConnectionInitiationSendRequest = connector.state
        else {
            return Err(connector::general_err!(
                "invalid connector state (send request)"
            ));
        };

        debug_assert!(connector.next_pdu_hint().is_none());

        let written = connector.step_no_input(&mut buf)?;
        let x224_pdu_len = written.size().expect("written size");
        debug_assert_eq!(x224_pdu_len, buf.filled_len());
        let x224_pdu = buf.filled().to_vec();

        let rdcleanpath_req = ironrdp_rdcleanpath::RDCleanPathPdu::new_request(
            x224_pdu,
            destination,
            proxy_auth_token,
            pcb,
        )
        .map_err(|e| connector::custom_err!("new RDCleanPath request", e))?;
        debug!(message = ?rdcleanpath_req, "Send RDCleanPath request");
        let rdcleanpath_req = rdcleanpath_req
            .to_der()
            .map_err(|e| connector::custom_err!("RDCleanPath request encode", e))?;

        framed
            .write_all(&rdcleanpath_req)
            .await
            .map_err(|e| connector::custom_err!("couldn't write RDCleanPath request", e))?;
    }

    {
        // RDCleanPath response

        let rdcleanpath_res = framed
            .read_by_hint(&RDCLEANPATH_HINT)
            .await
            .map_err(|e| connector::custom_err!("read RDCleanPath request", e))?;

        let rdcleanpath_res = ironrdp_rdcleanpath::RDCleanPathPdu::from_der(&rdcleanpath_res)
            .map_err(|e| connector::custom_err!("RDCleanPath response decode", e))?;

        debug!(message = ?rdcleanpath_res, "Received RDCleanPath PDU");

        let (x224_connection_response, server_cert_chain) = match rdcleanpath_res
            .into_enum()
            .map_err(|e| connector::custom_err!("invalid RDCleanPath PDU", e))?
        {
            ironrdp_rdcleanpath::RDCleanPath::Request { .. } => {
                return Err(connector::general_err!(
                    "received an unexpected RDCleanPath type (request)",
                ));
            }
            ironrdp_rdcleanpath::RDCleanPath::Response {
                x224_connection_response,
                server_cert_chain,
                server_addr: _,
            } => (x224_connection_response, server_cert_chain),
            ironrdp_rdcleanpath::RDCleanPath::GeneralErr(error) => {
                return Err(connector::custom_err!(
                    "received an RDCleanPath error",
                    error
                ));
            }
            ironrdp_rdcleanpath::RDCleanPath::NegotiationErr {
                x224_connection_response,
            } => {
                // Try to decode as X.224 Connection Confirm to extract negotiation failure details.
                if let Ok(x224_confirm) = ironrdp_core::decode::<
                    ironrdp::pdu::x224::X224<ironrdp::pdu::nego::ConnectionConfirm>,
                >(&x224_connection_response)
                {
                    if let ironrdp::pdu::nego::ConnectionConfirm::Failure { code } = x224_confirm.0
                    {
                        // Convert to negotiation failure instead of generic RDCleanPath error.
                        let negotiation_failure = connector::NegotiationFailure::from(code);
                        return Err(connector::ConnectorError::new(
                            "RDP negotiation failed",
                            connector::ConnectorErrorKind::Negotiation(negotiation_failure),
                        ));
                    }
                }

                // Fallback to generic error if we can't decode the negotiation failure.
                return Err(connector::general_err!(
                    "received an RDCleanPath negotiation error"
                ));
            }
        };

        let connector::ClientConnectorState::ConnectionInitiationWaitConfirm { .. } =
            connector.state
        else {
            return Err(connector::general_err!(
                "invalid connector state (wait confirm)"
            ));
        };

        debug_assert!(connector.next_pdu_hint().is_some());

        buf.clear();
        let written = connector.step(x224_connection_response.as_bytes(), &mut buf)?;

        debug_assert!(written.is_nothing());

        let server_cert = server_cert_chain.into_iter().next().ok_or_else(|| {
            connector::general_err!("server cert chain missing from rdcleanpath response")
        })?;

        let cert = x509_cert::Certificate::from_der(server_cert.as_bytes()).map_err(|e| {
            connector::custom_err!("server cert chain missing from rdcleanpath response", e)
        })?;

        let server_public_key = cert
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| connector::general_err!("subject public key BIT STRING is not aligned"))?
            .to_owned();

        let should_upgrade = ironrdp_tokio::skip_connect_begin(connector);

        // At this point, proxy established the TLS session.

        let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, connector);

        Ok((upgraded, server_public_key))
    }
}

#[derive(Clone)]
pub struct ArboardClipboardFactory {
    sender: mpsc::UnboundedSender<RdpInputEvent>,
}

impl ArboardClipboardFactory {
    pub fn new(sender: mpsc::UnboundedSender<RdpInputEvent>) -> Self {
        Self { sender }
    }
}

impl CliprdrBackendFactory for ArboardClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(ArboardClipboardBackend::new(self.sender.clone()))
    }
}

#[derive(Debug)]
struct ArboardClipboardBackend {
    sender: mpsc::UnboundedSender<RdpInputEvent>,
    clipboard_state: Arc<Mutex<ClipboardState>>,
    running: Arc<AtomicBool>,
    watcher: Option<thread::JoinHandle<()>>,
    temp_dir: String,
}

impl_as_any!(ArboardClipboardBackend);

const FILE_LIST_CLIPBOARD_FORMAT_ID: ClipboardFormatId = ClipboardFormatId(0xC0FE);
const FILE_LIST_CLIPBOARD_FORMAT_ID_FALLBACK: ClipboardFormatId = ClipboardFormatId(0);

fn is_file_descriptor_format(format: ClipboardFormatId) -> bool {
    format == FILE_LIST_CLIPBOARD_FORMAT_ID || format == FILE_LIST_CLIPBOARD_FORMAT_ID_FALLBACK
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClipboardState {
    text: Option<String>,
    files: Option<FileClipboard>,
}

impl ClipboardState {
    fn from_clipboard(clipboard: &mut Clipboard) -> Self {
        let text = clipboard.get_text().ok();
        let files = clipboard
            .get()
            .file_list()
            .ok()
            .and_then(FileClipboard::from_paths);

        let text = Self::sanitize_text(text, files.is_some());

        Self { text, files }
    }

    fn formats(&self) -> Vec<ClipboardFormat> {
        let mut formats = Vec::new();

        if self.text.is_some() {
            formats.push(ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT));
        }

        if self.files.is_some() {
            formats.push(ClipboardFormat::new(ClipboardFormatId::CF_HDROP));
            formats.push(
                ClipboardFormat::new(FILE_LIST_CLIPBOARD_FORMAT_ID)
                    .with_name(ClipboardFormatName::FILE_LIST),
            );
        }

        formats
    }

    fn has_any(&self) -> bool {
        self.text.is_some() || self.files.is_some()
    }

    fn sanitize_text(text: Option<String>, has_files: bool) -> Option<String> {
        if !has_files {
            return text;
        }

        text.and_then(|value| {
            let trimmed = value.trim();
            let looks_like_file_payload = trimmed.starts_with("copy\nfile://")
                || trimmed.starts_with("cut\nfile://")
                || trimmed
                    .lines()
                    .all(|line| line.trim().is_empty() || line.starts_with("file://"));

            if looks_like_file_payload {
                None
            } else {
                Some(value)
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileClipboard {
    entries: Vec<FileEntry>,
}

impl FileClipboard {
    fn from_paths(paths: Vec<PathBuf>) -> Option<Self> {
        let mut entries = Vec::new();

        for path in paths {
            let sanitized_path = sanitize_path(path);

            if sanitized_path.as_os_str().is_empty() {
                continue;
            }

            if let Some(entry) = FileEntry::from_path(sanitized_path) {
                entries.push(entry);
            }
        }

        if entries.is_empty() {
            None
        } else {
            Some(Self { entries })
        }
    }

    fn to_file_group_descriptor(&self) -> Option<OwnedFormatDataResponse> {
        let descriptors: Vec<FileDescriptor> = self
            .entries
            .iter()
            .map(|entry| FileDescriptor {
                attributes: Some(entry.attributes),
                last_write_time: entry.last_write_time,
                file_size: entry.file_size,
                name: entry.name.clone(),
            })
            .collect();

        FormatDataResponse::new_file_list(&PackedFileList { files: descriptors })
            .ok()
            .map(FormatDataResponse::into_owned)
    }

    fn to_hdrop(&self) -> Option<OwnedFormatDataResponse> {
        let mut file_buffer = Vec::new();

        for entry in &self.entries {
            let mut wide_name: Vec<u8> = to_utf16_bytes(entry.name.as_str());
            wide_name.push(0);
            wide_name.push(0);
            file_buffer.extend_from_slice(&wide_name);
        }

        file_buffer.extend_from_slice(&[0, 0]);

        const DROPFILES_HEADER_SIZE: u32 = 20;

        let mut data = Vec::with_capacity(DROPFILES_HEADER_SIZE as usize + file_buffer.len());
        data.extend_from_slice(&DROPFILES_HEADER_SIZE.to_le_bytes());
        data.extend_from_slice(&[0u8; 8]);
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&file_buffer);

        Some(FormatDataResponse::new_data(data).into_owned())
    }

    fn handle_file_contents_request(
        &self,
        request: &FileContentsRequest,
    ) -> FileContentsResponse<'static> {
        let Some(entry) = self.entries.get(request.index as usize).cloned() else {
            return FileContentsResponse::new_error(request.stream_id).into_owned();
        };

        if request.flags.contains(FileContentsFlags::SIZE) {
            let size = entry.file_size.unwrap_or(0);
            return FileContentsResponse::new_size_response(request.stream_id, size).into_owned();
        }

        if !request.flags.contains(FileContentsFlags::DATA) {
            return FileContentsResponse::new_error(request.stream_id).into_owned();
        }

        if entry.is_directory {
            return FileContentsResponse::new_error(request.stream_id).into_owned();
        }

        let Some(file_size) = entry.file_size else {
            return FileContentsResponse::new_error(request.stream_id).into_owned();
        };

        if request.position > file_size {
            return FileContentsResponse::new_error(request.stream_id).into_owned();
        }

        let mut to_read = u64::from(request.requested_size);
        let remaining = file_size - request.position;
        if to_read > remaining {
            to_read = remaining;
        }

        let read_len = match usize::try_from(to_read) {
            Ok(len) => len,
            Err(_) => usize::MAX,
        };

        let mut file = match File::open(&entry.path) {
            Ok(file) => file,
            Err(err) => {
                warn!("Failed to open clipboard file {:?}: {err}", entry.path);
                return FileContentsResponse::new_error(request.stream_id).into_owned();
            }
        };

        if let Err(err) = file.seek(SeekFrom::Start(request.position)) {
            warn!("Failed to seek clipboard file {:?}: {err}", entry.path);
            return FileContentsResponse::new_error(request.stream_id).into_owned();
        }

        let mut buffer = vec![0u8; read_len];
        match file.read(&mut buffer) {
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);
                FileContentsResponse::new_data_response(request.stream_id, buffer).into_owned()
            }
            Err(err) => {
                warn!("Failed to read clipboard file {:?}: {err}", entry.path);
                FileContentsResponse::new_error(request.stream_id).into_owned()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileEntry {
    path: PathBuf,
    name: String,
    attributes: ClipboardFileAttributes,
    file_size: Option<u64>,
    last_write_time: Option<u64>,
    is_directory: bool,
}

impl FileEntry {
    fn from_path(path: PathBuf) -> Option<Self> {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                warn!(
                    "Failed to read metadata for clipboard file {:?}: {err}",
                    path
                );
                return None;
            }
        };

        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())?;

        let is_directory = metadata.is_dir();
        let file_size = if metadata.is_file() {
            Some(metadata.len())
        } else {
            None
        };

        let last_write_time = metadata.modified().ok().and_then(system_time_to_filetime);

        let attributes = if is_directory {
            ClipboardFileAttributes::DIRECTORY
        } else {
            ClipboardFileAttributes::ARCHIVE
        };

        Some(Self {
            path,
            name,
            attributes,
            file_size,
            last_write_time,
            is_directory,
        })
    }
}

fn sanitize_path(path: PathBuf) -> PathBuf {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let mut bytes = path.into_os_string().into_vec();

        while bytes
            .last()
            .is_some_and(|value| matches!(value, b'\r' | b'\n'))
        {
            bytes.pop();
        }

        PathBuf::from(OsString::from_vec(bytes))
    }

    #[cfg(not(unix))]
    {
        let mut path_string = path.to_string_lossy().into_owned();

        while path_string.ends_with(['\r', '\n']) {
            path_string.pop();
        }

        PathBuf::from(path_string)
    }
}

fn system_time_to_filetime(time: SystemTime) -> Option<u64> {
    const WINDOWS_TO_UNIX_EPOCH_DIFF: u64 = 11_644_473_600;

    let duration = time.duration_since(UNIX_EPOCH).ok()?;

    let secs = duration.as_secs().checked_add(WINDOWS_TO_UNIX_EPOCH_DIFF)?;
    let nanos = u64::from(duration.subsec_nanos());

    secs.checked_mul(10_000_000)?.checked_add(nanos / 100)
}

impl ArboardClipboardBackend {
    fn new(sender: mpsc::UnboundedSender<RdpInputEvent>) -> Self {
        Self {
            sender,
            clipboard_state: Arc::new(Mutex::new(ClipboardState::default())),
            running: Arc::new(AtomicBool::new(false)),
            watcher: None,
            temp_dir: std::env::temp_dir().display().to_string(),
        }
    }

    fn ensure_watcher(&mut self) {
        if self.watcher.is_some() {
            return;
        }

        self.running.store(true, Ordering::Relaxed);
        let running = Arc::clone(&self.running);
        let sender = self.sender.clone();
        let clipboard_state = Arc::clone(&self.clipboard_state);

        self.watcher = Some(thread::spawn(move || {
            let mut clipboard = Clipboard::new().ok();

            while running.load(Ordering::Relaxed) {
                if clipboard.is_none() {
                    clipboard = Clipboard::new().ok();
                }

                if let Some(cb) = clipboard.as_mut() {
                    let state = ClipboardState::from_clipboard(cb);

                    let should_advertise = {
                        let mut guard = clipboard_state.lock().unwrap();
                        if *guard != state {
                            *guard = state.clone();
                            state.has_any()
                        } else {
                            false
                        }
                    };

                    if should_advertise {
                        let formats = state.formats();
                        if !formats.is_empty() {
                            let _ = sender.send(RdpInputEvent::Clipboard(
                                ClipboardMessage::SendInitiateCopy(formats),
                            ));
                        }
                    }
                }

                thread::sleep(Duration::from_millis(500));
            }
        }));
    }

    fn advertise_current_clipboard(&self) {
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(err) => {
                trace!("Failed to access clipboard: {err}");
                return;
            }
        };

        let state = ClipboardState::from_clipboard(&mut clipboard);

        {
            let mut guard = self.clipboard_state.lock().unwrap();
            *guard = state.clone();
        }

        let formats = state.formats();
        if !formats.is_empty() {
            let _ = self.sender.send(RdpInputEvent::Clipboard(
                ClipboardMessage::SendInitiateCopy(formats),
            ));
        }
    }

    fn read_clipboard_text(&self) -> Option<String> {
        let mut clipboard = Clipboard::new().ok()?;
        ClipboardState::from_clipboard(&mut clipboard).text
    }

    fn set_clipboard_text(&self, text: &str) {
        match Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(err) = clipboard.set_text(text.to_owned()) {
                    warn!("Failed to set clipboard text: {err}");
                }
            }
            Err(err) => warn!("Failed to access clipboard: {err}"),
        }

        let mut state = self.clipboard_state.lock().unwrap();
        state.text = Some(text.to_owned());
        state.files = None;
    }
}

impl Drop for ArboardClipboardBackend {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.watcher.take() {
            let _ = handle.join();
        }
    }
}

impl CliprdrBackend for ArboardClipboardBackend {
    fn temporary_directory(&self) -> &str {
        &self.temp_dir
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        self.ensure_watcher();
        self.advertise_current_clipboard();
    }

    fn on_request_format_list(&mut self) {
        self.ensure_watcher();
        self.advertise_current_clipboard();
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if let Some(format) = available_formats
            .iter()
            .find(|fmt| fmt.id == ClipboardFormatId::CF_UNICODETEXT)
        {
            let _ = self.sender.send(RdpInputEvent::Clipboard(
                ClipboardMessage::SendInitiatePaste(format.id),
            ));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let response = if request.format == ClipboardFormatId::CF_UNICODETEXT {
            let text = { self.clipboard_state.lock().unwrap().text.clone() }
                .or_else(|| self.read_clipboard_text());

            match text {
                Some(text) => {
                    {
                        let mut guard = self.clipboard_state.lock().unwrap();
                        guard.text = Some(text.clone());
                    }
                    FormatDataResponse::new_unicode_string(&text).into_owned()
                }
                None => FormatDataResponse::new_error().into_owned(),
            }
        } else if is_file_descriptor_format(request.format) {
            let files = self.clipboard_state.lock().unwrap().files.clone();
            files
                .and_then(|files| files.to_file_group_descriptor())
                .unwrap_or_else(|| FormatDataResponse::new_error().into_owned())
        } else if request.format == ClipboardFormatId::CF_HDROP {
            let files = self.clipboard_state.lock().unwrap().files.clone();
            files
                .and_then(|files| files.to_hdrop())
                .unwrap_or_else(|| FormatDataResponse::new_error().into_owned())
        } else {
            FormatDataResponse::new_error().into_owned()
        };

        let _ = self
            .sender
            .send(RdpInputEvent::Clipboard(ClipboardMessage::SendFormatData(
                response,
            )));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        match response.to_unicode_string() {
            Ok(text) => {
                self.set_clipboard_text(&text);
            }
            Err(err) => warn!("Failed to decode clipboard data: {err}"),
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        let files = self.clipboard_state.lock().unwrap().files.clone();

        let response = files
            .map(|files| files.handle_file_contents_request(&request))
            .unwrap_or_else(|| FileContentsResponse::new_error(request.stream_id).into_owned());

        let _ = self
            .sender
            .send(RdpInputEvent::ClipboardFileContents(response));
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {
        warn!("Receiving file clipboard data is not supported");
    }

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

/// Check if a frame contains a multitransport request (SEC_TRANSPORT_REQ flag)
/// Detects MultiTransportRequest PDUs in the stream
/// Returns Some((request_id, protocol, security_cookie)) if detected, None otherwise
#[derive(Debug, Clone, Copy)]
struct MultitransportRequestInfo {
    request_id: u32,
    protocol: MultitransportProtocol,
    security_cookie: [u8; 16],
    security_flags_hi: u16,
    initiator_id: u16,
    channel_id: u16,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct MultitransportHandshakeContext {
    request: MultitransportRequestInfo,
    cookie_hash: [u8; 32],
    correlation_id: [u8; 16],
}

fn detect_multitransport_request(
    action: ironrdp::pdu::Action,
    payload: &[u8],
    message_channel_id: Option<u16>,
) -> Option<MultitransportRequestInfo> {
    use ironrdp::pdu::Action;
    use ironrdp_core::{Decode, ReadCursor};

    // Only check X224 frames (FastPath doesn't have security headers)
    if action != Action::X224 {
        return None;
    }

    // Decode the SendDataIndication envelope to extract initiator/channel IDs and payload
    let send_ctx = legacy::decode_send_data_indication(payload).ok()?;
    let initiator_id = send_ctx.initiator_id;
    let channel_id = send_ctx.channel_id;
    let user_data = send_ctx.user_data;

    // CRITICAL: MultiTransportRequest PDUs are ONLY sent on the MCS Message Channel!
    // Scanning other channels (especially Virtual Channels with arbitrary binary data)
    // causes false positives when random bytes match the security header pattern.
    if let Some(expected_channel) = message_channel_id {
        if channel_id != expected_channel {
            // Not on the message channel - skip scanning to avoid false positives
            trace!(
                "Skipping MultiTransport scan on channel 0x{:04x} (expected message channel 0x{:04x})",
                channel_id, expected_channel
            );
            return None;
        }
    }

    // Search for a BasicSecurityHeader with the TRANSPORT_REQ flag inside the user_data
    for offset in 0..user_data.len().saturating_sub(8) {
        let mut cursor = ReadCursor::new(&user_data[offset..]);

        // Manually decode the security header so we can capture the high flag field
        let flags_bits = cursor.read_u16();
        let flags = match BasicSecurityHeaderFlags::from_bits(flags_bits) {
            Some(f) => f,
            None => continue,
        };

        if !flags.contains(BasicSecurityHeaderFlags::TRANSPORT_REQ) {
            continue;
        }

        // A valid multitransport request shouldn't mix other security flags such as RESET_SEQNO
        // or ENCRYPT, which are commonly present in ordinary Share Data PDUs. Filter those out
        // early to avoid false positives.
        let allowed_flags =
            BasicSecurityHeaderFlags::TRANSPORT_REQ | BasicSecurityHeaderFlags::FLAGSHI_VALID;
        if !(flags - allowed_flags).is_empty() {
            trace!(
                offset,
                flags = format_args!("0x{flags_bits:04x}"),
                "Skipping candidate security header because it sets unrelated flags",
            );
            continue;
        }

        let security_flags_hi = cursor.read_u16();

        if !flags.contains(BasicSecurityHeaderFlags::FLAGSHI_VALID) && security_flags_hi != 0 {
            trace!(
                offset,
                flags = format_args!("0x{flags_bits:04x}"),
                security_flags_hi,
                "Skipping candidate security header because FLAGSHI_VALID is not set",
            );
            continue;
        }

        // Attempt to decode the request body
        if let Ok(request) = InitiateMultitransportRequest::decode(&mut cursor) {
            let protocol = request.requested_protocol;
            let protocol_bits = protocol.as_u16();
            let has_known_bits =
                MultitransportProtocol::contains_known_transport_bits(protocol_bits);
            let extra_bits = MultitransportProtocol::extra_bits(protocol_bits);

            info!(
                "Received Initiate Multitransport Request: request_id={}, protocol={:?} (raw=0x{:04x})",
                request.request_id, protocol, protocol_bits
            );

            if !has_known_bits {
                warn!(
                    offset,
                    protocol_bits = format_args!("0x{protocol_bits:04x}"),
                    "Protocol value does not set known reliable/lossy flags; accepting for compatibility",
                );
            }

            if extra_bits != 0 {
                debug!(
                    offset,
                    extra_bits = format_args!("0x{extra_bits:04x}"),
                    "Protocol value includes additional, currently-unknown flags",
                );
            }

            return Some(MultitransportRequestInfo {
                request_id: request.request_id,
                protocol,
                security_cookie: request.security_cookie,
                security_flags_hi,
                initiator_id,
                channel_id,
            });
        }
    }

    None
}

fn encode_multitransport_response_frame(
    request: MultitransportRequestInfo,
    message_channel_id: Option<u16>,
) -> ConnectorResult<Vec<u8>> {
    struct MultitransportResponsePdu {
        security_flags: BasicSecurityHeaderFlags,
        security_flags_hi: u16,
        response: InitiateMultitransportResponse,
    }

    impl Encode for MultitransportResponsePdu {
        fn encode(&self, dst: &mut WriteCursor<'_>) -> ironrdp_core::EncodeResult<()> {
            ironrdp_core::ensure_size!(in: dst, size: self.size());

            dst.write_u16(self.security_flags.bits());
            dst.write_u16(self.security_flags_hi);
            self.response.encode(dst)
        }

        fn name(&self) -> &'static str {
            "MultitransportResponsePdu"
        }

        fn size(&self) -> usize {
            ironrdp::pdu::rdp::headers::BASIC_SECURITY_HEADER_SIZE + self.response.size()
        }
    }

    let response = InitiateMultitransportResponse::success(request.request_id);
    let security_flags =
        BasicSecurityHeaderFlags::TRANSPORT_RSP | BasicSecurityHeaderFlags::FLAGSHI_VALID;

    let response_pdu = MultitransportResponsePdu {
        security_flags,
        security_flags_hi: request.security_flags_hi,
        response,
    };

    let mut buf = WriteBuf::new();

    // Use message channel ID from server if available (per MS-RDPBCGR spec requirement)
    // Otherwise fall back to the channel from the request
    let channel_id = message_channel_id.unwrap_or(request.channel_id);

    if let Some(msg_ch_id) = message_channel_id {
        info!(
            "📨 Sending Multitransport Response on MCS Message Channel 0x{:04x} (from ServerMessageChannelData)",
            msg_ch_id
        );
    } else {
        warn!(
            "⚠️  No message channel ID available - falling back to request channel 0x{:04x}",
            request.channel_id
        );
    }

    legacy::encode_send_data_request(request.initiator_id, channel_id, &response_pdu, &mut buf)?;

    Ok(buf.filled().to_vec())
}

async fn active_session<T: RdpEventSender + Clone>(
    framed: UpgradedFramed,
    connection_result: ConnectionResult,
    event_loop_proxy: &T,
    input_event_receiver: &mut mpsc::UnboundedReceiver<RdpInputEvent>,
    destination: Destination,
    client_addr: SocketAddr,
) -> SessionResult<RdpControlFlow> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let mut image = DecodedImage::new(
        PixelFormat::RgbA32,
        connection_result.desktop_size.width,
        connection_result.desktop_size.height,
    );

    // Extract needed values before connection_result is consumed
    let correlation_id = connection_result.correlation_id;
    let message_channel_id = connection_result.message_channel_id;
    let selected_protocol = connection_result.selected_protocol;

    let mut active_stage = ActiveStage::new(connection_result);

    // Set the event sender on GFX processor now that we have the real event loop proxy
    {
        use crate::gfx_channel::GfxDvcProcessor;
        if let Some(dvc) = active_stage.get_dvc_mut::<GfxDvcProcessor>() {
            if let Some(gfx_processor) = dvc.channel_processor_downcast_mut::<GfxDvcProcessor>() {
                info!("🔌 Connecting GFX event sender to UI");
                gfx_processor.set_event_sender(Box::new(event_loop_proxy.clone()));
            }
        }
    }

    // Initialize Desktop Composition handler with output dimensions
    let mut desktop_comp_handler =
        DesktopCompositionHandler::new(image.width() as u32, image.height() as u32);

    let mut last_frame_dimensions = (image.width(), image.height());
    let mut frame_ready = false;

    // Track pending initial resize request (to be sent once DisplayControl channel is ready)
    let mut pending_initial_resize: Option<(u16, u16, u32, Option<(u32, u32)>)> = None;

    // Track multitransport requests with their authentication data
    // Maps request_id -> metadata needed for TCP response and UDP handshake
    // Each request_id represents a separate UDP transport channel
    let mut multitransport_requests: HashMap<u32, MultitransportHandshakeContext> = HashMap::new();

    // UDP transport channels (established when multitransport is requested)
    // Store ALL transports, not just one! Multiple may be active simultaneously.
    let mut udp_transports: Vec<mpsc::UnboundedReceiver<UdpTransportEvent>> = Vec::new();

    // Track if we're currently establishing a UDP connection to avoid parallel attempts
    let mut udp_connection_in_progress = false;

    let disconnect_reason = 'outer: loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                match frame {
                    Ok((action, payload)) => {
                        trace!(?action, frame_length = payload.len(), "Frame received");

                        let mut extra_outputs = Vec::new();

                // Check for multitransport request before processing
                let outputs = if let Some(request_info) = detect_multitransport_request(action, &payload, message_channel_id) {
                    let request_id = request_info.request_id;

                    let protocol_bits = request_info.protocol.as_u16();
                    let has_known_protocol_bits =
                        MultitransportProtocol::contains_known_transport_bits(protocol_bits);
                    let extra_protocol_bits =
                        MultitransportProtocol::extra_bits(protocol_bits);

                    info!(
                        "🔥 Detected Initiate Multitransport Request: request_id={}, protocol={:?} (raw=0x{:04x})",
                        request_id,
                        request_info.protocol,
                        protocol_bits
                    );

                    if !has_known_protocol_bits {
                        warn!(
                            "   request_id={}, Protocol does not set known reliable/lossy bits; proceeding for compatibility",
                            request_id
                        );
                    }

                    if extra_protocol_bits != 0 {
                        info!(
                            "   request_id={}, Protocol includes additional flags: 0x{:04x}",
                            request_id,
                            extra_protocol_bits
                        );
                    }

                    // Check if we've already processed this request_id
                    let is_duplicate = multitransport_requests.contains_key(&request_id);
                    if is_duplicate {
                        warn!("⚠️  Ignoring duplicate multitransport request_id={} (already processed)", request_id);
                    } else {
                        info!("   request_id={}, Protocol: {:?}", request_id, request_info.protocol);
                        info!(
                            "   request_id={}, Security Cookie (full 16 bytes): {:02x?}",
                            request_id,
                            request_info.security_cookie
                        );
                        info!(
                            "   request_id={}, Security Flags (HI): 0x{:04x}",
                            request_id,
                            request_info.security_flags_hi
                        );
                        info!(
                            "   request_id={}, Initiator: 0x{:04x}, Channel: 0x{:04x}",
                            request_id,
                            request_info.initiator_id,
                            request_info.channel_id
                        );
                    }

                    // Check if security cookie is all zeros (server doesn't support/require authentication)
                    let cookie_is_zero = request_info.security_cookie.iter().all(|&b| b == 0);

                    if cookie_is_zero {
                        // Skip zero-cookie requests - these appear to be probes that the server rejects
                        warn!("   request_id={}, Skipping zero-cookie multitransport request (appears to be a probe)", request_id);
                        warn!("   Waiting for subsequent requests with valid security cookies");
                        continue;
                    }

                    // Calculate SHA-256 hash of security cookie for UDP authentication
                    let cookie_hash = if !cookie_is_zero {
                        let mut hasher = Sha256::new();
                        hasher.update(&request_info.security_cookie);
                        let hash_raw: [u8; 32] = hasher.finalize().into();

                        // CRITICAL: The hash must be byte-swapped in 4-byte (32-bit) chunks to little-endian
                        // Windows RDP client does this transformation before sending the hash
                        let mut cookie_hash = [0u8; 32];
                        for i in 0..8 {
                            let offset = i * 4;
                            cookie_hash[offset] = hash_raw[offset + 3];
                            cookie_hash[offset + 1] = hash_raw[offset + 2];
                            cookie_hash[offset + 2] = hash_raw[offset + 1];
                            cookie_hash[offset + 3] = hash_raw[offset];
                        }

                        if !is_duplicate {
                            info!("   request_id={}, Cookie Hash (SHA-256 raw): {:02x?}", request_id, hash_raw);
                            info!(
                                "   request_id={}, Cookie Hash (LE-swapped for UDP): {:02x?}",
                                request_id,
                                cookie_hash
                            );
                        }
                        cookie_hash
                    } else {
                        if !is_duplicate {
                            warn!("   request_id={}, Security cookie is all zeros - server doesn't require MS-RDPEMT authentication", request_id);
                            warn!("   request_id={}, Will use UDPv2 without cookie hash", request_id);
                        }
                        [0u8; 32] // All-zero hash indicates no authentication
                    };

                    // Only start UDP handshake if this is a new request (not a duplicate)
                    // AND if we're not already establishing a UDP connection
                    if !is_duplicate && !udp_connection_in_progress {
                        udp_connection_in_progress = true;  // Mark connection attempt as in progress

                        match encode_multitransport_response_frame(request_info, message_channel_id) {
                            Ok(frame) => {
                                info!("📨 Sending Initiate Multitransport Response (S_OK) for request_id={}", request_id);
                                eprintln!("🔍 Multitransport Response bytes ({} bytes):", frame.len());
                                for chunk in frame.chunks(32) {
                                    eprintln!("    {:02x?}", chunk);
                                }
                                extra_outputs.push(ActiveStageOutput::ResponseFrame(frame));
                            }
                            Err(err) => {
                                error!(
                                    "❌ Failed to encode InitiateMultitransportResponse for request_id={}: {:?}",
                                    request_id,
                                    err
                                );
                            }
                        }

                        if let Some(corr_id) = correlation_id {
                            info!("✅ Correlation ID from TCP negotiation: {:02x?}", corr_id);
                            info!(
                                "   (This will be used in UDP SYN packet for request_id={})",
                                request_id
                            );

                            // Store this request with its authentication data
                            multitransport_requests.insert(
                                request_id,
                                MultitransportHandshakeContext {
                                    request: request_info,
                                    cookie_hash,
                                    correlation_id: corr_id,
                                },
                            );
                            info!(
                                "📝 Stored request_id={} with cookie_hash, correlation_id, and metadata",
                                request_id
                            );

                            // CRITICAL FIX: Working client sends UDP SYN ~1.5ms after TCP ACK
                            info!("🚀 Starting UDP handshake for request_id={}...", request_id);

                            // Enable UDP mode IMMEDIATELY to prevent TCP/UDP collision
                            // (TCP GFX data may arrive before UDP handshake completes)
                            if let Some(dvc) = active_stage.get_dvc_mut::<crate::gfx_channel::GfxDvcProcessor>() {
                                if let Some(gfx) = dvc.channel_processor_downcast_mut::<crate::gfx_channel::GfxDvcProcessor>() {
                                    gfx.enable_udp_mode();
                                }
                            }

                            // Establish UDP transport using transport manager
                            let dest_clone = destination.clone();
                            let addr_clone = client_addr;
                            let req_id = request_id;
                            let protocol = request_info.protocol;
                            let protocol_bits = protocol.as_u16();
                            let use_lossy = {
                                let has_known = MultitransportProtocol::contains_known_transport_bits(protocol_bits);
                                let lossy = protocol.has_lossy_bit();
                                if !has_known && req_id == 0 {
                                    // Special case: Windows sends the zero-cookie probe with protocol==0.
                                    // Treat it as reliable transport so the server can validate the socket.
                                    info!(
                                        "   request_id=0 uses reliable mode for compatibility (protocol bits = 0x{:04x})",
                                        protocol_bits
                                    );
                                    false
                                } else if has_known {
                                    lossy
                                } else {
                                    // BUG FIX: Windows 11 sends protocol values without known transport bits (e.g., 0x001C).
                                    // Testing shows servers expect RELIABLE mode (SYN without lossy flag) in these cases.
                                    // Defaulting to lossy caused the server to ignore SYN packets entirely.
                                    info!(
                                        "   request_id={}, Protocol lacks known transport bits (0x{:04x}); defaulting to RELIABLE mode",
                                        req_id, protocol_bits
                                    );
                                    false
                                }
                            };

                            // Wait 2ms to allow TCP ACK to be sent
                            tokio::time::sleep(std::time::Duration::from_millis(2)).await;

                            info!(
                                "🚀 Establishing UDP transport for request_id={} (protocol: {:?}, lossy: {})",
                                req_id, protocol, use_lossy
                            );

                            match establish_udp_transport(
                                dest_clone,
                                addr_clone,
                                corr_id,
                                use_lossy,
                                Some(req_id),
                                Some(request_info.security_cookie),
                                selected_protocol,
                            ).await {
                                Ok((_cmd_tx, evt_rx)) => {
                                    info!(
                                        "✅ UDP transport established successfully for request_id={}!",
                                        req_id
                                    );
                                    info!("   UDP transport manager running with keepalive and FEC");
                                    info!(
                                        "📌 Storing UDP transport #{} for request_id={}",
                                        udp_transports.len() + 1,
                                        req_id
                                    );
                                    udp_transports.push(evt_rx);
                                    udp_connection_in_progress = false;  // Connection succeeded
                                    // Note: command sender (_cmd_tx) currently unused
                                }
                                Err(e) => {
                                    error!(
                                        "❌ Failed to establish UDP transport for request_id={}: {:?}",
                                        req_id, e
                                    );
                                    udp_connection_in_progress = false;  // Connection failed, allow next attempt
                                }
                            }
                        } else {
                            warn!(
                                "⚠️  Multitransport requested but no correlation_id available for request_id={}",
                                request_id
                            );
                        }
                    } else if !is_duplicate {
                        info!(
                            "⏸️  Skipping request_id={} - UDP connection already in progress",
                            request_id
                        );
                    }

                    // IMPORTANT: Multitransport request PDUs are control PDUs that should NOT be processed
                    // by the normal RDP state machine. We've already sent the response above, so just return.
                    // Additionally, we ignore all TCP data until UDP SYN+ACK completes, since the server
                    // may send protocol errors or control messages that don't apply to UDP session init.
                    info!("✅ Multitransport request handled, skipping TCP processing during UDP handshake");
                    extra_outputs
                } else {
                    // Not a multitransport request - process normally
                    match active_stage.process(&mut image, action, &payload) {
                        Ok(outputs) => outputs,
                        Err(e) => {
                            // Check if it's an unknown PDU type error we can safely ignore
                            let err_msg = format!("{:?}", e);
                            if err_msg.contains("Unknown pduType") || err_msg.contains("Unknown") {
                                warn!("⚠️  Unknown PDU type during process, ignoring: {}", err_msg);
                                vec![]
                            } else {
                                error!("❌ Fatal error during PDU processing: {}", err_msg);
                                return Err(e);
                            }
                        }
                    }
                };

                outputs
                    }
                    Err(e) => {
                        // Check if it's an "Unknown pduType" error that we can safely ignore
                        let err_msg = format!("{:?}", e);
                        let err_str = format!("{}", e);
                        eprintln!("🔍 PDU read error details (Debug): {}", err_msg);
                        eprintln!("🔍 PDU read error details (Display): {}", err_str);
                        eprintln!("   Error type object: {:?}", e);
                        warn!("⚠️  PDU read error: {} / {}", err_msg, err_str);

                        //  FOR NOW: Accept ANY error during active session and continue
                        // This allows UDP timeout errors and unknown PDU types to not crash the session
                        warn!("⚠️  Accepting error and continuing (UDP may have timed out or unknown PDU received)");
                        vec![]
                    }
                }
            }
            // Handle UDP transport events from ALL active transports
            // Poll all transports and process whichever has data first
            udp_event = async {
                if udp_transports.is_empty() {
                    // No transports yet - wait forever
                    std::future::pending::<Option<(usize, UdpTransportEvent)>>().await
                } else {
                    // Poll all transports simultaneously using select_all
                    use futures_util::stream::StreamExt;

                    let mut futures = udp_transports
                        .iter_mut()
                        .enumerate()
                        .map(|(idx, rx)| async move {
                            match rx.recv().await {
                                Some(event) => Some((idx, event)),
                                None => None,
                            }
                        })
                        .collect::<futures_util::stream::FuturesUnordered<_>>();

                    futures.next().await.flatten()
                }
            } => {
                match udp_event {
                    Some((transport_idx, UdpTransportEvent::Connected)) => {
                        info!("✅ UDP transport #{} connected!", transport_idx + 1);
                        vec![]
                    }
                    Some((transport_idx, UdpTransportEvent::TunnelEstablished)) => {
                        info!("🔐 MS-RDPEMT tunnel established on transport #{}", transport_idx + 1);
                        vec![]
                    }
                    Some((transport_idx, UdpTransportEvent::DataReceived(data))) => {
                        info!("📦 Received {} bytes via UDP on transport #{}", data.len(), transport_idx + 1);

                        // Route UDP data to GFX processor via DVC
                        if let Some(dvc) = active_stage.get_dvc_mut::<GfxDvcProcessor>() {
                            let channel_id = dvc.channel_id();

                            if let Some(gfx) = dvc.channel_processor_downcast_mut::<GfxDvcProcessor>() {
                                match gfx.process_udp_data(&data) {
                                    Ok(dvc_messages) => {
                                        if !dvc_messages.is_empty() {
                                            debug!("✅ Processed UDP GFX data, sending {} response messages via DVC", dvc_messages.len());

                                            if let Some(channel_id) = channel_id {
                                                match ironrdp_dvc::encode_dvc_messages(
                                                    channel_id,
                                                    dvc_messages,
                                                    ChannelFlags::empty()
                                                ) {
                                                    Ok(svc_messages) => {
                                                        let frame = active_stage.encode_dvc_messages(svc_messages)?;
                                                        vec![ActiveStageOutput::ResponseFrame(frame)]
                                                    }
                                                    Err(e) => {
                                                        warn!("⚠️  Failed to encode DVC messages: {:?}", e);
                                                        vec![]
                                                    }
                                                }
                                            } else {
                                                warn!("⚠️  GFX channel not open, discarding response messages");
                                                vec![]
                                            }
                                        } else {
                                            vec![]
                                        }
                                    }
                                    Err(e) => {
                                        warn!("⚠️  Failed to process UDP GFX data: {:?}", e);
                                        vec![]
                                    }
                                }
                            } else {
                                trace!("GFX DVC processor downcast failed");
                                vec![]
                            }
                        } else {
                            trace!("GFX DVC not available, discarding {} UDP bytes", data.len());
                            vec![]
                        }
                    }
                    Some((transport_idx, UdpTransportEvent::Disconnected(reason))) => {
                        warn!("⚠️  UDP transport #{} disconnected: {}", transport_idx + 1, reason);
                        vec![]
                    }
                    None => {
                        // All UDP event receivers closed or no data available
                        vec![]
                    }
                }
            }
            input_event = input_event_receiver.recv() => {
                let input_event = input_event.ok_or_else(|| session::general_err!("GUI is stopped"))?;

                match input_event {
                    RdpInputEvent::Resize { width, height, scale_factor, physical_size } => {
                        info!(width, height, scale_factor, ?physical_size, "📐 Resize event received");

                        // Attempt to encode the resize request
                        if let Some(result) = active_stage.encode_resize(
                            width as u32,
                            height as u32,
                            Some(scale_factor),
                            physical_size,
                        ) {
                            vec![ActiveStageOutput::ResponseFrame(result?)]
                        } else {
                            // Display Control channel not available yet - queue for later
                            debug!("Resize requested but Display Control channel not available, queueing");
                            pending_initial_resize = Some((width, height, scale_factor, physical_size));
                            vec![]
                        }
                    },
                    RdpInputEvent::FastPath(events) => {
                        trace!(?events);
                        active_stage.process_fastpath_input(&mut image, &events)?
                    }
                    RdpInputEvent::Close => {
                        active_stage.graceful_shutdown()?
                    }
                    RdpInputEvent::Clipboard(event) => {
                        if let Some(cliprdr) = active_stage.get_svc_processor::<cliprdr::CliprdrClient>() {
                            if let Some(svc_messages) = match event {
                                ClipboardMessage::SendInitiateCopy(formats) => {
                                    Some(cliprdr.initiate_copy(&formats)
                                        .map_err(|e| session::custom_err!("CLIPRDR", e))?)
                                }
                                ClipboardMessage::SendFormatData(response) => {
                                    Some(cliprdr.submit_format_data(response)
                                    .map_err(|e| session::custom_err!("CLIPRDR", e))?)
                                }
                                ClipboardMessage::SendInitiatePaste(format) => {
                                    Some(cliprdr.initiate_paste(format)
                                        .map_err(|e| session::custom_err!("CLIPRDR", e))?)
                                }
                                ClipboardMessage::Error(e) => {
                                    error!("Clipboard backend error: {}", e);
                                    None
                                }
                            } {
                                let frame = active_stage.process_svc_processor_messages(svc_messages)?;
                                // Send the messages to the server
                                vec![ActiveStageOutput::ResponseFrame(frame)]
                            } else {
                                // No messages to send to the server
                                Vec::new()
                            }
                        } else  {
                            warn!("Clipboard event received, but Cliprdr is not available");
                            Vec::new()
                        }
                    }
                    RdpInputEvent::ClipboardFileContents(response) => {
                        if let Some(cliprdr) =
                            active_stage.get_svc_processor::<cliprdr::CliprdrClient>()
                        {
                            let svc_messages = cliprdr
                                .submit_file_contents(response)
                                .map_err(|e| session::custom_err!("CLIPRDR", e))?;

                            let frame = active_stage.process_svc_processor_messages(svc_messages)?;

                            vec![ActiveStageOutput::ResponseFrame(frame)]
                        } else {
                            warn!("File contents response received, but Cliprdr is not available");
                            Vec::new()
                        }
                    }
                    RdpInputEvent::SendDvcMessages { channel_id, messages } => {
                        trace!(channel_id, ?messages, "Send DVC messages");

                        let frame = active_stage.encode_dvc_messages(messages)?;
                        vec![ActiveStageOutput::ResponseFrame(frame)]
                    }
                }
            }
        };

        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if !frame.is_empty() {
                        debug!(
                            "📡 RDP: Writing {} bytes response frame to server",
                            frame.len()
                        );
                    }
                    writer
                        .write_all(&frame)
                        .await
                        .map_err(|e| session::custom_err!("write response", e))?
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    let width = NonZeroU16::new(image.width())
                        .ok_or_else(|| session::general_err!("width is zero"))?;
                    let height = NonZeroU16::new(image.height())
                        .ok_or_else(|| session::general_err!("height is zero"))?;

                    let dimensions = (width.get(), height.get());
                    if dimensions != last_frame_dimensions {
                        frame_ready = false;
                        last_frame_dimensions = dimensions;
                    }

                    let is_full_frame = region.left == 0
                        && region.top == 0
                        && region.width() == width.get()
                        && region.height() == height.get();

                    if !frame_ready || is_full_frame {
                        let buffer = image.data().to_vec();

                        event_loop_proxy
                            .send_event(RdpOutputEvent::Image {
                                buffer,
                                width,
                                height,
                                region: None,
                            })
                            .map_err(|_| session::general_err!("failed to send image event"))?;

                        frame_ready = true;
                    } else {
                        let bytes_per_pixel = image.bytes_per_pixel();
                        let stride = image.stride();
                        let rect_width = usize::from(region.width());
                        let rect_height = usize::from(region.height());
                        let mut buffer = vec![0u8; rect_width * rect_height * bytes_per_pixel];
                        let data = image.data();

                        for row in 0..rect_height {
                            let src_offset = (usize::from(region.top) + row) * stride
                                + usize::from(region.left) * bytes_per_pixel;
                            let dst_offset = row * rect_width * bytes_per_pixel;
                            buffer[dst_offset..dst_offset + rect_width * bytes_per_pixel]
                                .copy_from_slice(
                                    &data[src_offset..src_offset + rect_width * bytes_per_pixel],
                                );
                        }

                        let region = ImageRegion {
                            x: region.left,
                            y: region.top,
                            width: NonZeroU16::new(region.width())
                                .expect("region width is always non-zero"),
                            height: NonZeroU16::new(region.height())
                                .expect("region height is always non-zero"),
                        };

                        event_loop_proxy
                            .send_event(RdpOutputEvent::Image {
                                buffer,
                                width,
                                height,
                                region: Some(region),
                            })
                            .map_err(|_| session::general_err!("failed to send image event"))?;
                    }
                }
                ActiveStageOutput::PointerDefault => {
                    event_loop_proxy
                        .send_event(RdpOutputEvent::PointerDefault)
                        .map_err(|_| {
                            session::general_err!("failed to send pointer default event")
                        })?;
                }
                ActiveStageOutput::PointerHidden => {
                    event_loop_proxy
                        .send_event(RdpOutputEvent::PointerHidden)
                        .map_err(|_| {
                            session::general_err!("failed to send pointer hidden event")
                        })?;
                }
                ActiveStageOutput::PointerPosition { x, y } => {
                    event_loop_proxy
                        .send_event(RdpOutputEvent::PointerPosition { x, y })
                        .map_err(|_| {
                            session::general_err!("failed to send pointer position event")
                        })?;
                }
                ActiveStageOutput::PointerBitmap(pointer) => {
                    event_loop_proxy
                        .send_event(RdpOutputEvent::PointerBitmap(pointer))
                        .map_err(|_| {
                            session::general_err!("failed to send pointer bitmap event")
                        })?;
                }
                ActiveStageOutput::Orders(orders) => {
                    // Process Desktop Composition orders through our handler
                    for order in orders {
                        if let DrawingOrder::DesktopComposition(comp_order) = order {
                            desktop_comp_handler
                                .process_order(&comp_order)
                                .map_err(|e| session::custom_err!("Desktop Composition", e))?;
                        }
                    }

                    // Check if we need to flush and send composed output
                    if let Some(output) = desktop_comp_handler.flush() {
                        let width = NonZeroU16::new(output.width as u16).ok_or_else(|| {
                            session::general_err!("compositor output width is zero")
                        })?;
                        let height = NonZeroU16::new(output.height as u16).ok_or_else(|| {
                            session::general_err!("compositor output height is zero")
                        })?;

                        event_loop_proxy
                            .send_event(RdpOutputEvent::Image {
                                buffer: output.data.clone(),
                                width,
                                height,
                                region: None,
                            })
                            .map_err(|_| {
                                session::general_err!("failed to send compositor image event")
                            })?;
                    }
                }
                ActiveStageOutput::DeactivateAll(mut connection_activation) => {
                    // Execute the Deactivation-Reactivation Sequence:
                    // https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dfc234ce-481a-4674-9a5d-2a7bafb14432
                    debug!(
                        "Received Server Deactivate All PDU, executing Deactivation-Reactivation Sequence"
                    );
                    let mut buf = WriteBuf::new();
                    'activation_seq: loop {
                        let written = single_sequence_step_read(
                            &mut reader,
                            &mut *connection_activation,
                            &mut buf,
                        )
                        .await
                        .map_err(|e| {
                            session::custom_err!("read deactivation-reactivation sequence step", e)
                        })?;

                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await.map_err(|e| {
                                session::custom_err!(
                                    "write deactivation-reactivation sequence step",
                                    e
                                )
                            })?;
                        }

                        if let ConnectionActivationState::Finalized {
                            io_channel_id,
                            user_channel_id,
                            desktop_size,
                            enable_server_pointer,
                            pointer_software_rendering,
                        } = connection_activation.state
                        {
                            debug!(
                                ?desktop_size,
                                "Deactivation-Reactivation Sequence completed"
                            );
                            // Update image size with the new desktop size.
                            image = DecodedImage::new(
                                PixelFormat::RgbA32,
                                desktop_size.width,
                                desktop_size.height,
                            );
                            // Update the active stage with the new channel IDs and pointer settings.
                            active_stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id,
                                    user_channel_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                }
                                .build(),
                            );
                            active_stage.set_enable_server_pointer(enable_server_pointer);
                            break 'activation_seq;
                        }
                    }
                }
                ActiveStageOutput::Terminate(reason) => break 'outer reason,
            }
        }

        // Check if we have a pending resize and DisplayControl is now available
        if let Some((width, height, scale_factor, physical_size)) = pending_initial_resize.take() {
            if let Some(result) = active_stage.encode_resize(
                width as u32,
                height as u32,
                Some(scale_factor),
                physical_size,
            ) {
                info!(
                    width,
                    height, scale_factor, "📐 Sending queued initial resize request"
                );
                match result {
                    Ok(frame) => {
                        writer
                            .write_all(&frame)
                            .await
                            .map_err(|e| session::custom_err!("write pending resize", e))?;
                    }
                    Err(e) => {
                        warn!("Failed to encode queued resize: {}", e);
                    }
                }
            } else {
                // Still not available, put it back
                pending_initial_resize = Some((width, height, scale_factor, physical_size));
            }
        }
    };

    Ok(RdpControlFlow::TerminatedGracefully(disconnect_reason))
}
