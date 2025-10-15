use bitflags::bitflags;
use ironrdp_core::ReadCursor;

use crate::error::{UdpError, UdpErrorExt as _, UdpResult};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SynDataExFlags: u16 {
        const VERSION_INFO_VALID = 0x0001;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SynDataEx {
    pub flags: SynDataExFlags,
    pub udp_version: Option<UdpProtocolVersion>,
    pub cookie_hash: Option<[u8; 32]>,
}

impl SynDataEx {
    pub const NAME: &'static str = "RDPUDP_SYNDATAEX_PAYLOAD";
    pub const BASE_SIZE: usize = 4;

    pub fn decode(cursor: &mut ReadCursor<'_>) -> UdpResult<Self> {
        use ironrdp_core::NotEnoughBytesError;

        let flags_raw = cursor
            .try_read_u16_be()
            .map_err(|e: NotEnoughBytesError| UdpError::decode(Self::NAME, e))?;
        let flags = SynDataExFlags::from_bits(flags_raw).ok_or_else(|| {
            UdpError::invalid_field(Self::NAME, "uSynExFlags", "unknown bits set in SYNEX flags")
        })?;

        let udp_version_raw = cursor
            .try_read_u16_be()
            .map_err(|e| UdpError::decode(Self::NAME, e))?;

        let udp_version = if flags.contains(SynDataExFlags::VERSION_INFO_VALID) {
            Some(UdpProtocolVersion::from_raw(udp_version_raw))
        } else {
            None
        };

        let cookie_hash = if cursor.len() >= 32 {
            Some(cursor.read_array::<32>())
        } else {
            None
        };

        Ok(Self {
            flags,
            udp_version,
            cookie_hash,
        })
    }

    pub fn encode_into(&self, output: &mut Vec<u8>) {
        // SynDataEx uses BIG-ENDIAN byte order (matches decode which uses read_u16_be)
        output.extend_from_slice(&self.flags.bits().to_be_bytes());
        let version = self.udp_version.map(|v| v.raw_value()).unwrap_or_default();
        output.extend_from_slice(&version.to_be_bytes());
        if let Some(cookie_hash) = self.cookie_hash {
            output.extend_from_slice(&cookie_hash);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UdpProtocolVersion(u16);

impl UdpProtocolVersion {
    pub const V1: Self = Self(0x0001);
    pub const V2: Self = Self(0x0002);
    pub const V3: Self = Self(0x0101);

    pub const fn raw_value(self) -> u16 {
        self.0
    }

    pub fn from_raw(raw: u16) -> Self {
        Self(raw)
    }
}
