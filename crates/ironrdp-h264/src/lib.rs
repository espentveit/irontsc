//! H.264 decoder for RDPEGFX
//!
//! This crate provides H.264 decoding for RDP graphics, supporting:
//! - AVC420 (YUV 4:2:0)
//! - AVC444 (YUV 4:4:4 via dual stream or progressive)
//! - AVC444v2 (YUV 4:4:4 enhanced)
//!
//! # Example
//! ```no_run
//! use ironrdp_h264::{H264Decoder, FfmpegDecoder, AvcKind};
//!
//! let mut decoder = FfmpegDecoder::new(false)?; // false = software decoding
//!
//! let gfx_payload = vec![...]; // From WIRE_TO_SURFACE
//! let frame = decoder.decode_gfx_stream(AvcKind::Avc420, &gfx_payload)?;
//!
//! // Access decoded frame data
//! println!("Decoded {}x{} frame", frame.width, frame.height);
//! ```

use anyhow::{bail, Result};
use bytes::Buf;

#[cfg(feature = "ffmpeg-sw")]
pub mod ffmpeg;

#[cfg(feature = "ffmpeg-sw")]
pub use ffmpeg::FfmpegDecoder;

/// AVC encoding kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvcKind {
    /// YUV 4:2:0
    Avc420,
    /// YUV 4:4:4 (dual stream or progressive)
    Avc444,
    /// YUV 4:4:4 v2
    Avc444v2,
}

/// Pixel format of decoded frame
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Yuv420p,
    Yuv444p,
    Bgra,
    Bgrx,
}

/// Decoded H.264 frame
#[derive(Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Planar data: Y, U, V planes (or packed BGRA)
    pub planes: Vec<Vec<u8>>,
    /// Line sizes for each plane
    pub line_sizes: Vec<usize>,
}

/// H.264 decoder trait
pub trait H264Decoder {
    /// Decode RDPEGFX H.264 stream
    ///
    /// The optional `region` parameter (left, top, width, height) specifies which sub-rectangle
    /// of the decoded frame to convert to BGRA. This significantly reduces CPU usage when only
    /// a portion of the frame needs to be updated.
    fn decode_gfx_stream(
        &mut self,
        kind: AvcKind,
        gfx_payload: &[u8],
        region: Option<(u16, u16, u16, u16)>,
    ) -> Result<DecodedFrame>;
}

/// H.264 quantization/quality data
#[derive(Debug, Clone, Copy)]
pub struct H264QuantQuality {
    pub qp: u8,      // QP value (0-51)
    pub r: u8,       // R flag
    pub p: u8,       // P flag
    pub quality: u8, // Quality value (0-100)
}

impl H264QuantQuality {
    pub fn from_bytes(qp_val: u8, quality_val: u8) -> Self {
        Self {
            qp: qp_val & 0x3F,
            r: (qp_val >> 6) & 1,
            p: (qp_val >> 7) & 1,
            quality: quality_val,
        }
    }
}

/// H.264 metablock (region info)
#[derive(Debug, Clone)]
pub struct H264Metablock {
    pub num_regions: u32,
    pub region_rects: Vec<(u16, u16, u16, u16)>, // (left, top, right, bottom)
    pub quant_quality: Vec<H264QuantQuality>,
}

impl H264Metablock {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        let data_start = data.len();
        tracing::debug!(
            "🎬 H264Metablock parsing: starting with {} bytes",
            data_start
        );

        if data.len() < 4 {
            bail!(
                "Not enough data for H264Metablock (need 4, got {})",
                data.len()
            );
        }

        let num_regions = data.get_u32_le();
        tracing::debug!(
            "🎬 H264Metablock: num_regions={}, {} bytes remaining",
            num_regions,
            data.len()
        );

        let mut region_rects = Vec::with_capacity(num_regions as usize);
        let mut quant_quality = Vec::with_capacity(num_regions as usize);

        for i in 0..num_regions {
            if data.len() < 8 {
                bail!(
                    "Not enough data for region rect {} (need 8, got {})",
                    i,
                    data.len()
                );
            }

            let left = data.get_u16_le();
            let top = data.get_u16_le();
            let right = data.get_u16_le();
            let bottom = data.get_u16_le();
            region_rects.push((left, top, right, bottom));
        }

        for i in 0..num_regions {
            if data.len() < 2 {
                bail!(
                    "Not enough data for quant/quality {} (need 2, got {})",
                    i,
                    data.len()
                );
            }

            let qp_val = data.get_u8();
            let quality_val = data.get_u8();
            quant_quality.push(H264QuantQuality::from_bytes(qp_val, quality_val));
        }

        tracing::debug!(
            "🎬 H264Metablock parsed successfully, {} bytes remaining",
            data.len()
        );
        Ok(Self {
            num_regions,
            region_rects,
            quant_quality,
        })
    }
}

/// AVC420 bitstream structure
#[derive(Debug)]
pub struct Avc420Bitstream {
    pub meta: H264Metablock,
    pub h264_data: Vec<u8>,
}

impl Avc420Bitstream {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        let data_start = data.len();
        tracing::debug!("🎬 AVC420 parsing: starting with {} bytes", data_start);

        let meta = H264Metablock::parse(data)?;
        tracing::debug!(
            "🎬 AVC420 parsed metablock, {} bytes remaining for H.264 data",
            data.len()
        );

        // Per MS-RDPEGFX spec: avc420EncodedBitstream is a variable-length array of bytes
        // representing H.264 Annex B format data. There is NO length field - the data
        // extends to the end of the parent structure (determined by caller).
        // The H.264 data should start with NAL unit start codes (00 00 00 01 or 00 00 01)

        let h264_data = data.to_vec();
        data.advance(data.len());

        tracing::debug!(
            "🎬 AVC420 parsed successfully: metablock + {} bytes of H.264 data",
            h264_data.len()
        );
        Ok(Self { meta, h264_data })
    }
}

/// AVC444 LC (layer count) field
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Avc444Lc {
    /// Two separate streams (YUV + UV)
    DualStream = 0,
    /// Single progressive stream (mode 1)
    Progressive1 = 1,
    /// Single progressive stream (mode 2)
    Progressive2 = 2,
}

impl TryFrom<u8> for Avc444Lc {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Avc444Lc::DualStream),
            1 => Ok(Avc444Lc::Progressive1),
            2 => Ok(Avc444Lc::Progressive2),
            _ => bail!("Invalid AVC444 LC value: {}", value),
        }
    }
}

/// AVC444 bitstream structure
#[derive(Debug)]
pub struct Avc444Bitstream {
    pub lc: Avc444Lc,
    pub bitstream1: Avc420Bitstream,
    pub bitstream2: Option<Avc420Bitstream>,
}

impl Avc444Bitstream {
    pub fn parse(data: &mut &[u8]) -> Result<Self> {
        let original_len = data.len();

        if data.len() < 4 {
            bail!(
                "Not enough data for AVC444 header (need 4, got {})",
                data.len()
            );
        }

        // Read first 32 bits
        let header = data.get_u32_le();

        // Extract LC (bits 30-31) and length (bits 0-29)
        let lc = Avc444Lc::try_from(((header >> 30) & 0x03) as u8)?;
        let bitstream1_len_field = (header & 0x3FFFFFFF) as usize;

        // FreeRDP behavior: For non-dual-stream modes (LC != 0), the length field is ignored
        // and all remaining data is used. This handles Progressive1/Progressive2 modes where
        // the length field may be 0 but data is present.
        let bitstream1_len = if lc == Avc444Lc::DualStream {
            // Dual-stream mode: use the length field
            bitstream1_len_field
        } else {
            // Progressive modes: use all remaining data
            data.len()
        };

        tracing::debug!(
            "🎬 AVC444 header parsed: LC={:?} len_field={} actual_len={} bytes, data_remaining={} bytes (header=0x{:08X})",
            lc,
            bitstream1_len_field,
            bitstream1_len,
            data.len(),
            header
        );

        // Validate we have data
        if bitstream1_len == 0 {
            bail!(
                "AVC444 bitstream1 has no data (LC={:?}, len_field={}, remaining={})",
                lc,
                bitstream1_len_field,
                data.len()
            );
        }

        // Validate length for dual-stream mode
        if lc == Avc444Lc::DualStream && data.len() < bitstream1_len {
            bail!(
                "Not enough data for AVC444 bitstream1: need {} bytes, got {} (total was {})",
                bitstream1_len,
                data.len(),
                original_len
            );
        }

        // Parse first bitstream
        let mut stream1_data = &data[..bitstream1_len];
        let bitstream1 = Avc420Bitstream::parse(&mut stream1_data)?;
        data.advance(bitstream1_len);

        // Parse second bitstream if dual-stream mode
        let bitstream2 = if lc == Avc444Lc::DualStream {
            if data.is_empty() {
                bail!("Missing second bitstream in dual-stream mode");
            }
            Some(Avc420Bitstream::parse(data)?)
        } else {
            None
        };

        Ok(Self {
            lc,
            bitstream1,
            bitstream2,
        })
    }
}

/// AVC444 stream information with LC mode
#[derive(Debug)]
pub struct Avc444StreamInfo {
    pub h264_streams: Vec<Vec<u8>>,
    pub lc_mode: Avc444Lc,
}

/// Parse RDPEGFX AVC stream to extract raw H.264 NAL units
pub fn parse_gfx_avc_stream(kind: AvcKind, gfx_payload: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut data = gfx_payload;

    match kind {
        AvcKind::Avc420 => {
            let bitstream = Avc420Bitstream::parse(&mut data)?;
            Ok(vec![bitstream.h264_data])
        }
        AvcKind::Avc444 | AvcKind::Avc444v2 => {
            let bitstream = Avc444Bitstream::parse(&mut data)?;

            let mut streams = vec![bitstream.bitstream1.h264_data];

            if let Some(bs2) = bitstream.bitstream2 {
                streams.push(bs2.h264_data);
            }

            Ok(streams)
        }
    }
}

/// Parse RDPEGFX AVC444 stream with LC mode information
pub fn parse_gfx_avc444_stream(kind: AvcKind, gfx_payload: &[u8]) -> Result<Avc444StreamInfo> {
    let mut data = gfx_payload;

    match kind {
        AvcKind::Avc420 => {
            let bitstream = Avc420Bitstream::parse(&mut data)?;
            Ok(Avc444StreamInfo {
                h264_streams: vec![bitstream.h264_data],
                lc_mode: Avc444Lc::Progressive1, // Treat AVC420 as Progressive1 (luma)
            })
        }
        AvcKind::Avc444 | AvcKind::Avc444v2 => {
            let bitstream = Avc444Bitstream::parse(&mut data)?;
            let lc_mode = bitstream.lc;

            let mut streams = vec![bitstream.bitstream1.h264_data];

            if let Some(bs2) = bitstream.bitstream2 {
                streams.push(bs2.h264_data);
            }

            Ok(Avc444StreamInfo {
                h264_streams: streams,
                lc_mode,
            })
        }
    }
}
