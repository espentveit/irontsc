//! FFmpeg-based H.264 decoder

use crate::{
    parse_gfx_avc444_stream, parse_gfx_avc_stream, Avc444Lc, AvcKind, DecodedFrame, H264Decoder,
    PixelFormat,
};
use anyhow::{bail, ensure, Context, Result};
use ffmpeg::color::Range;
use ffmpeg_next as ffmpeg;
use std::sync::Once;
use tracing::{debug, trace};

static FFMPEG_INIT: Once = Once::new();

fn align_to(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        value
    } else {
        (value + alignment - 1) / alignment * alignment
    }
}

fn is_full_range(pixel: ffmpeg::format::Pixel) -> bool {
    use ffmpeg::format::Pixel::*;

    matches!(pixel, YUVJ420P | YUVJ422P | YUVJ444P | YUVJ440P | YUVJ411P)
}

fn normalize_pixel_format(pixel: ffmpeg::format::Pixel) -> (ffmpeg::format::Pixel, Option<Range>) {
    use ffmpeg::format::Pixel::*;

    match pixel {
        YUVJ420P => (YUV420P, Some(Range::JPEG)),
        YUVJ422P => (YUV422P, Some(Range::JPEG)),
        YUVJ444P => (YUV444P, Some(Range::JPEG)),
        YUVJ440P => (YUV440P, Some(Range::JPEG)),
        YUVJ411P => (YUV411P, Some(Range::JPEG)),
        _ => (pixel, Option::<Range>::None),
    }
}

struct Yuv444Frame {
    width: usize,
    height: usize,
    stride: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    full_range: bool,
}

fn yuv420_frame_to_yuv444(frame: &ffmpeg::util::frame::Video) -> Result<Yuv444Frame> {
    let width = frame.width() as usize;
    let height = frame.height() as usize;

    ensure!(width > 0 && height > 0, "Invalid frame dimensions");

    let stride_y = frame.stride(0) as usize;
    let stride_u = frame.stride(1) as usize;
    let stride_v = frame.stride(2) as usize;

    ensure!(
        stride_y >= width,
        "Y plane stride ({stride_y}) too small for width ({width})"
    );

    let half_width = (width + 1) / 2;
    let half_height = (height + 1) / 2;

    ensure!(
        stride_u >= half_width,
        "U plane stride ({stride_u}) too small for width/2 ({half_width})"
    );
    ensure!(
        stride_v >= half_width,
        "V plane stride ({stride_v}) too small for width/2 ({half_width})"
    );

    let data_y = frame.data(0);
    let data_u = frame.data(1);
    let data_v = frame.data(2);

    ensure!(
        data_y.len() >= stride_y * height,
        "Y plane buffer shorter than expected"
    );
    ensure!(
        data_u.len() >= stride_u * half_height,
        "U plane buffer shorter than expected"
    );
    ensure!(
        data_v.len() >= stride_v * half_height,
        "V plane buffer shorter than expected"
    );

    let mut y = vec![0u8; width * height];
    let mut u = vec![0u8; width * height];
    let mut v = vec![0u8; width * height];

    for row in 0..height {
        let src_offset = row * stride_y;
        let dst_offset = row * width;
        y[dst_offset..dst_offset + width].copy_from_slice(&data_y[src_offset..src_offset + width]);
    }

    for block_y in 0..half_height {
        let src_u_row_offset = block_y * stride_u;
        let src_v_row_offset = block_y * stride_v;
        let src_u_row = &data_u[src_u_row_offset..src_u_row_offset + half_width];
        let src_v_row = &data_v[src_v_row_offset..src_v_row_offset + half_width];

        let top = 2 * block_y;
        let bottom = top + 1;

        for block_x in 0..half_width {
            let sample_u = src_u_row[block_x];
            let sample_v = src_v_row[block_x];
            let left = 2 * block_x;
            let right = left + 1;

            if top < height {
                if left < width {
                    u[top * width + left] = sample_u;
                    v[top * width + left] = sample_v;
                }
                if right < width {
                    u[top * width + right] = sample_u;
                    v[top * width + right] = sample_v;
                }
            }

            if bottom < height {
                if left < width {
                    u[bottom * width + left] = sample_u;
                    v[bottom * width + left] = sample_v;
                }
                if right < width {
                    u[bottom * width + right] = sample_u;
                    v[bottom * width + right] = sample_v;
                }
            }
        }
    }

    Ok(Yuv444Frame {
        width,
        height,
        stride: width,
        y,
        u,
        v,
        full_range: is_full_range(frame.format()),
    })
}

fn yuv444_to_ffmpeg_frame(frame: &Yuv444Frame) -> Result<ffmpeg::util::frame::Video> {
    let mut video = ffmpeg::util::frame::Video::empty();
    let pixel_format = ffmpeg::format::Pixel::YUV444P;
    let width = frame.width as u32;
    let height = frame.height as u32;

    video.set_format(pixel_format);
    video.set_width(width);
    video.set_height(height);
    video.set_color_range(if frame.full_range {
        Range::JPEG
    } else {
        Range::MPEG
    });

    unsafe {
        video.alloc(pixel_format, width, height);
    }

    let dst_stride_y = video.stride(0) as usize;
    let dst_stride_u = video.stride(1) as usize;
    let dst_stride_v = video.stride(2) as usize;

    {
        let data_y = video.data_mut(0);
        for row in 0..frame.height {
            let src_offset = row * frame.stride;
            let dst_offset = row * dst_stride_y;
            data_y[dst_offset..dst_offset + frame.width]
                .copy_from_slice(&frame.y[src_offset..src_offset + frame.width]);
        }
    }

    {
        let data_u = video.data_mut(1);
        for row in 0..frame.height {
            let src_offset = row * frame.stride;
            let dst_offset = row * dst_stride_u;
            data_u[dst_offset..dst_offset + frame.width]
                .copy_from_slice(&frame.u[src_offset..src_offset + frame.width]);
        }
    }

    {
        let data_v = video.data_mut(2);
        for row in 0..frame.height {
            let src_offset = row * frame.stride;
            let dst_offset = row * dst_stride_v;
            data_v[dst_offset..dst_offset + frame.width]
                .copy_from_slice(&frame.v[src_offset..src_offset + frame.width]);
        }
    }

    Ok(video)
}

fn apply_progressive2_chroma_to_yuv444(
    state: &mut Yuv444Frame,
    chroma_frame: &ffmpeg::util::frame::Video,
) -> Result<()> {
    let width = state.width;
    let height = state.height;

    ensure!(
        chroma_frame.width() as usize == width && chroma_frame.height() as usize == height,
        "Progressive2 chroma frame dimensions ({:?}x{:?}) do not match previous frame ({}x{})",
        chroma_frame.width(),
        chroma_frame.height(),
        width,
        height
    );

    let src_stride_y = chroma_frame.stride(0) as usize;
    let src_stride_u = chroma_frame.stride(1) as usize;
    let src_stride_v = chroma_frame.stride(2) as usize;

    ensure!(
        src_stride_y >= width,
        "Chroma Y stride ({src_stride_y}) too small for width ({width})"
    );

    let aligned_width = align_to(width, 16);
    let n_total_width = aligned_width.min(src_stride_y - (src_stride_y % 2));
    ensure!(
        n_total_width >= width,
        "Total width after alignment ({n_total_width}) smaller than frame width ({width})"
    );

    let required_u_stride = (n_total_width / 2).max(1);
    ensure!(
        src_stride_u >= required_u_stride,
        "Chroma U stride ({src_stride_u}) too small for required {required_u_stride}"
    );
    ensure!(
        src_stride_v >= required_u_stride,
        "Chroma V stride ({src_stride_v}) too small for required {required_u_stride}"
    );

    let half_width = (width + 1) / 2;
    let half_height = (height + 1) / 2;
    let quarter_width = (width + 3) / 4;
    let quarter_span = (n_total_width / 4).max(1);

    let src_y = chroma_frame.data(0);
    let src_u = chroma_frame.data(1);
    let src_v = chroma_frame.data(2);

    ensure!(
        src_y.len() >= src_stride_y * height,
        "Chroma Y buffer shorter than expected"
    );
    ensure!(
        src_u.len() >= src_stride_u * half_height,
        "Chroma U buffer shorter than expected"
    );
    ensure!(
        src_v.len() >= src_stride_v * half_height,
        "Chroma V buffer shorter than expected"
    );

    // Update odd column chroma samples (B4/B5 in FreeRDP implementation)
    for y in 0..height {
        let src_offset = y * src_stride_y;
        let row = &src_y[src_offset..src_offset + n_total_width];
        let (left_half, right_half) = row.split_at(n_total_width / 2);

        let dst_offset = y * state.stride;
        let dst_u_row = &mut state.u[dst_offset..dst_offset + width];
        let dst_v_row = &mut state.v[dst_offset..dst_offset + width];

        let max_u = half_width.min(left_half.len());
        for x in 0..max_u {
            let odd = 2 * x + 1;
            if odd < width {
                dst_u_row[odd] = left_half[x];
            }
        }

        let max_v = half_width.min(right_half.len());
        for x in 0..max_v {
            let odd = 2 * x + 1;
            if odd < width {
                dst_v_row[odd] = right_half[x];
            }
        }
    }

    // Update remaining chroma samples on odd rows (B6-B9 in FreeRDP implementation)
    for block_y in 0..half_height {
        let dst_row = 2 * block_y + 1;
        if dst_row >= height {
            break;
        }

        let src_u_offset = block_y * src_stride_u;
        let src_v_offset = block_y * src_stride_v;

        let row_u = &src_u[src_u_offset..src_u_offset + required_u_stride];
        let row_v = &src_v[src_v_offset..src_v_offset + required_u_stride];

        let (u_left, u_right) = row_u.split_at(quarter_span.min(row_u.len()));
        let (v_left, v_right) = row_v.split_at(quarter_span.min(row_v.len()));

        let max_elements = quarter_width
            .min(u_left.len())
            .min(u_right.len())
            .min(v_left.len())
            .min(v_right.len());

        let dst_offset = dst_row * state.stride;
        let dst_u_row = &mut state.u[dst_offset..dst_offset + width];
        let dst_v_row = &mut state.v[dst_offset..dst_offset + width];

        for x in 0..max_elements {
            let col0 = 4 * x;
            if col0 < width {
                dst_u_row[col0] = u_left[x];
                dst_v_row[col0] = u_right[x];
            }

            let col2 = col0 + 2;
            if col2 < width {
                dst_u_row[col2] = v_left[x];
                dst_v_row[col2] = v_right[x];
            }
        }
    }

    Ok(())
}

/// Initialize FFmpeg library (call once)
fn init_ffmpeg() -> Result<()> {
    static mut INIT_SUCCESS: bool = false;
    static mut INIT_ERROR: Option<String> = None;

    FFMPEG_INIT.call_once(|| match ffmpeg::init() {
        Ok(()) => unsafe { INIT_SUCCESS = true },
        Err(e) => unsafe { INIT_ERROR = Some(format!("Failed to initialize FFmpeg: {:?}", e)) },
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
    /// Previous decoded YUV frame for Progressive2 chroma updates
    previous_yuv_frame: Option<ffmpeg::util::frame::Video>,
    previous_yuv444_frame: Option<Yuv444Frame>,
    converter_src_format: Option<ffmpeg::format::Pixel>,
    converter_width: Option<u32>,
    converter_height: Option<u32>,
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
            previous_yuv_frame: None,
            previous_yuv444_frame: None,
            converter_src_format: None,
            converter_width: None,
            converter_height: None,
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

    /// Convert FFmpeg frame to BGRA format
    ///
    /// This matches FreeRDP's approach: convert YUV to RGB first, then the
    /// data is interpreted as BGRA by the graphics system.
    ///
    /// Note: FFmpeg's RGBA pixel format produces R,G,B,A byte order which
    /// gets interpreted as B,G,R,A when displayed by the graphics system
    /// (due to little-endian interpretation of 32-bit pixels).
    fn convert_to_bgra(&mut self, frame: &ffmpeg::util::frame::Video) -> Result<DecodedFrame> {
        let (src_format, color_range) = normalize_pixel_format(frame.format());
        let mut owned_frame: Option<ffmpeg::util::frame::Video> = None;

        if src_format != frame.format() || color_range.is_some() {
            let mut clone = frame.clone();
            clone.set_format(src_format);
            if let Some(range) = color_range {
                clone.set_color_range(range);
            }
            owned_frame = Some(clone);
        }

        let input_frame = owned_frame.as_ref().unwrap_or(frame);

        let width = input_frame.width();
        let height = input_frame.height();

        // Reset converter if source parameters changed
        if self.converter_src_format != Some(src_format)
            || self.converter_width != Some(width)
            || self.converter_height != Some(height)
        {
            self.converter = None;
        }

        // Initialize converter if needed
        // Use RGBA pixel format to match FreeRDP's YUV-to-RGB conversion
        if self.converter.is_none() {
            self.converter = Some(
                ffmpeg::software::scaling::Context::get(
                    src_format,
                    width,
                    height,
                    ffmpeg::format::Pixel::RGBA,
                    width,
                    height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )
                .context("Failed to create scaler")?,
            );
            self.converter_src_format = Some(src_format);
            self.converter_width = Some(width);
            self.converter_height = Some(height);
        }

        // Convert frame
        let converter = self.converter.as_mut().unwrap();
        let mut rgba_frame = ffmpeg::util::frame::Video::empty();
        converter
            .run(input_frame, &mut rgba_frame)
            .context("Failed to convert frame")?;

        // Extract RGBA data
        let stride = rgba_frame.stride(0);
        let data = rgba_frame.data(0);

        // Copy to contiguous buffer
        let mut rgba_data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            let row_start = y * stride;
            let row_end = row_start + (width as usize * 4);
            rgba_data.extend_from_slice(&data[row_start..row_end]);
        }

        Ok(DecodedFrame {
            width,
            height,
            format: PixelFormat::Bgra,
            planes: vec![rgba_data],
            line_sizes: vec![width as usize * 4],
        })
    }

    /// Combine chroma from Progressive2 frame with luma from previous frame
    ///
    /// For Progressive2 frames, we take the Y (luma) plane from the previous frame
    /// and the U/V (chroma) planes from the current frame.
    fn combine_progressive2_chroma(
        &mut self,
        chroma_frame: &ffmpeg::util::frame::Video,
    ) -> Result<ffmpeg::util::frame::Video> {
        if self.previous_yuv_frame.is_none() {
            bail!("Progressive2 frame received before any base frame");
        }

        let prev444 = match self.previous_yuv444_frame.as_mut() {
            Some(frame) => frame,
            None => bail!("Missing cached YUV444 state for Progressive2 frame"),
        };

        apply_progressive2_chroma_to_yuv444(prev444, chroma_frame)?;
        yuv444_to_ffmpeg_frame(prev444)
    }
}

impl H264Decoder for FfmpegDecoder {
    fn decode_gfx_stream(&mut self, kind: AvcKind, gfx_payload: &[u8]) -> Result<DecodedFrame> {
        trace!("Decoding {:?} stream, {} bytes", kind, gfx_payload.len());

        match kind {
            AvcKind::Avc420 => {
                // Parse GFX stream to extract H.264 NAL units
                let h264_streams = parse_gfx_avc_stream(kind, gfx_payload)?;

                if h264_streams.is_empty() {
                    bail!("No H.264 data in AVC420 stream");
                }

                let frame = self.decode_h264_stream(&h264_streams[0])?;

                // Save as previous frame for potential Progressive2 updates
                self.previous_yuv_frame = Some(frame.clone());
                self.previous_yuv444_frame = Some(yuv420_frame_to_yuv444(&frame)?);

                self.convert_to_bgra(&frame)
            }
            AvcKind::Avc444 | AvcKind::Avc444v2 => {
                // Parse with LC mode information
                let stream_info = parse_gfx_avc444_stream(kind, gfx_payload)?;

                if stream_info.h264_streams.is_empty() {
                    bail!("No H.264 data in AVC444 stream");
                }

                // Decode first stream
                let frame1 = self.decode_h264_stream(&stream_info.h264_streams[0])?;

                let final_frame = match stream_info.lc_mode {
                    Avc444Lc::DualStream => {
                        // op=0: YUV420 in stream 1, Chroma420 in stream 2
                        if stream_info.h264_streams.len() == 2 {
                            let _frame2 = self.decode_h264_stream(&stream_info.h264_streams[1])?;
                            // TODO: Implement dual-stream chroma merging
                            trace!("AVC444 dual-stream: using primary stream (chroma merge TODO)");
                        }
                        frame1
                    }
                    Avc444Lc::Progressive1 => {
                        // op=1: YUV420 in stream 1 (luma + chroma update)
                        // This is a full frame update - save for future Progressive2 frames
                        self.previous_yuv_frame = Some(frame1.clone());
                        self.previous_yuv444_frame = Some(yuv420_frame_to_yuv444(&frame1)?);
                        frame1
                    }
                    Avc444Lc::Progressive2 => {
                        // op=2: Chroma420 only in stream 1
                        // Combine with previous luma (like FreeRDP's AVC444_CHROMAv2)
                        if self.previous_yuv_frame.is_some() && self.previous_yuv444_frame.is_some()
                        {
                            let prev_frame = self.previous_yuv_frame.as_ref().unwrap();
                            debug!(
                                "Progressive2: Combining chroma frame ({}x{}, fmt={:?}) with previous luma ({}x{}, fmt={:?})",
                                frame1.width(), frame1.height(), frame1.format(),
                                prev_frame.width(), prev_frame.height(), prev_frame.format()
                            );
                            self.combine_progressive2_chroma(&frame1)?
                        } else {
                            debug!("Progressive2 frame without previous luma - using chroma-only (will appear pink/green)");
                            frame1
                        }
                    }
                };

                self.convert_to_bgra(&final_frame)
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
        assert!(
            result.is_ok(),
            "Failed to create decoder: {:?}",
            result.err()
        );
    }
}
