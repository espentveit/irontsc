//! FFmpeg-based H.264 decoder

use crate::{
    parse_gfx_avc444_stream, parse_gfx_avc_stream, Avc444Lc, AvcKind, DecodedFrame, H264Decoder,
    PixelFormat,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use ffmpeg::color::Range;
use ffmpeg_next as ffmpeg;
use std::env;
use std::ffi::CString;
use std::os::raw::c_void;
use std::path::Path;
use std::ptr;
use std::sync::Once;
use tracing::{debug, trace, warn};

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

struct ChromaState {
    width: usize,
    height: usize,
    u: Vec<u8>,
    v: Vec<u8>,
    full_range: bool,
}

struct HardwareContext {
    device_ctx: *mut ffmpeg::ffi::AVBufferRef,
    hw_pix_fmt: ffmpeg::format::Pixel,
    device_type: ffmpeg::ffi::AVHWDeviceType,
}

struct DeviceCandidate {
    device_type: ffmpeg::ffi::AVHWDeviceType,
    device: Option<String>,
}

impl HardwareContext {
    fn try_setup(
        codec: &ffmpeg::Codec,
        context: &mut ffmpeg::codec::Context,
        enable_hw_accel: bool,
    ) -> Result<Option<Box<Self>>> {
        if !enable_hw_accel {
            debug!("Hardware acceleration disabled by configuration");
            return Ok(None);
        }

        let candidates = HardwareContext::default_candidates();

        if candidates.is_empty() {
            return Ok(None);
        }

        let mut errors = Vec::new();

        for candidate in candidates {
            if let Some(hw_pix_fmt) =
                unsafe { HardwareContext::find_hw_pix_fmt(codec, candidate.device_type) }
            {
                match unsafe { HardwareContext::create(&candidate, hw_pix_fmt, context) } {
                    Ok(ctx) => {
                        debug!(
                            "Using FFmpeg hardware decoding backend {} (pix_fmt={:?})",
                            HardwareContext::candidate_label(&candidate),
                            ctx.hw_pix_fmt
                        );
                        return Ok(Some(ctx));
                    }
                    Err(e) => {
                        let label = HardwareContext::candidate_label(&candidate);
                        warn!("Hardware decoder init failed for {}: {:#}", label, e);
                        errors.push((label, e));
                        continue;
                    }
                }
            } else {
                trace!(
                    "Codec does not expose HW configuration for {:?}; skipping candidate",
                    candidate.device_type
                );
            }
        }

        if !errors.is_empty() {
            debug!("All hardware decoders failed; falling back to software decode");
        }

        Ok(None)
    }

    fn parse_forced_list(value: &str) -> Result<Option<Vec<DeviceCandidate>>> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower == "none" || lower == "software" {
            return Ok(Some(Vec::new()));
        }

        let mut devices = Vec::new();
        for entry in trimmed.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }

            let (kind, device) = if let Some((prefix, rest)) = entry.split_once(':') {
                (
                    prefix.trim().to_ascii_lowercase(),
                    Some(rest.trim().to_string()),
                )
            } else {
                (entry.to_ascii_lowercase(), None)
            };

            let device_type = HardwareContext::parse_device_type(&kind)?;
            devices.push(DeviceCandidate {
                device_type,
                device,
            });
        }

        Ok(Some(devices))
    }

    fn parse_device_type(name: &str) -> Result<ffmpeg::ffi::AVHWDeviceType> {
        let ty = match name {
            "vaapi" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            "qsv" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV,
            "vdpau" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
            "cuda" | "nvdec" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            "d3d11va" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            "dxva2" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2,
            "videotoolbox" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            "drm" => ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
            other => bail!("Unknown hardware accel device '{other}'"),
        };
        Ok(ty)
    }

    fn default_candidates() -> Vec<DeviceCandidate> {
        #[cfg(target_os = "linux")]
        {
            let has_display = env::var_os("DISPLAY").is_some();
            let mut result = Vec::new();

            if has_display {
                result.push(DeviceCandidate {
                    device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    device: None,
                });
            } else if let Some(render_node) = HardwareContext::find_render_node() {
                result.push(DeviceCandidate {
                    device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    device: Some(render_node),
                });
            }

            if has_display {
                result.push(DeviceCandidate {
                    device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
                    device: None,
                });
            }

            result.push(DeviceCandidate {
                device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV,
                device: None,
            });
            result.push(DeviceCandidate {
                device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                device: None,
            });

            result
        }
        #[cfg(target_os = "windows")]
        {
            vec![
                DeviceCandidate {
                    device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
                    device: None,
                },
                DeviceCandidate {
                    device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2,
                    device: None,
                },
            ]
        }
        #[cfg(target_os = "macos")]
        {
            vec![DeviceCandidate {
                device_type: ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                device: None,
            }]
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Vec::new()
        }
    }

    #[cfg(target_os = "linux")]
    fn find_render_node() -> Option<String> {
        for index in 128..=135 {
            let candidate = format!("/dev/dri/renderD{index}");
            if Path::new(&candidate).exists() {
                return Some(candidate);
            }
        }
        None
    }

    #[cfg(not(target_os = "linux"))]
    fn find_render_node() -> Option<String> {
        None
    }

    fn candidate_label(candidate: &DeviceCandidate) -> String {
        match &candidate.device {
            Some(device) => format!("{:?}:{device}", candidate.device_type),
            None => format!("{:?}", candidate.device_type),
        }
    }

    unsafe fn find_hw_pix_fmt(
        codec: &ffmpeg::Codec,
        device_type: ffmpeg::ffi::AVHWDeviceType,
    ) -> Option<ffmpeg::format::Pixel> {
        let mut index = 0;
        loop {
            let config = ffmpeg::ffi::avcodec_get_hw_config(codec.as_ptr(), index);
            if config.is_null() {
                break;
            }

            if (*config).methods & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
                && (*config).device_type == device_type
            {
                return Some(ffmpeg::format::Pixel::from((*config).pix_fmt));
            }

            index += 1;
        }

        None
    }

    unsafe fn create(
        candidate: &DeviceCandidate,
        hw_pix_fmt: ffmpeg::format::Pixel,
        context: &mut ffmpeg::codec::Context,
    ) -> Result<Box<Self>> {
        let device_cstring = match candidate.device.as_ref() {
            Some(path) => Some(CString::new(path.as_str()).map_err(|_| {
                anyhow!("Hardware device path contains interior null byte: {}", path)
            })?),
            None => None,
        };
        let device_ptr = device_cstring
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(ptr::null());

        let mut device_ctx = ptr::null_mut();
        let ret = ffmpeg::ffi::av_hwdevice_ctx_create(
            &mut device_ctx,
            candidate.device_type,
            device_ptr,
            ptr::null_mut(),
            0,
        );

        if ret < 0 {
            return Err(anyhow!(
                "av_hwdevice_ctx_create failed: {:?}",
                ffmpeg::Error::from(ret)
            ));
        }

        let device_ref = ffmpeg::ffi::av_buffer_ref(device_ctx);
        if device_ref.is_null() {
            ffmpeg::ffi::av_buffer_unref(&mut device_ctx);
            bail!("Failed to reference hardware device context");
        }

        (*context.as_mut_ptr()).hw_device_ctx = device_ref;
        (*context.as_mut_ptr()).opaque = ptr::null_mut();
        (*context.as_mut_ptr()).get_format = Some(hw_get_format);

        let mut ctx = Box::new(HardwareContext {
            device_ctx,
            hw_pix_fmt,
            device_type: candidate.device_type,
        });

        (*context.as_mut_ptr()).opaque = (&mut *ctx) as *mut HardwareContext as *mut c_void;

        Ok(ctx)
    }

    fn hw_pix_fmt(&self) -> ffmpeg::format::Pixel {
        self.hw_pix_fmt
    }

    fn transfer_to_cpu(
        &self,
        frame: &ffmpeg::util::frame::Video,
    ) -> Result<ffmpeg::util::frame::Video> {
        unsafe {
            let mut sw_frame = ffmpeg::util::frame::Video::empty();
            let ret =
                ffmpeg::ffi::av_hwframe_transfer_data(sw_frame.as_mut_ptr(), frame.as_ptr(), 0);
            if ret < 0 {
                bail!(
                    "av_hwframe_transfer_data failed: {:?}",
                    ffmpeg::Error::from(ret)
                );
            }

            let ret = ffmpeg::ffi::av_frame_copy_props(sw_frame.as_mut_ptr(), frame.as_ptr());
            if ret < 0 {
                bail!("av_frame_copy_props failed: {:?}", ffmpeg::Error::from(ret));
            }

            Ok(sw_frame)
        }
    }
}

impl Drop for HardwareContext {
    fn drop(&mut self) {
        unsafe {
            if !self.device_ctx.is_null() {
                ffmpeg::ffi::av_buffer_unref(&mut self.device_ctx);
                self.device_ctx = ptr::null_mut();
            }
        }
    }
}

unsafe extern "C" fn hw_get_format(
    context: *mut ffmpeg::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    if context.is_null() || pix_fmts.is_null() {
        return ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }

    let hw_ctx_ptr = (*context).opaque as *mut HardwareContext;
    if hw_ctx_ptr.is_null() {
        return *pix_fmts;
    }

    let hw_pix_fmt: ffmpeg::ffi::AVPixelFormat = (*hw_ctx_ptr).hw_pix_fmt().into();
    let mut current = pix_fmts;

    while (*current) != ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
        if *current == hw_pix_fmt {
            trace!("Selected HW pixel format {:?}", hw_pix_fmt);
            return *current;
        }
        current = current.add(1);
    }

    debug!(
        "Hardware pixel format {:?} not offered by decoder, falling back to software",
        hw_pix_fmt
    );
    *pix_fmts
}

fn copy_luma(frame: &ffmpeg::util::frame::Video) -> Result<Vec<u8>> {
    let width = frame.width() as usize;
    let height = frame.height() as usize;

    ensure!(width > 0 && height > 0, "Invalid frame dimensions");

    let stride_y = frame.stride(0) as usize;
    ensure!(
        stride_y >= width,
        "Y plane stride ({stride_y}) too small for width ({width})"
    );

    let data_y = frame.data(0);
    ensure!(
        data_y.len() >= stride_y * height,
        "Y plane buffer shorter than expected"
    );

    let mut luma = vec![0u8; width * height];
    for row in 0..height {
        let src_offset = row * stride_y;
        let dst_offset = row * width;
        luma[dst_offset..dst_offset + width]
            .copy_from_slice(&data_y[src_offset..src_offset + width]);
    }

    Ok(luma)
}

fn yuv420_to_chroma_state(frame: &ffmpeg::util::frame::Video) -> Result<ChromaState> {
    let width = frame.width() as usize;
    let height = frame.height() as usize;

    ensure!(width > 0 && height > 0, "Invalid frame dimensions");

    let stride_u = frame.stride(1) as usize;
    let stride_v = frame.stride(2) as usize;

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

    let data_u = frame.data(1);
    let data_v = frame.data(2);

    ensure!(
        data_u.len() >= stride_u * half_height,
        "U plane buffer shorter than expected"
    );
    ensure!(
        data_v.len() >= stride_v * half_height,
        "V plane buffer shorter than expected"
    );

    let mut u = vec![128u8; width * height];
    let mut v = vec![128u8; width * height];

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

    Ok(ChromaState {
        width,
        height,
        u,
        v,
        full_range: is_full_range(frame.format()),
    })
}

fn apply_progressive2_chroma_to_yuv444(
    state: &mut ChromaState,
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
        let slice_end = (src_offset + n_total_width).min(src_y.len());
        if slice_end <= src_offset {
            continue;
        }
        let row = &src_y[src_offset..slice_end];
        let (left_half, right_half) = row.split_at(n_total_width / 2);

        let dst_offset = y * width;
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

        let dst_offset = dst_row * width;
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
fn chroma_state_to_ffmpeg_frame(
    luma: &[u8],
    chroma: &ChromaState,
) -> Result<ffmpeg::util::frame::Video> {
    let width = chroma.width;
    let height = chroma.height;
    let expected = width * height;

    ensure!(
        luma.len() >= expected && chroma.u.len() >= expected && chroma.v.len() >= expected,
        "Luma/chroma buffers smaller than expected ({}x{})",
        width,
        height
    );

    let mut video = ffmpeg::util::frame::Video::empty();
    let pixel_format = if chroma.full_range {
        ffmpeg::format::Pixel::YUVJ444P
    } else {
        ffmpeg::format::Pixel::YUV444P
    };

    video.set_format(pixel_format);
    video.set_width(width as u32);
    video.set_height(height as u32);
    video.set_color_range(if chroma.full_range {
        Range::JPEG
    } else {
        Range::MPEG
    });

    unsafe {
        video.alloc(pixel_format, width as u32, height as u32);
    }

    let stride_y = video.stride(0) as usize;
    {
        let data_y = video.data_mut(0);
        for row in 0..height {
            let src_offset = row * width;
            let dst_offset = row * stride_y;
            data_y[dst_offset..dst_offset + width]
                .copy_from_slice(&luma[src_offset..src_offset + width]);
        }
    }

    let stride_u = video.stride(1) as usize;
    {
        let data_u = video.data_mut(1);
        for row in 0..height {
            let src_offset = row * width;
            let dst_offset = row * stride_u;
            data_u[dst_offset..dst_offset + width]
                .copy_from_slice(&chroma.u[src_offset..src_offset + width]);
        }
    }

    let stride_v = video.stride(2) as usize;
    {
        let data_v = video.data_mut(2);
        for row in 0..height {
            let src_offset = row * width;
            let dst_offset = row * stride_v;
            data_v[dst_offset..dst_offset + width]
                .copy_from_slice(&chroma.v[src_offset..src_offset + width]);
        }
    }

    Ok(video)
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
    prev_luma: Option<Vec<u8>>,
    prev_chroma: Option<ChromaState>,
    converter_src_format: Option<ffmpeg::format::Pixel>,
    converter_width: Option<u32>,
    converter_height: Option<u32>,
    hardware: Option<Box<HardwareContext>>,
}

impl FfmpegDecoder {
    /// Create a new FFmpeg H.264 decoder
    pub fn new(enable_hw_accel: bool) -> Result<Self> {
        init_ffmpeg()?;

        // Find H.264 decoder codec
        let codec = ffmpeg::codec::decoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| anyhow::anyhow!("H.264 decoder not found"))?;

        let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);
        let hardware = HardwareContext::try_setup(&codec, &mut context, enable_hw_accel)?;

        if let Some(ref hw) = hardware {
            debug!(
                "Using FFmpeg hardware decoding: {:?} (pixel format {:?})",
                hw.device_type,
                hw.hw_pix_fmt()
            );
        } else if enable_hw_accel {
            debug!("Hardware acceleration requested but not available; using software decoding");
        }

        // Create decoder context from codec
        let decoder = context
            .decoder()
            .video()
            .context("Failed to create H.264 decoder")?;

        debug!("Initialized FFmpeg H.264 decoder");

        Ok(Self {
            decoder,
            converter: None,
            prev_luma: None,
            prev_chroma: None,
            converter_src_format: None,
            converter_width: None,
            converter_height: None,
            hardware,
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
        .and_then(|frame| self.normalize_decoded_frame(frame))
    }

    /// Convert FFmpeg frame to BGRA format
    ///
    /// Converts YUV frame to BGRA pixel format for display.
    /// Uses FFmpeg's BGRA pixel format which produces B,G,R,A byte order.
    ///
    /// If `region` is specified as (left, top, width, height), only converts that sub-rectangle.
    /// This significantly reduces CPU usage when only a portion of the frame is needed.
    fn convert_to_bgra(
        &mut self,
        frame: &ffmpeg::util::frame::Video,
        region: Option<(u16, u16, u16, u16)>,
    ) -> Result<DecodedFrame> {
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

        let full_width = input_frame.width();
        let full_height = input_frame.height();

        // Determine conversion region
        let (region_left, region_top, width, height) = if let Some((left, top, w, h)) = region {
            (left as u32, top as u32, w as u32, h as u32)
        } else {
            (0, 0, full_width, full_height)
        };

        // Reset converter if source parameters changed
        if self.converter_src_format != Some(src_format)
            || self.converter_width != Some(width)
            || self.converter_height != Some(height)
        {
            self.converter = None;
        }

        // Initialize converter if needed
        // Use BGRA pixel format for proper color channel ordering
        if self.converter.is_none() {
            self.converter = Some(
                ffmpeg::software::scaling::Context::get(
                    src_format,
                    width,
                    height,
                    ffmpeg::format::Pixel::BGRA,
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

        // Convert frame (or sub-rectangle of it)
        let converter = self.converter.as_mut().unwrap();
        let mut bgra_frame = ffmpeg::util::frame::Video::empty();

        if let Some((left, top, w, h)) = region {
            // For sub-rectangle conversion, we create a new YUV frame that contains only the region
            // This avoids converting pixels we don't need
            let mut region_frame = ffmpeg::util::frame::Video::new(src_format, w as u32, h as u32);

            // Copy Y plane (luma) - full resolution
            {
                let src_y_plane = input_frame.data(0);
                let src_y_stride = input_frame.stride(0);
                let dst_y_stride = region_frame.stride(0);
                let dst_y_plane = region_frame.data_mut(0);

                for y in 0..(h as usize) {
                    let src_offset = ((top as usize + y) * src_y_stride) + left as usize;
                    let dst_offset = y * dst_y_stride;
                    dst_y_plane[dst_offset..dst_offset + w as usize]
                        .copy_from_slice(&src_y_plane[src_offset..src_offset + w as usize]);
                }
            }

            // For YUV420P and YUV444P, we need to handle chroma planes
            // YUV420P: U/V are half resolution (subsampled 2x2)
            // YUV444P: U/V are full resolution
            let (chroma_w, chroma_h, chroma_left, chroma_top) = match src_format {
                ffmpeg::format::Pixel::YUV420P => {
                    // Chroma is half resolution
                    ((w + 1) / 2, (h + 1) / 2, left / 2, top / 2)
                }
                ffmpeg::format::Pixel::YUV444P => {
                    // Chroma is full resolution
                    (w, h, left, top)
                }
                _ => {
                    // For other formats, fall back to full frame conversion
                    converter
                        .run(input_frame, &mut bgra_frame)
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

                    return Ok(DecodedFrame {
                        width,
                        height,
                        format: PixelFormat::Bgra,
                        planes: vec![bgra_data],
                        line_sizes: vec![width as usize * 4],
                    });
                }
            };

            // Copy U plane (Cb - blue chroma)
            {
                let src_u_plane = input_frame.data(1);
                let src_u_stride = input_frame.stride(1);
                let dst_u_stride = region_frame.stride(1);
                let dst_u_plane = region_frame.data_mut(1);

                for y in 0..(chroma_h as usize) {
                    let src_offset =
                        ((chroma_top as usize + y) * src_u_stride) + chroma_left as usize;
                    let dst_offset = y * dst_u_stride;
                    dst_u_plane[dst_offset..dst_offset + chroma_w as usize]
                        .copy_from_slice(&src_u_plane[src_offset..src_offset + chroma_w as usize]);
                }
            }

            // Copy V plane (Cr - red chroma)
            {
                let src_v_plane = input_frame.data(2);
                let src_v_stride = input_frame.stride(2);
                let dst_v_stride = region_frame.stride(2);
                let dst_v_plane = region_frame.data_mut(2);

                for y in 0..(chroma_h as usize) {
                    let src_offset =
                        ((chroma_top as usize + y) * src_v_stride) + chroma_left as usize;
                    let dst_offset = y * dst_v_stride;
                    dst_v_plane[dst_offset..dst_offset + chroma_w as usize]
                        .copy_from_slice(&src_v_plane[src_offset..src_offset + chroma_w as usize]);
                }
            }

            // Now convert only this smaller YUV region to BGRA
            converter
                .run(&region_frame, &mut bgra_frame)
                .context("Failed to convert region frame")?;
        } else {
            converter
                .run(input_frame, &mut bgra_frame)
                .context("Failed to convert frame")?;
        }

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

    fn normalize_decoded_frame(
        &mut self,
        mut frame: ffmpeg::util::frame::Video,
    ) -> Result<ffmpeg::util::frame::Video> {
        if let Some(ref hw) = self.hardware {
            if frame.format() == hw.hw_pix_fmt() {
                trace!(
                    "Transferring hardware frame ({:?}) to system memory",
                    frame.format()
                );
                frame = hw.transfer_to_cpu(&frame)?;
            }
        }

        Ok(frame)
    }
}

impl H264Decoder for FfmpegDecoder {
    fn decode_gfx_stream(
        &mut self,
        kind: AvcKind,
        gfx_payload: &[u8],
        region: Option<(u16, u16, u16, u16)>,
    ) -> Result<DecodedFrame> {
        trace!(
            "Decoding {:?} stream, {} bytes, region={:?}",
            kind,
            gfx_payload.len(),
            region
        );

        match kind {
            AvcKind::Avc420 => {
                // Parse GFX stream to extract H.264 NAL units
                let h264_streams = parse_gfx_avc_stream(kind, gfx_payload)?;

                if h264_streams.is_empty() {
                    bail!("No H.264 data in AVC420 stream");
                }

                let frame = self.decode_h264_stream(&h264_streams[0])?;
                let luma = copy_luma(&frame)?;
                let chroma_state = yuv420_to_chroma_state(&frame)?;
                let decoded = self.convert_to_bgra(&frame, region)?;
                self.prev_luma = Some(luma);
                self.prev_chroma = Some(chroma_state);
                Ok(decoded)
            }
            AvcKind::Avc444 | AvcKind::Avc444v2 => {
                // Parse with LC mode information
                let stream_info = parse_gfx_avc444_stream(kind, gfx_payload)?;

                if stream_info.h264_streams.is_empty() {
                    bail!("No H.264 data in AVC444 stream");
                }

                // Decode first stream
                let frame1 = self.decode_h264_stream(&stream_info.h264_streams[0])?;
                match stream_info.lc_mode {
                    Avc444Lc::DualStream => {
                        // op=0: YUV420 in stream 1, Chroma420 in stream 2
                        if stream_info.h264_streams.len() == 2 {
                            let _frame2 = self.decode_h264_stream(&stream_info.h264_streams[1])?;
                            // TODO: Implement dual-stream chroma merging
                            trace!("AVC444 dual-stream: using primary stream (chroma merge TODO)");
                        }
                        let luma = copy_luma(&frame1)?;
                        let chroma_state = yuv420_to_chroma_state(&frame1)?;
                        let decoded = self.convert_to_bgra(&frame1, region)?;
                        self.prev_luma = Some(luma);
                        self.prev_chroma = Some(chroma_state);
                        Ok(decoded)
                    }
                    Avc444Lc::Progressive1 => {
                        // op=1: YUV420 in stream 1 (luma + chroma update)
                        // This is a full frame update - save for future Progressive2 frames
                        let luma = copy_luma(&frame1)?;
                        let chroma_state = yuv420_to_chroma_state(&frame1)?;
                        let decoded = self.convert_to_bgra(&frame1, region)?;
                        self.prev_luma = Some(luma);
                        self.prev_chroma = Some(chroma_state);
                        Ok(decoded)
                    }
                    Avc444Lc::Progressive2 => {
                        // op=2: Chroma420 only in stream 1
                        if let (Some(ref luma), Some(ref mut chroma_state)) =
                            (&self.prev_luma, &mut self.prev_chroma)
                        {
                            debug!(
                                "Progressive2: applying chroma update for frame {}x{}",
                                frame1.width(),
                                frame1.height()
                            );
                            apply_progressive2_chroma_to_yuv444(chroma_state, &frame1)?;
                            let yuv_frame = chroma_state_to_ffmpeg_frame(luma, chroma_state)?;
                            let decoded = self.convert_to_bgra(&yuv_frame, region)?;
                            Ok(decoded)
                        } else {
                            debug!(
                                "Progressive2 frame without cached base - falling back to direct decode"
                            );
                            let decoded = self.convert_to_bgra(&frame1, region)?;
                            Ok(decoded)
                        }
                    }
                }
            }
        }
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        if let Some(_hw) = self.hardware.take() {
            unsafe {
                let ctx_ptr = self.decoder.as_mut().as_mut_ptr();
                if !ctx_ptr.is_null() {
                    ffmpeg::ffi::av_buffer_unref(&mut (*ctx_ptr).hw_device_ctx);
                    (*ctx_ptr).hw_device_ctx = ptr::null_mut();
                    (*ctx_ptr).opaque = ptr::null_mut();
                    (*ctx_ptr).get_format = None;
                }
            }
        }
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
        let result = FfmpegDecoder::new(false); // Test software decoding
        assert!(
            result.is_ok(),
            "Failed to create decoder: {:?}",
            result.err()
        );
    }
}
