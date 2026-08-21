use thiserror::Error;

/// Result alias for the UDP crate.
pub type Result<T, E = UdpError> = std::result::Result<T, E>;

/// Error type used throughout the UDP protocol modules.
#[derive(Debug, Error)]
pub enum UdpError {
    /// The supplied buffer is too short for the expected structure.
    #[error("buffer too short: expected at least {expected} bytes, got {actual}")]
    BufferTooShort { expected: usize, actual: usize },

    /// The supplied buffer length is not aligned as required by the protocol.
    #[error("invalid padding length: {0}")]
    InvalidPadding(usize),

    /// An invalid or unsupported flag combination was encountered.
    #[error("invalid flag combination: 0x{0:04x}")]
    InvalidFlags(u16),

    /// One or more fields contained inconsistent values.
    #[error("inconsistent field value: {0}")]
    InvalidField(&'static str),

    /// Encountered an unrecognised value when decoding an enumeration.
    #[error("unknown enum discriminant: {0}")]
    UnknownDiscriminant(u8),

    /// The caller attempted to recover more packets than supported by the FEC helper.
    #[error("fec recovery requires exactly one missing packet in range")]
    FecRecoveryUnsupported,

    /// General catch-all error with protocol specific context.
    #[error("protocol error: {0}")]
    Protocol(&'static str),
}

impl UdpError {
    /// Helper to raise a `BufferTooShort` error.
    pub fn too_short(expected: usize, actual: usize) -> Self {
        Self::BufferTooShort { expected, actual }
    }
}

/// Small helper used by decoding routines to assert minimum buffer lengths.
#[inline]
pub(crate) fn ensure_min_length(buf: &[u8], min: usize) -> Result<(), UdpError> {
    if buf.len() < min {
        return Err(UdpError::too_short(min, buf.len()));
    }
    Ok(())
}

/// Helper trait implemented by structures that can encode themselves into a byte sink.
pub trait EncodeInto {
    /// Writes the representation of `self` to the provided buffer, appending to it.
    fn encode_into(&self, out: &mut Vec<u8>);
}

/// Helper trait implemented by structures that can be decoded from a byte slice.
pub trait DecodeFrom<'a>: Sized {
    /// Parses `Self` from the start of `input`, returning the parsed value and the tail slice.
    fn decode_from(input: &'a [u8]) -> Result<(Self, &'a [u8])>;
}
