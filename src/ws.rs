// WebSocket compatibility layer - currently disabled
// TODO: Implement when ironrdp-async exposes required types publicly

use bytes::Bytes;
use futures_util::{Sink, SinkExt as _, Stream, StreamExt as _};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite;

// Placeholder that returns an error
pub(crate) fn websocket_compat<S>(_stream: S) -> impl AsyncRead + AsyncWrite + Unpin + Send + 'static
where
    S: Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    // Return a dummy stream that immediately errors
    tokio::io::empty()
}

/*
// Original implementation - requires ironrdp-async internal types
pub(crate) fn websocket_compat<S>(stream: S) -> impl AsyncRead + AsyncWrite + Unpin + Send + 'static
where
    S: Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    use ironrdp_async::{transport, WsStream};
    
    let compat = stream
        .filter_map(|item| {
            let mapped = item
                .map(|msg| match msg {
                    tungstenite::Message::Text(s) => Some(transport::WsReadMsg::Payload(Bytes::from(s))),
                    tungstenite::Message::Binary(data) => Some(transport::WsReadMsg::Payload(Bytes::from(data))),
                    tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => None,
                    tungstenite::Message::Close(_) => Some(transport::WsReadMsg::Close),
                    tungstenite::Message::Frame(_) => unreachable!("raw frames are never returned when reading"),
                })
                .transpose();

            core::future::ready(mapped)
        })
        .with(|item: Bytes| {
            core::future::ready(Ok::<_, tungstenite::Error>(tungstenite::Message::Binary(
                item.to_vec(),
            )))
        });

    WsStream::new(compat)
}
*/