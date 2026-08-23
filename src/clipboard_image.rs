//! Pictures on the clipboard, in the shape Windows keeps them.
//!
//! A picture copied in Windows arrives as a device-independent bitmap: a header, sometimes a
//! set of colour masks, and then rows of pixels stored bottom-up, each padded to a multiple of
//! four bytes. That is the same layout a `.bmp` file has after its first fourteen bytes, and it
//! is what has to be produced going the other way.
//!
//! Two headers are in circulation. `CF_DIB` carries the forty-byte one from Windows 3, and
//! `CF_DIBV5` the hundred-and-twenty-four byte one that added colour management. Only the first
//! few fields of either are needed, and both begin with their own length, so one reader handles
//! them and anything in between.
//!
//! Alpha is the awkward part. A thirty-two bit DIB has a fourth byte per pixel that the older
//! header has no way to describe, so Windows writes zero there and means "opaque" -- while a
//! picture that really is transparent writes zero and means "invisible". Taking it at face value
//! turns every screenshot into nothing at all, so a picture whose alpha is zero everywhere is
//! read as opaque, which is what it always turns out to be.

/// A picture, in the arrangement both this machine's clipboard and the wire want it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Four bytes per pixel, red first, top row first.
    pub rgba: Vec<u8>,
}

/// The oldest header, and the smallest one worth reading.
const CORE_HEADER: usize = 40;
/// Uncompressed.
const BI_RGB: u32 = 0;
/// Uncompressed, with the colour layout given by three masks.
const BI_BITFIELDS: u32 = 3;

/// Reads a `CF_DIB` or `CF_DIBV5` payload.
///
/// Only the uncompressed twenty-four and thirty-two bit forms are read. Anything else -- a
/// palette, JPEG or PNG wrapped in a DIB header, sixteen bits with masks -- is left alone rather
/// than guessed at.
pub fn from_dib(bytes: &[u8]) -> Option<Picture> {
    let word = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let short = |at: usize| -> Option<u16> {
        bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    };

    let header_size = word(0)? as usize;
    if header_size < CORE_HEADER {
        return None;
    }

    let width = word(4)? as i32;
    let raw_height = word(8)? as i32;
    let bit_count = short(14)?;
    let compression = word(16)?;
    let colours_used = word(32)? as usize;

    if width <= 0 || raw_height == 0 {
        return None;
    }
    if !matches!(compression, BI_RGB | BI_BITFIELDS) || !matches!(bit_count, 24 | 32) {
        return None;
    }

    // Rows run bottom-up unless the height says otherwise, which is how a DIB says top-down.
    let upside_down = raw_height > 0;
    let height = raw_height.unsigned_abs();
    let width = width as u32;

    // What sits between the header and the pixels: the masks, when an old header needs them
    // written after it rather than inside it, and a palette, which a deep bitmap may still
    // carry for the benefit of a screen that cannot show them all.
    let masks = if compression == BI_BITFIELDS && header_size == CORE_HEADER {
        12
    } else {
        0
    };
    let palette = colours_used.checked_mul(4)?;
    let start = header_size.checked_add(masks)?.checked_add(palette)?;

    let bytes_per_pixel = usize::from(bit_count / 8);
    let stride = (width as usize)
        .checked_mul(bytes_per_pixel)?
        .checked_add(3)?
        & !3;
    let pixels = bytes.get(start..)?;
    if pixels.len() < stride.checked_mul(height as usize)? {
        return None;
    }

    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut any_alpha = false;

    for y in 0..height as usize {
        let source_row = if upside_down {
            height as usize - 1 - y
        } else {
            y
        };
        let row = &pixels[source_row * stride..source_row * stride + stride];
        for x in 0..width as usize {
            let pixel = &row[x * bytes_per_pixel..x * bytes_per_pixel + bytes_per_pixel];
            let out = (y * width as usize + x) * 4;
            // A DIB stores blue first.
            rgba[out] = pixel[2];
            rgba[out + 1] = pixel[1];
            rgba[out + 2] = pixel[0];
            rgba[out + 3] = if bytes_per_pixel == 4 { pixel[3] } else { 255 };
            any_alpha |= rgba[out + 3] != 0;
        }
    }

    // Zero everywhere means the fourth byte was never filled in, not that the picture is
    // invisible.
    if !any_alpha {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }

    Some(Picture {
        width,
        height,
        rgba,
    })
}

/// Writes a `CF_DIB` payload: twenty-four bit, uncompressed, bottom-up.
///
/// Twenty-four rather than thirty-two because every Windows program that takes a picture off the
/// clipboard understands it, and the fourth byte is the one they disagree about. What is being
/// sent is a screenshot or a photograph, which has nothing to say with it.
pub fn to_dib(picture: &Picture) -> Vec<u8> {
    let width = picture.width as usize;
    let height = picture.height as usize;
    let stride = (width * 3 + 3) & !3;

    let mut dib = Vec::with_capacity(CORE_HEADER + stride * height);
    dib.extend_from_slice(&(CORE_HEADER as u32).to_le_bytes()); // biSize
    dib.extend_from_slice(&(picture.width as i32).to_le_bytes()); // biWidth
    dib.extend_from_slice(&(picture.height as i32).to_le_bytes()); // biHeight, bottom-up
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&BI_RGB.to_le_bytes()); // biCompression
    dib.extend_from_slice(&((stride * height) as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..height).rev() {
        let row_start = dib.len();
        for x in 0..width {
            let pixel = &picture.rgba[(y * width + x) * 4..(y * width + x) * 4 + 4];
            dib.push(pixel[2]);
            dib.push(pixel[1]);
            dib.push(pixel[0]);
        }
        dib.resize(row_start + stride, 0);
    }

    dib
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pixels wide and one tall, so the row padding is exercised: six bytes of colour in a
    /// row that has to reach eight.
    fn two_pixels() -> Picture {
        Picture {
            width: 2,
            height: 1,
            rgba: vec![10, 20, 30, 255, 40, 50, 60, 255],
        }
    }

    #[test]
    fn a_picture_survives_the_round_trip() {
        let dib = to_dib(&two_pixels());
        assert_eq!(from_dib(&dib), Some(two_pixels()));
    }

    #[test]
    fn rows_are_padded_to_four_bytes() {
        let dib = to_dib(&two_pixels());
        assert_eq!(dib.len(), CORE_HEADER + 8);
    }

    #[test]
    fn a_top_down_bitmap_is_not_read_upside_down() {
        let mut upright = to_dib(&Picture {
            width: 1,
            height: 2,
            rgba: vec![1, 1, 1, 255, 2, 2, 2, 255],
        });
        // The same pixels, with the height negated and the rows in the other order.
        let (header, pixels) = upright.split_at_mut(CORE_HEADER);
        header[8..12].copy_from_slice(&(-2i32).to_le_bytes());
        pixels.swap(0, 4);
        pixels.swap(1, 5);
        pixels.swap(2, 6);

        let read = from_dib(&upright).expect("a top-down bitmap");
        assert_eq!(read.rgba, vec![1, 1, 1, 255, 2, 2, 2, 255]);
    }

    #[test]
    fn alpha_that_was_never_filled_in_is_opaque() {
        // A thirty-two bit DIB with every fourth byte zero, as Windows writes them.
        let mut dib = Vec::new();
        dib.extend_from_slice(&(CORE_HEADER as u32).to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&BI_RGB.to_le_bytes());
        dib.extend_from_slice(&[0u8; 20]);
        dib.extend_from_slice(&[30, 20, 10, 0]);

        let read = from_dib(&dib).expect("a thirty-two bit bitmap");
        assert_eq!(read.rgba, vec![10, 20, 30, 255]);
    }

    #[test]
    fn a_palette_bitmap_is_left_alone() {
        let mut dib = to_dib(&two_pixels());
        dib[14..16].copy_from_slice(&8u16.to_le_bytes()); // eight bits, with a palette
        assert_eq!(from_dib(&dib), None);
    }
}
