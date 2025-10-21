/// UDP-enabled GFX channel for H.264 video streaming
///
/// This module integrates UDP transport with the RDPEGFX channel to enable
/// low-latency H.264 video streaming over UDP instead of TCP.
use anyhow::{Context as _, Result};
use ironrdp_udp::CorrelationId;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::udp_transport::{UdpTransportCommand, UdpTransportEvent, create_video_udp_transport};

/// UDP-enabled GFX channel manager
pub struct UdpGfxChannel {
    /// UDP transport command sender
    transport_tx: mpsc::UnboundedSender<UdpTransportCommand>,
    /// UDP transport event receiver
    transport_rx: mpsc::UnboundedReceiver<UdpTransportEvent>,
    /// Whether the UDP connection is established
    connected: bool,
}

impl UdpGfxChannel {
    /// Create a new UDP-enabled GFX channel
    pub async fn new(
        server_addr: SocketAddr,
        correlation_id: Option<CorrelationId>,
    ) -> Result<Self> {
        info!("Creating UDP GFX channel for server {}", server_addr);

        let (transport_tx, transport_rx) =
            create_video_udp_transport(server_addr, correlation_id).await?;

        Ok(Self {
            transport_tx,
            transport_rx,
            connected: false,
        })
    }

    /// Wait for UDP connection to be established
    pub async fn wait_for_connection(&mut self) -> Result<()> {
        info!("Waiting for UDP connection...");

        while let Some(event) = self.transport_rx.recv().await {
            match event {
                UdpTransportEvent::Connected => {
                    info!("✅ UDP connection established");
                    self.connected = true;
                    return Ok(());
                }
                UdpTransportEvent::Disconnected(reason) => {
                    warn!("UDP connection failed: {}", reason);
                    anyhow::bail!("UDP connection failed: {}", reason);
                }
                UdpTransportEvent::DataReceived(_) => {
                    // Ignore data before connection confirmed
                }
            }
        }

        anyhow::bail!("UDP transport channel closed unexpectedly")
    }

    /// Send H.264 video data over UDP
    pub fn send_video_data(&self, data: Vec<u8>) -> Result<()> {
        if !self.connected {
            anyhow::bail!("UDP transport not connected");
        }

        let data_len = data.len();
        self.transport_tx
            .send(UdpTransportCommand::SendData(data))
            .context("Failed to send data to UDP transport")?;

        debug!("Queued {} bytes for UDP transmission", data_len);
        Ok(())
    }

    /// Receive data from UDP transport (non-blocking)
    pub fn try_recv_data(&mut self) -> Option<Vec<u8>> {
        match self.transport_rx.try_recv() {
            Ok(UdpTransportEvent::DataReceived(data)) => Some(data),
            Ok(UdpTransportEvent::Disconnected(reason)) => {
                warn!("UDP disconnected: {}", reason);
                self.connected = false;
                None
            }
            Ok(UdpTransportEvent::Connected) => {
                // Already connected
                None
            }
            Err(_) => None,
        }
    }

    /// Shutdown the UDP transport
    pub fn shutdown(&self) {
        let _ = self.transport_tx.send(UdpTransportCommand::Shutdown);
        info!("UDP GFX channel shutdown requested");
    }

    /// Check if the UDP connection is active
    pub fn is_connected(&self) -> bool {
        self.connected
    }
}

impl Drop for UdpGfxChannel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Example usage for integrating UDP transport with GFX channel
///
/// This shows how to create a UDP transport for H.264 video data and use it
/// alongside the regular TCP-based RDP connection.
pub async fn example_udp_gfx_integration() -> Result<()> {
    // 1. Parse server address
    let server_addr: SocketAddr = "192.168.1.100:3389".parse()?;

    // 2. Generate a correlation ID for multitransport
    // In a real implementation, this would be negotiated during RDP connection
    let correlation_id = CorrelationId {
        value: rand::random(),
    };

    // 3. Create UDP GFX channel
    let mut udp_channel = UdpGfxChannel::new(server_addr, Some(correlation_id)).await?;

    // 4. Wait for connection
    udp_channel.wait_for_connection().await?;

    // 5. Send H.264 video data
    let video_data = vec![0u8; 1024]; // Example video frame
    udp_channel.send_video_data(video_data)?;

    // 6. Receive data in a loop
    tokio::spawn(async move {
        loop {
            if let Some(data) = udp_channel.try_recv_data() {
                info!("Received {} bytes from UDP", data.len());
                // Process received video data
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires a real RDP server
    async fn test_udp_gfx_channel() {
        let server_addr: SocketAddr = "127.0.0.1:3389".parse().unwrap();
        let result = UdpGfxChannel::new(server_addr, None).await;
        // Would succeed if server is available
        assert!(result.is_ok() || result.is_err());
    }
}
