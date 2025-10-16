use bitflags::bitflags;
use ironrdp_core::{
    ensure_fixed_part_size, Decode, DecodeResult, Encode, EncodeResult, ReadCursor, WriteCursor,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTransportChannelData {
    pub flags: MultiTransportFlags,
}

impl MultiTransportChannelData {
    const NAME: &'static str = "MultiTransportChannelData";
    const FIXED_PART_SIZE: usize = 4; /* flags */
}

impl Encode for MultiTransportChannelData {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_fixed_part_size!(in: dst);
        dst.write_u32(self.flags.bits());
        Ok(())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn size(&self) -> usize {
        Self::FIXED_PART_SIZE
    }
}

impl<'de> Decode<'de> for MultiTransportChannelData {
    fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_fixed_part_size!(in: src);
        let flags_raw = src.read_u32();
        let flags = MultiTransportFlags::from_bits_truncate(flags_raw);
        Ok(Self { flags })
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct MultiTransportFlags: u32 {
        /// 0x01: Client supports and requests Reliable UDP transport (RDP-UDP-R).
        const TRANSPORT_TYPE_UDP_FECR = 0x01;

        /// 0x02: Server-only flag indicating UDP is preferred.
        const TRANSPORT_TYPE_UDP_PREFERRED = 0x02;

        /// 0x04: Client supports and requests Lossy UDP transport (RDP-UDP-L).
        const TRANSPORT_TYPE_UDP_FECL = 0x04;

        /// 0x100: This PDU is a request from the client to the server.
        const MULTITRANSPORT_TYPE_FLAGS_REQUEST = 0x0100;

        /// 0x200: Client supports soft-syncing from TCP to UDP.
        const SOFT_SYNC_TCP_TO_UDP = 0x0200;
    }
}
