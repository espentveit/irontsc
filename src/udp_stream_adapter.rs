/// UDP Stream Adapter for TLS
///
/// Adapts MS-RDPEUDP reliable connection to AsyncRead+AsyncWrite stream for TLS
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use ironrdp_udp::UdpConnection;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

struct StreamState {
    read_queue: VecDeque<u8>,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    closed: bool,
}

/// Wraps a UDP socket + MS-RDPEUDP connection to provide stream semantics for TLS
#[derive(Clone)]
pub struct UdpStreamAdapter {
    socket: Arc<UdpSocket>,
    connection: Arc<Mutex<UdpConnection>>,
    state: Arc<Mutex<StreamState>>,
}

impl UdpStreamAdapter {
    pub fn new(
        socket: Arc<UdpSocket>,
        connection: Arc<Mutex<UdpConnection>>,
    ) -> Self {
        let state = Arc::new(Mutex::new(StreamState {
            read_queue: VecDeque::new(),
            read_waker: None,
            write_waker: None,
            closed: false,
        }));

        Self {
            socket,
            connection,
            state,
        }
    }

    /// Feed a received UDP packet to the stream adapter
    /// This should be called by the main loop when a packet is received
    pub async fn feed_packet(&self, packet: &[u8]) -> io::Result<()> {
        tracing::info!("feed_packet: {} bytes, first 16 bytes: {:02x?}", packet.len(), &packet[..packet.len().min(16)]);
        // Try to process as source packet (data)
        let mut conn = self.connection.lock().await;
        match conn.process_source_packet(packet) {
            Ok(datas) => {
                tracing::info!("process_source_packet OK: {} payloads", datas.len());
                if !datas.is_empty() {
                    // Add all data to read queue
                    let mut st = self.state.lock().await;
                    for data in &datas {
                        tracing::info!("adding {} bytes to queue", data.len());
                        st.read_queue.extend(data);
                    }
                    tracing::info!("queue total: {} bytes", st.read_queue.len());
                    // Wake up any pending read
                    if let Some(waker) = st.read_waker.take() {
                        tracing::info!("waking pending read");
                        waker.wake();
                    }
                }
                Ok(())
            }
            Err(e) => {
                tracing::info!("process_source_packet ERR: {}", e);
                // Try as ACK packet
                match conn.process_ack_packet(packet) {
                    Ok(()) => {
                        tracing::info!("processed as ACK");
                        Ok(())
                    }
                    Err(e2) => {
                        tracing::warn!("invalid packet: src={}, ack={}", e, e2);
                        Err(io::Error::new(io::ErrorKind::InvalidData, format!("Invalid packet: {}", e)))
                    }
                }
            }
        }
    }

    /// Close the stream adapter
    pub async fn close(&self) {
        let mut st = self.state.lock().await;
        st.closed = true;
        if let Some(waker) = st.read_waker.take() {
            waker.wake();
        }
    }
}

impl AsyncRead for UdpStreamAdapter {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = match self.state.try_lock() {
            Ok(s) => s,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        // Check if stream is closed
        if state.closed && state.read_queue.is_empty() {
            return Poll::Ready(Ok(()));
        }

        // If we have data, copy it to the output buffer
        if !state.read_queue.is_empty() {
            let to_copy = state.read_queue.len().min(buf.remaining());
            for _ in 0..to_copy {
                if let Some(byte) = state.read_queue.pop_front() {
                    buf.put_slice(&[byte]);
                }
            }
            return Poll::Ready(Ok(()));
        }

        // No data available, register waker and return pending
        state.read_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl AsyncWrite for UdpStreamAdapter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let connection = self.connection.clone();
        let socket = self.socket.clone();
        let data = buf.to_vec();
        let len = data.len();
        let waker = cx.waker().clone();
        let data_preview = data[..len.min(16)].to_vec();

        // Spawn task to send data
        tokio::spawn(async move {
            tracing::info!("poll_write: sending {} bytes TLS data, first 16: {:02x?}", len, data_preview);
            let mut conn = connection.lock().await;
            match conn.send_data(data) {
                Ok(packet) => {
                    tracing::info!("poll_write: wrapped into {} byte RDPEUDP packet, first 16: {:02x?}", packet.len(), &packet[..packet.len().min(16)]);
                    if let Err(e) = socket.send(&packet).await {
                        tracing::error!("UDP stream adapter send error: {}", e);
                    } else {
                        tracing::info!("poll_write: packet sent successfully");
                    }
                }
                Err(e) => {
                    tracing::error!("UDP stream adapter send_data error: {}", e);
                }
            }
            waker.wake();
        });

        Poll::Ready(Ok(len))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // UDP is message-oriented, flush is a no-op
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = match self.state.try_lock() {
            Ok(s) => s,
            Err(_) => return Poll::Pending,
        };
        state.closed = true;
        Poll::Ready(Ok(()))
    }
}
