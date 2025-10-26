//! Multitransport Bootstrapping PDUs (MS-RDPBCGR section 2.2.15)
//!
//! These PDUs are used to bootstrap the creation of sideband channels using UDP transport.

use ironrdp_core::{
    ensure_fixed_part_size, ensure_size, invalid_field_err, Decode, DecodeResult, Encode,
    EncodeResult, ReadCursor, WriteCursor,
};

/// Protocol types for multitransport
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum MultitransportProtocol {
    /// RDP-UDP Forward Error Correction (FEC) reliable transport
    UdpFecReliable = 0x01,
    /// RDP-UDP FEC lossy transport
    UdpFecLossy = 0x02,
    /// Unknown/unsupported protocol (preserved for debugging)
    Unknown(u16),
}

impl MultitransportProtocol {
    pub const RELIABLE_BIT: u16 = 0x0001;
    pub const LOSSY_BIT: u16 = 0x0002;
    pub const KNOWN_TRANSPORT_MASK: u16 = Self::RELIABLE_BIT | Self::LOSSY_BIT;

    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x01 => Some(Self::UdpFecReliable),
            0x02 => Some(Self::UdpFecLossy),
            _ => Some(Self::Unknown(value)), // Accept but mark as unknown
        }
    }

    pub const fn as_u16(self) -> u16 {
        match self {
            Self::UdpFecReliable => 0x01,
            Self::UdpFecLossy => 0x02,
            Self::Unknown(v) => v,
        }
    }

    pub const fn contains_known_transport_bits(value: u16) -> bool {
        value & Self::KNOWN_TRANSPORT_MASK != 0
    }

    pub const fn extra_bits(value: u16) -> u16 {
        value & !Self::KNOWN_TRANSPORT_MASK
    }

    /// Returns true if the requestedProtocol bitmask asked for a reliable tunnel.
    pub const fn has_reliable_bit(self) -> bool {
        self.as_u16() & Self::RELIABLE_BIT != 0
    }

    /// Returns true if the requestedProtocol bitmask asked for a lossy tunnel.
    pub const fn has_lossy_bit(self) -> bool {
        self.as_u16() & Self::LOSSY_BIT != 0
    }
}

/// Server Initiate Multitransport Request PDU
///
/// Sent by the server to the client to bootstrap the creation of a sideband channel.
/// The client should create the requested channel using the specified transport protocol
/// and then secure the channel using TLS or DTLS.
///
/// # MSDN
///
/// * [Server Initiate Multitransport Request PDU](https://docs.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/...)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitiateMultitransportRequest {
    /// Unique ID to associate with the Tunnel Create Request PDU
    pub request_id: u32,
    /// Protocol to use in the transport
    pub requested_protocol: MultitransportProtocol,
    /// Randomly generated data used to validate the channel setup (16 bytes)
    pub security_cookie: [u8; 16],
}

impl InitiateMultitransportRequest {
    const NAME: &'static str = "InitiateMultitransportRequest";
    const FIXED_PART_SIZE: usize = 4 + 2 + 2 + 16; // requestId + protocol + reserved + securityCookie

    pub fn new(request_id: u32, requested_protocol: MultitransportProtocol) -> Self {
        let mut security_cookie = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut security_cookie);

        Self {
            request_id,
            requested_protocol,
            security_cookie,
        }
    }

    pub fn with_cookie(
        request_id: u32,
        requested_protocol: MultitransportProtocol,
        security_cookie: [u8; 16],
    ) -> Self {
        Self {
            request_id,
            requested_protocol,
            security_cookie,
        }
    }
}

impl Encode for InitiateMultitransportRequest {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);

        dst.write_u32(self.request_id);
        dst.write_u16(self.requested_protocol.as_u16());
        dst.write_u16(0); // reserved
        dst.write_slice(&self.security_cookie);

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for InitiateMultitransportRequest {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let request_id = src.read_u32();
        let protocol_value = src.read_u16();
        let requested_protocol = MultitransportProtocol::from_u16(protocol_value)
            .ok_or_else(|| invalid_field_err!("requestedProtocol", "invalid protocol value"))?;
        let _reserved = src.read_u16();

        let security_cookie_bytes = src.read_slice(16);
        let mut security_cookie = [0u8; 16];
        security_cookie.copy_from_slice(security_cookie_bytes);

        Ok(Self {
            request_id,
            requested_protocol,
            security_cookie,
        })
    }
}

/// Client Initiate Multitransport Response PDU
///
/// Sent by the client to the server to indicate whether the client was able to
/// complete the multitransport initiation request.
///
/// # MSDN
///
/// * [Client Initiate Multitransport Response PDU](https://docs.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/...)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitiateMultitransportResponse {
    /// ID from the corresponding request
    pub request_id: u32,
    /// HRESULT indicating success or failure
    /// 0x00000000 = S_OK (success)
    /// Other values indicate failure
    pub hr_response: u32,
}

impl InitiateMultitransportResponse {
    const NAME: &'static str = "InitiateMultitransportResponse";
    const FIXED_PART_SIZE: usize = 4 + 4; // requestId + hrResponse

    pub const S_OK: u32 = 0x00000000;
    pub const E_ABORT: u32 = 0x80004004;

    pub fn success(request_id: u32) -> Self {
        Self {
            request_id,
            hr_response: Self::S_OK,
        }
    }

    pub fn failure(request_id: u32, hr_response: u32) -> Self {
        Self {
            request_id,
            hr_response,
        }
    }

    pub fn is_success(&self) -> bool {
        self.hr_response == Self::S_OK
    }
}

impl Encode for InitiateMultitransportResponse {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);

        dst.write_u32(self.request_id);
        dst.write_u32(self.hr_response);

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for InitiateMultitransportResponse {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let request_id = src.read_u32();
        let hr_response = src.read_u32();

        Ok(Self {
            request_id,
            hr_response,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initiate_multitransport_request_encode_decode() {
        let request = InitiateMultitransportRequest::with_cookie(
            12345,
            MultitransportProtocol::UdpFecReliable,
            [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
                0x0F, 0x10,
            ],
        );

        let mut buffer = vec![0u8; request.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        request.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buffer);
        let decoded = InitiateMultitransportRequest::decode(&mut read_cursor).unwrap();

        assert_eq!(request, decoded);
    }

    #[test]
    fn test_initiate_multitransport_response_encode_decode() {
        let response = InitiateMultitransportResponse::success(12345);

        let mut buffer = vec![0u8; response.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        response.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buffer);
        let decoded = InitiateMultitransportResponse::decode(&mut read_cursor).unwrap();

        assert_eq!(response, decoded);
        assert!(decoded.is_success());
    }
}
