//! MS-RDPEMT: Multitransport Extension Protocol
//!
//! This module implements the tunnel protocol used to carry DVC traffic over UDP transports.
//! After a UDP transport is established via MS-RDPEUDP handshake, tunnels are created to bind
//! specific DVC channels (like RDPEGFX) to those transports.

use ironrdp_core::{
    ensure_fixed_part_size, ensure_size, invalid_field_err, Decode, DecodeResult, Encode,
    EncodeResult, ReadCursor, WriteCursor,
};

/// Tunnel action types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TunnelAction {
    /// Create a new tunnel (client → server over UDP)
    CreateRequest = 0x00,
    /// Tunnel creation response (server → client over UDP)
    CreateResponse = 0x01,
    /// Tunnel data transfer (DVC payload)
    Data = 0x02,
}

impl TunnelAction {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value & 0x0F {
            0x00 => Some(Self::CreateRequest),
            0x01 => Some(Self::CreateResponse),
            0x02 => Some(Self::Data),
            _ => None,
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// RDP Tunnel Header (MS-RDPEMT 2.2.1.1)
///
/// Appears at the start of all tunnel packets sent over UDP multitransport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelHeader {
    /// Action (lower 4 bits) + Flags (upper 4 bits)
    pub action: TunnelAction,
    pub flags: u8,
    /// Length of the payload following the header
    pub payload_length: u16,
    /// Total length of the header (including subheaders)
    pub header_length: u8,
    /// Subheaders (auto-detect, bandwidth measurement, etc.)
    pub subheaders: Vec<u8>,
}

impl TunnelHeader {
    const NAME: &'static str = "TunnelHeader";
    const MIN_SIZE: usize = 4; // action+flags(1) + payload_len(2) + header_len(1)
    const FIXED_PART_SIZE: usize = Self::MIN_SIZE; // For macro compatibility

    pub fn new(action: TunnelAction, payload_length: u16) -> Self {
        Self {
            action,
            flags: 0,
            payload_length,
            header_length: Self::MIN_SIZE as u8,
            subheaders: Vec::new(),
        }
    }

    pub fn with_subheaders(action: TunnelAction, payload_length: u16, subheaders: Vec<u8>) -> Self {
        let header_length = (Self::MIN_SIZE + subheaders.len()) as u8;
        Self {
            action,
            flags: 0,
            payload_length,
            header_length,
            subheaders,
        }
    }
}

impl Encode for TunnelHeader {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: Self::MIN_SIZE + self.subheaders.len());

        // Pack action (lower 4 bits) and flags (upper 4 bits)
        let action_flags = (self.action.as_u8() & 0x0F) | ((self.flags & 0x0F) << 4);
        dst.write_u8(action_flags);
        dst.write_u16(self.payload_length);
        dst.write_u8(self.header_length);

        if !self.subheaders.is_empty() {
            dst.write_slice(&self.subheaders);
        }

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::MIN_SIZE + self.subheaders.len()
    }
}

impl Decode<'_> for TunnelHeader {
    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let action_flags = src.read_u8();
        let action = TunnelAction::from_u8(action_flags & 0x0F)
            .ok_or_else(|| invalid_field_err!("action", "unknown action type"))?;
        let flags = (action_flags >> 4) & 0x0F;

        let payload_length = src.read_u16();
        let header_length = src.read_u8();

        // Read subheaders if present
        let subheader_len = header_length.saturating_sub(Self::MIN_SIZE as u8) as usize;
        let subheaders = if subheader_len > 0 {
            ensure_size!(in: src, size: subheader_len);
            src.read_slice(subheader_len).to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            action,
            flags,
            payload_length,
            header_length,
            subheaders,
        })
    }
}

/// Tunnel Create Request (MS-RDPEMT 2.2.1.2)
///
/// Sent by client over UDP after handshake to bind a DVC channel to this transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelCreateRequest {
    /// Request ID from the Initiate Multitransport Request
    pub request_id: u32,
    /// Reserved (must be 0)
    pub reserved: u32,
    /// Security cookie from the Initiate Multitransport Request
    pub security_cookie: [u8; 16],
}

impl TunnelCreateRequest {
    const NAME: &'static str = "TunnelCreateRequest";
    const FIXED_PART_SIZE: usize = 4 + 4 + 16; // request_id + reserved + cookie

    pub fn new(request_id: u32, security_cookie: [u8; 16]) -> Self {
        Self {
            request_id,
            reserved: 0,
            security_cookie,
        }
    }
}

impl Encode for TunnelCreateRequest {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);

        dst.write_u32(self.request_id);
        dst.write_u32(self.reserved);
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

impl Decode<'_> for TunnelCreateRequest {
    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let request_id = src.read_u32();
        let reserved = src.read_u32();
        let security_cookie = src.read_array();

        Ok(Self {
            request_id,
            reserved,
            security_cookie,
        })
    }
}

/// Tunnel Create Response (MS-RDPEMT 2.2.1.3)
///
/// Sent by server over UDP in response to TunnelCreateRequest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelCreateResponse {
    /// HRESULT indicating success (0) or failure
    pub hr_response: i32,
}

impl TunnelCreateResponse {
    const NAME: &'static str = "TunnelCreateResponse";
    const FIXED_PART_SIZE: usize = 4; // hr_response

    pub fn new(hr_response: i32) -> Self {
        Self { hr_response }
    }

    pub fn is_success(&self) -> bool {
        self.hr_response >= 0
    }
}

impl Encode for TunnelCreateResponse {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_i32(self.hr_response);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl Decode<'_> for TunnelCreateResponse {
    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let hr_response = src.read_i32();
        Ok(Self { hr_response })
    }
}

/// Complete tunnel packet wrapper
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelPdu {
    /// Tunnel creation request
    CreateRequest {
        header: TunnelHeader,
        request: TunnelCreateRequest,
    },
    /// Tunnel creation response
    CreateResponse {
        header: TunnelHeader,
        response: TunnelCreateResponse,
    },
    /// Tunnel data (DVC payload)
    Data { header: TunnelHeader, payload: Vec<u8> },
}

impl TunnelPdu {
    pub fn create_request(request_id: u32, security_cookie: [u8; 16]) -> Self {
        let request = TunnelCreateRequest::new(request_id, security_cookie);
        let header = TunnelHeader::new(TunnelAction::CreateRequest, TunnelCreateRequest::FIXED_PART_SIZE as u16);
        Self::CreateRequest { header, request }
    }

    pub fn create_response(hr_response: i32) -> Self {
        let response = TunnelCreateResponse::new(hr_response);
        let header = TunnelHeader::new(TunnelAction::CreateResponse, TunnelCreateResponse::FIXED_PART_SIZE as u16);
        Self::CreateResponse { header, response }
    }

    pub fn data(payload: Vec<u8>) -> Self {
        let header = TunnelHeader::new(TunnelAction::Data, payload.len() as u16);
        Self::Data { header, payload }
    }
}

impl Encode for TunnelPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        match self {
            Self::CreateRequest { header, request } => {
                header.encode(dst)?;
                request.encode(dst)?;
            }
            Self::CreateResponse { header, response } => {
                header.encode(dst)?;
                response.encode(dst)?;
            }
            Self::Data { header, payload } => {
                header.encode(dst)?;
                ensure_size!(in: dst, size: payload.len());
                dst.write_slice(payload);
            }
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "TunnelPdu"
    }

    fn size(&self) -> usize {
        match self {
            Self::CreateRequest { header, request } => header.size() + request.size(),
            Self::CreateResponse { header, response } => header.size() + response.size(),
            Self::Data { header, payload } => header.size() + payload.len(),
        }
    }
}

impl Decode<'_> for TunnelPdu {
    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        let header = TunnelHeader::decode(src)?;

        match header.action {
            TunnelAction::CreateRequest => {
                let request = TunnelCreateRequest::decode(src)?;
                Ok(Self::CreateRequest { header, request })
            }
            TunnelAction::CreateResponse => {
                let response = TunnelCreateResponse::decode(src)?;
                Ok(Self::CreateResponse { header, response })
            }
            TunnelAction::Data => {
                ensure_size!(in: src, size: header.payload_length as usize);
                let payload = src.read_slice(header.payload_length as usize).to_vec();
                Ok(Self::Data { header, payload })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_header_encode_decode() {
        let header = TunnelHeader::new(TunnelAction::Data, 1234);
        let mut buf = vec![0u8; 100];
        let mut cursor = WriteCursor::new(&mut buf);
        header.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buf);
        let decoded = TunnelHeader::decode(&mut read_cursor).unwrap();

        assert_eq!(header, decoded);
    }

    #[test]
    fn tunnel_create_request_encode_decode() {
        let cookie = [0x42u8; 16];
        let request = TunnelCreateRequest::new(12345, cookie);

        let mut buf = vec![0u8; 100];
        let mut cursor = WriteCursor::new(&mut buf);
        request.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buf);
        let decoded = TunnelCreateRequest::decode(&mut read_cursor).unwrap();

        assert_eq!(request, decoded);
    }

    #[test]
    fn tunnel_pdu_create_request_round_trip() {
        let cookie = [0xABu8; 16];
        let pdu = TunnelPdu::create_request(99999, cookie);

        let mut buf = vec![0u8; 100];
        let mut cursor = WriteCursor::new(&mut buf);
        pdu.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buf);
        let decoded = TunnelPdu::decode(&mut read_cursor).unwrap();

        assert_eq!(pdu, decoded);
    }
}
