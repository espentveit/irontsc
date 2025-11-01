//! MS-RDPEUDP and MS-RDPEUDP2 data structures and helpers.
//!
//! This crate provides a minimal but thorough implementation of the UDP
//! transport extensions described in the Microsoft Remote Desktop protocol
//! specifications ([MS-RDPEUDP] and [MS-RDPEUDP2]).
//!
//! Two protocol revisions are covered:
//! - [`v1`]: covers the original MS-RDPEUDP transport (versions 1 and 2)
//! - [`v2`]: covers the MS-RDPEUDP2 transport (version 3)
//!
//! The modules expose strongly-typed representations of the packet headers and
//! payloads described in the specifications together with encoder/decoder
//! helpers and utilities for building higher-level state machines.

pub mod connection;
pub mod error;
pub mod v1;
pub mod v2;

pub use connection::{
    ConnectionState, CorrelationId, UdpConfig, UdpConnection, UdpProtocolVersion,
};
pub use error::{Result, UdpError};
pub use v1::fec;
pub use v1::{
    AckSection as V1AckSection, AckVector as V1AckVector, AckVectorElement, CorrelationIdPayload,
    FecPayload, FecPayloadHeader, HeaderFlags as V1HeaderFlags, Packet as V1Packet,
    ProtocolVersion, RdpUdpFecHeader, SourcePayload, SourcePayloadHeader, SynDataExPayload,
    SynDataPayload, TransportMode, VectorElementState,
};
pub use v2::{
    AckPayload as V2AckPayload, AckVecEntry, AckVectorPayload, DataBodyPayload, DataHeaderPayload,
    DelayAckInfoPayload, HeaderFlags as V2HeaderFlags, OverheadSizePayload, Packet as V2Packet,
    PacketHeader as V2PacketHeader, PacketPrefixByte, TimestampInfo,
};
