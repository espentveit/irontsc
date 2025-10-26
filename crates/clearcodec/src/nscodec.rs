use anyhow::{ensure, Result};

/// Rust implementation of the NSCodec decoder as used by ClearCodec subcodec tiles.
pub struct NsCodec {
    width: u16,
    height: u16,
    color_loss_level: u8,
    chroma_subsampling_level: u8,
    y_plane: Vec<u8>,
    co_plane: Vec<u8>,
    cg_plane: Vec<u8>,
    alpha_plane: Vec<u8>,
    bitmap: Vec<u8>,
}

impl NsCodec {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            color_loss_level: 1,
            chroma_subsampling_level: 0,
            y_plane: Vec::new(),
            co_plane: Vec::new(),
            cg_plane: Vec::new(),
            alpha_plane: Vec::new(),
            bitmap: Vec::new(),
        }
    }

    pub fn decode_tile(
        &mut self,
        tile_data: &[u8],
        tile_width: u16,
        tile_height: u16,
        dst_width: u32,
        dst_data: &mut [u8],
        dst_x: u32,
        dst_y: u32,
    ) -> Result<()> {
        self.process_message(tile_data, tile_width, tile_height)?;
        self.blit(dst_width, dst_data, dst_x, dst_y)
    }

    fn process_message(&mut self, payload: &[u8], tile_width: u16, tile_height: u16) -> Result<()> {
        const HEADER_LEN: usize = 20;
        ensure!(
            payload.len() >= HEADER_LEN,
            "NSCodec payload too short: {} bytes",
            payload.len()
        );

        let mut offset = 0usize;
        let mut plane_byte_count = [0usize; 4];
        for count in &mut plane_byte_count {
            let bytes: [u8; 4] = payload[offset..offset + 4]
                .try_into()
                .expect("slice with incorrect length");
            *count = u32::from_le_bytes(bytes) as usize;
            offset += 4;
        }

        let color_loss_level = payload[offset];
        offset += 1;
        ensure!(
            (1..=7).contains(&color_loss_level),
            "NSCodec ColorLossLevel={} out of range",
            color_loss_level
        );

        let chroma_subsampling_level = payload[offset];
        offset += 1;
        // Reserved 2 bytes
        offset += 2;

        let total_plane_bytes: usize = plane_byte_count.iter().sum();
        ensure!(
            payload.len() >= offset + total_plane_bytes,
            "NSCodec payload truncated: expected {} plane bytes, have {}",
            total_plane_bytes,
            payload.len() - offset
        );

        self.width = tile_width;
        self.height = tile_height;
        self.color_loss_level = color_loss_level;
        self.chroma_subsampling_level = chroma_subsampling_level;

        let width = tile_width as usize;
        let height = tile_height as usize;
        ensure!(width > 0 && height > 0, "NSCodec tile has zero dimension");

        let padded_width = round_up_to(width, 8);
        let padded_height = round_up_to(height, 2);
        let subsampled = chroma_subsampling_level != 0;

        let y_original = if subsampled {
            padded_width * height
        } else {
            width * height
        };
        let chroma_original = if subsampled {
            // 4:2:0 subsampling
            (padded_width / 2) * (padded_height / 2)
        } else {
            width * height
        };
        let alpha_original = width * height;

        self.y_plane.resize(y_original, 0);
        self.co_plane.resize(chroma_original, 0);
        self.cg_plane.resize(chroma_original, 0);
        self.alpha_plane.resize(alpha_original, 0);
        self.bitmap.resize(width * height * 4, 0);

        let mut plane_offset = 0usize;
        for plane in 0..4 {
            let plane_size = plane_byte_count[plane];
            ensure!(
                plane_offset + plane_size <= total_plane_bytes,
                "NSCodec plane {} exceeds payload",
                plane
            );
            let plane_data = &payload[offset + plane_offset..offset + plane_offset + plane_size];
            let target_slice = match plane {
                0 => &mut self.y_plane,
                1 => &mut self.co_plane,
                2 => &mut self.cg_plane,
                3 => &mut self.alpha_plane,
                _ => unreachable!(),
            };
            let original_size = match plane {
                0 => y_original,
                1 | 2 => chroma_original,
                3 => alpha_original,
                _ => unreachable!(),
            };
            target_slice.resize(original_size, 0);
            if plane_size == 0 {
                target_slice.fill(0xFF);
            } else if plane_size < original_size {
                Self::rle_decode(plane_data, target_slice.as_mut_slice())?;
            } else {
                ensure!(
                    plane_data.len() >= original_size,
                    "NSCodec plane {} truncated: need {} bytes, have {}",
                    plane,
                    original_size,
                    plane_data.len()
                );
                target_slice[..original_size].copy_from_slice(&plane_data[..original_size]);
            }
            plane_offset += plane_size;
        }
        ensure!(
            plane_offset == total_plane_bytes,
            "NSCodec plane byte count mismatch: consumed {} != {}",
            plane_offset,
            total_plane_bytes
        );

        self.decode_planes(width, height, padded_width, subsampled)
    }

    fn decode_planes(
        &mut self,
        width: usize,
        height: usize,
        padded_width: usize,
        subsampled: bool,
    ) -> Result<()> {
        let shift = self.color_loss_level.saturating_sub(1);
        if subsampled {
            ensure!(padded_width >= 2, "NSCodec padded width too small");
        }

        let y_stride = if subsampled { padded_width } else { width };
        let chroma_stride = if subsampled { padded_width / 2 } else { width };
        let alpha_stride = width;
        ensure!(
            self.y_plane.len() >= y_stride * height,
            "NSCodec Y plane underflow"
        );
        ensure!(
            self.alpha_plane.len() >= alpha_stride * height,
            "NSCodec alpha plane underflow"
        );
        if subsampled {
            ensure!(
                self.co_plane.len() >= chroma_stride * ((round_up_to(height, 2)) / 2),
                "NSCodec Co plane underflow"
            );
            ensure!(
                self.cg_plane.len() >= chroma_stride * ((round_up_to(height, 2)) / 2),
                "NSCodec Cg plane underflow"
            );
        } else {
            ensure!(
                self.co_plane.len() >= chroma_stride * height,
                "NSCodec Co plane underflow"
            );
            ensure!(
                self.cg_plane.len() >= chroma_stride * height,
                "NSCodec Cg plane underflow"
            );
        }

        let shift = shift as u8;
        let tile_stride = width * 4;
        for y in 0..height {
            let mut y_index = y * y_stride;
            let mut co_index = (y / 2) * chroma_stride;
            let mut cg_index = (y / 2) * chroma_stride;
            let mut alpha_index = y * alpha_stride;
            for x in 0..width {
                let y_val = self.y_plane[y_index] as i16;
                let co_val = signed_chroma(self.co_plane[co_index], shift);
                let cg_val = signed_chroma(self.cg_plane[cg_index], shift);

                let r_val = y_val + co_val - cg_val;
                let g_val = y_val + cg_val;
                let b_val = y_val - co_val - cg_val;
                let a_val = self.alpha_plane[alpha_index];

                let dst = y * tile_stride + x * 4;
                self.bitmap[dst] = clamp_to_u8(b_val);
                self.bitmap[dst + 1] = clamp_to_u8(g_val);
                self.bitmap[dst + 2] = clamp_to_u8(r_val);
                self.bitmap[dst + 3] = a_val;

                y_index += 1;
                if subsampled {
                    if x & 1 == 1 {
                        co_index += 1;
                        cg_index += 1;
                    }
                } else {
                    co_index += 1;
                    cg_index += 1;
                }
                alpha_index += 1;
            }
        }

        Ok(())
    }

    fn blit(&self, dst_width: u32, dst_data: &mut [u8], dst_x: u32, dst_y: u32) -> Result<()> {
        let width = self.width as usize;
        let height = self.height as usize;
        let dst_width = dst_width as usize;
        let dst_x = dst_x as usize;
        let dst_y = dst_y as usize;

        ensure!(
            dst_width >= dst_x + width,
            "NSCodec blit exceeds destination width"
        );
        let rows_needed = dst_y + height;
        let pixels_needed = dst_width
            .checked_mul(rows_needed)
            .ok_or_else(|| anyhow::anyhow!("NSCodec blit size overflow"))?;
        let bytes_needed = pixels_needed
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("NSCodec blit byte overflow"))?;
        ensure!(
            dst_data.len() >= bytes_needed,
            "NSCodec blit exceeds destination buffer"
        );

        let frame_stride = dst_width * 4;
        let tile_stride = width * 4;
        for row in 0..height {
            let src_offset = row * tile_stride;
            let dst_row = dst_y + row;
            let dst_offset = dst_row * frame_stride + dst_x * 4;
            dst_data[dst_offset..dst_offset + tile_stride]
                .copy_from_slice(&self.bitmap[src_offset..src_offset + tile_stride]);
        }

        Ok(())
    }

    fn rle_decode(input: &[u8], output: &mut [u8]) -> Result<()> {
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;
        let mut left = output.len();

        while left > 4 {
            ensure!(in_pos < input.len(), "NSCodec RLE input exhausted");
            let value = input[in_pos];
            in_pos += 1;

            if left == 5 {
                ensure!(out_pos < output.len(), "NSCodec RLE output overflow");
                output[out_pos] = value;
                out_pos += 1;
                left -= 1;
                continue;
            }

            ensure!(in_pos < input.len(), "NSCodec RLE input exhausted");
            if value == input[in_pos] {
                in_pos += 1;
                ensure!(in_pos < input.len(), "NSCodec RLE input exhausted");
                let marker = input[in_pos];
                in_pos += 1;
                let len = if marker < 0xFF {
                    marker as usize + 2
                } else {
                    ensure!(input.len() >= in_pos + 4, "NSCodec RLE long run truncated");
                    let len = (input[in_pos] as usize)
                        | ((input[in_pos + 1] as usize) << 8)
                        | ((input[in_pos + 2] as usize) << 16)
                        | ((input[in_pos + 3] as usize) << 24);
                    in_pos += 4;
                    len
                };

                ensure!(len <= left, "NSCodec RLE run exceeds output");
                ensure!(out_pos + len <= output.len(), "NSCodec RLE output overflow");
                output[out_pos..out_pos + len].fill(value);
                out_pos += len;
                left -= len;
            } else {
                ensure!(out_pos < output.len(), "NSCodec RLE output overflow");
                output[out_pos] = value;
                out_pos += 1;
                left -= 1;
            }
        }

        ensure!(left == 4, "NSCodec RLE remainder mismatch: {}", left);
        ensure!(input.len() >= in_pos + 4, "NSCodec RLE missing tail bytes");
        ensure!(out_pos + 4 <= output.len(), "NSCodec RLE tail overflow");
        output[out_pos..out_pos + 4].copy_from_slice(&input[in_pos..in_pos + 4]);
        Ok(())
    }
}

impl Default for NsCodec {
    fn default() -> Self {
        Self::new()
    }
}

fn round_up_to(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) / align * align
}

fn signed_chroma(value: u8, shift: u8) -> i16 {
    let shifted = (((value as u16) << shift) & 0xFF) as u8;
    i16::from(i8::from_ne_bytes([shifted]))
}

fn clamp_to_u8(value: i16) -> u8 {
    value.clamp(0, 255) as u8
}
