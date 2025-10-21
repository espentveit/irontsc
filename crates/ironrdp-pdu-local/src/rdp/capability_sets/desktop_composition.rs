//! Desktop Composition Capability Set
//!
//! [MS-RDPBCGR] 2.2.1.13.1.9 - Desktop Composition Capability Set

use ironrdp_core::{
    ensure_fixed_part_size, Decode, DecodeResult, Encode, EncodeResult, ReadCursor, WriteCursor,
};

/// Desktop Composition Capability Set (TS_COMPDESK_CAPABILITYSET)
///
/// [MS-RDPBCGR] 2.2.1.13.1.9
///
/// This capability is used to advertise support for the Desktop Composition
/// Virtual Channel Extension specified in [MS-RDPEDC].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopComposition {
    /// Composition support flags
    pub comp_desk_supported: u16,
}

impl DesktopComposition {
    const NAME: &'static str = "TS_COMPDESK_CAPABILITYSET";
    const FIXED_PART_SIZE: usize = 2; // compDeskSupported field

    /// Create a new Desktop Composition capability set
    ///
    /// Per spec, compDeskSupported is a 16-bit field, but the spec doesn't
    /// define specific bit flags. Based on MS-RDPEDC, a non-zero value
    /// indicates support.
    pub fn new(comp_desk_supported: u16) -> Self {
        Self {
            comp_desk_supported,
        }
    }

    /// Create a capability set indicating Desktop Composition is supported
    pub fn supported() -> Self {
        Self {
            comp_desk_supported: 1,
        }
    }

    /// Create a capability set indicating Desktop Composition is NOT supported
    pub fn not_supported() -> Self {
        Self {
            comp_desk_supported: 0,
        }
    }

    /// Check if Desktop Composition is supported
    pub fn is_supported(&self) -> bool {
        self.comp_desk_supported != 0
    }
}

impl Encode for DesktopComposition {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u16(self.comp_desk_supported);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for DesktopComposition {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let comp_desk_supported = src.read_u16();
        Ok(Self {
            comp_desk_supported,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_composition_supported() {
        let cap = DesktopComposition::supported();
        assert!(cap.is_supported());
        assert_eq!(cap.comp_desk_supported, 1);
    }

    #[test]
    fn test_desktop_composition_not_supported() {
        let cap = DesktopComposition::not_supported();
        assert!(!cap.is_supported());
        assert_eq!(cap.comp_desk_supported, 0);
    }

    #[test]
    fn test_desktop_composition_encode_decode() {
        let original = DesktopComposition::new(0x0001);

        let mut buffer = vec![0u8; original.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        original.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buffer);
        let decoded = DesktopComposition::decode(&mut read_cursor).unwrap();

        assert_eq!(original, decoded);
    }
}
