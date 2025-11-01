//! Forward Error Correction helpers (GF(256) arithmetic).

use std::sync::OnceLock;

use crate::error::{Result, UdpError};

/// Description of a source packet that participates in an FEC block.
#[derive(Debug, Clone)]
pub struct SourceBlock<'a> {
    pub sequence_number: u32,
    pub payload: &'a [u8],
}

/// FEC encoding result.
#[derive(Debug, Clone)]
pub struct EncodedFec {
    pub start_sequence: u32,
    pub range: u8,
    pub fec_index: u8,
    pub coefficients: Vec<u8>,
    pub payload: Vec<u8>,
}

/// Encode a set of sequential source packets into an FEC payload.
pub fn encode_block(block: &[SourceBlock<'_>], fec_index: &mut u8) -> Result<EncodedFec> {
    if block.is_empty() {
        return Err(UdpError::InvalidField("fec.empty_block"));
    }
    if block.len() > 255 {
        return Err(UdpError::InvalidField("fec.block_too_large"));
    }
    let start_sequence = block.first().unwrap().sequence_number;
    let mut prev = start_sequence;
    let mut max_payload = 0usize;
    for source in block {
        if source.sequence_number != prev {
            return Err(UdpError::InvalidField("fec.sequence_gap"));
        }
        prev = prev.wrapping_add(1);
        max_payload = max_payload.max(source.payload.len());
    }
    let end_sequence = block.last().unwrap().sequence_number;
    let span = end_sequence.wrapping_sub(start_sequence);
    if span > u8::MAX as u32 {
        return Err(UdpError::InvalidField("fec.range_exceeded"));
    }
    let range = span as u8;
    let prefixed_len = max_payload + 2;
    if prefixed_len < 2 {
        return Err(UdpError::InvalidField("fec.prefixed_length"));
    }

    let (coefficients, selected_index) =
        generate_coefficients(block.len(), start_sequence, *fec_index);
    *fec_index = selected_index;

    let mut prefixed = Vec::with_capacity(block.len());
    for source in block {
        prefixed.push(build_prefixed(source.payload, prefixed_len));
    }

    let mut payload = vec![0u8; prefixed_len];
    for i in 0..prefixed_len {
        let mut acc = 0u8;
        for (coeff, data) in coefficients.iter().zip(&prefixed) {
            acc ^= gf_mul(*coeff, data[i]);
        }
        payload[i] = acc;
    }

    Ok(EncodedFec {
        start_sequence,
        range,
        fec_index: selected_index,
        coefficients,
        payload,
    })
}

/// Regenerate the coefficient array from the FEC index.
pub fn regenerate_coefficients(fec_index: u8, count: usize, start_sequence: u32) -> Vec<u8> {
    let mut coeffs = Vec::with_capacity(count);
    let mut current = ((start_sequence & 0xff) as u8).wrapping_add(0);
    for _ in 0..count {
        coeffs.push(gf_div(1, fec_index ^ current));
        current = current.wrapping_add(1);
    }
    coeffs
}

/// Recover a single missing packet from the supplied FEC payload.
pub fn recover_single(
    fec_index: u8,
    start_sequence: u32,
    sources: &[Option<&[u8]>],
    fec_payload: &[u8],
) -> Result<Vec<u8>> {
    if sources.is_empty() {
        return Err(UdpError::InvalidField("fec.empty_block"));
    }
    if fec_payload.len() < 2 {
        return Err(UdpError::InvalidField("fec.payload_too_small"));
    }
    let missing_index = sources
        .iter()
        .enumerate()
        .filter_map(|(idx, src)| if src.is_none() { Some(idx) } else { None })
        .collect::<Vec<_>>();
    if missing_index.len() != 1 {
        return Err(UdpError::FecRecoveryUnsupported);
    }
    let missing = missing_index[0];
    let count = sources.len();
    let coefficients = regenerate_coefficients(fec_index, count, start_sequence);
    let prefixed_len = fec_payload.len();
    let mut prefixed_sources: Vec<Option<Vec<u8>>> = Vec::with_capacity(count);
    for src in sources {
        prefixed_sources.push(src.map(|payload| build_prefixed(payload, prefixed_len)));
    }

    let missing_coeff = coefficients[missing];
    if missing_coeff == 0 {
        return Err(UdpError::Protocol("fec.zero_coefficient"));
    }

    let mut recovered = vec![0u8; prefixed_len];
    for i in 0..prefixed_len {
        let mut acc = fec_payload[i];
        for j in 0..count {
            if j == missing {
                continue;
            }
            if let Some(ref data) = prefixed_sources[j] {
                acc ^= gf_mul(coefficients[j], data[i]);
            }
        }
        recovered[i] = gf_div(acc, missing_coeff);
    }

    let declared = u16::from_be_bytes([recovered[0], recovered[1]]) as usize;
    if declared > prefixed_len.saturating_sub(2) {
        return Err(UdpError::InvalidField("fec.invalid_length"));
    }
    Ok(recovered[2..2 + declared].to_vec())
}

fn generate_coefficients(count: usize, start_sequence: u32, mut fec_index: u8) -> (Vec<u8>, u8) {
    let start = (start_sequence & 0xff) as u8;
    let end = start.wrapping_add(count as u8 - 1);
    adjust_fec_index(&mut fec_index, start, end);
    let mut coeffs = Vec::with_capacity(count);
    let mut current = start;
    for _ in 0..count {
        coeffs.push(gf_div(1, fec_index ^ current));
        current = current.wrapping_add(1);
    }
    (coeffs, fec_index)
}

fn adjust_fec_index(fec_index: &mut u8, start: u8, end: u8) {
    if end >= start {
        if *fec_index >= start && *fec_index <= end {
            *fec_index = end.wrapping_add(1);
        }
    } else if *fec_index >= start || *fec_index <= end {
        *fec_index = end.wrapping_add(1);
    }
}

fn build_prefixed(payload: &[u8], total_len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; total_len];
    let size = payload.len().min(total_len.saturating_sub(2));
    let length_bytes = (payload.len() as u16).to_be_bytes();
    buf[0] = length_bytes[0];
    buf[1] = length_bytes[1];
    if size > 0 {
        buf[2..2 + size].copy_from_slice(&payload[..size]);
    }
    buf
}

#[derive(Debug)]
struct Tables {
    exp: [u8; 512],
    log: [u8; 256],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255 {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11d;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        log[0] = 0;
        Tables { exp, log }
    })
}

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let tbl = tables();
    let log_a = tbl.log[a as usize] as usize;
    let log_b = tbl.log[b as usize] as usize;
    tbl.exp[(log_a + log_b) % 255]
}

fn gf_div(a: u8, b: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    debug_assert!(b != 0, "division by zero in GF(256)");
    if b == 0 {
        return 0;
    }
    let tbl = tables();
    let log_a = tbl.log[a as usize] as isize;
    let log_b = tbl.log[b as usize] as isize;
    let mut log_result = log_a - log_b;
    while log_result < 0 {
        log_result += 255;
    }
    tbl.exp[(log_result as usize) % 255]
}
