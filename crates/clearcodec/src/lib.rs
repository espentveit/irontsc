//! Pure Rust ClearCodec decoder for RDP graphics
//!
//! This crate implements the ClearCodec compression algorithm used by Microsoft RDP
//! for graphics compression (MS-RDPEGFX Section 3.1.8.1.2).
//!
//! # Attribution
//!
//! This implementation is a Rust port of the FreeRDP ClearCodec decoder:
//! - Original source: FreeRDP libfreerdp/codec/clear.c
//! - Copyright 2014 Marc-Andre Moreau <marcandre.moreau@gmail.com>
//! - Copyright 2016 Armin Novak <armin.novak@thincast.com>
//! - Copyright 2016 Thincast Technologies GmbH
//! - Licensed under the Apache License, Version 2.0
//!
//! ## Included subcodecs
//!
//! - Residual, Bands, and RLEX processing
//! - **NSCodec** tile decoding (ported from FreeRDP `libfreerdp/codec/nsc.c`)
//!
//! # Example
//! ```
//! use clearcodec::ClearCodec;
//!
//! # fn example() -> anyhow::Result<()> {
//! let mut decoder = ClearCodec::new();
//! let width = 64;
//! let height = 64;
//! let compressed_data = vec![0u8; 100]; // Example compressed data
//! let mut output = vec![0u8; width * height * 4]; // BGRA output
//! // decoder.decompress(&compressed_data, width as u32, height as u32, &mut output)?;
//! # Ok(())
//! # }
//! ```

use anyhow::{bail, Context, Result};
use std::io::{Cursor, Read};

mod nscodec;
use nscodec::NsCodec;

// ClearCodec flags (MS-RDPEGFX 2.2.5.2)
const CLEARCODEC_FLAG_GLYPH_INDEX: u8 = 0x01;
const CLEARCODEC_FLAG_GLYPH_HIT: u8 = 0x02;
const CLEARCODEC_FLAG_CACHE_RESET: u8 = 0x04;

// VBar storage sizes
const CLEARCODEC_VBAR_SIZE: usize = 32768;
const CLEARCODEC_VBAR_SHORT_SIZE: usize = 16384;

// Glyph cache size (MS-RDPEGFX 3.1.8.1.2)
const GLYPH_CACHE_SIZE: usize = 4000;

// Maximum supported surface dimensions
const MAX_SURFACE_WIDTH: u32 = 4096;
const MAX_SURFACE_HEIGHT: u32 = 4096;

/// LOG2 floor lookup table for fast bit operations
const CLEAR_LOG2_FLOOR: [u8; 256] = [
    0, 0, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
    6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
];

/// Bit masks for extracting variable-length bit fields
const CLEAR_8BIT_MASKS: [u8; 9] = [0x00, 0x01, 0x03, 0x07, 0x0F, 0x1F, 0x3F, 0x7F, 0xFF];

/// Glyph cache entry (stores decompressed glyphs)
#[derive(Debug, Clone)]
struct GlyphEntry {
    pixels: Vec<u8>, // BGRA pixels
}

impl GlyphEntry {
    fn new() -> Self {
        Self { pixels: Vec::new() }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let size = (width * height * 4) as usize;
        self.pixels.resize(size, 0);
        Ok(())
    }
}

/// VBar (vertical bar) cache entry
#[derive(Debug, Clone)]
struct VBarEntry {
    pixels: Vec<u8>, // BGRA pixels
    count: u32,
}

impl VBarEntry {
    fn new() -> Self {
        Self {
            pixels: Vec::new(),
            count: 0,
        }
    }

    fn resize(&mut self, count: u32) -> Result<()> {
        let size = (count * 4) as usize;
        if size > self.pixels.len() {
            self.pixels.resize(size, 0);
        }
        self.count = count;
        Ok(())
    }
}

/// ClearCodec decoder context
pub struct ClearCodec {
    seq_number: u8,
    glyph_cache: Vec<GlyphEntry>,
    vbar_storage: Vec<VBarEntry>,
    vbar_cursor: usize,
    short_vbar_storage: Vec<VBarEntry>,
    short_vbar_cursor: usize,
    temp_buffer: Vec<u8>,
    ns_codec: NsCodec,
}

impl ClearCodec {
    /// Create a new ClearCodec decoder
    pub fn new() -> Self {
        Self {
            seq_number: 0,
            glyph_cache: (0..GLYPH_CACHE_SIZE).map(|_| GlyphEntry::new()).collect(),
            vbar_storage: (0..CLEARCODEC_VBAR_SIZE)
                .map(|_| VBarEntry::new())
                .collect(),
            vbar_cursor: 0,
            short_vbar_storage: (0..CLEARCODEC_VBAR_SHORT_SIZE)
                .map(|_| VBarEntry::new())
                .collect(),
            short_vbar_cursor: 0,
            temp_buffer: Vec::new(),
            ns_codec: NsCodec::new(),
        }
    }

    /// Reset VBar storage (called on CACHE_RESET flag)
    fn reset_vbar_storage(&mut self) {
        self.vbar_cursor = 0;
        self.short_vbar_cursor = 0;
    }

    /// Reset glyph cache
    fn reset_glyph_cache(&mut self) {
        for entry in &mut self.glyph_cache {
            entry.pixels.clear();
        }
    }

    /// Decompress ClearCodec data
    ///
    /// # Arguments
    ///
    /// * `src_data` - Compressed ClearCodec data
    /// * `width` - Surface width in pixels
    /// * `height` - Surface height in pixels
    /// * `dst_data` - Output buffer (BGRA format, width * height * 4 bytes)
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if decompression fails
    pub fn decompress(
        &mut self,
        src_data: &[u8],
        width: u32,
        height: u32,
        dst_data: &mut [u8],
    ) -> Result<()> {
        if width > MAX_SURFACE_WIDTH || height > MAX_SURFACE_HEIGHT {
            bail!(
                "Surface too large: {}x{} (max {}x{})",
                width,
                height,
                MAX_SURFACE_WIDTH,
                MAX_SURFACE_HEIGHT
            );
        }

        let expected_size = (width * height * 4) as usize;
        if dst_data.len() < expected_size {
            bail!(
                "Output buffer too small: {} bytes (need {})",
                dst_data.len(),
                expected_size
            );
        }

        let mut cursor = Cursor::new(src_data);

        // Read header
        let glyph_flags = read_u8(&mut cursor)?;
        let seq_number = read_u8(&mut cursor)?;

        // Validate sequence number
        if self.seq_number == 0 && seq_number != 0 {
            self.seq_number = seq_number;
        }
        if seq_number != self.seq_number {
            bail!(
                "Sequence number mismatch: expected {}, got {}",
                self.seq_number,
                seq_number
            );
        }
        self.seq_number = self.seq_number.wrapping_add(1);

        // Handle cache reset
        if glyph_flags & CLEARCODEC_FLAG_CACHE_RESET != 0 {
            self.reset_vbar_storage();
        }

        // Process glyph data - returns glyph index if miss, None if hit or no glyph
        let glyph_index_opt = if glyph_flags & CLEARCODEC_FLAG_GLYPH_INDEX != 0 {
            self.decompress_glyph_data(&mut cursor, glyph_flags, width, height, dst_data)?
        } else {
            None
        };

        // Check if we only have glyph hit
        if (glyph_flags & (CLEARCODEC_FLAG_GLYPH_HIT | CLEARCODEC_FLAG_GLYPH_INDEX))
            == (CLEARCODEC_FLAG_GLYPH_HIT | CLEARCODEC_FLAG_GLYPH_INDEX)
        {
            if cursor.position() >= src_data.len() as u64 {
                // Glyph hit only, no composition data - already copied to dst_data
                return Ok(());
            }
        }

        // Read composition payload header
        let residual_byte_count = read_u32(&mut cursor)?;
        let bands_byte_count = read_u32(&mut cursor)?;
        let subcodec_byte_count = read_u32(&mut cursor)?;

        // Decompress residual data
        if residual_byte_count > 0 {
            self.decompress_residual_data(
                &mut cursor,
                residual_byte_count,
                width,
                height,
                dst_data,
            )?;
        }

        // Decompress bands data
        if bands_byte_count > 0 {
            self.decompress_bands_data(&mut cursor, bands_byte_count, width, height, dst_data)?;
        }

        // Decompress subcodecs data
        if subcodec_byte_count > 0 {
            self.decompress_subcodecs_data(
                &mut cursor,
                subcodec_byte_count,
                width,
                height,
                dst_data,
            )?;
        }

        // Copy composed data to glyph cache if this was a glyph miss
        if let Some(glyph_index) = glyph_index_opt {
            let expected_size = (width * height * 4) as usize;
            let glyph = &mut self.glyph_cache[glyph_index];
            glyph.pixels.resize(expected_size, 0);
            glyph.pixels.copy_from_slice(&dst_data[..expected_size]);
        }

        Ok(())
    }

    /// Decompress glyph data - returns the glyph index if this is a glyph miss, None otherwise
    fn decompress_glyph_data(
        &mut self,
        cursor: &mut Cursor<&[u8]>,
        glyph_flags: u8,
        width: u32,
        height: u32,
        dst_data: &mut [u8],
    ) -> Result<Option<usize>> {
        if (glyph_flags & CLEARCODEC_FLAG_GLYPH_HIT != 0)
            && (glyph_flags & CLEARCODEC_FLAG_GLYPH_INDEX == 0)
        {
            bail!("Invalid glyph flags: HIT without INDEX");
        }

        if glyph_flags & CLEARCODEC_FLAG_GLYPH_INDEX == 0 {
            return Ok(None);
        }

        if width * height > 1024 * 1024 {
            bail!("Glyph too large: {}x{}", width, height);
        }

        let glyph_index = read_u16(cursor)? as usize;
        if glyph_index >= GLYPH_CACHE_SIZE {
            bail!("Invalid glyph index: {}", glyph_index);
        }

        if glyph_flags & CLEARCODEC_FLAG_GLYPH_HIT != 0 {
            // Glyph hit: copy from cache to output
            let glyph = &self.glyph_cache[glyph_index];
            let expected_size = (width * height * 4) as usize;
            if glyph.pixels.len() < expected_size {
                bail!(
                    "Cached glyph too small: {} bytes (need {})",
                    glyph.pixels.len(),
                    expected_size
                );
            }
            dst_data[..expected_size].copy_from_slice(&glyph.pixels[..expected_size]);
            Ok(None) // No index to store back, already have it
        } else {
            // Glyph miss: return index so we can store composed data later
            Ok(Some(glyph_index))
        }
    }

    /// Decompress residual data (run-length encoded RGB triplets)
    fn decompress_residual_data(
        &mut self,
        cursor: &mut Cursor<&[u8]>,
        residual_byte_count: u32,
        width: u32,
        height: u32,
        dst_data: &mut [u8],
    ) -> Result<()> {
        let pixel_count = width * height;

        // Resize temp buffer if needed
        let temp_size = (pixel_count * 4) as usize;
        if self.temp_buffer.len() < temp_size {
            self.temp_buffer.resize(temp_size, 0);
        }

        let mut pixel_index = 0u32;
        let mut suboffset = 0u32;

        while suboffset < residual_byte_count {
            let b = read_u8(cursor)?;
            let g = read_u8(cursor)?;
            let r = read_u8(cursor)?;
            let mut run_length = read_u8(cursor)? as u32;
            suboffset += 4;

            if run_length >= 0xFF {
                run_length = read_u16(cursor)? as u32;
                suboffset += 2;

                if run_length >= 0xFFFF {
                    run_length = read_u32(cursor)?;
                    suboffset += 4;
                }
            }

            if pixel_index + run_length > pixel_count {
                bail!(
                    "Residual overflow: pixel_index {} + run_length {} > pixel_count {}",
                    pixel_index,
                    run_length,
                    pixel_count
                );
            }

            // Write BGRA pixels to temp buffer
            for _ in 0..run_length {
                let offset = (pixel_index * 4) as usize;
                self.temp_buffer[offset] = b;
                self.temp_buffer[offset + 1] = g;
                self.temp_buffer[offset + 2] = r;
                self.temp_buffer[offset + 3] = 0xFF; // Alpha
                pixel_index += 1;
            }
        }

        if pixel_index != pixel_count {
            bail!(
                "Residual pixel count mismatch: {} != {}",
                pixel_index,
                pixel_count
            );
        }

        // Copy to output
        let copy_size = (pixel_count * 4) as usize;
        dst_data[..copy_size].copy_from_slice(&self.temp_buffer[..copy_size]);

        Ok(())
    }

    /// Decompress bands data (vertical bars)
    fn decompress_bands_data(
        &mut self,
        cursor: &mut Cursor<&[u8]>,
        bands_byte_count: u32,
        width: u32,
        height: u32,
        dst_data: &mut [u8],
    ) -> Result<()> {
        let mut suboffset = 0u32;

        while suboffset < bands_byte_count {
            let x_start = read_u16(cursor)?;
            let x_end = read_u16(cursor)?;
            let y_start = read_u16(cursor)?;
            let y_end = read_u16(cursor)?;
            let cb = read_u8(cursor)?;
            let cg = read_u8(cursor)?;
            let cr = read_u8(cursor)?;
            suboffset += 11;

            if x_end < x_start {
                bail!("Invalid band: x_end {} < x_start {}", x_end, x_start);
            }
            if y_end < y_start {
                bail!("Invalid band: y_end {} < y_start {}", y_end, y_start);
            }

            let vbar_count = (x_end - x_start + 1) as usize;
            let vbar_height = (y_end - y_start + 1) as u32;

            if vbar_height > 52 {
                bail!("VBar height too large: {}", vbar_height);
            }

            for i in 0..vbar_count {
                let vbar_header = read_u16(cursor)?;
                suboffset += 2;

                let vbar_entry = if (vbar_header & 0xC000) == 0x4000 {
                    // SHORT_VBAR_CACHE_HIT
                    let vbar_index = (vbar_header & 0x3FFF) as usize;
                    let vbar_y_on = read_u8(cursor)? as u32;
                    suboffset += 1;
                    
                    let short_entry = &self.short_vbar_storage[vbar_index];
                    let short_pixel_count = short_entry.count;
                    let vbar_y_off = vbar_y_on + short_pixel_count;
                    
                    // Build full vbar in vbar_storage
                    let full_entry = &mut self.vbar_storage[self.vbar_cursor];
                    full_entry.resize(vbar_height)?;

                    // Fill background before short pixels
                    for y in 0..vbar_y_on {
                        let offset = (y * 4) as usize;
                        full_entry.pixels[offset] = cb;
                        full_entry.pixels[offset + 1] = cg;
                        full_entry.pixels[offset + 2] = cr;
                        full_entry.pixels[offset + 3] = 0xFF;
                    }

                    // Copy short pixels from cache
                    for y in 0..short_pixel_count {
                        let src_offset = (y * 4) as usize;
                        let dst_offset = ((vbar_y_on + y) * 4) as usize;
                        if src_offset + 4 <= short_entry.pixels.len() 
                            && dst_offset + 4 <= full_entry.pixels.len() {
                            full_entry.pixels[dst_offset..dst_offset + 4]
                                .copy_from_slice(&short_entry.pixels[src_offset..src_offset + 4]);
                        }
                    }

                    // Fill background after short pixels
                    for y in vbar_y_off..vbar_height {
                        let offset = (y * 4) as usize;
                        full_entry.pixels[offset] = cb;
                        full_entry.pixels[offset + 1] = cg;
                        full_entry.pixels[offset + 2] = cr;
                        full_entry.pixels[offset + 3] = 0xFF;
                    }

                    self.vbar_cursor = (self.vbar_cursor + 1) % CLEARCODEC_VBAR_SIZE;
                    full_entry
                } else if (vbar_header & 0xC000) == 0x0000 {
                    // SHORT_VBAR_CACHE_MISS
                    let vbar_y_on = (vbar_header & 0xFF) as u32;
                    let vbar_y_off = ((vbar_header >> 8) & 0x3F) as u32;

                    if vbar_y_off < vbar_y_on {
                        bail!("Invalid vbar: y_off {} < y_on {}", vbar_y_off, vbar_y_on);
                    }

                    let short_pixel_count = vbar_y_off - vbar_y_on;
                    if short_pixel_count > 52 {
                        bail!("Short vbar pixel count too large: {}", short_pixel_count);
                    }

                    // Read short vbar pixels
                    let entry = &mut self.short_vbar_storage[self.short_vbar_cursor];
                    entry.resize(short_pixel_count)?;

                    for y in 0..short_pixel_count {
                        let b = read_u8(cursor)?;
                        let g = read_u8(cursor)?;
                        let r = read_u8(cursor)?;
                        let offset = (y * 4) as usize;
                        entry.pixels[offset] = b;
                        entry.pixels[offset + 1] = g;
                        entry.pixels[offset + 2] = r;
                        entry.pixels[offset + 3] = 0xFF;
                    }

                    suboffset += short_pixel_count * 3;
                    self.short_vbar_cursor =
                        (self.short_vbar_cursor + 1) % CLEARCODEC_VBAR_SHORT_SIZE;

                    // Build full vbar
                    let full_entry = &mut self.vbar_storage[self.vbar_cursor];
                    full_entry.resize(vbar_height)?;

                    // Fill background before short pixels
                    for y in 0..vbar_y_on {
                        let offset = (y * 4) as usize;
                        full_entry.pixels[offset] = cb;
                        full_entry.pixels[offset + 1] = cg;
                        full_entry.pixels[offset + 2] = cr;
                        full_entry.pixels[offset + 3] = 0xFF;
                    }

                    // Copy short pixels
                    for y in 0..short_pixel_count {
                        let src_offset = (y * 4) as usize;
                        let dst_offset = ((vbar_y_on + y) * 4) as usize;
                        full_entry.pixels[dst_offset..dst_offset + 4]
                            .copy_from_slice(&entry.pixels[src_offset..src_offset + 4]);
                    }

                    // Fill background after short pixels
                    for y in (vbar_y_on + short_pixel_count)..vbar_height {
                        let offset = (y * 4) as usize;
                        full_entry.pixels[offset] = cb;
                        full_entry.pixels[offset + 1] = cg;
                        full_entry.pixels[offset + 2] = cr;
                        full_entry.pixels[offset + 3] = 0xFF;
                    }

                    self.vbar_cursor = (self.vbar_cursor + 1) % CLEARCODEC_VBAR_SIZE;
                    full_entry
                } else if (vbar_header & 0x8000) == 0x8000 {
                    // VBAR_CACHE_HIT
                    let vbar_index = (vbar_header & 0x7FFF) as usize;
                    &self.vbar_storage[vbar_index]
                } else {
                    bail!("Invalid vbar header: 0x{:04X}", vbar_header);
                };

                // Blit vbar to output
                let x = x_start as u32 + i as u32;
                if x < width && vbar_entry.count > 0 {
                    let copy_height = vbar_entry.count.min(height - y_start as u32);
                    for y in 0..copy_height {
                        let src_offset = (y * 4) as usize;
                        let dst_x = x;
                        let dst_y = y_start as u32 + y;
                        if dst_y < height {
                            let dst_offset = ((dst_y * width + dst_x) * 4) as usize;
                            if dst_offset + 4 <= dst_data.len()
                                && src_offset + 4 <= vbar_entry.pixels.len()
                            {
                                dst_data[dst_offset..dst_offset + 4].copy_from_slice(
                                    &vbar_entry.pixels[src_offset..src_offset + 4],
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Decompress subcodecs data (NSCodec, RLEX, or uncompressed)
    fn decompress_subcodecs_data(
        &mut self,
        cursor: &mut Cursor<&[u8]>,
        subcodec_byte_count: u32,
        width: u32,
        height: u32,
        dst_data: &mut [u8],
    ) -> Result<()> {
        let mut suboffset = 0u32;

        while suboffset < subcodec_byte_count {
            let x_start = read_u16(cursor)?;
            let y_start = read_u16(cursor)?;
            let tile_width = read_u16(cursor)?;
            let tile_height = read_u16(cursor)?;
            let bitmap_data_byte_count = read_u32(cursor)?;
            let subcodec_id = read_u8(cursor)?;
            suboffset += 13;

            if x_start as u32 + tile_width as u32 > width {
                bail!(
                    "Subcodec tile overflow X: {} + {} > {}",
                    x_start,
                    tile_width,
                    width
                );
            }
            if y_start as u32 + tile_height as u32 > height {
                bail!(
                    "Subcodec tile overflow Y: {} + {} > {}",
                    y_start,
                    tile_height,
                    height
                );
            }

            match subcodec_id {
                0 => {
                    // Uncompressed BGR24
                    let expected_size = (tile_width as u32 * tile_height as u32 * 3) as usize;
                    if bitmap_data_byte_count as usize != expected_size {
                        bail!(
                            "Uncompressed size mismatch: {} != {}",
                            bitmap_data_byte_count,
                            expected_size
                        );
                    }

                    for y in 0..tile_height {
                        for x in 0..tile_width {
                            let b = read_u8(cursor)?;
                            let g = read_u8(cursor)?;
                            let r = read_u8(cursor)?;

                            let dst_x = x_start as u32 + x as u32;
                            let dst_y = y_start as u32 + y as u32;
                            let dst_offset = ((dst_y * width + dst_x) * 4) as usize;

                            if dst_offset + 4 <= dst_data.len() {
                                dst_data[dst_offset] = b;
                                dst_data[dst_offset + 1] = g;
                                dst_data[dst_offset + 2] = r;
                                dst_data[dst_offset + 3] = 0xFF;
                            }
                        }
                    }

                    suboffset += bitmap_data_byte_count;
                }
                1 => {
                    let start = cursor.position() as usize;
                    let end = start
                        .checked_add(bitmap_data_byte_count as usize)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "NSCodec tile length overflow: {} bytes",
                                bitmap_data_byte_count
                            )
                        })?;

                    let data = cursor.get_ref();
                    if end > data.len() {
                        bail!(
                            "NSCodec tile truncated: expected {} bytes, have {}",
                            bitmap_data_byte_count,
                            data.len().saturating_sub(start)
                        );
                    }

                    self.ns_codec.decode_tile(
                        &data[start..end],
                        tile_width,
                        tile_height,
                        width,
                        dst_data,
                        x_start as u32,
                        y_start as u32,
                    )?;

                    cursor.set_position(end as u64);
                    suboffset += bitmap_data_byte_count;
                }
                2 => {
                    // RLEX (run-length encoded with palette)
                    self.decompress_subcodec_rlex(
                        cursor,
                        bitmap_data_byte_count,
                        tile_width as u32,
                        tile_height as u32,
                        x_start as u32,
                        y_start as u32,
                        width,
                        dst_data,
                    )?;
                    suboffset += bitmap_data_byte_count;
                }
                _ => {
                    bail!("Unknown subcodec ID: {}", subcodec_id);
                }
            }
        }

        Ok(())
    }

    /// Decompress RLEX subcodec (run-length encoding with palette)
    fn decompress_subcodec_rlex(
        &mut self,
        cursor: &mut Cursor<&[u8]>,
        bitmap_data_byte_count: u32,
        tile_width: u32,
        tile_height: u32,
        x_dst: u32,
        y_dst: u32,
        dst_width: u32,
        dst_data: &mut [u8],
    ) -> Result<()> {
        let palette_count = read_u8(cursor)? as usize;

        if palette_count < 1 || palette_count > 127 {
            bail!("Invalid palette count: {}", palette_count);
        }

        // Read palette (BGR triplets)
        let mut palette = Vec::with_capacity(palette_count);
        for _ in 0..palette_count {
            let b = read_u8(cursor)?;
            let g = read_u8(cursor)?;
            let r = read_u8(cursor)?;
            palette.push([b, g, r, 0xFF]);
        }

        let mut bitmap_data_offset = 1u32 + (palette_count as u32 * 3);
        let pixel_count = tile_width * tile_height;
        let num_bits = CLEAR_LOG2_FLOOR[palette_count - 1] as u32 + 1;

        let mut pixel_index = 0u32;
        let mut x = 0u32;
        let mut y = 0u32;

        while bitmap_data_offset < bitmap_data_byte_count {
            let tmp = read_u8(cursor)?;
            let mut run_length = read_u8(cursor)? as u32;
            bitmap_data_offset += 2;

            let suite_depth = (tmp >> num_bits) & CLEAR_8BIT_MASKS[(8 - num_bits) as usize];
            let stop_index = (tmp & CLEAR_8BIT_MASKS[num_bits as usize]) as usize;
            let start_index = stop_index - suite_depth as usize;

            if run_length >= 0xFF {
                run_length = read_u16(cursor)? as u32;
                bitmap_data_offset += 2;

                if run_length >= 0xFFFF {
                    run_length = read_u32(cursor)?;
                    bitmap_data_offset += 4;
                }
            }

            if start_index >= palette_count {
                bail!(
                    "Start index {} >= palette count {}",
                    start_index,
                    palette_count
                );
            }
            if stop_index >= palette_count {
                bail!(
                    "Stop index {} >= palette count {}",
                    stop_index,
                    palette_count
                );
            }

            // Write run of suite start color
            let color = &palette[start_index];
            for _ in 0..run_length {
                let dst_x = x_dst + x;
                let dst_y = y_dst + y;
                if dst_x < dst_width {
                    let dst_offset = ((dst_y * dst_width + dst_x) * 4) as usize;
                    if dst_offset + 4 <= dst_data.len() {
                        dst_data[dst_offset..dst_offset + 4].copy_from_slice(color);
                    }
                }

                x += 1;
                if x >= tile_width {
                    x = 0;
                    y += 1;
                }
            }

            pixel_index += run_length;

            // Write suite (sequence of palette indices)
            for suite_idx in start_index..=stop_index {
                let color = &palette[suite_idx];
                let dst_x = x_dst + x;
                let dst_y = y_dst + y;
                if dst_x < dst_width {
                    let dst_offset = ((dst_y * dst_width + dst_x) * 4) as usize;
                    if dst_offset + 4 <= dst_data.len() {
                        dst_data[dst_offset..dst_offset + 4].copy_from_slice(color);
                    }
                }

                x += 1;
                if x >= tile_width {
                    x = 0;
                    y += 1;
                }
            }

            pixel_index += (suite_depth + 1) as u32;
        }

        if pixel_index != pixel_count {
            bail!(
                "RLEX pixel count mismatch: {} != {}",
                pixel_index,
                pixel_count
            );
        }

        Ok(())
    }
}

impl Default for ClearCodec {
    fn default() -> Self {
        Self::new()
    }
}

// Helper functions for reading binary data

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8> {
    let mut buf = [0u8; 1];
    cursor.read_exact(&mut buf).context("Failed to read u8")?;
    Ok(buf[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> Result<u16> {
    let mut buf = [0u8; 2];
    cursor.read_exact(&mut buf).context("Failed to read u16")?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32> {
    let mut buf = [0u8; 4];
    cursor.read_exact(&mut buf).context("Failed to read u32")?;
    Ok(u32::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clearcodec_new() {
        let codec = ClearCodec::new();
        assert_eq!(codec.seq_number, 0);
        assert_eq!(codec.glyph_cache.len(), GLYPH_CACHE_SIZE);
        assert_eq!(codec.vbar_storage.len(), CLEARCODEC_VBAR_SIZE);
        assert_eq!(codec.short_vbar_storage.len(), CLEARCODEC_VBAR_SHORT_SIZE);
    }

    #[test]
    fn test_clearcodec_surface_too_large() {
        let mut codec = ClearCodec::new();
        let mut output = vec![0u8; 100];
        let result = codec.decompress(&[], 10000, 10000, &mut output);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Surface too large"));
    }

    #[test]
    fn test_clearcodec_output_buffer_too_small() {
        let mut codec = ClearCodec::new();
        let mut output = vec![0u8; 10]; // Too small for even 2x2 surface
        let compressed = vec![0x00, 0x00]; // Header: no flags, seq 0
        let result = codec.decompress(&compressed, 10, 10, &mut output);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Output buffer too small"));
    }

    #[test]
    fn test_clearcodec_sequence_number() {
        let mut codec = ClearCodec::new();
        let mut output = vec![0u8; 16]; // 2x2 BGRA

        // First frame with seq 0
        let compressed1 = vec![
            0x00, 0x00, // flags=0, seq=0
            0x00, 0x00, 0x00, 0x00, // residual_byte_count=0
            0x00, 0x00, 0x00, 0x00, // bands_byte_count=0
            0x00, 0x00, 0x00, 0x00, // subcodec_byte_count=0
        ];
        assert!(codec.decompress(&compressed1, 2, 2, &mut output).is_ok());
        assert_eq!(codec.seq_number, 1);

        // Second frame with seq 1
        let compressed2 = vec![
            0x00, 0x01, // flags=0, seq=1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert!(codec.decompress(&compressed2, 2, 2, &mut output).is_ok());
        assert_eq!(codec.seq_number, 2);
    }

    #[test]
    fn test_clearcodec_sequence_mismatch() {
        let mut codec = ClearCodec::new();
        let mut output = vec![0u8; 16];

        // First frame seq 0
        let compressed1 = vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        codec.decompress(&compressed1, 2, 2, &mut output).unwrap();

        // Wrong sequence number (5 instead of 1)
        let compressed2 = vec![
            0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let result = codec.decompress(&compressed2, 2, 2, &mut output);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Sequence number mismatch"));
    }

    #[test]
    fn test_clearcodec_nscodec_tile() {
        let mut codec = ClearCodec::new();
        let width = 2u32;
        let height = 2u32;
        let mut output = vec![0u8; (width * height * 4) as usize];

        // Build NSCodec payload with simple constant Y plane and transparent chroma/alpha
        let mut nscodec_payload = Vec::new();
        for _ in 0..4 {
            nscodec_payload.extend_from_slice(&4u32.to_le_bytes());
        }
        nscodec_payload.push(1); // ColorLossLevel
        nscodec_payload.push(0); // ChromaSubsamplingLevel
        nscodec_payload.extend_from_slice(&[0, 0]); // Reserved
        nscodec_payload.extend_from_slice(&[100, 100, 100, 100]); // Y
        nscodec_payload.extend_from_slice(&[0, 0, 0, 0]); // Co
        nscodec_payload.extend_from_slice(&[0, 0, 0, 0]); // Cg
        nscodec_payload.extend_from_slice(&[255, 255, 255, 255]); // Alpha

        let bitmap_data_byte_count = nscodec_payload.len() as u32;

        let mut compressed = Vec::new();
        compressed.push(0x00); // glyph flags
        compressed.push(0x00); // seq number
        compressed.extend_from_slice(&0u32.to_le_bytes()); // residual
        compressed.extend_from_slice(&0u32.to_le_bytes()); // bands
        compressed.extend_from_slice(&(13u32 + bitmap_data_byte_count).to_le_bytes()); // subcodec size

        // Tile header covering entire surface
        compressed.extend_from_slice(&0u16.to_le_bytes()); // x_start
        compressed.extend_from_slice(&0u16.to_le_bytes()); // y_start
        compressed.extend_from_slice(&(width as u16).to_le_bytes()); // tile width
        compressed.extend_from_slice(&(height as u16).to_le_bytes()); // tile height
        compressed.extend_from_slice(&bitmap_data_byte_count.to_le_bytes());
        compressed.push(1); // subcodec ID = NSCodec

        compressed.extend_from_slice(&nscodec_payload);

        codec
            .decompress(&compressed, width, height, &mut output)
            .expect("NSCodec tile should decode");

        let expected = [100u8, 100u8, 100u8, 255u8];
        for chunk in output.chunks_exact(4) {
            assert_eq!(chunk, &expected);
        }
    }
}
