use alloc::format;
use core::fmt;

use ironrdp_core::{
    cast_length, ensure_fixed_part_size, ensure_size, invalid_field_err, unsupported_value_err,
    Decode, DecodeError, DecodeResult, Encode, EncodeResult, ReadCursor, WriteCursor,
};
use ironrdp_pdu::utils::{
    checked_sum, encoded_str_len, read_string_from_cursor, strict_sum, write_string_to_cursor,
    CharacterSet,
};
use ironrdp_svc::SvcEncode;

use alloc::collections::BTreeSet;

use crate::{DynamicChannelId, String, Vec};

/// Dynamic Virtual Channel PDU's that are sent by both client and server.
#[derive(Debug, PartialEq)]
pub enum DrdynvcDataPdu {
    DataFirst(DataFirstPdu),
    Data(DataPdu),
}

impl DrdynvcDataPdu {
    /// Maximum size of the `data` field in `DrdynvcDataPdu`.
    pub const MAX_DATA_SIZE: usize = 1590;

    pub fn channel_id(&self) -> DynamicChannelId {
        match self {
            DrdynvcDataPdu::DataFirst(pdu) => pdu.channel_id,
            DrdynvcDataPdu::Data(pdu) => pdu.channel_id,
        }
    }
}

impl Encode for DrdynvcDataPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        match self {
            DrdynvcDataPdu::DataFirst(pdu) => pdu.encode(dst),
            DrdynvcDataPdu::Data(pdu) => pdu.encode(dst),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            DrdynvcDataPdu::DataFirst(_) => DataFirstPdu::name(),
            DrdynvcDataPdu::Data(_) => DataPdu::name(),
        }
    }

    fn size(&self) -> usize {
        match self {
            DrdynvcDataPdu::DataFirst(pdu) => pdu.size(),
            DrdynvcDataPdu::Data(pdu) => pdu.size(),
        }
    }
}

/// Dynamic Virtual Channel PDU's that are sent by the client.
#[derive(Debug, PartialEq)]
pub enum DrdynvcClientPdu {
    Capabilities(CapabilitiesResponsePdu),
    Create(CreateResponsePdu),
    Close(ClosePdu),
    Data(DrdynvcDataPdu),
    SoftSyncRequest(SoftSyncRequestPdu),
    SoftSyncResponse(SoftSyncResponsePdu),
}

impl Encode for DrdynvcClientPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        match self {
            DrdynvcClientPdu::Capabilities(pdu) => pdu.encode(dst),
            DrdynvcClientPdu::Create(pdu) => pdu.encode(dst),
            DrdynvcClientPdu::Data(pdu) => pdu.encode(dst),
            DrdynvcClientPdu::Close(pdu) => pdu.encode(dst),
            DrdynvcClientPdu::SoftSyncRequest(pdu) => pdu.encode(dst),
            DrdynvcClientPdu::SoftSyncResponse(pdu) => pdu.encode(dst),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            DrdynvcClientPdu::Capabilities(_) => CapabilitiesResponsePdu::name(),
            DrdynvcClientPdu::Create(_) => CreateResponsePdu::name(),
            DrdynvcClientPdu::Data(pdu) => pdu.name(),
            DrdynvcClientPdu::Close(_) => ClosePdu::name(),
            DrdynvcClientPdu::SoftSyncRequest(_) => SoftSyncRequestPdu::name(),
            DrdynvcClientPdu::SoftSyncResponse(_) => SoftSyncResponsePdu::name(),
        }
    }

    fn size(&self) -> usize {
        match self {
            DrdynvcClientPdu::Capabilities(_) => CapabilitiesResponsePdu::size(),
            DrdynvcClientPdu::Create(pdu) => pdu.size(),
            DrdynvcClientPdu::Data(pdu) => pdu.size(),
            DrdynvcClientPdu::Close(pdu) => pdu.size(),
            DrdynvcClientPdu::SoftSyncRequest(pdu) => pdu.size(),
            DrdynvcClientPdu::SoftSyncResponse(pdu) => pdu.size(),
        }
    }
}

impl Decode<'_> for DrdynvcClientPdu {
    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        let header = Header::decode(src)?;
        match header.cmd {
            Cmd::Create => Ok(Self::Create(CreateResponsePdu::decode(header, src)?)),
            Cmd::DataFirst => Ok(Self::Data(DrdynvcDataPdu::DataFirst(DataFirstPdu::decode(
                header, src,
            )?))),
            Cmd::Data => Ok(Self::Data(DrdynvcDataPdu::Data(DataPdu::decode(
                header, src,
            )?))),
            Cmd::Close => Ok(Self::Close(ClosePdu::decode(header, src)?)),
            Cmd::Capability => Ok(Self::Capabilities(CapabilitiesResponsePdu::decode(
                header, src,
            )?)),
            Cmd::SoftSyncRequest => Ok(Self::SoftSyncRequest(SoftSyncRequestPdu::decode(
                header, src,
            )?)),
            Cmd::SoftSyncResponse => Ok(Self::SoftSyncResponse(SoftSyncResponsePdu::decode(
                header, src,
            )?)),
            _ => Err(unsupported_value_err!("Cmd", header.cmd.into())),
        }
    }
}

/// Dynamic Virtual Channel PDU's that are sent by the server.
#[derive(Debug, PartialEq)]
pub enum DrdynvcServerPdu {
    Capabilities(CapabilitiesRequestPdu),
    Create(CreateRequestPdu),
    Close(ClosePdu),
    Data(DrdynvcDataPdu),
    SoftSyncRequest(SoftSyncRequestPdu),
    SoftSyncResponse(SoftSyncResponsePdu),
}

impl Encode for DrdynvcServerPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        match self {
            DrdynvcServerPdu::Data(pdu) => pdu.encode(dst),
            DrdynvcServerPdu::Capabilities(pdu) => pdu.encode(dst),
            DrdynvcServerPdu::Create(pdu) => pdu.encode(dst),
            DrdynvcServerPdu::Close(pdu) => pdu.encode(dst),
            DrdynvcServerPdu::SoftSyncRequest(pdu) => pdu.encode(dst),
            DrdynvcServerPdu::SoftSyncResponse(pdu) => pdu.encode(dst),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            DrdynvcServerPdu::Data(pdu) => pdu.name(),
            DrdynvcServerPdu::Capabilities(pdu) => pdu.name(),
            DrdynvcServerPdu::Create(_) => CreateRequestPdu::name(),
            DrdynvcServerPdu::Close(_) => ClosePdu::name(),
            DrdynvcServerPdu::SoftSyncRequest(_) => SoftSyncRequestPdu::name(),
            DrdynvcServerPdu::SoftSyncResponse(_) => SoftSyncResponsePdu::name(),
        }
    }

    fn size(&self) -> usize {
        match self {
            DrdynvcServerPdu::Data(pdu) => pdu.size(),
            DrdynvcServerPdu::Capabilities(pdu) => pdu.size(),
            DrdynvcServerPdu::Create(pdu) => pdu.size(),
            DrdynvcServerPdu::Close(pdu) => pdu.size(),
            DrdynvcServerPdu::SoftSyncRequest(pdu) => pdu.size(),
            DrdynvcServerPdu::SoftSyncResponse(pdu) => pdu.size(),
        }
    }
}

impl Decode<'_> for DrdynvcServerPdu {
    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        let header = Header::decode(src)?;
        match header.cmd {
            Cmd::Create => Ok(Self::Create(CreateRequestPdu::decode(header, src)?)),
            Cmd::DataFirst => Ok(Self::Data(DrdynvcDataPdu::DataFirst(DataFirstPdu::decode(
                header, src,
            )?))),
            Cmd::Data => Ok(Self::Data(DrdynvcDataPdu::Data(DataPdu::decode(
                header, src,
            )?))),
            Cmd::Close => Ok(Self::Close(ClosePdu::decode(header, src)?)),
            Cmd::Capability => Ok(Self::Capabilities(CapabilitiesRequestPdu::decode(
                header, src,
            )?)),
            Cmd::SoftSyncRequest => Ok(Self::SoftSyncRequest(SoftSyncRequestPdu::decode(
                header, src,
            )?)),
            Cmd::SoftSyncResponse => Ok(Self::SoftSyncResponse(SoftSyncResponsePdu::decode(
                header, src,
            )?)),
            _ => Err(unsupported_value_err!("Cmd", header.cmd.into())),
        }
    }
}

// Dynamic virtual channel PDU's are sent over a static virtual channel, so they are `SvcEncode`.
impl SvcEncode for DrdynvcDataPdu {}
impl SvcEncode for DrdynvcClientPdu {}
impl SvcEncode for DrdynvcServerPdu {}

/// [2.2] Message Syntax
///
/// [2.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/0b07a750-bf51-4042-bcf2-a991b6729d6e
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    cb_id: FieldType, // 2 bit
    sp: FieldType,    // 2 bit; meaning depends on the cmd field
    cmd: Cmd,         // 4 bit
}

impl Header {
    pub const FIXED_PART_SIZE: usize = 1;
    /// Create a new `Header` with the given `cb_id_val`, `sp_val`, and `cmd`.
    ///
    /// If `cb_id_val` or `sp_val` is not relevant for a given `cmd`, it should be set to 0 respectively.
    fn new(cb_id_val: u32, sp_val: u32, cmd: Cmd) -> Self {
        Self {
            cb_id: FieldType::for_val(cb_id_val),
            sp: FieldType::for_val(sp_val),
            cmd,
        }
    }

    fn with_cb_id(self, cb_id: FieldType) -> Self {
        Self { cb_id, ..self }
    }

    fn with_sp(self, sp: FieldType) -> Self {
        Self { sp, ..self }
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u8(
            ((self.cmd as u8) << 4)
                | (Into::<u8>::into(self.sp) << 2)
                | Into::<u8>::into(self.cb_id),
        );
        Ok(())
    }

    fn decode(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::size());
        let byte = src.read_u8();
        let cmd = Cmd::try_from(byte >> 4)?;
        let sp = FieldType::from((byte >> 2) & 0b11);
        let cb_id = FieldType::from(byte & 0b11);
        Ok(Self { cb_id, sp, cmd })
    }

    fn size() -> usize {
        Self::FIXED_PART_SIZE
    }
}

/// [2.2] Message Syntax
///
/// [2.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/0b07a750-bf51-4042-bcf2-a991b6729d6e
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq)]
enum Cmd {
    Create = 0x01,
    DataFirst = 0x02,
    Data = 0x03,
    Close = 0x04,
    Capability = 0x05,
    DataFirstCompressed = 0x06,
    DataCompressed = 0x07,
    SoftSyncRequest = 0x08,
    SoftSyncResponse = 0x09,
}

impl TryFrom<u8> for Cmd {
    type Error = DecodeError;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        match byte {
            0x01 => Ok(Self::Create),
            0x02 => Ok(Self::DataFirst),
            0x03 => Ok(Self::Data),
            0x04 => Ok(Self::Close),
            0x05 => Ok(Self::Capability),
            0x06 => Ok(Self::DataFirstCompressed),
            0x07 => Ok(Self::DataCompressed),
            0x08 => Ok(Self::SoftSyncRequest),
            0x09 => Ok(Self::SoftSyncResponse),
            _ => Err(invalid_field_err!("Cmd", "invalid cmd")),
        }
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Cmd::Create => "Create",
            Cmd::DataFirst => "DataFirst",
            Cmd::Data => "Data",
            Cmd::Close => "Close",
            Cmd::Capability => "Capability",
            Cmd::DataFirstCompressed => "DataFirstCompressed",
            Cmd::DataCompressed => "DataCompressed",
            Cmd::SoftSyncRequest => "SoftSyncRequest",
            Cmd::SoftSyncResponse => "SoftSyncResponse",
        })
    }
}

impl From<Cmd> for String {
    fn from(cmd: Cmd) -> Self {
        format!("{cmd:?}")
    }
}

/// 2.2.3.1 DVC Data First PDU (DYNVC_DATA_FIRST)
///
/// [2.2.3.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/69377767-56a6-4ab8-996b-7758676e9261
#[derive(Debug, PartialEq)]
pub struct DataFirstPdu {
    header: Header,
    pub channel_id: DynamicChannelId,
    /// Length is the *total* length of the data to be sent, including the length
    /// of the data that will be sent by subsequent DVC_DATA PDUs.
    pub length: u32,
    /// Data is just the data to be sent in this PDU.
    pub data: Vec<u8>,
}

impl DataFirstPdu {
    /// Create a new `DataFirstPdu` with the given `channel_id`, `length`, and `data`.
    ///
    /// `length` is the *total* length of the data to be sent, including the length
    /// of the data that will be sent by subsequent `DataPdu`s.
    ///
    /// `data` is just the data to be sent in this PDU.
    pub fn new(channel_id: DynamicChannelId, total_length: u32, data: Vec<u8>) -> Self {
        Self {
            header: Header::new(channel_id, total_length, Cmd::DataFirst),
            channel_id,
            length: total_length,
            data,
        }
    }

    #[must_use]
    pub fn with_cb_id_type(self, cb_id: FieldType) -> Self {
        Self {
            header: self.header.with_cb_id(cb_id),
            ..self
        }
    }

    #[must_use]
    pub fn with_sp_type(self, sp: FieldType) -> Self {
        Self {
            header: self.header.with_sp(sp),
            ..self
        }
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        let fixed_part_size = checked_sum(&[header.cb_id.size_of_val(), header.sp.size_of_val()])?;
        ensure_size!(in: src, size: fixed_part_size);
        let channel_id = header.cb_id.decode_val(src)?;
        let length = header.sp.decode_val(src)?;
        let data = src.read_remaining().to_vec();
        Ok(Self {
            header,
            channel_id,
            length,
            data,
        })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        self.header.encode(dst)?;
        self.header.cb_id.encode_val(self.channel_id, dst)?;
        self.header
            .sp
            .encode_val(cast_length!("DataFirstPdu::Length", self.length)?, dst)?;
        dst.write_slice(&self.data);
        Ok(())
    }

    fn name() -> &'static str {
        "DYNVC_DATA_FIRST"
    }

    fn size(&self) -> usize {
        strict_sum(&[
            Header::size(),
            self.header.cb_id.size_of_val(),
            self.header.sp.size_of_val(),
            self.data.len(),
        ])
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FieldType(u8);

impl FieldType {
    pub const U8: Self = Self(0x00);
    pub const U16: Self = Self(0x01);
    pub const U32: Self = Self(0x02);

    fn encode_val(&self, value: u32, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size_of_val());
        match *self {
            FieldType::U8 => dst.write_u8(cast_length!("FieldType::encode", value)?),
            FieldType::U16 => dst.write_u16(cast_length!("FieldType::encode", value)?),
            FieldType::U32 => dst.write_u32(value),
            _ => return Err(invalid_field_err!("FieldType", "invalid field type")),
        };
        Ok(())
    }

    fn decode_val(&self, src: &mut ReadCursor<'_>) -> DecodeResult<u32> {
        ensure_size!(in: src, size: self.size_of_val());
        match *self {
            FieldType::U8 => Ok(u32::from(src.read_u8())),
            FieldType::U16 => Ok(u32::from(src.read_u16())),
            FieldType::U32 => Ok(src.read_u32()),
            _ => Err(invalid_field_err!("FieldType", "invalid field type")),
        }
    }

    /// Returns the size of the value in bytes.
    fn size_of_val(&self) -> usize {
        match *self {
            FieldType::U8 => 1,
            FieldType::U16 => 2,
            FieldType::U32 => 4,
            _ => 0,
        }
    }

    fn for_val(value: u32) -> Self {
        if u8::try_from(value).is_ok() {
            FieldType::U8
        } else if u16::try_from(value).is_ok() {
            FieldType::U16
        } else {
            FieldType::U32
        }
    }
}

impl From<u8> for FieldType {
    fn from(byte: u8) -> Self {
        match byte {
            0x00 => Self::U8,
            0x01 => Self::U16,
            0x02 => Self::U32,
            _ => Self(byte),
        }
    }
}

impl From<FieldType> for u8 {
    fn from(field_type: FieldType) -> Self {
        field_type.0
    }
}

/// 2.2.3.2 DVC Data PDU (DYNVC_DATA)
///
/// [2.2.3.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/15b59886-db44-47f1-8da3-47c8fcd82803
#[derive(Debug, PartialEq)]
pub struct DataPdu {
    header: Header,
    pub channel_id: DynamicChannelId,
    pub data: Vec<u8>,
}

impl DataPdu {
    pub fn new(channel_id: DynamicChannelId, data: Vec<u8>) -> Self {
        Self {
            header: Header::new(channel_id, 0, Cmd::Data),
            channel_id,
            data,
        }
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: header.cb_id.size_of_val());
        let channel_id = header.cb_id.decode_val(src)?;
        let data = src.read_remaining().to_vec();
        Ok(Self {
            header,
            channel_id,
            data,
        })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        self.header.encode(dst)?;
        self.header.cb_id.encode_val(self.channel_id, dst)?;
        dst.write_slice(&self.data);
        Ok(())
    }

    fn name() -> &'static str {
        "DYNVC_DATA"
    }

    fn size(&self) -> usize {
        strict_sum(&[
            Header::size(),
            self.header.cb_id.size_of_val(), // ChannelId
            self.data.len(),                 // Data
        ])
    }
}

/// 2.2.2.2 DVC Create Response PDU (DYNVC_CREATE_RSP)
///
/// [2.2.2.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/8f284ea3-54f3-4c24-8168-8a001c63b581
#[derive(Debug, PartialEq)]
pub struct CreateResponsePdu {
    header: Header,
    pub channel_id: DynamicChannelId,
    pub creation_status: CreationStatus,
}

impl CreateResponsePdu {
    pub fn new(channel_id: DynamicChannelId, creation_status: CreationStatus) -> Self {
        Self {
            header: Header::new(channel_id, 0, Cmd::Create),
            channel_id,
            creation_status,
        }
    }

    fn name() -> &'static str {
        "DYNVC_CREATE_RSP"
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::headerless_size(&header));
        let channel_id = header.cb_id.decode_val(src)?;
        let creation_status = CreationStatus(src.read_u32());
        Ok(Self {
            header,
            channel_id,
            creation_status,
        })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        self.header.encode(dst)?;
        self.header.cb_id.encode_val(self.channel_id, dst)?;
        self.creation_status.encode(dst)?;
        Ok(())
    }

    fn headerless_size(header: &Header) -> usize {
        strict_sum(&[
            header.cb_id.size_of_val(), // ChannelId
            CreationStatus::size(),     // CreationStatus
        ])
    }

    fn size(&self) -> usize {
        strict_sum(&[Header::size(), Self::headerless_size(&self.header)])
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct CreationStatus(u32);

impl CreationStatus {
    pub const OK: Self = Self(0x00000000);
    pub const NOT_FOUND: Self = Self(0xC0000225);
    pub const NO_LISTENER: Self = Self(0xC0000001);

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: Self::size());
        dst.write_u32(self.0);
        Ok(())
    }

    fn size() -> usize {
        4
    }
}

impl From<CreationStatus> for u32 {
    fn from(val: CreationStatus) -> Self {
        val.0
    }
}

/// 2.2.4 Closing a DVC (DYNVC_CLOSE)
///
/// [2.2.4]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/c02dfd21-ccbc-4254-985b-3ef6dd115dec
#[derive(Debug, PartialEq)]
pub struct ClosePdu {
    header: Header,
    pub channel_id: DynamicChannelId,
}

impl ClosePdu {
    pub fn new(channel_id: DynamicChannelId) -> Self {
        Self {
            header: Header::new(channel_id, 0, Cmd::Close),
            channel_id,
        }
    }

    #[must_use]
    pub fn with_cb_id_type(self, cb_id: FieldType) -> Self {
        Self {
            header: self.header.with_cb_id(cb_id),
            ..self
        }
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::headerless_size(&header));
        let channel_id = header.cb_id.decode_val(src)?;
        Ok(Self { header, channel_id })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        self.header.encode(dst)?;
        self.header.cb_id.encode_val(self.channel_id, dst)?;
        Ok(())
    }

    fn name() -> &'static str {
        "DYNVC_CLOSE"
    }

    fn headerless_size(header: &Header) -> usize {
        header.cb_id.size_of_val()
    }

    fn size(&self) -> usize {
        strict_sum(&[Header::size(), Self::headerless_size(&self.header)])
    }
}

/// 2.2.1.2 DVC Capabilities Response PDU (DYNVC_CAPS_RSP)
///
/// [2.2.1.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/d45cb2a6-e7bd-453e-8603-9c57600e24ce
#[derive(Debug, PartialEq)]
pub struct CapabilitiesResponsePdu {
    header: Header,
    version: CapsVersion,
}

impl CapabilitiesResponsePdu {
    const HEADERLESS_FIXED_PART_SIZE: usize = 1 /* Pad */ + CapsVersion::FIXED_PART_SIZE /* Version */;
    const FIXED_PART_SIZE: usize = Header::FIXED_PART_SIZE + Self::HEADERLESS_FIXED_PART_SIZE;

    pub fn new(version: CapsVersion) -> Self {
        Self {
            header: Header::new(0, 0, Cmd::Capability),
            version,
        }
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::HEADERLESS_FIXED_PART_SIZE);
        let _pad = src.read_u8();
        let version = CapsVersion::try_from(src.read_u16())?;
        Ok(Self { header, version })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: Self::size());
        self.header.encode(dst)?;
        dst.write_u8(0x00); // Pad, MUST be 0x00
        self.version.encode(dst)?;
        Ok(())
    }

    fn name() -> &'static str {
        "DYNVC_CAPS_RSP"
    }

    fn size() -> usize {
        Self::FIXED_PART_SIZE
    }
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum CapsVersion {
    V1 = 0x0001,
    V2 = 0x0002,
    V3 = 0x0003,
}

impl CapsVersion {
    const FIXED_PART_SIZE: usize = 2;

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: Self::size());
        dst.write_u16(*self as u16);
        Ok(())
    }

    fn size() -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl TryFrom<u16> for CapsVersion {
    type Error = DecodeError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x0001 => Ok(Self::V1),
            0x0002 => Ok(Self::V2),
            0x0003 => Ok(Self::V3),
            _ => Err(invalid_field_err!("CapsVersion", "invalid version")),
        }
    }
}

impl From<CapsVersion> for u16 {
    fn from(version: CapsVersion) -> Self {
        version as u16
    }
}

/// 2.2.1.1 DVC Capabilities Request PDU
///
/// [2.2.1.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/c07b15ae-304e-46b8-befe-39c6d95c25e0
#[derive(Debug, PartialEq)]
pub enum CapabilitiesRequestPdu {
    V1 {
        header: Header,
    },
    V2 {
        header: Header,
        charges: [u16; CapabilitiesRequestPdu::PRIORITY_CHARGE_COUNT],
    },
    V3 {
        header: Header,
        charges: [u16; CapabilitiesRequestPdu::PRIORITY_CHARGE_COUNT],
    },
}

impl CapabilitiesRequestPdu {
    const HEADERLESS_FIXED_PART_SIZE: usize = 1 /* Pad */ + 2 /* Version */;
    const FIXED_PART_SIZE: usize = Header::FIXED_PART_SIZE + Self::HEADERLESS_FIXED_PART_SIZE;
    const PRIORITY_CHARGE_SIZE: usize = 2; // 2 bytes for each priority charge
    const PRIORITY_CHARGE_COUNT: usize = 4; // 4 priority charges
    const PRIORITY_CHARGES_SIZE: usize = Self::PRIORITY_CHARGE_COUNT * Self::PRIORITY_CHARGE_SIZE;

    pub fn new(version: CapsVersion, charges: Option<[u16; Self::PRIORITY_CHARGE_COUNT]>) -> Self {
        let header = Header::new(0, 0, Cmd::Capability);
        let charges = charges.unwrap_or([0; Self::PRIORITY_CHARGE_COUNT]);

        match version {
            CapsVersion::V1 => Self::V1 { header },
            CapsVersion::V2 => Self::V2 { header, charges },
            CapsVersion::V3 => Self::V3 { header, charges },
        }
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::HEADERLESS_FIXED_PART_SIZE);
        let _pad = src.read_u8();
        let version = CapsVersion::try_from(src.read_u16())?;
        match version {
            CapsVersion::V1 => Ok(Self::V1 { header }),
            _ => {
                ensure_size!(in: src, size: Self::PRIORITY_CHARGES_SIZE);
                let mut charges = [0u16; Self::PRIORITY_CHARGE_COUNT];
                for charge in charges.iter_mut() {
                    *charge = src.read_u16();
                }

                match version {
                    CapsVersion::V2 => Ok(Self::V2 { header, charges }),
                    CapsVersion::V3 => Ok(Self::V3 { header, charges }),
                    _ => unreachable!(),
                }
            }
        }
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        match self {
            CapabilitiesRequestPdu::V1 { header }
            | CapabilitiesRequestPdu::V2 { header, .. }
            | CapabilitiesRequestPdu::V3 { header, .. } => header.encode(dst)?,
        };
        dst.write_u8(0x00); // Pad, MUST be 0x00
        match self {
            CapabilitiesRequestPdu::V1 { .. } => dst.write_u16(CapsVersion::V1.into()),
            CapabilitiesRequestPdu::V2 { .. } => dst.write_u16(CapsVersion::V2.into()),
            CapabilitiesRequestPdu::V3 { .. } => dst.write_u16(CapsVersion::V3.into()),
        }
        match self {
            CapabilitiesRequestPdu::V1 { .. } => {}
            CapabilitiesRequestPdu::V2 { charges, .. }
            | CapabilitiesRequestPdu::V3 { charges, .. } => {
                for charge in charges.iter() {
                    dst.write_u16(*charge);
                }
            }
        }
        Ok(())
    }

    fn size(&self) -> usize {
        match self {
            Self::V1 { .. } => Self::FIXED_PART_SIZE,
            _ => Self::FIXED_PART_SIZE + Self::PRIORITY_CHARGES_SIZE,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::V1 { .. } => "DYNVC_CAPS_VERSION1",
            Self::V2 { .. } => "DYNVC_CAPS_VERSION2",
            Self::V3 { .. } => "DYNVC_CAPS_VERSION3",
        }
    }
}

pub const SOFT_SYNC_TCP_FLUSHED: u16 = 0x0001;
pub const SOFT_SYNC_CHANNEL_LIST_PRESENT: u16 = 0x0002;

pub type SoftSyncTunnelType = u32;

#[derive(Debug, Clone, PartialEq)]
pub struct SoftSyncChannelListEntry {
    pub tunnel_type: SoftSyncTunnelType,
    pub channel_ids: Vec<DynamicChannelId>,
}

/// 2.2.5.1 Soft-Sync Request PDU (DYNVC_SOFT_SYNC_REQUEST)
#[derive(Debug, Clone, PartialEq)]
pub struct SoftSyncRequestPdu {
    header: Header,
    pub length: u32,
    pub flags: u16,
    pub tunnels: Vec<SoftSyncChannelListEntry>,
}

impl SoftSyncRequestPdu {
    const PAD_SIZE: usize = 1;

    pub fn new(flags: u16, tunnels: Vec<SoftSyncChannelListEntry>) -> Self {
        let mut this = Self {
            header: Header::new(0, 0, Cmd::SoftSyncRequest),
            length: 0,
            flags,
            tunnels,
        };
        this.length = this.payload_length();
        this
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: 1 + 4 + 2 + 2);
        let _pad = src.read_u8();
        let length = src.read_u32();
        let flags = src.read_u16();
        let tunnel_count = src.read_u16();

        let mut tunnels = Vec::new();
        if (flags & SOFT_SYNC_CHANNEL_LIST_PRESENT) != 0 {
            tunnels.reserve(tunnel_count as usize);
            for _ in 0..tunnel_count {
                ensure_size!(in: src, size: 6);
                let tunnel_type = src.read_u32();
                let dvc_count = src.read_u16();
                let mut channel_ids = Vec::with_capacity(dvc_count as usize);
                for _ in 0..dvc_count {
                    ensure_size!(in: src, size: 4);
                    channel_ids.push(src.read_u32());
                }
                tunnels.push(SoftSyncChannelListEntry {
                    tunnel_type,
                    channel_ids,
                });
            }
        } else if length > 4 {
            let to_skip = length as usize - 4;
            ensure_size!(in: src, size: to_skip);
            src.advance(to_skip);
        }

        Ok(Self {
            header,
            length,
            flags,
            tunnels,
        })
    }

    fn payload_length(&self) -> u32 {
        let mut len = 2 /* flags */ + 2 /* tunnel count */;
        for tunnel in &self.tunnels {
            len += 4 /* tunnel type */ + 2 /* channel count */
                + tunnel.channel_ids.len() * 4;
        }
        len as u32
    }

    fn total_size(&self) -> usize {
        Header::size()
            + Self::PAD_SIZE
            + 4 /* length */
            + self.payload_length() as usize
    }

    fn name() -> &'static str {
        "DYNVC_SOFT_SYNC_REQUEST"
    }
}

impl Encode for SoftSyncRequestPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.total_size());
        self.header.encode(dst)?;
        dst.write_u8(0);
        dst.write_u32(self.payload_length());
        dst.write_u16(self.flags);
        dst.write_u16(self.tunnels.len() as u16);
        for tunnel in &self.tunnels {
            dst.write_u32(tunnel.tunnel_type);
            dst.write_u16(tunnel.channel_ids.len() as u16);
            for &channel_id in &tunnel.channel_ids {
                dst.write_u32(channel_id);
            }
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::name()
    }

    fn size(&self) -> usize {
        self.total_size()
    }
}

/// 2.2.5.2 Soft-Sync Response PDU (DYNVC_SOFT_SYNC_RESPONSE)
#[derive(Debug, Clone, PartialEq)]
pub struct SoftSyncResponsePdu {
    header: Header,
    pub tunnels_to_switch: Vec<SoftSyncTunnelType>,
}

impl SoftSyncResponsePdu {
    fn new(tunnels_to_switch: Vec<SoftSyncTunnelType>) -> Self {
        Self {
            header: Header::new(0, 0, Cmd::SoftSyncResponse),
            tunnels_to_switch,
        }
    }

    /// Create a Soft-Sync response from a request, including all unique tunnel types from the request.
    ///
    /// Note: This method does not validate that the tunnels are actually available.
    /// Use `from_request_with_available` for spec-compliant behavior.
    pub fn from_request(request: &SoftSyncRequestPdu) -> Self {
        let mut seen = BTreeSet::new();
        let mut tunnels = Vec::new();
        for tunnel in &request.tunnels {
            if seen.insert(tunnel.tunnel_type) {
                tunnels.push(tunnel.tunnel_type);
            }
        }
        Self::new(tunnels)
    }

    /// Create a Soft-Sync response from a request, including only tunnel types that are both
    /// requested by the server AND available to the client.
    ///
    /// Per MS-RDPEDYC §3.2.5.3.2: "Each multitransport tunnel that the client manager will use
    /// to send dynamic virtual channel data MUST be specified in the TunnelsToSwitch field."
    ///
    /// This ensures the client only confirms tunnels it has successfully established.
    pub fn from_request_with_available(
        request: &SoftSyncRequestPdu,
        available_tunnels: &BTreeSet<SoftSyncTunnelType>,
    ) -> Self {
        let mut seen = BTreeSet::new();
        let mut tunnels = Vec::new();

        for tunnel in &request.tunnels {
            // Only include tunnels that are:
            // 1. Requested by the server (in the request)
            // 2. Available on the client (successfully established)
            // 3. Not already added (unique)
            if available_tunnels.contains(&tunnel.tunnel_type) && seen.insert(tunnel.tunnel_type) {
                tunnels.push(tunnel.tunnel_type);
            }
        }

        Self::new(tunnels)
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: 1 + 4);
        let _pad = src.read_u8();
        let tunnel_count = src.read_u32();

        let mut tunnels_to_switch = Vec::with_capacity(tunnel_count as usize);
        for _ in 0..tunnel_count {
            ensure_size!(in: src, size: 4);
            tunnels_to_switch.push(src.read_u32());
        }

        Ok(Self {
            header,
            tunnels_to_switch,
        })
    }

    fn total_size(&self) -> usize {
        Header::size() + 1 + 4 + self.tunnels_to_switch.len() * 4
    }

    fn name() -> &'static str {
        "DYNVC_SOFT_SYNC_RESPONSE"
    }
}

impl Encode for SoftSyncResponsePdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.total_size());
        self.header.encode(dst)?;
        dst.write_u8(0);
        dst.write_u32(self.tunnels_to_switch.len() as u32);
        for tunnel in &self.tunnels_to_switch {
            dst.write_u32(*tunnel);
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::name()
    }

    fn size(&self) -> usize {
        self.total_size()
    }
}

/// 2.2.2.1 DVC Create Request PDU (DYNVC_CREATE_REQ)
///
/// [2.2.2.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpedyc/4448ba4d-9a72-429f-8b65-6f4ec44f2985
#[derive(Debug, PartialEq)]
pub struct CreateRequestPdu {
    header: Header,
    pub channel_id: DynamicChannelId,
    pub channel_name: String,
}

impl CreateRequestPdu {
    pub fn new(channel_id: DynamicChannelId, channel_name: String) -> Self {
        Self {
            header: Header::new(channel_id, 0, Cmd::Create),
            channel_id,
            channel_name,
        }
    }

    fn decode(header: Header, src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: Self::headerless_fixed_part_size(&header));
        let channel_id = header.cb_id.decode_val(src)?;
        let channel_name = read_string_from_cursor(src, CharacterSet::Ansi, true)?;
        Ok(Self {
            header,
            channel_id,
            channel_name,
        })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        self.header.encode(dst)?;
        self.header.cb_id.encode_val(self.channel_id, dst)?;
        write_string_to_cursor(dst, &self.channel_name, CharacterSet::Ansi, true)?;
        Ok(())
    }

    fn name() -> &'static str {
        "DYNVC_CREATE_REQ"
    }

    fn headerless_fixed_part_size(header: &Header) -> usize {
        header.cb_id.size_of_val() // ChannelId
    }

    fn size(&self) -> usize {
        strict_sum(&[
            Header::size(),
            Self::headerless_fixed_part_size(&self.header), // ChannelId
            encoded_str_len(&self.channel_name, CharacterSet::Ansi, true), // ChannelName + Null terminator
        ])
    }
}
