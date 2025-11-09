/// Auto-Detect Request and Response PDUs for network characteristics detection
/// MS-RDPBCGR 2.2.14

use ironrdp_core::{
    ensure_fixed_part_size, ensure_size, Decode, DecodeResult, Encode, EncodeResult, ReadCursor, WriteCursor,
    other_err,
};

const HEADER_TYPE_ID_AUTODETECT_REQUEST: u8 = 0x00;
const HEADER_TYPE_ID_AUTODETECT_RESPONSE: u8 = 0x01;

/// Auto-Detect Request Types (sent by server)
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u16)]
pub enum AutoDetectRequestType {
    /// RTT Measure Request
    RttMeasureRequest = 0x0001,
    /// Bandwidth Measure Start
    BandwidthMeasureStart = 0x0014,
    /// Bandwidth Measure Stop (sequenced)
    BandwidthMeasureStop = 0x002B,
    /// Bandwidth Measure Stop (not sequenced)
    BandwidthMeasureStopNonSequenced = 0x0429,
    /// Bandwidth Measure Stop (connection startup)
    BandwidthMeasureStopConnectTime = 0x0629,
    /// Network Characteristics Result - baseRTT and averageRTT
    NetcharResultBaseRttAverageRtt = 0x0840,
    /// Network Characteristics Result - bandwidth and averageRTT  
    NetcharResultBandwidthAverageRtt = 0x0880,
    /// Network Characteristics Result - all fields
    NetcharResultAll = 0x08C0,
}

/// Auto-Detect Response Types (sent by client)
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u16)]
pub enum AutoDetectResponseType {
    /// RTT Measure Response
    RttMeasureResponse = 0x0000,
    /// Bandwidth Measure Results (connect-time)
    BandwidthMeasureResultsConnectTime = 0x0003,
    /// Bandwidth Measure Results (after connection)
    BandwidthMeasureResultsAfterConnect = 0x000B,
}

/// RTT Measure Request (MS-RDPBCGR 2.2.14.1.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RttMeasureRequest {
    pub sequence_number: u16,
}

impl RttMeasureRequest {
    const NAME: &'static str = "RttMeasureRequest";
    const FIXED_PART_SIZE: usize = 6;

    pub fn new(sequence_number: u16) -> Self {
        Self { sequence_number }
    }
}

impl Encode for RttMeasureRequest {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u8(0x06); // headerLength
        dst.write_u8(HEADER_TYPE_ID_AUTODETECT_REQUEST);
        dst.write_u16(self.sequence_number);
        dst.write_u16(AutoDetectRequestType::RttMeasureRequest as u16);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for RttMeasureRequest {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let _header_length = src.read_u8();
        let _header_type_id = src.read_u8();
        let sequence_number = src.read_u16();
        let _request_type = src.read_u16();
        Ok(Self { sequence_number })
    }
}

/// RTT Measure Response (MS-RDPBCGR 2.2.14.2.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RttMeasureResponse {
    pub sequence_number: u16,
}

impl RttMeasureResponse {
    const NAME: &'static str = "RttMeasureResponse";
    const FIXED_PART_SIZE: usize = 6;

    pub fn new(sequence_number: u16) -> Self {
        Self { sequence_number }
    }
}

impl Encode for RttMeasureResponse {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u8(0x06); // headerLength
        dst.write_u8(HEADER_TYPE_ID_AUTODETECT_RESPONSE);
        dst.write_u16(self.sequence_number);
        dst.write_u16(AutoDetectResponseType::RttMeasureResponse as u16);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for RttMeasureResponse {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let _header_length = src.read_u8();
        let _header_type_id = src.read_u8();
        let sequence_number = src.read_u16();
        let _response_type = src.read_u16();
        Ok(Self { sequence_number })
    }
}

/// Bandwidth Measure Start (MS-RDPBCGR 2.2.14.1.2)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandwidthMeasureStart {
    pub sequence_number: u16,
}

impl BandwidthMeasureStart {
    const NAME: &'static str = "BandwidthMeasureStart";
    const FIXED_PART_SIZE: usize = 6;

    pub fn new(sequence_number: u16) -> Self {
        Self { sequence_number }
    }
}

impl Encode for BandwidthMeasureStart {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u8(0x06);
        dst.write_u8(HEADER_TYPE_ID_AUTODETECT_REQUEST);
        dst.write_u16(self.sequence_number);
        dst.write_u16(AutoDetectRequestType::BandwidthMeasureStart as u16);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for BandwidthMeasureStart {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let _header_length = src.read_u8();
        let _header_type_id = src.read_u8();
        let sequence_number = src.read_u16();
        let _request_type = src.read_u16();
        Ok(Self { sequence_number })
    }
}

/// Bandwidth Measure Stop (MS-RDPBCGR 2.2.14.1.4)
/// 
/// Note: payloadLength is optional and only present when requestType is 0x002B
/// headerLength is 0x06 if payloadLength is absent, 0x08 if present
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandwidthMeasureStop {
    pub sequence_number: u16,
    pub request_type: u16,
    pub payload_length: Option<u16>,
}

impl BandwidthMeasureStop {
    const NAME: &'static str = "BandwidthMeasureStop";
    const MIN_SIZE: usize = 6;
    const MAX_SIZE: usize = 8;

    pub fn new(sequence_number: u16, request_type: u16, payload_length: Option<u16>) -> Self {
        Self {
            sequence_number,
            request_type,
            payload_length,
        }
    }
}

impl Encode for BandwidthMeasureStop {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        
        let header_length = if self.payload_length.is_some() { 0x08 } else { 0x06 };
        dst.write_u8(header_length);
        dst.write_u8(HEADER_TYPE_ID_AUTODETECT_REQUEST);
        dst.write_u16(self.sequence_number);
        dst.write_u16(self.request_type);
        
        if let Some(payload_length) = self.payload_length {
            dst.write_u16(payload_length);
        }
        
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        if self.payload_length.is_some() {
            Self::MAX_SIZE
        } else {
            Self::MIN_SIZE
        }
    }
}

impl<'de> Decode<'de> for BandwidthMeasureStop {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::MIN_SIZE);
        
        let header_length = src.read_u8();
        let _header_type_id = src.read_u8();
        let sequence_number = src.read_u16();
        let request_type = src.read_u16();
        
        // payloadLength is only present when headerLength is 0x08 (for requestType 0x002B)
        let payload_length = if header_length == 0x08 {
            Some(src.read_u16())
        } else {
            None
        };
        
        Ok(Self {
            sequence_number,
            request_type,
            payload_length,
        })
    }
}

/// Bandwidth Measure Results (MS-RDPBCGR 2.2.14.2.2)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandwidthMeasureResults {
    pub sequence_number: u16,
    pub response_type: u16,
    pub time_delta: u32,
    pub byte_count: u32,
}

impl BandwidthMeasureResults {
    const NAME: &'static str = "BandwidthMeasureResults";
    const FIXED_PART_SIZE: usize = 14;

    pub fn new(sequence_number: u16, response_type: u16, time_delta: u32, byte_count: u32) -> Self {
        Self {
            sequence_number,
            response_type,
            time_delta,
            byte_count,
        }
    }

    pub fn connect_time(sequence_number: u16, time_delta: u32, byte_count: u32) -> Self {
        Self::new(
            sequence_number,
            AutoDetectResponseType::BandwidthMeasureResultsConnectTime as u16,
            time_delta,
            byte_count,
        )
    }

    pub fn after_connect(sequence_number: u16, time_delta: u32, byte_count: u32) -> Self {
        Self::new(
            sequence_number,
            AutoDetectResponseType::BandwidthMeasureResultsAfterConnect as u16,
            time_delta,
            byte_count,
        )
    }
}

impl Encode for BandwidthMeasureResults {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u8(0x0E);
        dst.write_u8(HEADER_TYPE_ID_AUTODETECT_RESPONSE);
        dst.write_u16(self.sequence_number);
        dst.write_u16(self.response_type);
        dst.write_u32(self.time_delta);
        dst.write_u32(self.byte_count);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for BandwidthMeasureResults {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let _header_length = src.read_u8();
        let _header_type_id = src.read_u8();
        let sequence_number = src.read_u16();
        let response_type = src.read_u16();
        let time_delta = src.read_u32();
        let byte_count = src.read_u32();
        Ok(Self {
            sequence_number,
            response_type,
            time_delta,
            byte_count,
        })
    }
}

/// Network Characteristics Result (MS-RDPBCGR 2.2.14.1.5)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkCharacteristicsResult {
    pub sequence_number: u16,
    pub request_type: u16,
    pub base_rtt: Option<u32>,
    pub bandwidth: Option<u32>,
    pub average_rtt: Option<u32>,
}

impl NetworkCharacteristicsResult {
    const NAME: &'static str = "NetworkCharacteristicsResult";
    const MIN_SIZE: usize = 14;
    const MAX_SIZE: usize = 18;
}

impl Encode for NetworkCharacteristicsResult {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        let header_length = if self.request_type == 0x08C0 { 0x12 } else { 0x0E };
        dst.write_u8(header_length);
        dst.write_u8(HEADER_TYPE_ID_AUTODETECT_REQUEST);
        dst.write_u16(self.sequence_number);
        dst.write_u16(self.request_type);

        match self.request_type {
            0x0840 => {
                dst.write_u32(self.base_rtt.unwrap_or(0));
                dst.write_u32(self.average_rtt.unwrap_or(0));
            }
            0x0880 => {
                dst.write_u32(self.bandwidth.unwrap_or(0));
                dst.write_u32(self.average_rtt.unwrap_or(0));
            }
            0x08C0 => {
                dst.write_u32(self.base_rtt.unwrap_or(0));
                dst.write_u32(self.bandwidth.unwrap_or(0));
                dst.write_u32(self.average_rtt.unwrap_or(0));
            }
            _ => {}
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        if self.request_type == 0x08C0 {
            Self::MAX_SIZE
        } else {
            Self::MIN_SIZE
        }
    }
}

impl<'de> Decode<'de> for NetworkCharacteristicsResult {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        let _header_length = src.read_u8();
        let _header_type_id = src.read_u8();
        let sequence_number = src.read_u16();
        let request_type = src.read_u16();

        let (base_rtt, bandwidth, average_rtt) = match request_type {
            0x0840 => (Some(src.read_u32()), None, Some(src.read_u32())),
            0x0880 => (None, Some(src.read_u32()), Some(src.read_u32())),
            0x08C0 => (
                Some(src.read_u32()),
                Some(src.read_u32()),
                Some(src.read_u32()),
            ),
            _ => (None, None, None),
        };

        Ok(Self {
            sequence_number,
            request_type,
            base_rtt,
            bandwidth,
            average_rtt,
        })
    }
}

/// Auto-Detect Request (from server)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDetectRequest {
    RttMeasure(RttMeasureRequest),
    BandwidthMeasureStart(BandwidthMeasureStart),
    BandwidthMeasureStop(BandwidthMeasureStop),
    NetworkCharacteristicsResult(NetworkCharacteristicsResult),
}

impl AutoDetectRequest {
    /// Peek at buffer to determine request type, then decode
    pub fn decode_from_buffer(buf: &[u8]) -> DecodeResult<Self> {
        if buf.len() < 6 {
            return Err(ironrdp_core::other_err!(
                "AutoDetectRequest",
                "buffer too small for auto-detect request"
            ));
        }

        // Peek at request type
        let request_type = u16::from_le_bytes([buf[4], buf[5]]);

        let mut cursor = ReadCursor::new(buf);
        match request_type {
            0x0001 => Ok(Self::RttMeasure(RttMeasureRequest::decode(&mut cursor)?)),
            0x0014 => Ok(Self::BandwidthMeasureStart(BandwidthMeasureStart::decode(
                &mut cursor,
            )?)),
            0x002B | 0x0429 | 0x0629 => Ok(Self::BandwidthMeasureStop(
                BandwidthMeasureStop::decode(&mut cursor)?,
            )),
            0x0840 | 0x0880 | 0x08C0 => Ok(Self::NetworkCharacteristicsResult(
                NetworkCharacteristicsResult::decode(&mut cursor)?,
            )),
            _ => Err(ironrdp_core::other_err!(
                "AutoDetectRequest",
                "unknown request type"
            )),
        }
    }
}

/// Auto-Detect Response (from client)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDetectResponse {
    RttMeasure(RttMeasureResponse),
    BandwidthMeasureResults(BandwidthMeasureResults),
}

impl Encode for AutoDetectResponse {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        match self {
            Self::RttMeasure(r) => r.encode(dst),
            Self::BandwidthMeasureResults(r) => r.encode(dst),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::RttMeasure(r) => r.name(),
            Self::BandwidthMeasureResults(r) => r.name(),
        }
    }

    fn size(&self) -> usize {
        match self {
            Self::RttMeasure(r) => r.size(),
            Self::BandwidthMeasureResults(r) => r.size(),
        }
    }
}
