use core::num::NonZeroU16;
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
use ironrdp::displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::graphics::pointer::DecodedPointer;
use ironrdp::pdu::geometry::Rectangle;
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
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{Config, RDCleanPathConfig};

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

async fn active_session<T: RdpEventSender>(
    framed: UpgradedFramed,
    connection_result: ConnectionResult,
    event_loop_proxy: &T,
    input_event_receiver: &mut mpsc::UnboundedReceiver<RdpInputEvent>,
) -> SessionResult<RdpControlFlow> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let mut image = DecodedImage::new(
        PixelFormat::RgbA32,
        connection_result.desktop_size.width,
        connection_result.desktop_size.height,
    );

    let mut active_stage = ActiveStage::new(connection_result);

    let mut last_frame_dimensions = (image.width(), image.height());
    let mut frame_ready = false;

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
                ActiveStageOutput::ResponseFrame(frame) => writer
                    .write_all(&frame)
                    .await
                    .map_err(|e| session::custom_err!("write response", e))?,
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
    };

    Ok(RdpControlFlow::TerminatedGracefully(disconnect_reason))
}
