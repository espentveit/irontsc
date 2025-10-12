use core::num::NonZeroU16;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{ConnectionResult, ConnectorResult};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::graphics::pointer::DecodedPointer;
use ironrdp::pdu::PduResult;
use ironrdp::pdu::input::fast_path::FastPathInputEvent;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{
    ActiveStage, ActiveStageOutput, GracefulDisconnectReason, SessionResult, fast_path,
};
use ironrdp::svc::SvcMessage;
use ironrdp::{cliprdr, connector, rdpdr, rdpsnd, session};
use ironrdp_core::impl_as_any;
use ironrdp_core::{IntoOwned, WriteBuf};
use ironrdp_rdpsnd_native::cpal;
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use ironrdp_tokio::{FramedWrite, single_sequence_step_read, split_tokio_framed};
use rdpdr::NoopRdpdrBackend;
use smallvec::SmallVec;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use arboard::Clipboard;
use std::thread;
use std::time::Duration;

use crate::config::{Config, RDCleanPathConfig};

// Trait for sending RDP output events to the UI
pub trait RdpEventSender: Send + 'static {
    fn send_event(&self, event: RdpOutputEvent) -> Result<(), ()>;
}

#[derive(Debug)]
pub enum RdpOutputEvent {
    Image {
        buffer: Vec<u32>,
        width: NonZeroU16,
        height: NonZeroU16,
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

pub struct RdpClient<T: RdpEventSender> {
    pub config: Config,
    pub event_loop_proxy: T,
    pub input_event_receiver: mpsc::UnboundedReceiver<RdpInputEvent>,
    pub cliprdr_factory: Option<Box<dyn CliprdrBackendFactory + Send>>,
    pub dvc_pipe_proxy_factory: DvcPipeProxyFactory,
}

impl<T: RdpEventSender> RdpClient<T> {
    pub async fn run(mut self) {
        loop {
            let (connection_result, framed) =
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
) -> ConnectorResult<(ConnectionResult, UpgradedFramed)> {
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

    let mut drdynvc = ironrdp::dvc::DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));

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

    Ok((connection_result, upgraded_framed))
}

async fn connect_ws(
    config: &Config,
    rdcleanpath: &RDCleanPathConfig,
    cliprdr_factory: Option<&(dyn CliprdrBackendFactory + Send)>,
    dvc_pipe_proxy_factory: &DvcPipeProxyFactory,
) -> ConnectorResult<(ConnectionResult, UpgradedFramed)> {
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

    Ok((connection_result, upgraded_framed))
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
    last_text: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
    watcher: Option<thread::JoinHandle<()>>,
    temp_dir: String,
}

impl_as_any!(ArboardClipboardBackend);

impl ArboardClipboardBackend {
    fn new(sender: mpsc::UnboundedSender<RdpInputEvent>) -> Self {
        Self {
            sender,
            last_text: Arc::new(Mutex::new(None)),
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
        let last_text = Arc::clone(&self.last_text);

        self.watcher = Some(thread::spawn(move || {
            let mut clipboard = Clipboard::new().ok();

            while running.load(Ordering::Relaxed) {
                if clipboard.is_none() {
                    clipboard = Clipboard::new().ok();
                }

                if let Some(cb) = clipboard.as_mut() {
                    match cb.get_text() {
                        Ok(text) => {
                            let mut guard = last_text.lock().unwrap();
                            if guard.as_deref() != Some(text.as_str()) {
                                *guard = Some(text.clone());
                                drop(guard);

                                let formats =
                                    vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
                                let _ = sender.send(RdpInputEvent::Clipboard(
                                    ClipboardMessage::SendInitiateCopy(formats),
                                ));
                            }
                        }
                        Err(err) => {
                            trace!("Failed to read clipboard text: {err}");
                        }
                    }
                }

                thread::sleep(Duration::from_millis(500));
            }
        }));
    }

    fn advertise_current_clipboard(&self) {
        if let Some(text) = self.read_clipboard_text() {
            let mut state = self.last_text.lock().unwrap();
            if state.as_deref() == Some(text.as_str()) {
                return;
            }
            *state = Some(text);
            drop(state);

            let formats = vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
            let _ = self.sender.send(RdpInputEvent::Clipboard(
                ClipboardMessage::SendInitiateCopy(formats),
            ));
        }
    }

    fn read_clipboard_text(&self) -> Option<String> {
        Clipboard::new().ok()?.get_text().ok()
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
        if request.format == ClipboardFormatId::CF_UNICODETEXT {
            let text =
                { self.last_text.lock().unwrap().clone() }.or_else(|| self.read_clipboard_text());

            let response = match text {
                Some(text) => {
                    let mut guard = self.last_text.lock().unwrap();
                    *guard = Some(text.clone());
                    drop(guard);
                    FormatDataResponse::new_unicode_string(&text).into_owned()
                }
                None => FormatDataResponse::new_error().into_owned(),
            };

            let _ = self
                .sender
                .send(RdpInputEvent::Clipboard(ClipboardMessage::SendFormatData(
                    response,
                )));
        } else {
            let response = FormatDataResponse::new_error().into_owned();
            let _ = self
                .sender
                .send(RdpInputEvent::Clipboard(ClipboardMessage::SendFormatData(
                    response,
                )));
        }
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        match response.to_unicode_string() {
            Ok(text) => {
                {
                    let mut state = self.last_text.lock().unwrap();
                    *state = Some(text.clone());
                }
                self.set_clipboard_text(&text);
            }
            Err(err) => warn!("Failed to decode clipboard data: {err}"),
        }
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {
        warn!("File clipboard transfer is not supported");
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {
        warn!("File clipboard transfer is not supported");
    }

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

async fn active_session<T: RdpEventSender>(
    framed: UpgradedFramed,
    connection_result: ConnectionResult,
    event_loop_proxy: &T,
    input_event_receiver: &mut mpsc::UnboundedReceiver<RdpInputEvent>,
) -> SessionResult<RdpControlFlow> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let mut image = DecodedImage::new(
        PixelFormat::BgrX32,
        connection_result.desktop_size.width,
        connection_result.desktop_size.height,
    );

    let mut active_stage = ActiveStage::new(connection_result);

    let disconnect_reason = 'outer: loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                let (action, payload) = frame.map_err(|e| session::custom_err!("read frame", e))?;
                trace!(?action, frame_length = payload.len(), "Frame received");

                active_stage.process(&mut image, action, &payload)?
            }
            input_event = input_event_receiver.recv() => {
                let input_event = input_event.ok_or_else(|| session::general_err!("GUI is stopped"))?;

                match input_event {
                    RdpInputEvent::Resize { width, height, scale_factor, physical_size } => {
                        info!(width, height, scale_factor, ?physical_size, "Resize event received from UI");
                        let width = u32::from(width);
                        let height = u32::from(height);
                        // TODO: Make adjust_display_size take and return width and height as u16.
                        // From the function's doc comment, the width and height values must be less than or equal to 8192 pixels.
                        // Therefore, we can remove unnecessary casts from u16 to u32 and back.
                        let (width, height) = MonitorLayoutEntry::adjust_display_size(width, height);
                        info!(width, height, "Adjusted display size for request");
                        if let Some(response_frame) =
                            active_stage.encode_resize(width, height, Some(scale_factor), physical_size)
                        {
                            info!("Sending Display Control resize request over DVC");
                            vec![ActiveStageOutput::ResponseFrame(response_frame?)]
                        } else {
                            warn!(
                                "Display Control channel unavailable, performing fast reconnect to apply new size"
                            );
                            let width = u16::try_from(width).expect("always in the range");
                            let height = u16::try_from(height).expect("always in the range");
                            return Ok(RdpControlFlow::ReconnectWithNewSize { width, height });
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
                ActiveStageOutput::ResponseFrame(frame) => writer
                    .write_all(&frame)
                    .await
                    .map_err(|e| session::custom_err!("write response", e))?,
                ActiveStageOutput::GraphicsUpdate(_region) => {
                    let buffer: Vec<u32> = image
                        .data()
                        .chunks_exact(4)
                        .map(|pixel| {
                            let r = pixel[0];
                            let g = pixel[1];
                            let b = pixel[2];
                            u32::from_be_bytes([0, r, g, b])
                        })
                        .collect();

                    event_loop_proxy
                        .send_event(RdpOutputEvent::Image {
                            buffer,
                            width: NonZeroU16::new(image.width())
                                .ok_or_else(|| session::general_err!("width is zero"))?,
                            height: NonZeroU16::new(image.height())
                                .ok_or_else(|| session::general_err!("height is zero"))?,
                        })
                        .map_err(|_| session::general_err!("failed to send image event"))?;
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
                                PixelFormat::BgrX32,
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
    };

    Ok(RdpControlFlow::TerminatedGracefully(disconnect_reason))
}
