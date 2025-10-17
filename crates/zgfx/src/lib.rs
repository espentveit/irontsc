//! Pure Rust zGFX decompressor for RDP graphics
//!
//! This crate implements the zGFX compression algorithm used by Microsoft RDP
//! for graphics compression (MS-RDPEGFX). It is based on LZ77 with custom
//! Huffman-like prefix codes.
//!
//! # Example
//! ```no_run
//! use zgfx::decompress;
//!
//! let compressed = vec![...]; // zGFX compressed data
//! let decompressed = decompress(&compressed)?;
//! ```

use anyhow::{anyhow, bail, Result};

/// Packet descriptors
const ZGFX_SEGMENTED_SINGLE: u8 = 0xE0;
const ZGFX_SEGMENTED_MULTIPART: u8 = 0xE1;

/// Segment flags
const PACKET_COMPRESSED: u8 = 0x20;

/// Algorithm limits
const MAX_UNCOMPRESSED_SIZE: usize = 65535;
const HISTORY_BUFFER_SIZE: usize = 2_500_000;
const OUTPUT_BUFFER_SIZE: usize = 65536;

/// Token table entry for prefix code decoding
#[derive(Debug, Clone, Copy)]
struct Token {
    prefix_len: u32,
    prefix_code: u32,
    value_bits: u32,
    token_type: u32, // 0=Literal, 1=Match
    value_base: u32,
}

/// Token table (Huffman-like prefix codes)
/// Format: {prefix_len, prefix_code, value_bits, token_type, value_base}
const TOKEN_TABLE: &[Token] = &[
    // Literals with value bits
    Token {
        prefix_len: 1,
        prefix_code: 0,
        value_bits: 8,
        token_type: 0,
        value_base: 0,
    },
    // Special literals
    Token {
        prefix_len: 5,
        prefix_code: 0b11000,
        value_bits: 0,
        token_type: 0,
        value_base: 0x00,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b11001,
        value_bits: 0,
        token_type: 0,
        value_base: 0x01,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b110100,
        value_bits: 0,
        token_type: 0,
        value_base: 0x02,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b110101,
        value_bits: 0,
        token_type: 0,
        value_base: 0x03,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b110110,
        value_bits: 0,
        token_type: 0,
        value_base: 0xFF,
    },
    // Match/Distance tokens
    Token {
        prefix_len: 5,
        prefix_code: 0b10001,
        value_bits: 5,
        token_type: 1,
        value_base: 0,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b10010,
        value_bits: 7,
        token_type: 1,
        value_base: 32,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b10011,
        value_bits: 9,
        token_type: 1,
        value_base: 160,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b10100,
        value_bits: 10,
        token_type: 1,
        value_base: 672,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b10101,
        value_bits: 12,
        token_type: 1,
        value_base: 1696,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b10110,
        value_bits: 14,
        token_type: 1,
        value_base: 5792,
    },
    Token {
        prefix_len: 5,
        prefix_code: 0b10111,
        value_bits: 15,
        token_type: 1,
        value_base: 22176,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b110111,
        value_bits: 18,
        token_type: 1,
        value_base: 54944,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b111000,
        value_bits: 20,
        token_type: 1,
        value_base: 317088,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b111001,
        value_bits: 20,
        token_type: 1,
        value_base: 1365664,
    },
    Token {
        prefix_len: 6,
        prefix_code: 0b111010,
        value_bits: 20,
        token_type: 1,
        value_base: 2414240,
    },
];

/// zGFX decompressor state
struct Decompressor {
    /// History buffer (2.5MB circular buffer)
    history: Vec<u8>,
    /// Current position in history buffer
    history_index: usize,
    /// Output buffer for current segment
    output: Vec<u8>,
    /// Bit accumulator (MSB-first)
    bit_current: u32,
    /// Number of bits in accumulator
    bits_in_current: u32,
    /// Input buffer
    input: Vec<u8>,
    /// Current read position in input
    input_pos: usize,
    /// Bits remaining to decode
    bits_remaining: u32,
}

impl Decompressor {
    fn new() -> Self {
        Self {
            history: vec![0; HISTORY_BUFFER_SIZE],
            history_index: 0,
            output: Vec::with_capacity(OUTPUT_BUFFER_SIZE),
            bit_current: 0,
            bits_in_current: 0,
            input: Vec::new(),
            input_pos: 0,
            bits_remaining: 0,
        }
    }

    /// Get N bits from the input stream (MSB-first)
    fn get_bits(&mut self, n_bits: u32) -> Result<u32> {
        // Fill bit buffer
        while self.bits_in_current < n_bits {
            self.bit_current <<= 8;
            if self.input_pos < self.input.len() {
                self.bit_current |= self.input[self.input_pos] as u32;
                self.input_pos += 1;
            }
            self.bits_in_current += 8;
        }

        // Extract bits
        if self.bits_remaining < n_bits {
            bail!("Not enough bits remaining");
        }
        self.bits_remaining -= n_bits;
        self.bits_in_current -= n_bits;
        let bits = self.bit_current >> self.bits_in_current;
        self.bit_current &= (1 << self.bits_in_current) - 1;

        Ok(bits)
    }

    /// Write data to history buffer (ring buffer)
    fn history_write(&mut self, data: &[u8]) {
        let mut data = data;
        let mut count = data.len();

        // Handle overflow: keep only most recent bytes
        if count > HISTORY_BUFFER_SIZE {
            let residue = count - HISTORY_BUFFER_SIZE;
            count = HISTORY_BUFFER_SIZE;
            data = &data[residue..];
            self.history_index = (self.history_index + residue) % HISTORY_BUFFER_SIZE;
        }

        // Write with wrap-around
        if self.history_index + count <= HISTORY_BUFFER_SIZE {
            // No wrap
            self.history[self.history_index..self.history_index + count].copy_from_slice(data);
            self.history_index = (self.history_index + count) % HISTORY_BUFFER_SIZE;
        } else {
            // Wrap around
            let front = HISTORY_BUFFER_SIZE - self.history_index;
            self.history[self.history_index..].copy_from_slice(&data[..front]);
            self.history[..count - front].copy_from_slice(&data[front..]);
            self.history_index = count - front;
        }
    }

    /// Read from history buffer (for matches)
    /// Returns the decoded data instead of mutating output
    fn history_read(&self, distance: usize, count: usize) -> Result<Vec<u8>> {
        if distance == 0 || distance > HISTORY_BUFFER_SIZE {
            bail!("Invalid match distance: {}", distance);
        }

        // Calculate read position
        let mut index = (self.history_index + HISTORY_BUFFER_SIZE - distance) % HISTORY_BUFFER_SIZE;

        let mut output = Vec::with_capacity(count);

        // Handle overlapping matches (count > distance) for RLE-like patterns
        let mut bytes_left = count;

        // First copy from history (up to distance bytes)
        let initial_copy = count.min(distance);
        for _ in 0..initial_copy {
            output.push(self.history[index]);
            index = (index + 1) % HISTORY_BUFFER_SIZE;
        }
        bytes_left -= initial_copy;

        // For overlapping matches, copy from already-decoded output
        if bytes_left > 0 {
            let mut valid = initial_copy;
            let mut offset = 0;
            while bytes_left > 0 {
                let bytes = valid.min(bytes_left);
                for i in 0..bytes {
                    output.push(output[offset + i]);
                }
                offset += bytes;
                bytes_left -= bytes;
                valid *= 2; // Double the valid range each iteration
            }
        }

        Ok(output)
    }

    /// Decompress a single segment
    fn decompress_segment(&mut self, segment: &[u8]) -> Result<Vec<u8>> {
        if segment.is_empty() {
            bail!("Empty segment");
        }

        let flags = segment[0];
        let data = &segment[1..];

        self.output.clear();

        // Check if compressed
        if flags & PACKET_COMPRESSED == 0 {
            // Uncompressed: copy directly
            self.output.extend_from_slice(data);
            self.history_write(data);
            return Ok(self.output.clone());
        }

        // Compressed: decompress
        if data.is_empty() {
            bail!("Compressed segment has no data");
        }

        // Calculate bits to decode: ((num_bytes - 1) * 8) - last_byte_value
        let last_byte = data[data.len() - 1] as u32;
        self.bits_remaining = (data.len() as u32 - 1) * 8 - last_byte;

        // Initialize bit reader
        self.input = data[..data.len() - 1].to_vec();
        self.input_pos = 0;
        self.bit_current = 0;
        self.bits_in_current = 0;

        // Main decompression loop
        while self.bits_remaining > 0 {
            // Find matching token
            let mut matched = false;

            for token in TOKEN_TABLE {
                if self.bits_remaining < token.prefix_len {
                    continue;
                }

                // Peek prefix bits
                let prefix = self.peek_bits(token.prefix_len)?;

                if prefix == token.prefix_code {
                    // Consume prefix bits
                    self.get_bits(token.prefix_len)?;

                    if token.token_type == 0 {
                        // Literal
                        let value_bits = if token.value_bits > 0 {
                            self.get_bits(token.value_bits)?
                        } else {
                            0
                        };
                        let byte = (token.value_base + value_bits) as u8;
                        self.output.push(byte);
                        self.history[self.history_index] = byte;
                        self.history_index = (self.history_index + 1) % HISTORY_BUFFER_SIZE;
                    } else {
                        // Match
                        let value_bits = if token.value_bits > 0 {
                            self.get_bits(token.value_bits)?
                        } else {
                            0
                        };
                        let distance = (token.value_base + value_bits) as usize;

                        if distance == 0 {
                            // Special case: unencoded run
                            let count = self.get_bits(15)? as usize;

                            // Flush bit buffer and copy raw bytes
                            self.bits_remaining =
                                self.bits_remaining.saturating_sub(self.bits_in_current);
                            self.bits_in_current = 0;
                            self.bit_current = 0;

                            if self.input_pos + count > self.input.len() {
                                bail!("Not enough data for unencoded run");
                            }

                            // Copy raw data (need to clone to avoid borrow issues)
                            let raw_data =
                                self.input[self.input_pos..self.input_pos + count].to_vec();
                            self.output.extend_from_slice(&raw_data);
                            self.history_write(&raw_data);
                            self.input_pos += count;
                            self.bits_remaining =
                                self.bits_remaining.saturating_sub((8 * count) as u32);
                        } else {
                            // Regular match: decode length
                            let count = self.decode_match_length()?;

                            // Copy from history
                            let match_data = self.history_read(distance, count)?;
                            self.output.extend_from_slice(&match_data);
                            self.history_write(&match_data);
                        }
                    }

                    matched = true;
                    break;
                }
            }

            if !matched {
                bail!("No matching token found at bit position");
            }
        }

        Ok(self.output.clone())
    }

    /// Peek N bits without consuming
    fn peek_bits(&mut self, n_bits: u32) -> Result<u32> {
        // Save state
        let saved_bit_current = self.bit_current;
        let saved_bits_in_current = self.bits_in_current;
        let saved_input_pos = self.input_pos;
        let saved_bits_remaining = self.bits_remaining;

        // Get bits
        let result = self.get_bits(n_bits);

        // Restore state
        self.bit_current = saved_bit_current;
        self.bits_in_current = saved_bits_in_current;
        self.input_pos = saved_input_pos;
        self.bits_remaining = saved_bits_remaining;

        result
    }

    /// Decode match length using variable-length encoding
    fn decode_match_length(&mut self) -> Result<usize> {
        let first_bit = self.get_bits(1)?;

        if first_bit == 0 {
            // Minimum match length
            Ok(3)
        } else {
            // Variable-length encoding
            let mut count = 4;
            let mut extra = 2;

            loop {
                let next_bit = self.get_bits(1)?;
                if next_bit == 0 {
                    break;
                }
                count *= 2;
                extra += 1;
            }

            let value_bits = self.get_bits(extra)?;
            Ok(count + value_bits as usize)
        }
    }
}

/// Decompress zGFX compressed data
///
/// # Arguments
/// * `data` - Compressed data including zGFX headers
///
/// # Returns
/// Decompressed data
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() {
        bail!("Empty input data");
    }

    let mut decoder = Decompressor::new();
    let mut output = Vec::new();

    let descriptor = data[0];
    let mut pos = 1;

    match descriptor {
        ZGFX_SEGMENTED_SINGLE => {
            // Single segment
            if pos >= data.len() {
                bail!("Invalid single segment packet");
            }
            let segment = &data[pos..];
            let decompressed = decoder.decompress_segment(segment)?;
            output.extend_from_slice(&decompressed);
        }
        ZGFX_SEGMENTED_MULTIPART => {
            // Multi-part
            if pos + 6 > data.len() {
                bail!("Invalid multipart header");
            }

            let segment_count = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;

            let _uncompressed_size =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;

            // Process each segment
            for _ in 0..segment_count {
                if pos + 4 > data.len() {
                    bail!("Invalid segment header");
                }

                let segment_size =
                    u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                        as usize;
                pos += 4;

                if pos + segment_size > data.len() {
                    bail!("Segment extends beyond data");
                }

                let segment = &data[pos..pos + segment_size];
                let decompressed = decoder.decompress_segment(segment)?;
                output.extend_from_slice(&decompressed);

                pos += segment_size;
            }
        }
        _ => {
            bail!("Unknown zGFX descriptor: 0x{:02X}", descriptor);
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uncompressed_single_segment() {
        // Single segment, uncompressed
        let data = vec![
            ZGFX_SEGMENTED_SINGLE, // descriptor
            0x00,                  // flags (not compressed)
            0x01,
            0x02,
            0x03,
            0x04, // data
        ];

        let result = decompress(&data).unwrap();
        assert_eq!(result, vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn test_invalid_descriptor() {
        let data = vec![0xFF]; // Invalid descriptor
        assert!(decompress(&data).is_err());
    }

    #[test]
    fn test_empty_input() {
        assert!(decompress(&[]).is_err());
    }
}
