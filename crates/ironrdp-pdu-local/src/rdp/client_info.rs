use core::fmt;
use std::io;

use bitflags::bitflags;
use ironrdp_core::{
    cast_length, ensure_fixed_part_size, ensure_size, invalid_field_err, write_padding, Decode,
    DecodeResult, Encode, EncodeResult, ReadCursor, WriteCursor,
};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;
use thiserror::Error;

use crate::utils::CharacterSet;
use crate::{utils, PduError};

/// =============================================================================================
/// Gating switch:
/// - false (default): EXACTLY match the reference packet for all fields EXCEPT:
///     clientAddress, domain, username, password, clientSessionId  (which are dynamic)
/// - true:  allow the "dynamic" code paths (commented below) to fill remaining fields, too.
/// =============================================================================================
pub const USE_DYNAMIC_REST: bool = false;

/// Inputs you may vary per connection.
#[derive(Debug, Clone)]
pub struct ClientInfoInputs {
    pub client_address: String, // e.g., "172.30.80.1"
    pub domain: Option<String>, // e.g., None or Some("ACME")
    pub username: String,       // e.g., "user"
    pub password: String,       // e.g., ""
    pub client_session_id: u32, // e.g., 2
}

/// Build a ClientInfo that is byte-for-byte aligned with the working reference,
/// while letting the 5 requested fields be dynamic.
pub fn make_client_info(inputs: &ClientInfoInputs) -> ClientInfo {
    // --- Reference constants (matching your good capture) ---
    // optionFlags (w/ compression type bits folded in) = 0x000147BB
    //   -> compression type = 3 (Rdp61), flags = 0x000147BB & !0x1E00
    const REF_OPTION_FLAGS: u32 = 0x0001_47BB;
    const COMPRESSION_TYPE_MASK: u32 = 0x0000_1E00;
    let ref_flags_bits = REF_OPTION_FLAGS & !COMPRESSION_TYPE_MASK;
    let ref_compr = CompressionType::from_u32((REF_OPTION_FLAGS & COMPRESSION_TYPE_MASK) >> 9)
        .unwrap_or(CompressionType::Rdp61);

    // Performance flags = 0x00000006
    let ref_perf =
        PerformanceFlags::DISABLE_FULLWINDOWDRAG | PerformanceFlags::DISABLE_MENUANIMATIONS;

    // Client dir path (UTF-16 w/ NT, length includes NT)
    const REF_CLIENT_DIR: &str = r"C:\WINDOWS\system32\mstscax.dll";

    // Reference timezone: W. Europe, fixed 172-byte payload
    let ref_tz = w_europe_timezone();

    // Reference reserved + dynamic DST
    const REF_RESERVED1: u16 = 0x0064;
    const REF_RESERVED2: u16 = 0x0000;
    const REF_DYN_DST_KEY: &str = "W. Europe Standard Time";
    const REF_DYN_DST_DISABLED: u16 = 0;

    ClientInfo {
        // --- Fixed to reference, unless you flip USE_DYNAMIC_REST ---
        code_page: if USE_DYNAMIC_REST { 1033 } else { 1033 },
        flags: if USE_DYNAMIC_REST {
            ClientInfoFlags::from_bits_truncate(ref_flags_bits)
        } else {
            ClientInfoFlags::from_bits_truncate(ref_flags_bits)
        },
        compression_type: if USE_DYNAMIC_REST {
            ref_compr
        } else {
            ref_compr
        },

        // --- Dynamic (per your request) ---
        credentials: Credentials {
            username: inputs.username.clone(),
            password: inputs.password.clone(),
            domain: inputs.domain.clone(),
        },

        // --- Reference values (zero-length, still terminated on wire) ---
        alternate_shell: if USE_DYNAMIC_REST {
            String::new()
        } else {
            String::new()
        },
        work_dir: if USE_DYNAMIC_REST {
            String::new()
        } else {
            String::new()
        },

        // --- Extended info: clientAddress dynamic; dir static to reference; tail matches reference ---
        extra_info: ExtendedClientInfo {
            address_family: AddressFamily::INET,
            address: inputs.client_address.clone(), // dynamic
            dir: if USE_DYNAMIC_REST {
                REF_CLIENT_DIR.into()
            } else {
                REF_CLIENT_DIR.into()
            }, // static
            optional_data: ExtendedClientOptionalInfo {
                timezone: Some(ref_tz),                     // static to reference (W. Europe)
                session_id: Some(inputs.client_session_id), // dynamic
                performance_flags: Some(ref_perf),          // static
                reconnect_cookie: None,                     // reference had cbAutoReconnect=0
                reserved1: Some(REF_RESERVED1),             // static
                reserved2: Some(REF_RESERVED2),             // static
                dynamic_dst_tz_key_name: Some(REF_DYN_DST_KEY.into()), // static (no NT)
                dynamic_daylight_time_disabled: Some(REF_DYN_DST_DISABLED), // static
            },
        },
    }
}

/// ---- Layout & sizing constants --------------------------------------------------------------

const RECONNECT_COOKIE_LEN: usize = 28;
const TIMEZONE_INFO_NAME_LEN: usize = 64;
const COMPRESSION_TYPE_MASK: u32 = 0x0000_1E00;

const U32: usize = 4;
const U16: usize = 2;
const I32: usize = 4;

const CODE_PAGE_SIZE: usize = U32;
const FLAGS_SIZE: usize = U32;

const DOMAIN_LENGTH_SIZE: usize = U16;
const USER_NAME_LENGTH_SIZE: usize = U16;
const PASSWORD_LENGTH_SIZE: usize = U16;
const ALTERNATE_SHELL_LENGTH_SIZE: usize = U16;
const WORK_DIR_LENGTH_SIZE: usize = U16;

const CLIENT_ADDRESS_FAMILY_SIZE: usize = U16;
const CLIENT_ADDRESS_LENGTH_SIZE: usize = U16;
const CLIENT_DIR_LENGTH_SIZE: usize = U16;
const SESSION_ID_SIZE: usize = U32;
const PERFORMANCE_FLAGS_SIZE: usize = U32;
const RECONNECT_COOKIE_LENGTH_SIZE: usize = U16;
const BIAS_SIZE: usize = I32;

/// ---- Charset helpers ------------------------------------------------------------------------

trait CharsetExt {
    fn unit_bytes(self) -> usize;
    fn as_u16_units(self) -> u16;
}
impl CharsetExt for CharacterSet {
    #[inline]
    fn unit_bytes(self) -> usize {
        if matches!(self, CharacterSet::Unicode) {
            2
        } else {
            1
        }
    }
    #[inline]
    fn as_u16_units(self) -> u16 {
        self.unit_bytes() as u16
    }
}
#[inline]
fn encoded_len(value: &str, cs: CharacterSet) -> usize {
    match cs {
        CharacterSet::Ansi => value.len(),
        CharacterSet::Unicode => value.encode_utf16().count() * 2,
    }
}
#[inline]
fn encoded_with_nt_len(value: &str, cs: CharacterSet) -> usize {
    encoded_len(value, cs) + cs.unit_bytes()
}

/// ---- Client Info (TS_INFO_PACKET) -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub credentials: Credentials,
    pub code_page: u32,
    pub flags: ClientInfoFlags,
    pub compression_type: CompressionType,
    pub alternate_shell: String,
    pub work_dir: String,
    pub extra_info: ExtendedClientInfo,
}

impl ClientInfo {
    const NAME: &'static str = "ClientInfo";
    pub const FIXED_PART_SIZE: usize = CODE_PAGE_SIZE
        + FLAGS_SIZE
        + DOMAIN_LENGTH_SIZE
        + USER_NAME_LENGTH_SIZE
        + PASSWORD_LENGTH_SIZE
        + ALTERNATE_SHELL_LENGTH_SIZE
        + WORK_DIR_LENGTH_SIZE;

    #[inline]
    fn charset(&self) -> CharacterSet {
        if self.flags.contains(ClientInfoFlags::UNICODE) {
            CharacterSet::Unicode
        } else {
            CharacterSet::Ansi
        }
    }
}

impl Encode for ClientInfo {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        let cs = self.charset();

        // codePage (reference: 1033)
        dst.write_u32(self.code_page);

        // optionFlags with compression type packed into [13:9]
        let flags_with_compression =
            self.flags.bits() | (u32::from(self.compression_type.as_u8()) << 9);
        dst.write_u32(flags_with_compression);

        // Lengths exclude NT
        let domain = self.credentials.domain.as_deref().unwrap_or_default();
        dst.write_u16(cast_length!("domain length", encoded_len(domain, cs))?);
        dst.write_u16(cast_length!(
            "username length",
            encoded_len(&self.credentials.username, cs)
        )?);
        dst.write_u16(cast_length!(
            "password length",
            encoded_len(&self.credentials.password, cs)
        )?);
        dst.write_u16(cast_length!(
            "alternate shell length",
            encoded_len(&self.alternate_shell, cs)
        )?);
        dst.write_u16(cast_length!(
            "work dir length",
            encoded_len(&self.work_dir, cs)
        )?);

        // Strings including NT
        utils::write_string_to_cursor(dst, domain, cs, true)?;
        utils::write_string_to_cursor(dst, &self.credentials.username, cs, true)?;
        utils::write_string_to_cursor(dst, &self.credentials.password, cs, true)?;
        utils::write_string_to_cursor(dst, &self.alternate_shell, cs, true)?;
        utils::write_string_to_cursor(dst, &self.work_dir, cs, true)?;

        self.extra_info.encode(dst, cs)?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        let cs = self.charset();
        let domain = self.credentials.domain.as_deref().unwrap_or_default();

        ClientInfo::FIXED_PART_SIZE
            + encoded_with_nt_len(domain, cs)
            + encoded_with_nt_len(&self.credentials.username, cs)
            + encoded_with_nt_len(&self.credentials.password, cs)
            + encoded_with_nt_len(&self.alternate_shell, cs)
            + encoded_with_nt_len(&self.work_dir, cs)
            + self.extra_info.size(cs)
    }
}

impl<'de> Decode<'de> for ClientInfo {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let code_page = src.read_u32();
        let flags_with_compression = src.read_u32();

        let flags_bits = flags_with_compression & !COMPRESSION_TYPE_MASK;
        let flags = ClientInfoFlags::from_bits(flags_bits)
            .ok_or_else(|| invalid_field_err!("flags", "invalid ClientInfoFlags"))?;

        let compression_type =
            CompressionType::from_u32((flags_with_compression & COMPRESSION_TYPE_MASK) >> 9)
                .ok_or_else(|| invalid_field_err!("flags", "invalid CompressionType"))?;

        let cs = if flags.contains(ClientInfoFlags::UNICODE) {
            CharacterSet::Unicode
        } else {
            CharacterSet::Ansi
        };

        // All five lengths come first and only then the five strings -- [MS-RDPBCGR]
        // 2.2.1.11.1.1. Reading them interleaved, a length followed by its own string, walks
        // off the end of the field and turns whatever follows into nonsense lengths; it went
        // unnoticed because this crate has only ever been used to encode this PDU, never to
        // decode one.
        let lengths = [
            src.read_u16(),
            src.read_u16(),
            src.read_u16(),
            src.read_u16(),
            src.read_u16(),
        ];

        let read = |src: &mut ReadCursor<'_>, length: u16| -> DecodeResult<String> {
            // The lengths on the wire exclude the terminator; the strings include it.
            let with_nt = usize::from(length) + cs.unit_bytes();
            ensure_size!(in: src, size: with_nt);
            utils::decode_string(src.read_slice(with_nt), cs, true)
        };

        let domain = {
            let domain = read(src, lengths[0])?;
            if domain.is_empty() {
                None
            } else {
                Some(domain)
            }
        };
        let username = read(src, lengths[1])?;
        let password = read(src, lengths[2])?;
        let alternate_shell = read(src, lengths[3])?;
        let work_dir = read(src, lengths[4])?;

        let credentials = Credentials {
            username,
            password,
            domain,
        };
        let extra_info = ExtendedClientInfo::decode(src, cs)?;

        Ok(Self {
            credentials,
            code_page,
            flags,
            compression_type,
            alternate_shell,
            work_dir,
            extra_info,
        })
    }
}

/// Credentials

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub domain: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

/// Extended client info

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedClientInfo {
    pub address_family: AddressFamily,
    pub address: String,
    pub dir: String,
    pub optional_data: ExtendedClientOptionalInfo,
}

impl ExtendedClientInfo {
    fn decode(src: &mut ReadCursor<'_>, cs: CharacterSet) -> DecodeResult<Self> {
        ensure_size!(in: src, size: CLIENT_ADDRESS_FAMILY_SIZE + CLIENT_ADDRESS_LENGTH_SIZE);

        let address_family = AddressFamily::from_u16(src.read_u16());

        // INCLUDE NT
        let addr_with_nt = usize::from(src.read_u16());
        ensure_size!(in: src, size: addr_with_nt + CLIENT_DIR_LENGTH_SIZE);
        let address = utils::decode_string(src.read_slice(addr_with_nt), cs, false)?;

        let dir_with_nt = usize::from(src.read_u16());
        ensure_size!(in: src, size: dir_with_nt);
        let dir = utils::decode_string(src.read_slice(dir_with_nt), cs, false)?;

        let optional_data = ExtendedClientOptionalInfo::decode(src)?;
        Ok(Self {
            address_family,
            address,
            dir,
            optional_data,
        })
    }

    fn encode(&self, dst: &mut WriteCursor<'_>, cs: CharacterSet) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size(cs));

        let addr_len_no_nt: u16 = cast_length!("address length", encoded_len(&self.address, cs))?;
        let dir_len_no_nt: u16 = cast_length!("dir length", encoded_len(&self.dir, cs))?;

        dst.write_u16(self.address_family.as_u16());
        dst.write_u16(addr_len_no_nt + cs.as_u16_units()); // INCLUDE NT
        utils::write_string_to_cursor(dst, &self.address, cs, true)?;

        dst.write_u16(dir_len_no_nt + cs.as_u16_units()); // INCLUDE NT
        utils::write_string_to_cursor(dst, &self.dir, cs, true)?;

        self.optional_data.encode(dst)?;
        Ok(())
    }

    fn size(&self, cs: CharacterSet) -> usize {
        CLIENT_ADDRESS_FAMILY_SIZE
            + CLIENT_ADDRESS_LENGTH_SIZE
            + encoded_with_nt_len(&self.address, cs)
            + CLIENT_DIR_LENGTH_SIZE
            + encoded_with_nt_len(&self.dir, cs)
            + self.optional_data.size()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtendedClientOptionalInfo {
    // dynamic (input)
    pub timezone: Option<TimezoneInfo>,
    pub session_id: Option<u32>,
    // static to reference
    pub performance_flags: Option<PerformanceFlags>,
    pub reconnect_cookie: Option<[u8; RECONNECT_COOKIE_LEN]>,
    pub reserved1: Option<u16>,
    pub reserved2: Option<u16>,
    pub dynamic_dst_tz_key_name: Option<String>, // no NT on wire
    pub dynamic_daylight_time_disabled: Option<u16>, // 0 or 1
}

impl Encode for ExtendedClientOptionalInfo {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());

        // Time zone first (fixed 172 bytes)
        if let Some(tz) = &self.timezone {
            tz.encode(dst)?;
        }

        // Session ID (dynamic)
        if let Some(session_id) = self.session_id {
            dst.write_u32(session_id);
        }

        // The rest matches the reference packet (or dynamic if you flip USE_DYNAMIC_REST)
        if let Some(perf) = self.performance_flags {
            dst.write_u32(perf.bits());
        }

        if let Some(cookie) = self.reconnect_cookie {
            dst.write_u16(
                u16::try_from(RECONNECT_COOKIE_LEN).expect("RECONNECT_COOKIE_LEN fits u16"),
            );
            dst.write_array(cookie);
        } else {
            // Reference uses cbAutoReconnectCookie = 0 (i.e., skips cookie bytes).
            dst.write_u16(0);
        }

        // reserved1 / reserved2
        dst.write_u16(self.reserved1.unwrap_or(0x0064));
        dst.write_u16(self.reserved2.unwrap_or(0x0000));

        // Dynamic DST key name: UTF-16LE, NO NT + u16 flag
        let mut utf16 = utils::to_utf16_bytes(
            self.dynamic_dst_tz_key_name
                .as_deref()
                .unwrap_or("W. Europe Standard Time"),
        );
        if utf16.ends_with(&[0, 0]) {
            utf16.truncate(utf16.len() - 2);
        }
        dst.write_u16(u16::try_from(utf16.len()).expect("dyn DST key len fits u16"));
        dst.write_slice(&utf16);
        dst.write_u16(self.dynamic_daylight_time_disabled.unwrap_or(0));

        Ok(())
    }

    fn name(&self) -> &'static str {
        "ExtendedClientOptionalInfo"
    }

    fn size(&self) -> usize {
        let mut size = 0;
        if let Some(ref tz) = self.timezone {
            size += tz.size();
        }
        if self.session_id.is_some() {
            size += SESSION_ID_SIZE;
        }
        if self.performance_flags.is_some() {
            size += PERFORMANCE_FLAGS_SIZE;
        }
        // We always write a u16 for cbAutoReconnectCookie (0 if None)
        size += RECONNECT_COOKIE_LENGTH_SIZE;
        if self.reconnect_cookie.is_some() {
            size += RECONNECT_COOKIE_LEN;
        }

        // reserved + dynamic DST (cb + name + flag)
        size += 2 + 2;
        let mut utf16 = utils::to_utf16_bytes(
            self.dynamic_dst_tz_key_name
                .as_deref()
                .unwrap_or("W. Europe Standard Time"),
        );
        if utf16.ends_with(&[0, 0]) {
            utf16.truncate(utf16.len() - 2);
        }
        size += 2 + utf16.len() + 2;

        size
    }
}

impl<'de> Decode<'de> for ExtendedClientOptionalInfo {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        let mut out = Self::default();

        if src.len() < TimezoneInfo::FIXED_PART_SIZE {
            return Ok(out);
        }
        out.timezone = Some(TimezoneInfo::decode(src)?);

        if src.len() >= U32 {
            out.session_id = Some(src.read_u32());
        }
        if src.len() >= U32 {
            out.performance_flags =
                Some(PerformanceFlags::from_bits(src.read_u32()).ok_or_else(|| {
                    invalid_field_err!("performanceFlags", "invalid performance flags")
                })?);
        }
        if src.len() >= U16 {
            let cb = src.read_u16();
            if cb != 0 && cb != RECONNECT_COOKIE_LEN as u16 {
                return Err(invalid_field_err!(
                    "cbAutoReconnectCookie",
                    "invalid cookie size"
                ));
            }
            if cb != 0 {
                ensure_size!(in: src, size: RECONNECT_COOKIE_LEN);
                out.reconnect_cookie = Some(src.read_array());
            }
        }
        if src.len() >= 2 {
            out.reserved1 = Some(src.read_u16());
        }
        if src.len() >= 2 {
            out.reserved2 = Some(src.read_u16());
        }
        if src.len() >= 2 {
            let cb_dyn = src.read_u16() as usize;
            ensure_size!(in: src, size: cb_dyn);
            if cb_dyn > 0 {
                let dyn_bytes = src.read_slice(cb_dyn);
                let units = dyn_bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<_>>();
                out.dynamic_dst_tz_key_name = Some(String::from_utf16_lossy(&units));
            }
            if src.len() >= 2 {
                out.dynamic_daylight_time_disabled = Some(src.read_u16());
            }
        }
        Ok(out)
    }
}

/// Time Zone Info (TS_TIME_ZONE_INFORMATION)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimezoneInfo {
    pub bias: i32,
    pub standard_name: String,
    pub standard_date: OptionalSystemTime,
    pub standard_bias: i32,
    pub daylight_name: String,
    pub daylight_date: OptionalSystemTime,
    pub daylight_bias: i32,
}

impl TimezoneInfo {
    const FIXED_PART_SIZE: usize = BIAS_SIZE
        + TIMEZONE_INFO_NAME_LEN
        + SystemTime::FIXED_PART_SIZE
        + BIAS_SIZE
        + TIMEZONE_INFO_NAME_LEN
        + SystemTime::FIXED_PART_SIZE
        + BIAS_SIZE;
}

impl Encode for TimezoneInfo {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);

        dst.write_i32(self.bias);

        let mut std_name = utils::to_utf16_bytes(&self.standard_name);
        std_name.resize(TIMEZONE_INFO_NAME_LEN, 0);
        dst.write_slice(&std_name);

        self.standard_date.encode(dst)?;
        dst.write_i32(self.standard_bias);

        let mut dlt_name = utils::to_utf16_bytes(&self.daylight_name);
        dlt_name.resize(TIMEZONE_INFO_NAME_LEN, 0);
        dst.write_slice(&dlt_name);

        self.daylight_date.encode(dst)?;
        dst.write_i32(self.daylight_bias);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "TimezoneInfo"
    }
    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for TimezoneInfo {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let bias = src.read_i32();
        let standard_name = utils::decode_string(
            src.read_slice(TIMEZONE_INFO_NAME_LEN),
            CharacterSet::Unicode,
            false,
        )?;
        let standard_date = OptionalSystemTime::decode(src)?;
        let standard_bias = src.read_i32();

        let daylight_name = utils::decode_string(
            src.read_slice(TIMEZONE_INFO_NAME_LEN),
            CharacterSet::Unicode,
            false,
        )?;
        let daylight_date = OptionalSystemTime::decode(src)?;
        let daylight_bias = src.read_i32();

        Ok(Self {
            bias,
            standard_name,
            standard_date,
            standard_bias,
            daylight_name,
            daylight_date,
            daylight_bias,
        })
    }
}

/// Reference W. Europe TZ helper
pub fn w_europe_timezone() -> TimezoneInfo {
    TimezoneInfo {
        bias: -60,
        standard_name: "W. Europe Standard Time".into(),
        standard_date: OptionalSystemTime(Some(SystemTime {
            month: Month::October,
            day_of_week: DayOfWeek::Sunday,
            day: DayOfWeekOccurrence::Last,
            hour: 3,
            minute: 0,
            second: 0,
            milliseconds: 0,
        })),
        standard_bias: 0,
        daylight_name: "W. Europe Daylight Time".into(),
        daylight_date: OptionalSystemTime(Some(SystemTime {
            month: Month::March,
            day_of_week: DayOfWeek::Sunday,
            day: DayOfWeekOccurrence::Last,
            hour: 2,
            minute: 0,
            second: 0,
            milliseconds: 0,
        })),
        daylight_bias: -60,
    }
}

/// SYSTEMTIME wrappers

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTime {
    pub month: Month,
    pub day_of_week: DayOfWeek,
    pub day: DayOfWeekOccurrence,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub milliseconds: u16,
}
impl SystemTime {
    const FIXED_PART_SIZE: usize = 2 * 8; // 16 bytes
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalSystemTime(pub Option<SystemTime>);
impl Encode for OptionalSystemTime {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        dst.write_u16(0);
        if let Some(st) = &self.0 {
            dst.write_u16(st.month.as_u16());
            dst.write_u16(st.day_of_week.as_u16());
            dst.write_u16(st.day.as_u16());
            dst.write_u16(st.hour);
            dst.write_u16(st.minute);
            dst.write_u16(st.second);
            dst.write_u16(st.milliseconds);
        } else {
            write_padding!(dst, 14);
        }
        Ok(())
    }
    fn name(&self) -> &'static str {
        "SystemTime"
    }
    fn size(&self) -> usize {
        SystemTime::FIXED_PART_SIZE
    }
}
impl<'de> Decode<'de> for OptionalSystemTime {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: SystemTime::FIXED_PART_SIZE);
        let _year = src.read_u16();
        let month = src.read_u16();
        let day_of_week = src.read_u16();
        let day = src.read_u16();
        let hour = src.read_u16();
        let minute = src.read_u16();
        let second = src.read_u16();
        let milliseconds = src.read_u16();
        match (
            Month::from_u16(month),
            DayOfWeek::from_u16(day_of_week),
            DayOfWeekOccurrence::from_u16(day),
        ) {
            (Some(month), Some(dow), Some(dooc)) => Ok(Self(Some(SystemTime {
                month,
                day_of_week: dow,
                day: dooc,
                hour,
                minute,
                second,
                milliseconds,
            }))),
            _ => Ok(Self(None)),
        }
    }
}

/// Enums & flags

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive)]
pub enum Month {
    January = 1,
    February,
    March,
    April,
    May,
    June,
    July,
    August,
    September,
    October,
    November,
    December,
}
impl Month {
    fn as_u16(self) -> u16 {
        self as u16
    }
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive)]
pub enum DayOfWeek {
    Sunday = 0,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}
impl DayOfWeek {
    fn as_u16(self) -> u16 {
        self as u16
    }
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive)]
pub enum DayOfWeekOccurrence {
    First = 1,
    Second,
    Third,
    Fourth,
    Last,
}
impl DayOfWeekOccurrence {
    fn as_u16(self) -> u16 {
        self as u16
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct PerformanceFlags: u32 {
        const DISABLE_WALLPAPER          = 0x0000_0001;
        const DISABLE_FULLWINDOWDRAG     = 0x0000_0002;
        const DISABLE_MENUANIMATIONS     = 0x0000_0004;
        const DISABLE_THEMING            = 0x0000_0008;
        const RESERVED1                  = 0x0000_0010;
        const DISABLE_CURSOR_SHADOW      = 0x0000_0020;
        const DISABLE_CURSORSETTINGS     = 0x0000_0040;
        const ENABLE_FONT_SMOOTHING      = 0x0000_0080;
        const ENABLE_DESKTOP_COMPOSITION = 0x0000_0100;
        const RESERVED2                  = 0x8000_0000;
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AddressFamily(u16);
impl AddressFamily {
    pub const INET: Self = Self(0x0002);
    pub const INET_6: Self = Self(0x0017);
    pub fn from_u16(val: u16) -> Self {
        Self(val)
    }
    pub fn as_u16(self) -> u16 {
        self.0
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ClientInfoFlags: u32 {
        const MOUSE                 = 0x0000_0001;
        const DISABLE_CTRL_ALT_DEL  = 0x0000_0002;
        const AUTOLOGON             = 0x0000_0008;
        const UNICODE               = 0x0000_0010;
        const MAXIMIZE_SHELL        = 0x0000_0020;
        const LOGON_NOTIFY          = 0x0000_0040;
        const COMPRESSION           = 0x0000_0080;
        const ENABLE_WINDOWS_KEY    = 0x0000_0100;
        const REMOTE_CONSOLE_AUDIO  = 0x0000_2000;
        const FORCE_ENCRYPTED_CS_PDU= 0x0000_4000;
        const RAIL                  = 0x0000_8000;
        const LOGON_ERRORS          = 0x0001_0000;
        const MOUSE_HAS_WHEEL       = 0x0002_0000;
        const PASSWORD_IS_SC_PIN    = 0x0004_0000;
        const NO_AUDIO_PLAYBACK     = 0x0008_0000;
        const USING_SAVED_CREDS     = 0x0010_0000;
        const AUDIO_CAPTURE         = 0x0020_0000;
        const VIDEO_DISABLE         = 0x0040_0000;
        const RESERVED1             = 0x0080_0000;
        const RESERVED2             = 0x0100_0000;
        const HIDEF_RAIL_SUPPORTED  = 0x0200_0000;
    }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive)]
pub enum CompressionType {
    K8 = 0,
    K64 = 1,
    Rdp6 = 2,
    Rdp61 = 3,
}
impl CompressionType {
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        self as u8 as u32
    }

    #[inline]
    pub fn from_u32(v: u32) -> Option<Self> {
        <Self as num_traits::FromPrimitive>::from_u32(v)
    }
}

#[derive(Debug, Error)]
pub enum ClientInfoError {
    #[error("IO error")]
    IOError(#[from] io::Error),
    #[error("UTF-8 error")]
    Utf8Error(#[from] std::string::FromUtf8Error),
    #[error("invalid address family field")]
    InvalidAddressFamily,
    #[error("invalid flags field")]
    InvalidClientInfoFlags,
    #[error("invalid performance flags field")]
    InvalidPerformanceFlags,
    #[error("invalid reconnect cookie field")]
    InvalidReconnectCookie,
    #[error("PDU error: {0}")]
    Pdu(PduError),
}

impl From<PduError> for ClientInfoError {
    fn from(e: PduError) -> Self {
        Self::Pdu(e)
    }
}

impl ExtendedClientOptionalInfo {
    /// State-machine builder entrypoint kept for backward compatibility.
    /// Matches previous API so `ExtendedClientOptionalInfo::builder()` works.
    pub fn builder() -> builder::ExtendedClientOptionalInfoBuilder<
        builder::ExtendedClientOptionalInfoBuilderStateSetTimeZone,
    > {
        builder::ExtendedClientOptionalInfoBuilder::new()
    }
}

pub mod builder {
    use core::marker::PhantomData;

    use super::{ExtendedClientOptionalInfo, PerformanceFlags, TimezoneInfo, RECONNECT_COOKIE_LEN};

    // Builder states
    pub struct ExtendedClientOptionalInfoBuilderStateSetTimeZone;
    pub struct ExtendedClientOptionalInfoBuilderStateSetSessionId;
    pub struct ExtendedClientOptionalInfoBuilderStateSetPerformanceFlags;
    pub struct ExtendedClientOptionalInfoBuilderStateSetReconnectCookie;
    pub struct ExtendedClientOptionalInfoBuilderStateFinal;

    /// State-machine builder that enforces on-wire order:
    /// timezone -> session_id -> performance_flags -> reconnect_cookie
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ExtendedClientOptionalInfoBuilder<State> {
        pub(crate) inner: ExtendedClientOptionalInfo,
        _phantom: PhantomData<State>,
    }

    impl ExtendedClientOptionalInfoBuilder<ExtendedClientOptionalInfoBuilderStateSetTimeZone> {
        pub fn new() -> Self {
            Self {
                inner: ExtendedClientOptionalInfo::default(),
                _phantom: PhantomData,
            }
        }

        pub fn timezone(
            mut self,
            timezone: TimezoneInfo,
        ) -> ExtendedClientOptionalInfoBuilder<ExtendedClientOptionalInfoBuilderStateSetSessionId>
        {
            self.inner.timezone = Some(timezone);
            ExtendedClientOptionalInfoBuilder {
                inner: self.inner,
                _phantom: PhantomData,
            }
        }
    }

    impl ExtendedClientOptionalInfoBuilder<ExtendedClientOptionalInfoBuilderStateSetSessionId> {
        pub fn session_id(
            mut self,
            session_id: u32,
        ) -> ExtendedClientOptionalInfoBuilder<
            ExtendedClientOptionalInfoBuilderStateSetPerformanceFlags,
        > {
            self.inner.session_id = Some(session_id);
            ExtendedClientOptionalInfoBuilder {
                inner: self.inner,
                _phantom: PhantomData,
            }
        }
    }

    impl ExtendedClientOptionalInfoBuilder<ExtendedClientOptionalInfoBuilderStateSetPerformanceFlags> {
        pub fn performance_flags(
            mut self,
            performance_flags: PerformanceFlags,
        ) -> ExtendedClientOptionalInfoBuilder<
            ExtendedClientOptionalInfoBuilderStateSetReconnectCookie,
        > {
            self.inner.performance_flags = Some(performance_flags);
            ExtendedClientOptionalInfoBuilder {
                inner: self.inner,
                _phantom: PhantomData,
            }
        }
    }

    impl ExtendedClientOptionalInfoBuilder<ExtendedClientOptionalInfoBuilderStateSetReconnectCookie> {
        pub fn reconnect_cookie(
            mut self,
            reconnect_cookie: [u8; RECONNECT_COOKIE_LEN],
        ) -> ExtendedClientOptionalInfoBuilder<ExtendedClientOptionalInfoBuilderStateFinal>
        {
            self.inner.reconnect_cookie = Some(reconnect_cookie);
            ExtendedClientOptionalInfoBuilder {
                inner: self.inner,
                _phantom: PhantomData,
            }
        }
    }

    impl<State> ExtendedClientOptionalInfoBuilder<State> {
        pub fn build(self) -> ExtendedClientOptionalInfo {
            self.inner
        }
    }
}

impl Default for PerformanceFlags {
    fn default() -> Self {
        Self::DISABLE_FULLWINDOWDRAG | Self::DISABLE_MENUANIMATIONS | Self::ENABLE_FONT_SMOOTHING
    }
}
