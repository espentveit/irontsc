/// Forward Error Correction (FEC) implementation based on MS-RDPEUDP spec section 3.1.1.6
///
/// This module implements Galois Field (GF(2^8)) arithmetic and FEC encoding/decoding
/// using Reed-Solomon-like codes over GF(2^8).

/// Galois Field GF(2^8) implementation
/// Uses the irreducible polynomial: x^8 + x^4 + x^3 + x^2 + 1 (0x11D)
pub struct GaloisField {
    /// Logarithm table for multiplication
    log_table: [u8; 256],
    /// Exponent table for multiplication
    exp_table: [u8; 256],
}

impl GaloisField {
    /// Create a new Galois Field GF(2^8) with precomputed tables
    pub fn new() -> Self {
        let mut log_table = [0u8; 256];
        let mut exp_table = [0u8; 256];

        // Generator for GF(2^8) is 2
        let generator = 2u8;
        let mut x = 1u8;

        // Build logarithm and exponent tables
        for i in 0..255 {
            exp_table[i] = x;
            log_table[x as usize] = i as u8;

            // Multiply by generator in GF(2^8)
            x = Self::gf_multiply_raw(x, generator);
        }

        // Extend exp table for easier computation
        exp_table[255] = exp_table[0];

        Self {
            log_table,
            exp_table,
        }
    }

    /// Raw GF(2^8) multiplication without using tables
    /// Used during table initialization
    fn gf_multiply_raw(a: u8, b: u8) -> u8 {
        let mut p = 0u8;
        let mut a = a;
        let mut b = b;

        for _ in 0..8 {
            if b & 1 != 0 {
                p ^= a;
            }

            let hi_bit_set = a & 0x80 != 0;
            a <<= 1;

            if hi_bit_set {
                // XOR with irreducible polynomial 0x11D (without the high bit)
                a ^= 0x1D;
            }

            b >>= 1;
        }

        p
    }

    /// Add two elements in GF(2^8) - same as XOR
    #[inline]
    pub fn add(&self, a: u8, b: u8) -> u8 {
        a ^ b
    }

    /// Subtract two elements in GF(2^8) - same as add (XOR)
    #[inline]
    pub fn sub(&self, a: u8, b: u8) -> u8 {
        a ^ b
    }

    /// Multiply two elements in GF(2^8) using log/exp tables
    pub fn multiply(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            return 0;
        }

        let log_a = self.log_table[a as usize] as usize;
        let log_b = self.log_table[b as usize] as usize;
        let log_result = (log_a + log_b) % 255;

        self.exp_table[log_result]
    }

    /// Divide two elements in GF(2^8) using log/exp tables
    pub fn divide(&self, a: u8, b: u8) -> u8 {
        if a == 0 {
            return 0;
        }
        if b == 0 {
            panic!("Division by zero in GF(2^8)");
        }

        let log_a = self.log_table[a as usize] as usize;
        let log_b = self.log_table[b as usize] as usize;
        let log_result = (log_a + 255 - log_b) % 255;

        self.exp_table[log_result]
    }

    /// Raise an element to a power in GF(2^8)
    pub fn power(&self, a: u8, n: u8) -> u8 {
        if a == 0 {
            return 0;
        }

        let log_a = self.log_table[a as usize] as usize;
        let log_result = (log_a * n as usize) % 255;

        self.exp_table[log_result]
    }
}

impl Default for GaloisField {
    fn default() -> Self {
        Self::new()
    }
}

/// FEC encoder/decoder
pub struct FecCodec {
    gf: GaloisField,
}

impl FecCodec {
    /// Create a new FEC codec
    pub fn new() -> Self {
        Self {
            gf: GaloisField::new(),
        }
    }

    /// Encode source packets to generate an FEC packet
    ///
    /// # Arguments
    /// * `source_packets` - Source packet data to encode (all must be same length)
    /// * `fec_index` - Index of the FEC packet to generate (0-based)
    ///
    /// # Returns
    /// FEC packet data
    pub fn encode(&self, source_packets: &[Vec<u8>], fec_index: u8) -> Vec<u8> {
        if source_packets.is_empty() {
            return Vec::new();
        }

        let packet_len = source_packets[0].len();
        let mut fec_packet = vec![0u8; packet_len];

        // For each byte position in the packets
        for byte_pos in 0..packet_len {
            let mut sum = 0u8;

            // Combine source packets using coefficients
            for (i, source) in source_packets.iter().enumerate() {
                if byte_pos < source.len() {
                    let coeff = self.get_coefficient(i as u8, fec_index);
                    let product = self.gf.multiply(source[byte_pos], coeff);
                    sum = self.gf.add(sum, product);
                }
            }

            fec_packet[byte_pos] = sum;
        }

        fec_packet
    }

    /// Decode missing source packet using received source packets and FEC packet
    ///
    /// # Arguments
    /// * `received_packets` - List of (index, data) for received source packets
    /// * `fec_packet` - The FEC packet data
    /// * `missing_index` - Index of the missing source packet to recover
    /// * `fec_index` - Index of the FEC packet
    ///
    /// # Returns
    /// Recovered source packet data
    pub fn decode(
        &self,
        received_packets: &[(u8, Vec<u8>)],
        fec_packet: &[u8],
        missing_index: u8,
        fec_index: u8,
    ) -> Vec<u8> {
        let packet_len = fec_packet.len();
        let mut decoded = vec![0u8; packet_len];

        // For each byte position
        for byte_pos in 0..packet_len {
            let mut sum = fec_packet[byte_pos];

            // Subtract contributions from known source packets
            for (idx, source) in received_packets {
                if byte_pos < source.len() {
                    let coeff = self.get_coefficient(*idx, fec_index);
                    let product = self.gf.multiply(source[byte_pos], coeff);
                    sum = self.gf.sub(sum, product);
                }
            }

            // Divide by the coefficient of the missing packet
            let missing_coeff = self.get_coefficient(missing_index, fec_index);
            decoded[byte_pos] = self.gf.divide(sum, missing_coeff);
        }

        decoded
    }

    /// Get the coefficient for encoding based on source index and FEC index
    ///
    /// According to the spec (section 3.1.1.6.4), we use a Vandermonde-like matrix
    /// where coefficient[i][j] = (i+1)^j
    fn get_coefficient(&self, source_index: u8, fec_index: u8) -> u8 {
        if fec_index == 0 {
            // For first FEC packet, all coefficients are 1 (simple XOR)
            1
        } else {
            // Vandermonde matrix: coeff = (source_index + 1) ^ fec_index
            let base = source_index.wrapping_add(1);
            self.gf.power(base, fec_index)
        }
    }
}

impl Default for FecCodec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_galois_field_basic_operations() {
        let gf = GaloisField::new();

        // Test addition (XOR)
        assert_eq!(gf.add(3, 5), 6);
        assert_eq!(gf.add(255, 255), 0);

        // Test subtraction (same as addition)
        assert_eq!(gf.sub(3, 5), 6);

        // Test multiplication
        assert_eq!(gf.multiply(0, 100), 0);
        assert_eq!(gf.multiply(1, 100), 100);

        // Test multiplication is associative
        let a = 23u8;
        let b = 45u8;
        let c = 67u8;
        assert_eq!(
            gf.multiply(gf.multiply(a, b), c),
            gf.multiply(a, gf.multiply(b, c))
        );

        // Test division
        assert_eq!(gf.divide(100, 100), 1);
        assert_eq!(gf.divide(0, 100), 0);

        // Test a * (a / b) == a for non-zero b
        let divisor = 50u8;
        let result = gf.divide(100, divisor);
        assert_eq!(gf.multiply(result, divisor), 100);
    }

    #[test]
    fn test_fec_encode_decode_simple() {
        let codec = FecCodec::new();

        // Create 3 source packets
        let source1 = vec![1, 2, 3, 4, 5];
        let source2 = vec![6, 7, 8, 9, 10];
        let source3 = vec![11, 12, 13, 14, 15];

        let sources = vec![source1.clone(), source2.clone(), source3.clone()];

        // Generate FEC packet
        let fec = codec.encode(&sources, 0);
        assert_eq!(fec.len(), 5);

        // Simulate losing source2, recover it from source1, source3, and FEC
        let received = vec![(0, source1.clone()), (2, source3.clone())];

        let recovered = codec.decode(&received, &fec, 1, 0);
        assert_eq!(recovered, source2);
    }

    #[test]
    fn test_fec_with_different_fec_indices() {
        let codec = FecCodec::new();

        let source1 = vec![100, 200];
        let source2 = vec![50, 150];

        let sources = vec![source1.clone(), source2.clone()];

        // Generate FEC packet with index 1 (uses different coefficients)
        let fec = codec.encode(&sources, 1);

        // Lose source2, recover with FEC index 1
        let received = vec![(0, source1.clone())];
        let recovered = codec.decode(&received, &fec, 1, 1);

        assert_eq!(recovered, source2);
    }

    #[test]
    fn test_fec_with_varying_packet_sizes() {
        let codec = FecCodec::new();

        // Packets with same max size but different data
        let source1 = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let source2 = vec![10, 20, 30, 40, 50, 60, 70, 80];

        let sources = vec![source1.clone(), source2.clone()];
        let fec = codec.encode(&sources, 0);

        // Recover source1 from source2 and FEC
        let received = vec![(1, source2.clone())];
        let recovered = codec.decode(&received, &fec, 0, 0);

        assert_eq!(recovered, source1);
    }
}
