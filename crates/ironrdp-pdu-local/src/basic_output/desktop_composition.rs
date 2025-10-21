//! Desktop Composition Virtual Channel Extension
//!
//! Implementation of MS-RDPEDC-170601 specification
//! Remote Desktop Protocol: Desktop Composition Virtual Channel Extension

use bitflags::bitflags;
use ironrdp_core::{
    cast_length, ensure_fixed_part_size, ensure_size, invalid_field_err, Decode, DecodeResult,
    Encode, EncodeResult, ReadCursor, WriteCursor,
};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;

/// TS_ALTSEC_COMPDESK_FIRST - Alternate Secondary Order type for Desktop Composition
pub const TS_ALTSEC_COMPDESK_FIRST: u8 = 0x0C;

// ====================
// 2.2.1 Desktop Composition Mode Management
// ====================

/// TS_COMPDESK_TOGGLE - Drawing and Desktop Mode Changes Order
///
/// [MS-RDPEDC] 2.2.1.1
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskToggle {
    pub event_type: CompDeskToggleEventType,
}

impl CompDeskToggle {
    const NAME: &'static str = "TS_COMPDESK_TOGGLE";
    const FIXED_PART_SIZE: usize = 1; // eventType

    pub fn new(event_type: CompDeskToggleEventType) -> Self {
        Self { event_type }
    }
}

impl Encode for CompDeskToggle {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        dst.write_u8(self.event_type.as_u8());
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskToggle {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let event_type_raw = src.read_u8();
        let event_type = CompDeskToggleEventType::from_u8(event_type_raw).ok_or_else(|| {
            invalid_field_err!("eventType", "invalid desktop composition event type")
        })?;

        Ok(Self { event_type })
    }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive)]
pub enum CompDeskToggleEventType {
    /// Server is leaving desktop composition mode
    CompositionOff = 0x00,
    /// Reserved - not used
    Reserved00 = 0x01,
    /// Reserved - not used
    Reserved01 = 0x02,
    /// Server is entering desktop composition mode
    CompositionOn = 0x03,
    /// Server is switching from non-composed desktop to composed desktop
    DwmDeskEnter = 0x04,
    /// Server is switching from composed desktop to non-composed desktop
    DwmDeskLeave = 0x05,
}

impl CompDeskToggleEventType {
    #[expect(clippy::as_conversions)]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// ====================
// 2.2.2 Redirection Object Lifetime Management
// ====================

/// TS_COMPDESK_LSURFACE - Logical Surface Lifetime Management Order
///
/// [MS-RDPEDC] 2.2.2.1
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskLSurface {
    pub create: bool,
    pub flags: LSurfaceFlags,
    pub h_lsurface: u64,
    pub width: u32,  // Not used per spec, must be 0
    pub height: u32, // Not used per spec, must be 0
    pub hwnd: u64,
    pub luid: u64, // Not used per spec, must be 0
}

impl CompDeskLSurface {
    const NAME: &'static str = "TS_COMPDESK_LSURFACE";
    const FIXED_PART_SIZE: usize = 1 + 1 + 8 + 4 + 4 + 8 + 8; // 34 bytes

    pub fn new_create(h_lsurface: u64, flags: LSurfaceFlags, hwnd: u64) -> Self {
        Self {
            create: true,
            flags,
            h_lsurface,
            width: 0,
            height: 0,
            hwnd,
            luid: 0,
        }
    }

    pub fn new_destroy(h_lsurface: u64) -> Self {
        Self {
            create: false,
            flags: LSurfaceFlags::empty(),
            h_lsurface,
            width: 0,
            height: 0,
            hwnd: 0,
            luid: 0,
        }
    }
}

impl Encode for CompDeskLSurface {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());

        dst.write_u8(if self.create { 0x01 } else { 0x00 });
        dst.write_u8(self.flags.bits());
        dst.write_u64(self.h_lsurface);
        dst.write_u32(self.width);
        dst.write_u32(self.height);
        dst.write_u64(self.hwnd);
        dst.write_u64(self.luid);

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskLSurface {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let f_create = src.read_u8();
        let create = f_create == 0x01;

        let flags = LSurfaceFlags::from_bits_truncate(src.read_u8());
        let h_lsurface = src.read_u64();
        let width = src.read_u32();
        let height = src.read_u32();
        let hwnd = src.read_u64();
        let luid = src.read_u64();

        Ok(Self {
            create,
            flags,
            h_lsurface,
            width,
            height,
            hwnd,
            luid,
        })
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct LSurfaceFlags: u8 {
        /// This logical surface is a compose-once surface
        const COMPOSEONCE = 0x01;
        /// This logical surface is a redirection surface
        const REDIRECTION = 0x04;
    }
}

/// TS_COMPDESK_SURFOBJ - Redirection Surface Lifetime Management Order
///
/// [MS-RDPEDC] 2.2.2.2
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskSurfObj {
    /// Cache ID with high bit indicating create (0) or destroy (1)
    pub cache_id: u32,
    pub surface_bpp: u8,
    pub flags: u8, // Reserved, must be 0
    pub h_surf: u64,
    pub cx: u32,
    pub cy: u32,
}

impl CompDeskSurfObj {
    const NAME: &'static str = "TS_COMPDESK_SURFOBJ";
    const FIXED_PART_SIZE: usize = 4 + 1 + 1 + 8 + 4 + 4; // 22 bytes

    const DESTROY_FLAG: u32 = 0x8000_0000;

    pub fn new_create(cache_id: u32, surface_bpp: u8, h_surf: u64, cx: u32, cy: u32) -> Self {
        Self {
            cache_id: cache_id & !Self::DESTROY_FLAG, // Ensure high bit is 0 for create
            surface_bpp,
            flags: 0,
            h_surf,
            cx,
            cy,
        }
    }

    pub fn new_destroy(cache_id: u32, h_surf: u64) -> Self {
        Self {
            cache_id: cache_id | Self::DESTROY_FLAG, // Set high bit for destroy
            surface_bpp: 0,
            flags: 0,
            h_surf,
            cx: 0,
            cy: 0,
        }
    }

    pub fn is_create(&self) -> bool {
        (self.cache_id & Self::DESTROY_FLAG) == 0
    }

    pub fn is_destroy(&self) -> bool {
        !self.is_create()
    }

    pub fn get_cache_id(&self) -> u32 {
        self.cache_id & !Self::DESTROY_FLAG
    }
}

impl Encode for CompDeskSurfObj {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());

        dst.write_u32(self.cache_id);
        dst.write_u8(self.surface_bpp);
        dst.write_u8(self.flags);
        dst.write_u64(self.h_surf);
        dst.write_u32(self.cx);
        dst.write_u32(self.cy);

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskSurfObj {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let cache_id = src.read_u32();
        let surface_bpp = src.read_u8();
        let flags = src.read_u8();
        let h_surf = src.read_u64();
        let cx = src.read_u32();
        let cy = src.read_u32();

        Ok(Self {
            cache_id,
            surface_bpp,
            flags,
            h_surf,
            cx,
            cy,
        })
    }
}

/// TS_COMPDESK_REDIRSURF_ASSOC_LSURFACE - Redirection Surface and Logical Surface Association Order
///
/// [MS-RDPEDC] 2.2.2.3
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskRedirSurfAssocLSurface {
    pub associate: bool,
    pub h_lsurface: u64,
    pub h_surf: u64,
}

impl CompDeskRedirSurfAssocLSurface {
    const NAME: &'static str = "TS_COMPDESK_REDIRSURF_ASSOC_LSURFACE";
    const FIXED_PART_SIZE: usize = 1 + 8 + 8; // 17 bytes

    pub fn new_associate(h_lsurface: u64, h_surf: u64) -> Self {
        Self {
            associate: true,
            h_lsurface,
            h_surf,
        }
    }

    pub fn new_disassociate(h_lsurface: u64, h_surf: u64) -> Self {
        Self {
            associate: false,
            h_lsurface,
            h_surf,
        }
    }
}

impl Encode for CompDeskRedirSurfAssocLSurface {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());

        dst.write_u8(if self.associate { 0x01 } else { 0x00 });
        dst.write_u64(self.h_lsurface);
        dst.write_u64(self.h_surf);

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskRedirSurfAssocLSurface {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let f_associate = src.read_u8();
        let associate = f_associate == 0x01;
        let h_lsurface = src.read_u64();
        let h_surf = src.read_u64();

        Ok(Self {
            associate,
            h_lsurface,
            h_surf,
        })
    }
}

/// TS_COMPDESK_LSURFACE_COMPREF_PENDING - Logical Surface Compositor Reference
///
/// [MS-RDPEDC] 2.2.2.4
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskLSurfaceCompRefPending {
    pub h_lsurface: u64,
}

impl CompDeskLSurfaceCompRefPending {
    const NAME: &'static str = "TS_COMPDESK_LSURFACE_COMPREF_PENDING";
    const FIXED_PART_SIZE: usize = 8; // hLSurface

    pub fn new(h_lsurface: u64) -> Self {
        Self { h_lsurface }
    }
}

impl Encode for CompDeskLSurfaceCompRefPending {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        dst.write_u64(self.h_lsurface);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskLSurfaceCompRefPending {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let h_lsurface = src.read_u64();
        Ok(Self { h_lsurface })
    }
}

// ====================
// 2.2.3 Drawing Operations Management
// ====================

/// TS_COMPDESK_SWITCH_SURFOBJ - Retargeting Drawing Order
///
/// [MS-RDPEDC] 2.2.3.1
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskSwitchSurfObj {
    pub cache_id: u32, // High bit must be 0
}

impl CompDeskSwitchSurfObj {
    const NAME: &'static str = "TS_COMPDESK_SWITCH_SURFOBJ";
    const FIXED_PART_SIZE: usize = 4; // cacheId

    pub fn new(cache_id: u32) -> Self {
        Self {
            cache_id: cache_id & 0x7FFF_FFFF, // Ensure high bit is 0
        }
    }
}

impl Encode for CompDeskSwitchSurfObj {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        dst.write_u32(self.cache_id);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskSwitchSurfObj {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let cache_id = src.read_u32();
        Ok(Self { cache_id })
    }
}

/// TS_COMPDESK_FLUSH_COMPOSEONCE - FlushComposeOnce Drawing Order
///
/// [MS-RDPEDC] 2.2.3.2
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompDeskFlushComposeOnce {
    pub cache_id: u32, // High bit must be 0
    pub h_lsurface: u64,
}

impl CompDeskFlushComposeOnce {
    const NAME: &'static str = "TS_COMPDESK_FLUSH_COMPOSEONCE";
    const FIXED_PART_SIZE: usize = 4 + 8; // cacheId + hLSurface

    pub fn new(cache_id: u32, h_lsurface: u64) -> Self {
        Self {
            cache_id: cache_id & 0x7FFF_FFFF, // Ensure high bit is 0
            h_lsurface,
        }
    }
}

impl Encode for CompDeskFlushComposeOnce {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());
        dst.write_u32(self.cache_id);
        dst.write_u64(self.h_lsurface);
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for CompDeskFlushComposeOnce {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let cache_id = src.read_u32();
        let h_lsurface = src.read_u64();
        Ok(Self {
            cache_id,
            h_lsurface,
        })
    }
}

// ====================
// Alternate Secondary Order Header
// ====================

/// Alternate Secondary Order Header for Desktop Composition
///
/// [MS-RDPEGDI] 2.2.2.2.1.3.1.1
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltSecCompDeskHeader {
    pub order_type: u8, // Must be TS_ALTSEC_COMPDESK_FIRST (0x0C)
    pub operation: CompDeskOperation,
    pub size: u16,
}

impl AltSecCompDeskHeader {
    const NAME: &'static str = "AltSecCompDeskHeader";
    const FIXED_PART_SIZE: usize = 1 + 1 + 2; // header + operation + size

    pub fn new(operation: CompDeskOperation, size: u16) -> Self {
        Self {
            order_type: TS_ALTSEC_COMPDESK_FIRST,
            operation,
            size,
        }
    }
}

impl Encode for AltSecCompDeskHeader {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());

        // Encode order header with bits
        let mut header = 0u8;
        header |= (self.order_type & 0x1F) << 2; // Bits 2-6 contain order type
        header |= 0x02; // Bits 0-1 = 0x2 for TS_SECONDARY (Alternate Secondary Order)
        dst.write_u8(header);

        dst.write_u8(self.operation.as_u8());
        dst.write_u16(self.size);

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for AltSecCompDeskHeader {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);

        let header = src.read_u8();
        let order_type = (header >> 2) & 0x1F; // Extract bits 2-6

        if order_type != TS_ALTSEC_COMPDESK_FIRST {
            return Err(invalid_field_err!(
                "orderType",
                "expected TS_ALTSEC_COMPDESK_FIRST"
            ));
        }

        let operation_raw = src.read_u8();
        let operation = CompDeskOperation::from_u8(operation_raw).ok_or_else(|| {
            invalid_field_err!("operation", "invalid desktop composition operation")
        })?;

        let size = src.read_u16();

        Ok(Self {
            order_type,
            operation,
            size,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive)]
pub enum CompDeskOperation {
    CompDeskToggle = 0x01,
    LSurfaceCreateDestroy = 0x02,
    SurfObjCreateDestroy = 0x03,
    RedirSurfAssocDissocLSurface = 0x04,
    LSurfaceCompRefPending = 0x05,
    SurfObjSwitch = 0x06,
    FlushComposeOnce = 0x07,
}

impl CompDeskOperation {
    #[expect(clippy::as_conversions)]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// ====================
// Top-level Desktop Composition Order
// ====================

/// Complete Desktop Composition Order with header and data
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopCompositionOrder {
    Toggle(CompDeskToggle),
    LSurface(CompDeskLSurface),
    SurfObj(CompDeskSurfObj),
    RedirSurfAssoc(CompDeskRedirSurfAssocLSurface),
    LSurfaceCompRef(CompDeskLSurfaceCompRefPending),
    SwitchSurfObj(CompDeskSwitchSurfObj),
    FlushComposeOnce(CompDeskFlushComposeOnce),
}

impl DesktopCompositionOrder {
    const NAME: &'static str = "DesktopCompositionOrder";

    pub fn decode_with_header(src: &mut ReadCursor<'_>) -> DecodeResult<Self> {
        let header = AltSecCompDeskHeader::decode(src)?;

        // Size includes the data after the header
        let expected_data_size = usize::from(header.size);
        ensure_size!(in: src, size: expected_data_size);

        let data_slice = src.read_slice(expected_data_size);
        let mut data_cursor = ReadCursor::new(data_slice);

        let order = match header.operation {
            CompDeskOperation::CompDeskToggle => {
                Self::Toggle(CompDeskToggle::decode(&mut data_cursor)?)
            }
            CompDeskOperation::LSurfaceCreateDestroy => {
                Self::LSurface(CompDeskLSurface::decode(&mut data_cursor)?)
            }
            CompDeskOperation::SurfObjCreateDestroy => {
                Self::SurfObj(CompDeskSurfObj::decode(&mut data_cursor)?)
            }
            CompDeskOperation::RedirSurfAssocDissocLSurface => {
                Self::RedirSurfAssoc(CompDeskRedirSurfAssocLSurface::decode(&mut data_cursor)?)
            }
            CompDeskOperation::LSurfaceCompRefPending => {
                Self::LSurfaceCompRef(CompDeskLSurfaceCompRefPending::decode(&mut data_cursor)?)
            }
            CompDeskOperation::SurfObjSwitch => {
                Self::SwitchSurfObj(CompDeskSwitchSurfObj::decode(&mut data_cursor)?)
            }
            CompDeskOperation::FlushComposeOnce => {
                Self::FlushComposeOnce(CompDeskFlushComposeOnce::decode(&mut data_cursor)?)
            }
        };

        Ok(order)
    }

    pub fn operation(&self) -> CompDeskOperation {
        match self {
            Self::Toggle(_) => CompDeskOperation::CompDeskToggle,
            Self::LSurface(_) => CompDeskOperation::LSurfaceCreateDestroy,
            Self::SurfObj(_) => CompDeskOperation::SurfObjCreateDestroy,
            Self::RedirSurfAssoc(_) => CompDeskOperation::RedirSurfAssocDissocLSurface,
            Self::LSurfaceCompRef(_) => CompDeskOperation::LSurfaceCompRefPending,
            Self::SwitchSurfObj(_) => CompDeskOperation::SurfObjSwitch,
            Self::FlushComposeOnce(_) => CompDeskOperation::FlushComposeOnce,
        }
    }

    pub fn data_size(&self) -> usize {
        match self {
            Self::Toggle(order) => order.size(),
            Self::LSurface(order) => order.size(),
            Self::SurfObj(order) => order.size(),
            Self::RedirSurfAssoc(order) => order.size(),
            Self::LSurfaceCompRef(order) => order.size(),
            Self::SwitchSurfObj(order) => order.size(),
            Self::FlushComposeOnce(order) => order.size(),
        }
    }
}

impl Encode for DesktopCompositionOrder {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.size());

        // Encode header
        let header =
            AltSecCompDeskHeader::new(self.operation(), cast_length!("size", self.data_size())?);
        header.encode(dst)?;

        // Encode data
        match self {
            Self::Toggle(order) => order.encode(dst)?,
            Self::LSurface(order) => order.encode(dst)?,
            Self::SurfObj(order) => order.encode(dst)?,
            Self::RedirSurfAssoc(order) => order.encode(dst)?,
            Self::LSurfaceCompRef(order) => order.encode(dst)?,
            Self::SwitchSurfObj(order) => order.encode(dst)?,
            Self::FlushComposeOnce(order) => order.encode(dst)?,
        }

        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        AltSecCompDeskHeader::FIXED_PART_SIZE + self.data_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comp_desk_toggle_encode_decode() {
        let original = CompDeskToggle::new(CompDeskToggleEventType::CompositionOn);

        let mut buffer = vec![0u8; original.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        original.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buffer);
        let decoded = CompDeskToggle::decode(&mut read_cursor).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_comp_desk_lsurface_create_encode_decode() {
        let original = CompDeskLSurface::new_create(0x111201a7, LSurfaceFlags::REDIRECTION, 0xc5a8);

        let mut buffer = vec![0u8; original.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        original.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buffer);
        let decoded = CompDeskLSurface::decode(&mut read_cursor).unwrap();

        assert_eq!(original, decoded);
        assert_eq!(decoded.create, true);
        assert_eq!(decoded.h_lsurface, 0x111201a7);
    }

    #[test]
    fn test_comp_desk_surfobj_create_destroy() {
        let create = CompDeskSurfObj::new_create(0x9, 32, 0x7050184, 64, 64);
        assert!(create.is_create());
        assert!(!create.is_destroy());
        assert_eq!(create.get_cache_id(), 0x9);

        let destroy = CompDeskSurfObj::new_destroy(0x9, 0x7050184);
        assert!(!destroy.is_create());
        assert!(destroy.is_destroy());
        assert_eq!(destroy.get_cache_id(), 0x9);
    }

    #[test]
    fn test_desktop_composition_order_full_cycle() {
        let order = DesktopCompositionOrder::Toggle(CompDeskToggle::new(
            CompDeskToggleEventType::CompositionOn,
        ));

        let mut buffer = vec![0u8; order.size()];
        let mut cursor = WriteCursor::new(&mut buffer);
        order.encode(&mut cursor).unwrap();

        let mut read_cursor = ReadCursor::new(&buffer);
        let decoded = DesktopCompositionOrder::decode_with_header(&mut read_cursor).unwrap();

        match decoded {
            DesktopCompositionOrder::Toggle(toggle) => {
                assert_eq!(toggle.event_type, CompDeskToggleEventType::CompositionOn);
            }
            _ => panic!("Expected Toggle order"),
        }
    }
}
