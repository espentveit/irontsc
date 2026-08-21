use core::num::NonZeroU16;
use std::future::pending;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp::cliprdr::pdu::FileDescriptor;
use ironrdp::cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, ClipboardFormatName,
    ClipboardGeneralCapabilityFlags, FileContentsFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse, PackedFileList,
};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{ConnectionResult, ConnectorResult};
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::graphics::pointer::DecodedPointer;
use ironrdp::pdu::PduResult;
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
use ironrdp::svc::{ChannelFlags, SvcMessage, SvcProcessor, TransportContext};
use ironrdp::{cliprdr, connector, rdpdr, rdpsnd, session};
use ironrdp_connector::legacy;
use ironrdp_core::impl_as_any;
use ironrdp_core::{Encode, IntoOwned, WriteBuf, WriteCursor};
use ironrdp_pdu::nego;
use ironrdp_rdpsnd_native::cpal;
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use ironrdp_tokio::{FramedWrite, single_sequence_step_read, split_tokio_framed};

use crate::transport_rules::{TransportRules, TunnelState, TransportRoute};
use rdpdr::NoopRdpdrBackend;
use smallvec::SmallVec;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, lookup_host};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use arboard::Clipboard;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{Config, Destination, RDCleanPathConfig};
use crate::udp_transport::{
    UdpTransportCommand, UdpTransportConfig, UdpTransportEvent, UdpTransportManager,
};
use ironrdp_udp::{CorrelationId as UdpCorrelationId, TransportMode};

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

#[derive(Debug, Clone)]
pub struct ConnectionStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub roundtrip_time_ms: Option<u32>,
    pub transport_protocol: String, // "TCP", "UDP", "TCP+UDP"
}

#[derive(Debug)]
pub enum RdpOutputEvent {
    Image {
        buffer: Arc<Vec<u8>>,
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
    ConnectionStats(ConnectionStats),
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
                self.config.disable_udp,
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
    let mut drdynvc = ironrdp::dvc::DrdynvcClient::new();

    // NOTE: CoreInput and MouseCursor channels are intentionally NOT registered here.
    // The server will create these channels via DVC CREATE requests, and we respond with NO_LISTENER.
    // This allows the server's native input handling to work properly without interference from mock handlers.

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

        let gfx_state = GfxState::new(Box::new(DummyEventSender), config.h264_hw_accel)
            .expect("Failed to initialize GFX state");
        let gfx_processor = GfxDvcProcessor::new(gfx_state);

        drdynvc = drdynvc.with_dynamic_channel(gfx_processor);

        info!("📌 RDPEGFX channel registered (will reject CREATE until Soft-Sync completes)");
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
            VideoRedirectionManager::new(Box::new(DummyEventSender), config.h264_hw_accel)
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

    // Add Display Control channel for dynamic resolution changes
    {
        use ironrdp_displaycontrol::client::DisplayControlClient;

        info!("Registering Display Control channel (Microsoft::Windows::RDS::DisplayControl)");

        let display_control = DisplayControlClient::new(Box::new(|_caps| {
            // Capabilities received from server - we could log/process these if needed
            Ok(Vec::new())
        }));

        drdynvc = drdynvc.with_dynamic_channel(display_control);
    }

    // Register proper DVC handlers for protocol compliance
    // These channels MUST respond with success to CREATE requests or the graphics pipeline fails
    {
        use crate::core_input_channel::CoreInputProcessor;
        use crate::mouse_cursor_channel::MouseCursorProcessor;
        use crate::stub_dvc::StubDvcProcessor;

        info!("Registering DVC handlers for protocol compliance...");

        // Input-related channels with proper implementations
        drdynvc = drdynvc.with_dynamic_channel(CoreInputProcessor::new());
        drdynvc = drdynvc.with_dynamic_channel(MouseCursorProcessor::new());

        // Input-related channels - stubs for now (Microsoft::Windows::RDS::Input uses advanced input protocol MS-RDPEAI)
        drdynvc =
            drdynvc.with_dynamic_channel(StubDvcProcessor::new("cliprdr"));
        drdynvc =
            drdynvc.with_dynamic_channel(StubDvcProcessor::new("Microsoft::Windows::RDS::Input"));
        drdynvc =
            drdynvc.with_dynamic_channel(StubDvcProcessor::new("TextInput_ServerToClientDVC"));

        // // Other protocol channels - stubs (would need full protocol implementations)
        // drdynvc =
        //     drdynvc.with_dynamic_channel(StubDvcProcessor::new("Microsoft::Windows::RDS::Notify"));
        // drdynvc = drdynvc.with_dynamic_channel(StubDvcProcessor::new("RDCamera_Device_Enumerator"));

        info!(
            "DVC handlers registered: CoreInput, MouseCursor, Input (stub), TextInput (stub), Notify (stub), RDCamera (stub)"
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

    info!("✅ Multitransport capability advertised, waiting for server request...");

    // Extract correlation_id from connection_result for later use
    let correlation_id = connection_result.correlation_id;
    if let Some(ref corr_id) = correlation_id {
        debug!(
            "Correlation ID available for multitransport: {:02x?}",
            &corr_id[..8]
        );
    } else {
        warn!("⚠️  No correlation ID - server-initiated multitransport will not be possible");
    }

    Ok((connection_result, upgraded_framed, client_addr))
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

    let mut drdynvc = ironrdp::dvc::DrdynvcClient::new();

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

struct ActiveUdpTunnel {
    request_id: u32,
    protocol: MultitransportProtocol,
    security_cookie: [u8; 16],
    initiator_id: u16,
    channel_id: u16,
    command_tx: mpsc::UnboundedSender<UdpTransportCommand>,
    event_rx: mpsc::UnboundedReceiver<UdpTransportEvent>,
    connected: bool,
    tunnel_established: bool,
    tunnel_established_time: Option<std::time::Instant>,
    soft_sync_received: bool,
}

impl Drop for ActiveUdpTunnel {
    fn drop(&mut self) {
        debug!(
            request_id = self.request_id,
            protocol = ?self.protocol,
            "Shutting down UDP transport manager"
        );

        if let Err(err) = self.command_tx.send(UdpTransportCommand::Shutdown) {
            trace!(
                request_id = self.request_id,
                "Failed to signal UDP shutdown (likely already dropped): {err}"
            );
        }
    }
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
            trace!(
                channel_id,
                expected_channel,
                "Skipping multitransport detection on non-message MCS channel (avoiding false positives from binary data)"
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

/// Detect and decode auto-detect requests (RTT measure, bandwidth measure, etc.)
fn detect_autodetect_request(
    action: ironrdp::pdu::Action,
    payload: &[u8],
    message_channel_id: Option<u16>,
) -> Option<Vec<u8>> {
    use ironrdp::pdu::Action;
    use ironrdp_core::{Decode, ReadCursor};

    // Only check X224 frames
    if action != Action::X224 {
        return None;
    }

    // Decode the SendDataIndication envelope
    let send_ctx = legacy::decode_send_data_indication(payload).ok()?;
    let channel_id = send_ctx.channel_id;
    let user_data = send_ctx.user_data;

    // Auto-detect messages are sent on the MCS Message Channel
    if let Some(expected_channel) = message_channel_id {
        if channel_id != expected_channel {
            return None;
        }
    }

    // Look for security header with AUTODETECT_REQ flag
    for offset in 0..user_data.len().saturating_sub(8) {
        let mut cursor = ReadCursor::new(&user_data[offset..]);

        let flags_bits = cursor.read_u16();
        let flags = BasicSecurityHeaderFlags::from_bits(flags_bits)?;

        if !flags.contains(BasicSecurityHeaderFlags::AUTODETECT_REQ) {
            continue;
        }

        let _security_flags_hi = cursor.read_u16();

        // The remaining data should be the auto-detect request PDU
        let remaining = cursor.remaining();
        if remaining.len() >= 6 {
            // Minimum auto-detect header size
            debug!(
                "📊 Detected auto-detect request ({} bytes)",
                remaining.len()
            );
            return Some(remaining.to_vec());
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
    // Note: Working Windows mstsc client does NOT set FLAGSHI_VALID for MultitransportResponse
    // even though flagsHi is present. Match that behavior for compatibility.
    let security_flags = BasicSecurityHeaderFlags::TRANSPORT_RSP;

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

/// Encode auto-detect response for sending
fn encode_autodetect_response(
    response: &ironrdp::pdu::rdp::auto_detect::AutoDetectResponse,
    user_channel_id: u16,
    message_channel_id: u16,
) -> Option<Vec<u8>> {
    use ironrdp::pdu::rdp::headers::{BasicSecurityHeader, BasicSecurityHeaderFlags};
    use ironrdp_connector::legacy;
    use ironrdp_core::{Encode, WriteBuf, WriteCursor};

    // Step 1: Encode the auto-detect response PDU
    let mut pdu_buffer = vec![0u8; response.size()];
    let mut pdu_cursor = WriteCursor::new(&mut pdu_buffer);
    if let Err(e) = response.encode(&mut pdu_cursor) {
        warn!("Failed to encode auto-detect response PDU: {:?}", e);
        return None;
    }

    // Step 2: Wrap in security header with AUTODETECT_RSP flag
    let sec_header = BasicSecurityHeader {
        flags: BasicSecurityHeaderFlags::AUTODETECT_RSP,
    };

    let header_size = sec_header.size();
    let total_size = header_size + pdu_buffer.len();
    let mut wrapped_pdu = vec![0u8; total_size];

    let mut header_cursor = WriteCursor::new(&mut wrapped_pdu[..header_size]);
    if let Err(e) = sec_header.encode(&mut header_cursor) {
        warn!("Failed to encode auto-detect security header: {:?}", e);
        return None;
    }

    wrapped_pdu[header_size..].copy_from_slice(&pdu_buffer);

    // Step 3: Wrap in MCS Send Data Request
    let mut buf = WriteBuf::new();
    if let Err(e) = legacy::encode_send_data_request(
        user_channel_id,
        message_channel_id,
        &wrapped_pdu,
        &mut buf,
    ) {
        warn!("Failed to encode auto-detect send data request: {:?}", e);
        return None;
    }

    Some(buf.filled().to_vec())
}

/// Handle auto-detect request and generate response
fn handle_autodetect_request(
    request_data: &[u8],
    bandwidth_start_time: &mut Option<std::time::Instant>,
    bandwidth_byte_count: &mut u32,
    bandwidth_sequence: &mut Option<u16>,
    last_rtt_ms: &mut Option<u32>,
    user_channel_id: u16,
    message_channel_id: u16,
) -> Option<Vec<u8>> {
    use ironrdp::pdu::rdp::auto_detect::*;
    use ironrdp_core::{Encode, WriteCursor};

    // Decode the auto-detect request
    let request = match AutoDetectRequest::decode_from_buffer(request_data) {
        Ok(req) => req,
        Err(e) => {
            warn!("Failed to decode auto-detect request: {:?}", e);
            return None;
        }
    };

    match request {
        AutoDetectRequest::RttMeasure(rtt_req) => {
            trace!(
                "📊 Received RTT Measure Request (seq={})",
                rtt_req.sequence_number
            );

            // Respond immediately with RTT Measure Response
            let response =
                AutoDetectResponse::RttMeasure(RttMeasureResponse::new(rtt_req.sequence_number));

            trace!(
                "📤 Sending RTT Measure Response (seq={})",
                rtt_req.sequence_number
            );
            encode_autodetect_response(&response, user_channel_id, message_channel_id)
        }

        AutoDetectRequest::BandwidthMeasureStart(start_req) => {
            trace!(
                "📊 Received Bandwidth Measure Start (seq={})",
                start_req.sequence_number
            );

            // Start bandwidth measurement
            *bandwidth_start_time = Some(std::time::Instant::now());
            *bandwidth_byte_count = 0;
            *bandwidth_sequence = Some(start_req.sequence_number);

            // No immediate response needed
            None
        }

        AutoDetectRequest::BandwidthMeasureStop(stop_req) => {
            trace!(
                "📊 Received Bandwidth Measure Stop (seq={}, type=0x{:04x}, payload_len={:?})",
                stop_req.sequence_number, stop_req.request_type, stop_req.payload_length
            );

            // Calculate bandwidth and send results
            if let Some(start_time) = *bandwidth_start_time {
                let elapsed = start_time.elapsed();
                let time_delta_ms = elapsed.as_millis() as u32;

                // Estimate RTT from bandwidth measurement timing (rough approximation)
                *last_rtt_ms = Some(time_delta_ms);

                trace!(
                    "📊 Bandwidth measurement: {} bytes in {}ms",
                    *bandwidth_byte_count, time_delta_ms
                );

                // Determine response type based on request type
                let response_type = match stop_req.request_type {
                    0x002B => AutoDetectResponseType::BandwidthMeasureResultsConnectTime as u16,
                    0x0429 => AutoDetectResponseType::BandwidthMeasureResultsAfterConnect as u16,
                    0x0629 => AutoDetectResponseType::BandwidthMeasureResultsAfterConnect as u16,
                    _ => AutoDetectResponseType::BandwidthMeasureResultsAfterConnect as u16,
                };

                let response =
                    AutoDetectResponse::BandwidthMeasureResults(BandwidthMeasureResults::new(
                        stop_req.sequence_number,
                        response_type,
                        time_delta_ms,
                        *bandwidth_byte_count,
                    ));

                // Reset measurement state
                *bandwidth_start_time = None;
                *bandwidth_byte_count = 0;
                *bandwidth_sequence = None;

                trace!(
                    "📤 Sending Bandwidth Measure Results (seq={}, {}ms, {} bytes)",
                    stop_req.sequence_number, time_delta_ms, *bandwidth_byte_count
                );
                encode_autodetect_response(&response, user_channel_id, message_channel_id)
            } else {
                warn!("Received Bandwidth Measure Stop without Start");
                None
            }
        }

        AutoDetectRequest::NetworkCharacteristicsResult(result) => {
            trace!(
                "📊 Received Network Characteristics Result: baseRTT={:?}ms, bandwidth={:?}kbps, avgRTT={:?}ms",
                result.base_rtt, result.bandwidth, result.average_rtt
            );
            // This is informational from server, no response needed
            None
        }
    }
}

async fn start_udp_tunnel(
    destination: &Destination,
    correlation_id: Option<[u8; 16]>,
    selected_protocol: nego::SecurityProtocol,
    request: &MultitransportRequestInfo,
) -> anyhow::Result<ActiveUdpTunnel> {
    let target = format!("{}:{}", destination.name(), destination.port());
    let mut addrs = lookup_host(&target)
        .await
        .with_context(|| format!("failed to resolve UDP endpoint {target}"))?;
    let server_addr = addrs
        .next()
        .ok_or_else(|| anyhow!("no resolved address for UDP endpoint {target}"))?;

    let mut config = UdpTransportConfig::default();
    config.server_addr = server_addr;
    let protocol_bits = request.protocol.as_u16();
    let requested_reliable = (protocol_bits & MultitransportProtocol::RELIABLE_BIT) != 0;
    let requested_lossy = (protocol_bits & MultitransportProtocol::LOSSY_BIT) != 0;
    let fallback_reliable = !requested_reliable && !requested_lossy;

    if requested_lossy && !requested_reliable && !fallback_reliable {
        warn!(
            "Server requested UDP lossy transport only; declining because client only implements reliable mode"
        );
        return Err(anyhow!("server requested unsupported lossy-only UDP mode"));
    }

    if requested_lossy && requested_reliable {
        info!("Server offered both reliable and lossy UDP. Selecting reliable mode.");
    } else if fallback_reliable {
        info!(
            "Server did not explicitly advertise reliable UDP; proceeding with reliable mode for compatibility."
        );
    } else if requested_lossy && !requested_reliable {
        info!(
            "Server set unknown bits alongside lossy flag (0x{:04x}); proceeding with reliable mode",
            protocol_bits
        );
    }

    // Per MS-RDPEMT Section 1.5 and Appendix A:
    // - TLS is REQUIRED for RELIABLE UDP when Enhanced RDP Security is in effect
    // - DTLS is REQUIRED for LOSSY UDP when Enhanced RDP Security is in effect
    // - Standard RDP Security allows unencrypted UDP (can be reliable or lossy)
    //
    // The transport mode is determined by the encryption protocol used:
    // - use_tls=true → Reliable mode (with retransmits, ordering, etc.)
    // - use_dtls=true → Lossy mode (best-effort delivery)
    // - Both false → Reliable mode (default for compatibility)

    let use_enhanced_security = selected_protocol.contains(nego::SecurityProtocol::SSL)
        || selected_protocol.contains(nego::SecurityProtocol::HYBRID)
        || selected_protocol.contains(nego::SecurityProtocol::HYBRID_EX);

    if use_enhanced_security {
        // For Enhanced RDP Security, use TLS with Reliable mode
        // (DTLS/Lossy would be used for scenarios requiring lower latency at cost of reliability)
        info!("🔐 Enhanced RDP Security detected - TLS will be used for reliable UDP tunnel");
        config.use_tls = true;
        config.use_dtls = false;
    } else {
        // Standard RDP Security - use unencrypted Reliable mode for compatibility
        info!("ℹ️  Standard RDP Security - UDP tunnel will be unencrypted (reliable mode)");
        config.use_tls = false;
        config.use_dtls = false;
    }

    let correlation = correlation_id.map(UdpCorrelationId::new);
    let server_name = destination.name().to_string();

    let (mut manager, command_tx, event_rx) =
        UdpTransportManager::new(config, correlation, server_name)
            .await
            .context("failed to create UDP transport manager")?;

    manager.set_tunnel_params(request.request_id, request.security_cookie);

    tokio::spawn(async move {
        if let Err(e) = manager.run().await {
            error!("UDP transport task error: {}", e);
        }
    });

    Ok(ActiveUdpTunnel {
        request_id: request.request_id,
        protocol: request.protocol,
        security_cookie: request.security_cookie,
        initiator_id: request.initiator_id,
        channel_id: request.channel_id,
        command_tx,
        event_rx,
        connected: false,
        tunnel_established: false,
        tunnel_established_time: None,
        soft_sync_received: false,
    })
}

async fn active_session<T: RdpEventSender + Clone>(
    framed: UpgradedFramed,
    connection_result: ConnectionResult,
    event_loop_proxy: &T,
    input_event_receiver: &mut mpsc::UnboundedReceiver<RdpInputEvent>,
    destination: Destination,
    client_addr: SocketAddr,
    disable_udp: bool,
) -> SessionResult<RdpControlFlow> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let mut image = DecodedImage::new(
        PixelFormat::RgbA32,
        connection_result.desktop_size.width,
        connection_result.desktop_size.height,
    );

    // Extract needed values before connection_result is consumed
    let io_channel_id = connection_result.io_channel_id;
    let user_channel_id = connection_result.user_channel_id;
    let correlation_id = connection_result.correlation_id;
    let message_channel_id = connection_result.message_channel_id;
    let selected_protocol = connection_result.selected_protocol;

    // Extract multitransport information received during connection
    let multitransport_request_id = connection_result.multitransport_request_id;
    let multitransport_security_cookie = connection_result.multitransport_security_cookie;
    let multitransport_protocol = connection_result.multitransport_protocol;

    info!(
        "🔍 Multitransport info: request_id={:?}, cookie={:?}, protocol={:?}",
        multitransport_request_id,
        multitransport_security_cookie.as_ref().map(|c| &c[..8]),
        multitransport_protocol
    );

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

    // Initialize transport rule engine for UDP multitransport
    let mut transport_rules = TransportRules::new();

    // Track active UDP transport tunnel (currently only one tunnel is supported)
    let mut udp_tunnels: std::collections::HashMap<u32, ActiveUdpTunnel> =
        std::collections::HashMap::new();

    // Track bandwidth measurement state for auto-detect
    let mut bandwidth_measure_start_time: Option<std::time::Instant> = None;
    let mut bandwidth_measure_byte_count: u32 = 0;
    let mut bandwidth_measure_sequence: Option<u16> = None;

    // Track connection statistics for UI
    let mut total_bytes_sent: u64 = 0;
    let mut total_bytes_received: u64 = 0;
    let mut last_rtt_ms: Option<u32> = None;
    let mut stats_timer = tokio::time::interval(tokio::time::Duration::from_millis(500));
    stats_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // **CRITICAL**: Automatically create UDP tunnel if multitransport was negotiated during connection
    // Per MS-RDPEMT spec, the InitiateMultitransportRequest is sent during CapabilitiesExchange,
    // and we responded with InitiateMultitransportResponse. Now we need to create the actual
    // UDP tunnel and send the Tunnel Create Request PDU.
    if disable_udp {
        info!("🚫 UDP transport disabled by user configuration");
    } else if let (Some(request_id), Some(security_cookie), Some(protocol)) = (
        multitransport_request_id,
        multitransport_security_cookie,
        multitransport_protocol,
    ) {
        info!(
            "🚀 Auto-starting UDP tunnel from stored multitransport info: request_id={}, protocol={:?}",
            request_id, protocol
        );

        // Create a synthetic MultitransportRequestInfo from the stored data
        let protocol_bits = protocol.as_u16();
        let requested_reliable = (protocol_bits & MultitransportProtocol::RELIABLE_BIT) != 0;
        let requested_lossy = (protocol_bits & MultitransportProtocol::LOSSY_BIT) != 0;
        let fallback_reliable = !requested_reliable && !requested_lossy;

        if requested_lossy && !requested_reliable && !fallback_reliable {
            warn!(
                "Server requested UDP lossy transport only; declining because client only implements reliable mode"
            );
        } else {
            // Create synthetic request info for start_udp_tunnel
            // Use actual MCS channel IDs from the connection instead of 0
            // initiator_id = user_channel_id (the client's MCS user ID)
            // channel_id = message_channel_id or io_channel_id as fallback
            let request_info = MultitransportRequestInfo {
                request_id,
                protocol,
                security_cookie,
                security_flags_hi: 0x0fd0, // Standard flags
                initiator_id: user_channel_id,
                channel_id: message_channel_id.unwrap_or(io_channel_id),
            };

            match start_udp_tunnel(
                &destination,
                correlation_id,
                selected_protocol,
                &request_info,
            )
            .await
            {
                Ok(handle) => {
                    info!(
                        "✅ UDP transport auto-initialized successfully for request_id={}",
                        request_id
                    );
                    
                    // Register tunnel with transport rules engine
                    // tunnel_type is derived from protocol (0x00000001 for UDPFECR reliable)
                    let tunnel_type = 0x00000001u32; // MS-RDPEMT TUNNELTYPE_UDPFECR
                    transport_rules.register_tunnel(request_id, tunnel_type);
                    transport_rules.transition_tunnel(request_id, TunnelState::Requested);
                    
                    udp_tunnels.insert(request_id, handle);
                    
                    // CRITICAL: Send MultitransportResponse immediately BEFORE TunnelCreateRequest
                    // Per MS-RDPBCGR §1.3.6.2, client MUST respond to InitiateMultitransportRequest
                    // The server will not send TunnelCreateResponse until it receives this response
                    info!("📨 Encoding MultitransportResponse for stored request (required by MS-RDPBCGR)");
                    match encode_multitransport_response_frame(request_info, message_channel_id) {
                        Ok(frame) => {
                            info!(
                                "✅ Sending MultitransportResponse (S_OK) immediately for request_id={} BEFORE TunnelCreateRequest",
                                request_id
                            );
                            // Send immediately - this must happen BEFORE the UDP transport sends TunnelCreateRequest
                            total_bytes_sent += frame.len() as u64;
                            if let Err(e) = writer.write_all(&frame).await {
                                error!("❌ Failed to send MultitransportResponse: {:?}", e);
                            } else {
                                info!("📤 MultitransportResponse sent successfully ({} bytes)", frame.len());
                            }
                        }
                        Err(err) => {
                            error!(
                                "❌ Failed to encode MultitransportResponse for request_id={}: {:?}",
                                request_id, err
                            );
                        }
                    }
                }
                Err(err) => {
                    error!(
                        "❌ Failed to auto-initialize UDP transport for request_id={}: {}",
                        request_id, err
                    );
                }
            }
        }
    } else if multitransport_request_id.is_some()
        || multitransport_security_cookie.is_some()
        || multitransport_protocol.is_some()
    {
        warn!(
            "⚠️  Incomplete multitransport info stored (request_id={:?}, cookie={:?}, protocol={:?})",
            multitransport_request_id.is_some(),
            multitransport_security_cookie.is_some(),
            multitransport_protocol.is_some()
        );
    }

    let disconnect_reason = 'outer: loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                match frame {
                    Ok((action, payload)) => {
                        trace!(?action, frame_length = payload.len(), "Frame received");

                        // Track bytes for bandwidth measurement if active
                        if bandwidth_measure_start_time.is_some() {
                            bandwidth_measure_byte_count += payload.len() as u32;
                        }

                        // Track total bytes received
                        total_bytes_received += payload.len() as u64;

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

                // NOTE: MultitransportResponse will be sent AFTER TunnelEstablished event
                // (see TunnelEstablished handler below)
                // Per MS-RDPBCGR: Response must be sent after tunnel is fully established
                // Sending it too early causes Windows RDP servers to send Demand Active prematurely
                info!("⏸️  Deferring MultitransportResponse until tunnel is established");

                let protocol_bits = request_info.protocol.as_u16();
                let requested_reliable =
                    (protocol_bits & MultitransportProtocol::RELIABLE_BIT) != 0;
                let requested_lossy =
                    (protocol_bits & MultitransportProtocol::LOSSY_BIT) != 0;
                let fallback_reliable = !requested_reliable && !requested_lossy;

                if fallback_reliable {
                    info!(
                        "   request_id={}, Protocol omitted reliable/lossy bits; defaulting to reliable mode",
                        request_id
                    );
                }

                // Start UDP transport if supported
                if disable_udp {
                    info!(
                        "🚫 Multitransport request ignored: UDP disabled by user (request_id={})",
                        request_id
                    );
                } else if udp_tunnels.contains_key(&request_id) {
                    warn!(
                        "⚠️  Duplicate multitransport request (id={}) received; ignoring",
                        request_id
                    );
                } else if requested_lossy || requested_reliable || fallback_reliable {
                    match start_udp_tunnel(
                        &destination,
                        correlation_id,
                        selected_protocol,
                        &request_info,
                    )
                    .await
                    {
                        Ok(handle) => {
                            info!("🚀 UDP transport initialization started for request_id={}", request_id);
                            udp_tunnels.insert(request_id, handle);
                        }
                        Err(err) => {
                            error!(
                                "❌ Failed to initialize UDP transport for request_id={}: {}",
                                request_id, err
                            );
                        }
                    }
                } else {
                    warn!(
                        "⚠️  Unsupported multitransport protocol {:?} (request_id={}), ignoring",
                        request_info.protocol,
                        request_id
                    );
                }

                info!("✅ Multitransport request handled, awaiting UDP handshake events");
                extra_outputs
                } else if let Some(autodetect_data) = detect_autodetect_request(action, &payload, message_channel_id) {
                    // Process auto-detect request
                    let msg_ch_id = message_channel_id.unwrap_or(io_channel_id);
                    if let Some(response_bytes) = handle_autodetect_request(
                        &autodetect_data,
                        &mut bandwidth_measure_start_time,
                        &mut bandwidth_measure_byte_count,
                        &mut bandwidth_measure_sequence,
                        &mut last_rtt_ms,
                        user_channel_id,
                        msg_ch_id,
                    ) {
                        extra_outputs.push(ActiveStageOutput::ResponseFrame(response_bytes.into()));
                        extra_outputs
                    } else {
                        // No response needed or error occurred
                        vec![]
                    }
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
            udp_event = async {
                // Poll all active tunnels for events
                use futures_util::future::select_all;
                if udp_tunnels.is_empty() {
                    pending::<Option<UdpTransportEvent>>().await
                } else {
                    // Create futures for all tunnel receivers
                    let futures: Vec<_> = udp_tunnels
                        .values_mut()
                        .map(|tunnel| Box::pin(tunnel.event_rx.recv()))
                        .collect();

                    if futures.is_empty() {
                        pending::<Option<UdpTransportEvent>>().await
                    } else {
                        // select_all returns (result, index, remaining_futures)
                        let (result, _index, _remaining) = select_all(futures).await;
                        result
                    }
                }
            } => {
                let mut outputs = Vec::new();
                match udp_event {
                    Some(UdpTransportEvent::Connected { request_id }) => {
                        if let Some(tunnel) = udp_tunnels.get_mut(&request_id) {
                            tunnel.connected = true;
                        }
                        // Transition tunnel state: Connected (UDP handshake complete)
                        transport_rules.transition_tunnel(request_id, TunnelState::Connected);
                        info!("✅ UDP transport connected (request_id={}, SYN/SYN+ACK complete)", request_id);
                    }
                    Some(UdpTransportEvent::HandshakeComplete { request_id }) => {
                        // Transition tunnel state: TlsReady (TLS/DTLS handshake complete)
                        transport_rules.transition_tunnel(request_id, TunnelState::TlsReady);
                        info!("🔐 TLS/DTLS handshake complete for request_id={}, awaiting TunnelCreateResponse", request_id);
                        // Note: MultitransportResponse will be sent after TunnelEstablished event
                        // This is critical - we must wait for TunnelCreateResponse before sending MultitransportResponse
                    }
                    Some(UdpTransportEvent::TunnelEstablished { request_id }) => {
                        info!("🔐 UDP tunnel established (request_id={}, MS-RDPEMT)", request_id);
                        
                        // Transition tunnel state: Established (ready for Soft-Sync)
                        transport_rules.transition_tunnel(request_id, TunnelState::Established);
                        
                        // Register the available tunnel with DVC client for Soft-Sync IMMEDIATELY
                        // This MUST happen before processing any incoming DVC messages (like Soft-Sync Request)
                        // Per MS-RDPEDYC §3.2.5.3.2: Client should only confirm tunnels it has successfully established
                        let tunnel_type = 0x00000001; // TUNNELTYPE_UDPFECR (reliable UDP)
                        if let Some(drdynvc) = active_stage.get_svc_processor_mut::<ironrdp_dvc::DrdynvcClient>() {
                            match drdynvc.register_available_tunnel(tunnel_type) {
                                Ok(deferred_messages) => {
                                    info!("   Registered tunnel_type=0x{:08X} as available for Soft-Sync", tunnel_type);
                                    if !deferred_messages.is_empty() {
                                        info!("   Processing {} deferred DVC message(s)", deferred_messages.len());
                                        let frame = active_stage.encode_dvc_messages(deferred_messages)?;
                                        outputs.push(ActiveStageOutput::ResponseFrame(frame));

                                        // Check if we have a pending resize now that channels (like DisplayControl) may be ready
                                        if let Some((width, height, scale_factor, physical_size)) = pending_initial_resize.take() {
                                            match active_stage.encode_resize(width as u32, height as u32, Some(scale_factor), physical_size) {
                                                Some(Ok(frame)) => {
                                                    info!(width, height, scale_factor, "📐 Sending queued resize after channel creation");
                                                    outputs.push(ActiveStageOutput::ResponseFrame(frame));
                                                }
                                                Some(Err(e)) => {
                                                    warn!("Failed to encode queued resize: {}", e);
                                                }
                                                None => {
                                                    pending_initial_resize = Some((width, height, scale_factor, physical_size));
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to register available tunnel: {:?}", e);
                                }
                            }
                        }

                        if let Some(tunnel) = udp_tunnels.get_mut(&request_id) {
                            tunnel.tunnel_established = true;
                            tunnel.tunnel_established_time = Some(std::time::Instant::now());
                        }

                        // NOW send MultitransportResponse on TCP channel (per MS-RDPBCGR spec)
                        // This must be sent AFTER the tunnel is fully established (TunnelCreateResponse received)
                        // Sending it too early (e.g., after TLS handshake) causes server-side RDP_SEC error 0x8007139F
                        if let Some(tunnel) = udp_tunnels.get(&request_id) {
                            let request_info = MultitransportRequestInfo {
                                request_id,
                                protocol: tunnel.protocol,
                                security_cookie: tunnel.security_cookie,
                                security_flags_hi: 0x0fd0, // Standard flags
                                initiator_id: tunnel.initiator_id,
                                channel_id: tunnel.channel_id,
                            };

                            match encode_multitransport_response_frame(request_info, message_channel_id) {
                                Ok(frame) => {
                                    info!(
                                        "📨 Sending MultitransportResponse (S_OK) on TCP for request_id={} (AFTER tunnel established)",
                                        request_id
                                    );
                                    outputs.push(ActiveStageOutput::ResponseFrame(frame));
                                }
                                Err(err) => {
                                    error!(
                                        "❌ Failed to encode MultitransportResponse for request_id={}: {:?}",
                                        request_id, err
                                    );
                                }
                            }
                        } else {
                            warn!("⚠️  TunnelEstablished received but tunnel info not found for request_id={}", request_id);
                        }

                        info!("⏳ Awaiting Soft-Sync negotiation before switching graphics to UDP");
                        info!("   Note: Server has 10 seconds to send Soft-Sync request, otherwise traffic stays on TCP");
                    }
                    Some(UdpTransportEvent::TunnelDvcData { request_id, data: dvc_data }) => {
                        // DVC data extracted from tunnel DATA packet - process it via unified processor
                        info!("📨 Processing {} bytes of DVC data from UDP tunnel (request_id={})", dvc_data.len(), request_id);
                        if dvc_data.len() > 0 {
                            info!("   First 32 bytes: {:02x?}", &dvc_data[..dvc_data.len().min(32)]);
                        }

                        // Skip empty payloads (keep-alive/framing packets)
                        if dvc_data.is_empty() {
                            info!("   Skipping empty TunnelData payload (keep-alive)");
                            continue;
                        }

                        // Determine transport context using rule engine
                        // RULE: Use UDP only if tunnel has completed Soft-Sync, otherwise TCP
                        // Per MS-RDPEDYC: "The server manager and client manager MUST NOT send or receive
                        // any dynamic virtual channel data on the multitransport tunnels until the Soft-Sync
                        // negotiation has completed."
                        let transport_context = transport_rules.route_incoming_tunnel_data(request_id).to_context();
                        
                        info!("   🎯 Transport rule decision: {:?} for tunnel {}", transport_context, request_id);

                        // Process via unified DVC processor with transport context
                        // The processor will tag responses with the appropriate transport (TCP or UDP tunnel)
                        if let Some(drdynvc) = active_stage.get_svc_processor_mut::<ironrdp_dvc::DrdynvcClient>() {
                            info!("   📋 Feeding to DRDYNVC processor with {:?} transport context...", transport_context);
                            match drdynvc.process(&dvc_data, transport_context) {
                                Ok(response_messages) => {
                                    info!("   ✅ DRDYNVC returned {} response messages", response_messages.len());

                                    // Route responses based on their transport context
                                    for msg in response_messages {
                                        info!("   🔍 Response has transport: {:?}, PDU: {}", msg.transport(), msg.pdu_name());
                                        match msg.transport() {
                                            Some(TransportContext::Tcp) | None => {
                                                // Send via TCP
                                                info!("   📡 Routing {} response to TCP", msg.pdu_name());
                                                debug!("   � Routing {} response to TCP", msg.pdu_name());
                                                match active_stage.encode_dvc_messages(vec![msg]) {
                                                    Ok(frame) if !frame.is_empty() => {
                                                        if let Err(e) = writer.write_all(&frame).await {
                                                            warn!("Failed to write DVC response to TCP: {:?}", e);
                                                        }
                                                    }
                                                    Err(e) => warn!("Failed to encode DVC TCP message: {:?}", e),
                                                    _ => {}
                                                }
                                            }
                                            Some(TransportContext::UdpTunnel(tunnel_id)) => {
                                                // Send via UDP tunnel. The tunnel carries the bare
                                                // channel PDU wrapped in an MS-RDPEMT tunnel header and
                                                // encrypted with the tunnel's TLS session, so the
                                                // TCP-framed encoding must not be used here, and the
                                                // payload must not be sent as raw UDP data.
                                                info!("   📤 Routing {} response to UDP tunnel {}", msg.pdu_name(), tunnel_id);
                                                match msg.to_pdu_bytes() {
                                                    Ok(dvc_pdu) if !dvc_pdu.is_empty() => {
                                                        if let Some(tunnel) = udp_tunnels.get_mut(&tunnel_id) {
                                                            let command = UdpTransportCommand::SendDvcData {
                                                                request_id: tunnel_id,
                                                                data: dvc_pdu,
                                                            };
                                                            if let Err(e) = tunnel.command_tx.send(command) {
                                                                warn!("Failed to send response via UDP: {:?}", e);
                                                            }
                                                        } else {
                                                            warn!("No UDP tunnel found for tunnel_id={}", tunnel_id);
                                                        }
                                                    }
                                                    Ok(_) => warn!("Encoded an empty {} PDU for UDP tunnel {}", msg.pdu_name(), tunnel_id),
                                                    Err(e) => warn!("Failed to encode DVC UDP message: {:?}", e),
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("❌ Failed to process tunnel DVC data: {:?}", e);
                                }
                            }
                        } else {
                            warn!("Tunnel DVC data received but DRDYNVC processor not available");
                        }
                    }
                    Some(UdpTransportEvent::SoftSyncCompleted { request_id, tunnel_type }) => {
                        info!("🔄 Received SoftSyncCompleted event for request_id={}, tunnel_type=0x{:08X}", request_id, tunnel_type);

                        // Transition tunnel state: SoftSyncComplete (tunnel can now carry DVC data)
                        transport_rules.transition_tunnel(request_id, TunnelState::SoftSyncComplete);

                        // Mark that Soft-Sync was received (legacy field, will be removed)
                        if let Some(tunnel) = udp_tunnels.get_mut(&request_id) {
                            tunnel.soft_sync_received = true;
                        }

                        // Enable UDP mode for the GFX channel
                        use crate::gfx_channel::GfxDvcProcessor;
                        if let Some(channel) = active_stage.get_dvc_mut::<GfxDvcProcessor>() {
                            if let Some(gfx) = channel.channel_processor_downcast_mut::<GfxDvcProcessor>() {
                                gfx.enable_udp_mode();
                                info!("✅ UDP mode enabled for graphics channel");
                            } else {
                                warn!("SoftSyncCompleted received but GFX processor could not be downcast");
                            }
                        } else {
                            warn!("SoftSyncCompleted received but GFX processor is unavailable");
                        }

                        // Process any pending resize now that soft sync is complete
                        if let Some((width, height, scale_factor, physical_size)) = pending_initial_resize.take() {
                            match active_stage.encode_resize(width as u32, height as u32, Some(scale_factor), physical_size) {
                                Some(Ok(frame)) if !frame.is_empty() => {
                                    info!(width, height, scale_factor, "📐 Sending queued resize after soft-sync");
                                    if let Err(e) = writer.write_all(&frame).await {
                                        warn!("Failed to write queued resize: {}", e);
                                    }
                                }
                                Some(Err(e)) => warn!("Failed to encode queued resize: {}", e),
                                _ => {
                                    // DisplayControl is not up yet. Dropping the resize here is
                                    // what left the session at the wrong DPI: the scale is only
                                    // discovered once the window has a surface, so this queued
                                    // resize is the only thing carrying it, and nothing produces
                                    // another until the user resizes by hand.
                                    info!(
                                        width,
                                        height,
                                        scale_factor,
                                        "📐 DisplayControl not ready, keeping resize queued"
                                    );
                                    pending_initial_resize =
                                        Some((width, height, scale_factor, physical_size));
                                }
                            }
                        }
                    }
                    Some(UdpTransportEvent::RoundTrip { micros, .. }) => {
                        // The server here has network characteristics detection switched off, so
                        // the auto-detect RTT never arrives. The tunnel's own acknowledgements
                        // give a real measurement instead.
                        last_rtt_ms = Some((micros as f64 / 1000.0).round() as u32);
                    }
                    Some(UdpTransportEvent::DataReceived { request_id, data }) => {
                        debug!("📦 Received UDP data from tunnel request_id={} ({} bytes)", request_id, data.len());
                        use crate::gfx_channel::GfxDvcProcessor;
                        if let Some(channel) = active_stage.get_dvc_mut::<GfxDvcProcessor>() {
                            if let Some(gfx) = channel.channel_processor_downcast_mut::<GfxDvcProcessor>() {
                                match gfx.process_udp_data(&data) {
                                    Ok(messages) => {
                                        if !messages.is_empty() {
                                            if let Some(channel_id) = channel.channel_id() {
                                                let svc_messages = ironrdp_dvc::encode_dvc_messages(
                                                    channel_id,
                                                    messages,
                                                    ironrdp::svc::ChannelFlags::empty(),
                                                )
                                                .map_err(|e| session::custom_err!("DRDYNVC", e))?;
                                                let frame = active_stage.encode_dvc_messages(svc_messages)?;
                                                outputs.push(ActiveStageOutput::ResponseFrame(frame));
                                            } else {
                                                warn!("GFX UDP data received before channel ID was assigned");
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        warn!("❌ Failed to process RDPEGFX UDP payload: {:?}", err);
                                    }
                                }
                            } else {
                                warn!("UDP data received but GFX processor could not be downcast");
                            }
                        } else {
                            warn!("UDP data received but GFX processor is unavailable");
                        }
                    }
                    Some(UdpTransportEvent::Disconnected { request_id, reason }) => {
                        warn!("⚠️  UDP transport disconnected (request_id={}): {}", request_id, reason);
                        udp_tunnels.remove(&request_id);
                    }
                    None => {
                        warn!("⚠️  UDP transport event stream closed (one tunnel died)");
                        // Note: We can't easily determine which tunnel closed here
                        // The receiver returning None means that specific tunnel's sender was dropped
                    }
                }

                vec![]
            }
            _ = stats_timer.tick() => {
                // Send periodic connection statistics to UI
                // Update connection statistics for UI
                let stats = transport_rules.stats();
                let transport_protocol = if stats.total_tunnels == 0 {
                    "TCP".to_string()
                } else if stats.active_tunnels > 0 {
                    format!("TCP+UDP ({} active)", stats.active_tunnels)
                } else {
                    format!("TCP+UDP ({} tunnels)", stats.total_tunnels)
                };

                let conn_stats = ConnectionStats {
                    bytes_sent: total_bytes_sent,
                    bytes_received: total_bytes_received,
                    roundtrip_time_ms: last_rtt_ms,
                    transport_protocol,
                };

                let _ = event_loop_proxy.send_event(RdpOutputEvent::ConnectionStats(conn_stats));
                vec![]
            }
            input_event = input_event_receiver.recv() => {
                let input_event = input_event.ok_or_else(|| session::general_err!("GUI is stopped"))?;

                match input_event {
                    RdpInputEvent::Resize { width, height, scale_factor, physical_size } => {
                        info!(width, height, scale_factor, ?physical_size, "📐 Resize event received");

                        // Check if any tunnel is in Established state (waiting for Soft-Sync)
                        let waiting_for_soft_sync = transport_rules.active_tunnels().count() == 0
                            && udp_tunnels.values().any(|t| t.tunnel_established && !t.soft_sync_received);

                        if waiting_for_soft_sync {
                            // UDP tunnel is active but soft sync not complete yet - queue for later
                            info!("📐 Resize deferred: waiting for UDP soft-sync to complete");
                            pending_initial_resize = Some((width, height, scale_factor, physical_size));
                            vec![]
                        } else if let Some(result) = active_stage.encode_resize(
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
                        total_bytes_sent += frame.len() as u64;
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
                        let buffer = Arc::new(image.data().to_vec());

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
                                buffer: Arc::new(buffer),
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
                                buffer: Arc::new(output.data.clone()),
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
                ActiveStageOutput::Tunnel(data) => {
                    // Data received on the multitransport/tunnel channel (1008)
                    // This needs to be forwarded to the UDP transport layer(s)
                    debug!(
                        "📦 Received {} bytes on tunnel channel, forwarding to {} UDP tunnel(s)",
                        data.len(),
                        udp_tunnels.len()
                    );

                    // Forward to all active tunnels (each will process what's relevant)
                    // Per MS-RDPEMT, tunnel control messages come via TCP channel 1008
                    for (request_id, tunnel) in &udp_tunnels {
                        if let Err(e) = tunnel
                            .command_tx
                            .send(UdpTransportCommand::TunnelData(data.clone()))
                        {
                            warn!(
                                "Failed to forward tunnel data to UDP tunnel {}: {}",
                                request_id, e
                            );
                        }
                    }

                    if udp_tunnels.is_empty() {
                        warn!("Received tunnel data but no UDP tunnels active");
                    }
                }
                ActiveStageOutput::Terminate(reason) => break 'outer reason,
            }
        }

        // Check if Soft-Sync was completed by DRDYNVC processor
        if let Some(drdynvc_client) =
            active_stage.get_svc_processor_mut::<ironrdp_dvc::DrdynvcClient>()
        {
            if let Some(tunnel_type) = drdynvc_client.soft_sync_completed() {
                info!(
                    "✅ Soft-Sync completed for tunnel_type=0x{:08X}",
                    tunnel_type
                );
                info!("   GFX channel CREATE requests will now be accepted (over UDP transport)");

                // Clear the completion flag
                drdynvc_client.clear_soft_sync_completed();

                // Signal all active UDP transports that soft-sync is complete
                for (request_id, tunnel) in &udp_tunnels {
                    if let Err(e) = tunnel
                        .command_tx
                        .send(UdpTransportCommand::SoftSyncComplete { tunnel_type })
                    {
                        warn!(
                            "Failed to send SoftSyncComplete to UDP tunnel {}: {}",
                            request_id, e
                        );
                    }
                }

                if udp_tunnels.is_empty() {
                    warn!("Soft-Sync completed but no UDP tunnels active");
                }
            }
        }

        // Check for Soft-Sync timeout using transport rules engine
        // Per MS-RDPEDYC §3.1.5.3, servers MAY skip Soft-Sync
        for tunnel_id in transport_rules.check_soft_sync_timeouts() {
            warn!(
                "⏱️  Soft-Sync timeout for tunnel {}: Server did not send Soft-Sync request within 10s",
                tunnel_id
            );
            info!("ℹ️  Per MS-RDPEDYC spec, this is allowed. Graphics traffic will remain on TCP.");
            info!("   This may indicate: (1) Server doesn't support Soft-Sync, (2) Network issues, or (3) Server policy");
            
            // Mark legacy tunnel field (will be removed)
            if let Some(tunnel) = udp_tunnels.get_mut(&tunnel_id) {
                tunnel.soft_sync_received = true;
                tunnel.tunnel_established_time = None;
            }
        }

        // Check if we have a pending resize and DisplayControl is now available
        if let Some((width, height, scale_factor, physical_size)) = pending_initial_resize.take() {
            match active_stage.encode_resize(width as u32, height as u32, Some(scale_factor), physical_size) {
                Some(Ok(frame)) => {
                    info!(width, height, scale_factor, "📐 Sending queued initial resize");
                    writer.write_all(&frame).await
                        .map_err(|e| session::custom_err!("write pending resize", e))?;
                }
                Some(Err(e)) => {
                    warn!("Failed to encode queued resize: {}", e);
                }
                None => {
                    pending_initial_resize = Some((width, height, scale_factor, physical_size));
                }
            }
        }
    };

    // Explicitly shut down UDP tunnels before closing main connection
    // Per MS-RDPEMT: "There is no explicit connection-termination protocol over a multitransport connection.
    // The client and server terminate the multitransport connection and disconnect the underlying transports
    // when the main RDP connection is disconnected."
    // We send shutdown commands and wait a bit for cleanup before closing the main connection.
    info!("🧹 Cleaning up {} UDP tunnel(s) before connection close", udp_tunnels.len());
    for (request_id, tunnel) in udp_tunnels.iter() {
        info!("📤 Sending shutdown to UDP tunnel request_id={}", request_id);
        if let Err(e) = tunnel.command_tx.send(UdpTransportCommand::Shutdown) {
            debug!("Failed to send shutdown to tunnel {}: {} (may already be closed)", request_id, e);
        }
    }
    
    // Give tunnels a moment to process shutdown cleanly
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    
    // Drop all tunnels explicitly
    udp_tunnels.clear();
    info!("✅ UDP tunnels cleaned up");

    Ok(RdpControlFlow::TerminatedGracefully(disconnect_reason))
}
