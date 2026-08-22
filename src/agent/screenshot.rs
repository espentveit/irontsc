//! Turning the BGRA mirror into a PNG an agent can look at.
//!
//! Two things matter here beyond correctness. The alpha byte coming out of the decoder is not
//! always meaningful -- the window forces the desktop opaque for the same reason -- so it is
//! dropped rather than encoded. And a 1920x1080 screenshot is a lot of tokens for a model to
//! read, so the caller can cap the width; the downscale is a box average, which keeps text
//! legible far better than dropping pixels does.

use std::num::NonZeroU32;

/// A PNG and the size it was encoded at.
#[derive(Debug, Clone)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The desktop's own size, which is the coordinate space clicks must use.
    pub source_width: u32,
    pub source_height: u32,
}

impl Screenshot {
    /// True when the image was scaled down, so callers can warn about coordinate scaling.
    pub fn is_scaled(&self) -> bool {
        self.width != self.source_width || self.height != self.source_height
    }
}

/// A rectangle of the desktop, in desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Copies a rectangle out of a BGRA framebuffer.
///
/// The rectangle is clamped to the desktop rather than rejected when it overhangs: an agent
/// asking for a margin around something it found should get what exists, not an error.
pub fn crop(
    bgra: &[u8],
    width: u16,
    height: u16,
    rect: Crop,
) -> Result<(Vec<u8>, u16, u16), String> {
    if rect.x >= width || rect.y >= height {
        return Err(format!(
            "({}, {}) is outside the {width}x{height} desktop",
            rect.x, rect.y
        ));
    }
    if rect.width == 0 || rect.height == 0 {
        return Err("a region needs a non-zero width and height".to_owned());
    }

    let out_width = rect.width.min(width - rect.x);
    let out_height = rect.height.min(height - rect.y);

    let mut out = Vec::with_capacity(usize::from(out_width) * usize::from(out_height) * 4);
    for row in 0..usize::from(out_height) {
        let start = ((usize::from(rect.y) + row) * usize::from(width) + usize::from(rect.x)) * 4;
        let end = start + usize::from(out_width) * 4;
        let slice = bgra
            .get(start..end)
            .ok_or_else(|| "the framebuffer is shorter than the desktop".to_owned())?;
        out.extend_from_slice(slice);
    }

    Ok((out, out_width, out_height))
}

/// Encodes a BGRA framebuffer as a PNG, optionally capping the width.
pub fn encode(
    bgra: &[u8],
    width: u16,
    height: u16,
    max_width: Option<NonZeroU32>,
) -> Result<Screenshot, String> {
    let source_width = u32::from(width);
    let source_height = u32::from(height);

    if source_width == 0 || source_height == 0 {
        return Err("the desktop has no size yet".to_owned());
    }
    let expected = source_width as usize * source_height as usize * 4;
    if bgra.len() < expected {
        return Err(format!(
            "framebuffer is {} bytes, expected {expected}",
            bgra.len()
        ));
    }

    let (rgb, out_width, out_height) = match max_width {
        Some(cap) if cap.get() < source_width => {
            let target_width = cap.get().max(1);
            // Round the height rather than truncating, so a 16:9 desktop stays 16:9.
            let target_height =
                ((u64::from(target_width) * u64::from(source_height) + u64::from(source_width) / 2)
                    / u64::from(source_width))
                .max(1) as u32;
            (
                downscale_to_rgb(
                    bgra,
                    source_width,
                    source_height,
                    target_width,
                    target_height,
                ),
                target_width,
                target_height,
            )
        }
        _ => (
            bgra_to_rgb(bgra, source_width, source_height),
            source_width,
            source_height,
        ),
    };

    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, out_width, out_height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // A screenshot is on the critical path of every agent step, and the extra ratio `Balanced`
    // buys is not worth the milliseconds.
    encoder.set_compression(png::Compression::Fast);

    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("failed to write the PNG header: {error}"))?;
    writer
        .write_image_data(&rgb)
        .map_err(|error| format!("failed to write the PNG body: {error}"))?;
    writer
        .finish()
        .map_err(|error| format!("failed to finish the PNG: {error}"))?;

    Ok(Screenshot {
        png,
        width: out_width,
        height: out_height,
        source_width,
        source_height,
    })
}

/// Drops the alpha byte and swaps the channels into PNG order.
fn bgra_to_rgb(bgra: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixels = width as usize * height as usize;
    let mut rgb = Vec::with_capacity(pixels * 3);
    for pixel in bgra.chunks_exact(4).take(pixels) {
        rgb.push(pixel[2]);
        rgb.push(pixel[1]);
        rgb.push(pixel[0]);
    }
    rgb
}

/// Box-average downscale straight into RGB.
///
/// Each output pixel averages the source rectangle that maps onto it, which is what keeps
/// small text readable; nearest-neighbour at these ratios turns it into noise.
fn downscale_to_rgb(
    bgra: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(target_width as usize * target_height as usize * 3);

    for target_y in 0..target_height {
        let start_y = (u64::from(target_y) * u64::from(source_height) / u64::from(target_height))
            as u32;
        let end_y = ((u64::from(target_y) + 1) * u64::from(source_height))
            .div_ceil(u64::from(target_height)) as u32;
        let end_y = end_y.min(source_height).max(start_y + 1);

        for target_x in 0..target_width {
            let start_x = (u64::from(target_x) * u64::from(source_width) / u64::from(target_width))
                as u32;
            let end_x = ((u64::from(target_x) + 1) * u64::from(source_width))
                .div_ceil(u64::from(target_width)) as u32;
            let end_x = end_x.min(source_width).max(start_x + 1);

            let (mut blue, mut green, mut red, mut count) = (0u32, 0u32, 0u32, 0u32);
            for y in start_y..end_y {
                let row = y as usize * source_width as usize;
                for x in start_x..end_x {
                    let offset = (row + x as usize) * 4;
                    blue += u32::from(bgra[offset]);
                    green += u32::from(bgra[offset + 1]);
                    red += u32::from(bgra[offset + 2]);
                    count += 1;
                }
            }

            let count = count.max(1);
            rgb.push((red / count) as u8);
            rgb.push((green / count) as u8);
            rgb.push((blue / count) as u8);
        }
    }

    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u16, height: u16, bgra_pixel: [u8; 4]) -> Vec<u8> {
        bgra_pixel
            .iter()
            .copied()
            .cycle()
            .take(usize::from(width) * usize::from(height) * 4)
            .collect()
    }

    #[test]
    fn encodes_at_full_size_by_default() {
        let buffer = solid(8, 4, [10, 20, 30, 255]);
        let shot = encode(&buffer, 8, 4, None).expect("encodes");
        assert_eq!((shot.width, shot.height), (8, 4));
        assert!(!shot.is_scaled());
        assert_eq!(&shot.png[1..4], b"PNG");
    }

    #[test]
    fn caps_the_width_and_keeps_the_aspect_ratio() {
        let buffer = solid(1920, 1080, [0, 0, 0, 255]);
        let cap = NonZeroU32::new(960).expect("non-zero");
        let shot = encode(&buffer, 1920, 1080, Some(cap)).expect("encodes");
        assert_eq!((shot.width, shot.height), (960, 540));
        assert!(shot.is_scaled());
        assert_eq!((shot.source_width, shot.source_height), (1920, 1080));
    }

    #[test]
    fn does_not_upscale() {
        let buffer = solid(64, 64, [0, 0, 0, 255]);
        let cap = NonZeroU32::new(4096).expect("non-zero");
        let shot = encode(&buffer, 64, 64, Some(cap)).expect("encodes");
        assert_eq!((shot.width, shot.height), (64, 64));
    }

    #[test]
    fn averages_rather_than_samples_when_downscaling() {
        // A 2x1 desktop, one black pixel and one white, scaled to a single pixel: the box
        // average is mid grey, whereas nearest-neighbour would pick one of the two.
        let bgra = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let rgb = downscale_to_rgb(&bgra, 2, 1, 1, 1);
        assert_eq!(rgb, vec![127, 127, 127]);
    }

    #[test]
    fn crops_a_rectangle_out_of_the_desktop() {
        // A 4x2 desktop where each pixel's blue channel is its index, so the crop is checkable.
        let mut bgra = Vec::new();
        for index in 0..8u8 {
            bgra.extend_from_slice(&[index, 0, 0, 255]);
        }
        let (out, width, height) = crop(
            &bgra,
            4,
            2,
            Crop {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .expect("crops");

        assert_eq!((width, height), (2, 2));
        // Row 0 pixels 1..3, then row 1 pixels 5..7.
        assert_eq!(
            out.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(),
            vec![1, 2, 5, 6]
        );
    }

    #[test]
    fn clamps_a_region_that_overhangs_the_desktop() {
        let buffer = solid(8, 4, [0, 0, 0, 255]);
        let (_, width, height) = crop(
            &buffer,
            8,
            4,
            Crop {
                x: 6,
                y: 3,
                width: 100,
                height: 100,
            },
        )
        .expect("clamps rather than failing");
        assert_eq!((width, height), (2, 1));
    }

    #[test]
    fn rejects_a_region_that_starts_off_the_desktop() {
        let buffer = solid(8, 4, [0, 0, 0, 255]);
        assert!(
            crop(&buffer, 8, 4, Crop { x: 8, y: 0, width: 1, height: 1 }).is_err()
        );
    }

    #[test]
    fn rejects_a_short_framebuffer() {
        let buffer = vec![0; 16];
        assert!(encode(&buffer, 64, 64, None).is_err());
    }
}
