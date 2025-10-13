//! FFmpeg-based H.264 decoder

use crate::{AvcKind, DecodedFrame, H264Decoder, PixelFormat, parse_gfx_avc_stream};
use anyhow::{bail, Context, Result};
use ffmpeg_next as ffmpeg;
use std::sync::Once;
use tracing::{debug, trace};

static FFMPEG_INIT: Once = Once::new();

/// Initialize FFmpeg library (call once)
fn init_ffmpeg() -> Result<()> {
    static mut INIT_SUCCESS: bool = false;
    static mut INIT_ERROR: Option<String> = None;

    FFMPEG_INIT.call_once(|| {
        match ffmpeg::init() {
            Ok(()) => unsafe { INIT_SUCCESS = true },
            Err(e) => unsafe { INIT_ERROR = Some(format!("Failed to initialize FFmpeg: {:?}", e)) },
        }
    });

    unsafe {
        if INIT_SUCCESS {
            Ok(())
        } else if let Some(ref err) = INIT_ERROR {
            bail!("{}", err)
        } else {
            bail!("FFmpeg initialization status unknown")
        }
    }
}

/// FFmpeg-based H.264 decoder
pub struct FfmpegDecoder {
    decoder: ffmpeg::codec::decoder::Video,
    converter: Option<ffmpeg::software::scaling::Context>,
}

impl FfmpegDecoder {
    /// Create a new FFmpeg H.264 decoder
    pub fn new() -> Result<Self> {
        init_ffmpeg()?;

        // Find H.264 decoder codec
        let codec = ffmpeg::codec::decoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| anyhow::anyhow!("H.264 decoder not found"))?;

        // Create decoder context from codec
        let decoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .decoder()
            .video()
            .context("Failed to create H.264 decoder")?;

        debug!("Initialized FFmpeg H.264 decoder");

        Ok(Self {
            decoder,
            converter: None,
        })
    }

    /// Decode a single H.264 NAL stream
    fn decode_h264_stream(&mut self, h264_data: &[u8]) -> Result<ffmpeg::util::frame::Video> {
        // Create packet
        let packet = ffmpeg::codec::packet::Packet::copy(h264_data);

        // Send packet to decoder
        self.decoder
            .send_packet(&packet)
            .context("Failed to send packet to decoder")?;

        // Receive frame
        let mut frame = ffmpeg::util::frame::Video::empty();
        match self.decoder.receive_frame(&mut frame) {
            Ok(()) => Ok(frame),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                // Need more data - try flushing
                self.decoder.send_eof().ok();
                self.decoder
                    .receive_frame(&mut frame)
                    .context("Failed to receive frame after flush")?;
                Ok(frame)
            }
            Err(e) => bail!("Failed to decode frame: {:?}", e),
        }
    }

    /// Convert FFmpeg frame to BGRA
    fn convert_to_bgra(&mut self, frame: &ffmpeg::util::frame::Video) -> Result<DecodedFrame> {
        let width = frame.width();
        let height = frame.height();

        // Initialize converter if needed
        if self.converter.is_none() {
            self.converter = Some(
                ffmpeg::software::scaling::Context::get(
                    frame.format(),
                    width,
                    height,
                    ffmpeg::format::Pixel::BGRA,
                    width,
                    height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )
                .context("Failed to create scaler")?,
            );
        }

        // Convert frame
        let converter = self.converter.as_mut().unwrap();
        let mut bgra_frame = ffmpeg::util::frame::Video::empty();
        converter
            .run(frame, &mut bgra_frame)
            .context("Failed to convert frame")?;

        // Extract BGRA data
        let stride = bgra_frame.stride(0);
        let data = bgra_frame.data(0);

        // Copy to contiguous buffer
        let mut bgra_data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            let row_start = y * stride;
            let row_end = row_start + (width as usize * 4);
            bgra_data.extend_from_slice(&data[row_start..row_end]);
        }

        Ok(DecodedFrame {
            width,
            height,
            format: PixelFormat::Bgra,
            planes: vec![bgra_data],
            line_sizes: vec![width as usize * 4],
        })
    }
}

impl H264Decoder for FfmpegDecoder {
    fn decode_gfx_stream(&mut self, kind: AvcKind, gfx_payload: &[u8]) -> Result<DecodedFrame> {
        trace!("Decoding {:?} stream, {} bytes", kind, gfx_payload.len());

        // Parse GFX stream to extract H.264 NAL units
        let h264_streams = parse_gfx_avc_stream(kind, gfx_payload)?;

        match kind {
            AvcKind::Avc420 => {
                // Single stream: decode directly
                if h264_streams.is_empty() {
                    bail!("No H.264 data in AVC420 stream");
                }

                let frame = self.decode_h264_stream(&h264_streams[0])?;
                self.convert_to_bgra(&frame)
            }
            AvcKind::Avc444 | AvcKind::Avc444v2 => {
                // Dual stream or progressive
                if h264_streams.is_empty() {
                    bail!("No H.264 data in AVC444 stream");
                }

                // Decode first stream (Y + U/V or full YUV444)
                let frame1 = self.decode_h264_stream(&h264_streams[0])?;

                if h264_streams.len() == 2 {
                    // Dual stream: decode second stream and combine
                    let _frame2 = self.decode_h264_stream(&h264_streams[1])?;

                    // For now, just use first stream (TODO: proper recombination)
                    // Full AVC444 recombination requires merging chroma planes
                    trace!("AVC444 dual-stream: using primary stream (chroma merge TODO)");
                    self.convert_to_bgra(&frame1)
                } else {
                    // Progressive: single stream
                    self.convert_to_bgra(&frame1)
                }
            }
        }
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        // Flush decoder
        if let Err(e) = self.decoder.send_eof() {
            trace!("Error flushing decoder: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoder_creation() {
        let result = FfmpegDecoder::new();
        assert!(result.is_ok(), "Failed to create decoder: {:?}", result.err());
    }
}
