use ironrdp_error::Source;
use thiserror::Error;

pub type UdpResult<T> = Result<T, UdpError>;

pub type UdpError = ironrdp_error::Error<UdpErrorKind>;

#[derive(Debug, Clone, Error)]
pub enum UdpErrorKind {
    #[error("decode error")]
    Decode,
    #[error("invalid field {field} in {context}: {message}")]
    InvalidField {
        context: &'static str,
        field: &'static str,
        message: &'static str,
    },
    #[error("invalid state: {0}")]
    InvalidState(&'static str),
}

pub trait UdpErrorExt {
    fn decode<E: Source>(context: &'static str, source: E) -> UdpError;

    fn invalid_field(context: &'static str, field: &'static str, message: &'static str)
        -> UdpError;

    fn invalid_state(context: &'static str, message: &'static str) -> UdpError;
}

impl UdpErrorExt for UdpError {
    fn decode<E: Source>(context: &'static str, source: E) -> UdpError {
        UdpError::new(context, UdpErrorKind::Decode).with_source(source)
    }

    fn invalid_field(
        context: &'static str,
        field: &'static str,
        message: &'static str,
    ) -> UdpError {
        UdpError::new(
            context,
            UdpErrorKind::InvalidField {
                context,
                field,
                message,
            },
        )
    }

    fn invalid_state(context: &'static str, message: &'static str) -> UdpError {
        UdpError::new(context, UdpErrorKind::InvalidState(message))
    }
}
