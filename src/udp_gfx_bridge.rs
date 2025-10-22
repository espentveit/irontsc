//! UDP-GFX Bridge
//!
//! This module provides the bridge between UDP transport and GFX (H.264 video) processing.
//! It receives UDP packets containing H.264 frames and makes them available to the GFX processor.

use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

// Import UDP packet types for proper parsing
use ironrdp_udp::{SourcePacket, UdpProtocolVersion};

/// H.264 video frame received via UDP
#[derive(Debug, Clone)]
pub struct UdpVideoFrame {
    pub data: Vec<u8>,
    pub sequence_number: u32,
    pub timestamp: u64,
}

/// Creates a UDP-to-GFX bridge that receives UDP packets and extracts video frames
pub fn create_udp_gfx_bridge(socket: Arc<UdpSocket>) -> mpsc::UnboundedReceiver<UdpVideoFrame> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        udp_video_receiver_task(socket, tx).await;
    });

    rx
}

/// Background task that receives UDP packets and extracts H.264 frames
async fn udp_video_receiver_task(
    socket: Arc<UdpSocket>,
    frame_tx: mpsc::UnboundedSender<UdpVideoFrame>,
) {
    let mut buffer = vec![0u8; 65536]; // Max UDP packet size
    let mut expected_sequence = 0u32;

    info!("🎥 UDP video receiver task started");

    loop {
        match socket.recv(&mut buffer).await {
            Ok(n) => {
                if n == 0 {
                    warn!("UDP socket closed");
                    break;
                }

                trace!("📦 Received UDP packet: {} bytes", n);

                // Parse RDPUDP packet using proper packet decoder
                match SourcePacket::decode(&buffer[..n], UdpProtocolVersion::V1) {
                    Ok(packet) => {
                        let seq = packet.sequence_number();
                        let payload = &packet.data;

                        trace!(
                            "UDP SOURCE packet decoded - seq: {}, payload: {} bytes",
                            seq,
                            payload.len()
                        );

                        // Detect sequence gaps for packet loss tracking
                        if seq != expected_sequence {
                            let gap = seq.wrapping_sub(expected_sequence);
                            if gap < 1000 {
                                // Reasonable gap check
                                warn!(
                                    "⚠️  Sequence gap detected: expected {}, got {} (lost {} packets)",
                                    expected_sequence, seq, gap
                                );
                            }
                        }
                        expected_sequence = seq.wrapping_add(1);

                        // Check if this looks like H.264 data (NAL unit)
                        // H.264 NAL units typically start with 0x00 0x00 0x00 0x01 or 0x00 0x00 0x01
                        let is_h264 = payload.len() >= 4
                            && ((payload[0] == 0x00
                                && payload[1] == 0x00
                                && payload[2] == 0x00
                                && payload[3] == 0x01)
                                || (payload[0] == 0x00
                                    && payload[1] == 0x00
                                    && payload[2] == 0x01));

                        if is_h264 {
                            debug!(
                                "📹 Received H.264 frame via UDP: {} bytes (seq: {})",
                                payload.len(),
                                seq
                            );
                        } else {
                            trace!(
                                "Received non-H.264 payload via UDP: {} bytes (seq: {})",
                                payload.len(),
                                seq
                            );
                        }

                        let frame = UdpVideoFrame {
                            data: payload.to_vec(),
                            sequence_number: seq,
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64,
                        };

                        if let Err(e) = frame_tx.send(frame) {
                            error!("Failed to send frame to GFX processor: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        // Not a source packet, might be ACK or FEC packet
                        trace!(
                            "Failed to decode as SOURCE packet: {} (might be ACK/FEC)",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                error!("UDP receive error: {}", e);
                break;
            }
        }
    }

    info!("UDP video receiver task terminated");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_udp_video_frame_creation() {
        let frame = UdpVideoFrame {
            data: vec![0x00, 0x00, 0x00, 0x01, 0x67], // H.264 SPS NAL
            sequence_number: 42,
            timestamp: 1000,
        };

        assert_eq!(frame.sequence_number, 42);
        assert_eq!(frame.timestamp, 1000);
        assert_eq!(frame.data.len(), 5);
    }
}
