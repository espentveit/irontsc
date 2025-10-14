#![cfg_attr(doc, doc = include_str!("../README.md"))]
#![doc(
    html_logo_url = "https://cdnweb.devolutions.net/images/projects/devolutions/logos/devolutions-icon-shadow.svg"
)]

mod ack;
mod connection;
mod correlation;
mod error;
mod fec;
mod flags;
mod handshake;
mod header;
mod packet;
mod payload;
mod syndata;
mod syndataex;

pub use crate::ack::{AckOfAckVectorHeader, AckVectorElement, AckVectorHeader, VectorElementState};
pub use crate::connection::{ConnectionState, TransportMode, UdpConfig, UdpConnection};
pub use crate::correlation::CorrelationId;
pub use crate::error::{UdpError, UdpErrorKind, UdpResult};
pub use crate::fec::{FecCodec, GaloisField};
pub use crate::flags::DatagramFlags;
pub use crate::handshake::{SynAckPacket, SynPacket};
pub use crate::header::FecHeader;
pub use crate::packet::{AckPacket, FecPacket, SourcePacket};
pub use crate::payload::{FecPayloadHeader, PayloadPrefix, SourcePayloadHeader};
pub use crate::syndata::SynData;
pub use crate::syndataex::{SynDataEx, SynDataExFlags, UdpProtocolVersion};
