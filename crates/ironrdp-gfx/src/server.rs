//! The graphics pipeline in the direction a server sends it.
//!
//! Everything else in this crate reads what a server sent, because everything else in this
//! tree is a client. These are the same PDUs written rather than parsed, laid out from
//! `MS-RDPEGFX` and checked against the parsers next door -- a round trip through both is
//! worth more than either side read carefully on its own.
//!
//! Two details are easy to get wrong and are handled here. `RDPGFX_RESET_GRAPHICS_PDU` is
//! padded to exactly 340 bytes no matter how many monitors it describes, and every PDU on this
//! channel travels inside a ZGFX segment even when nothing is compressed -- a server may
//! declare a segment uncompressed and send the bytes as they are, which is what this does
//! rather than carrying a compressor it does not need.

use bytes::BufMut as _;

use crate::caps::CapabilitySet;
use crate::pdu::{CmdId, Header, Rectangle};

/// A ZGFX descriptor saying the packet is one segment rather than several.
const ZGFX_SEGMENTED_SINGLE: u8 = 0xE0;

/// The size `RDPGFX_RESET_GRAPHICS_PDU` is required to be, whatever it contains.
const RESET_GRAPHICS_SIZE: usize = 340;

/// Wraps encoded PDUs in the framing the graphics channel always carries.
///
/// The segment is declared uncompressed, which is legal and costs nothing: the pixels inside
/// are already an encoded bitstream, and compressing an AV1 frame with the ZGFX matcher would
/// spend CPU to make it slightly larger.
pub fn segment(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.put_u8(ZGFX_SEGMENTED_SINGLE);
    // Segment flags. Bit 0 is "compressed"; everything else is reserved.
    out.put_u8(0x00);
    out.extend_from_slice(payload);
    out
}

/// Writes a header whose length field covers the whole PDU, which is what `pduLength` means.
fn header(buf: &mut Vec<u8>, cmd_id: CmdId, body_len: usize) {
    Header {
        cmd_id,
        flags: 0,
        pdu_length: (Header::SIZE + body_len) as u32,
    }
    .write(buf);
}

/// `RDPGFX_CAPS_CONFIRM_PDU`: the one capability set the server picked.
pub fn caps_confirm(caps: &CapabilitySet) -> Vec<u8> {
    let body = caps.to_bytes();

    let mut buf = Vec::with_capacity(Header::SIZE + body.len());
    header(&mut buf, CmdId::CapsConfirm, body.len());
    buf.extend_from_slice(&body);
    buf
}

/// `RDPGFX_RESET_GRAPHICS_PDU`: the size of the graphics output buffer, and the monitors in it.
///
/// Padded to 340 bytes because the specification says the message is that size, not merely at
/// most that size; a client that trusts `pduLength` will read past a shorter one.
pub fn reset_graphics(width: u32, height: u32, monitors: &[MonitorDefinition]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(RESET_GRAPHICS_SIZE);

    header(&mut buf, CmdId::ResetGraphics, RESET_GRAPHICS_SIZE - Header::SIZE);
    buf.put_u32_le(width);
    buf.put_u32_le(height);
    buf.put_u32_le(monitors.len() as u32);

    for monitor in monitors {
        buf.put_u32_le(monitor.left);
        buf.put_u32_le(monitor.top);
        buf.put_u32_le(monitor.right);
        buf.put_u32_le(monitor.bottom);
        buf.put_u32_le(monitor.flags);
    }

    buf.resize(RESET_GRAPHICS_SIZE, 0);
    buf
}

/// One monitor, as `RDPGFX_RESET_GRAPHICS_PDU` describes it.
#[derive(Debug, Clone, Copy)]
pub struct MonitorDefinition {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    /// 1 marks the primary monitor.
    pub flags: u32,
}

impl MonitorDefinition {
    /// A single primary monitor covering the whole output.
    pub fn primary(width: u32, height: u32) -> Self {
        Self {
            left: 0,
            top: 0,
            // Inclusive edges: a 1920-wide monitor ends at 1919.
            right: width.saturating_sub(1),
            bottom: height.saturating_sub(1),
            flags: 1,
        }
    }
}

/// `RDPGFX_CREATE_SURFACE_PDU`.
pub fn create_surface(surface_id: u16, width: u16, height: u16, pixel_format: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(Header::SIZE + 7);
    header(&mut buf, CmdId::CreateSurface, 7);
    buf.put_u16_le(surface_id);
    buf.put_u16_le(width);
    buf.put_u16_le(height);
    buf.put_u8(pixel_format);
    buf
}

/// `RDPGFX_DELETE_SURFACE_PDU`.
pub fn delete_surface(surface_id: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(Header::SIZE + 2);
    header(&mut buf, CmdId::DeleteSurface, 2);
    buf.put_u16_le(surface_id);
    buf
}

/// `RDPGFX_MAP_SURFACE_TO_OUTPUT_PDU`: where on the desktop a surface appears.
pub fn map_surface_to_output(surface_id: u16, origin_x: u32, origin_y: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(Header::SIZE + 12);
    header(&mut buf, CmdId::MapSurfaceToOutput, 12);
    buf.put_u16_le(surface_id);
    // reserved
    buf.put_u16_le(0);
    buf.put_u32_le(origin_x);
    buf.put_u32_le(origin_y);
    buf
}

/// `RDPGFX_START_FRAME_PDU`. The id is what the client quotes back in its acknowledgement.
pub fn start_frame(frame_id: u32, timestamp: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(Header::SIZE + 8);
    header(&mut buf, CmdId::StartFrame, 8);
    buf.put_u32_le(timestamp);
    buf.put_u32_le(frame_id);
    buf
}

/// `RDPGFX_END_FRAME_PDU`.
pub fn end_frame(frame_id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(Header::SIZE + 4);
    header(&mut buf, CmdId::EndFrame, 4);
    buf.put_u32_le(frame_id);
    buf
}

/// `RDPGFX_WIRE_TO_SURFACE_PDU_1`: a codec's bitstream, and where it lands.
///
/// For the H.264 codecs the destination rectangle is a bounding box and the real geometry is
/// inside the bitstream's own metadata; for everything else it is the rectangle itself.
pub fn wire_to_surface_1(
    surface_id: u16,
    codec_id: u16,
    pixel_format: u8,
    dest: Rectangle,
    bitmap: &[u8],
) -> Vec<u8> {
    let body_len = 2 + 2 + 1 + 8 + 4 + bitmap.len();

    let mut buf = Vec::with_capacity(Header::SIZE + body_len);
    header(&mut buf, CmdId::WireToSurface1, body_len);
    buf.put_u16_le(surface_id);
    buf.put_u16_le(codec_id);
    buf.put_u8(pixel_format);
    buf.put_u16_le(dest.left);
    buf.put_u16_le(dest.top);
    buf.put_u16_le(dest.right);
    buf.put_u16_le(dest.bottom);
    buf.put_u32_le(bitmap.len() as u32);
    buf.extend_from_slice(bitmap);
    buf
}

/// `RDPGFX_SOLID_FILL_PDU`, which is the cheapest way to prove a surface is mapped.
pub fn solid_fill(surface_id: u16, colour: [u8; 4], rects: &[Rectangle]) -> Vec<u8> {
    let body_len = 2 + 4 + 2 + rects.len() * 8;

    let mut buf = Vec::with_capacity(Header::SIZE + body_len);
    header(&mut buf, CmdId::SolidFill, body_len);
    buf.put_u16_le(surface_id);
    buf.put_u8(colour[0]);
    buf.put_u8(colour[1]);
    buf.put_u8(colour[2]);
    buf.put_u8(colour[3]);
    buf.put_u16_le(rects.len() as u16);

    for rect in rects {
        buf.put_u16_le(rect.left);
        buf.put_u16_le(rect.top);
        buf.put_u16_le(rect.right);
        buf.put_u16_le(rect.bottom);
    }

    buf
}

/// Reads the client's `RDPGFX_CAPS_ADVERTISE_PDU`, whose header has already been consumed.
///
/// The client offers every version it can speak and the server confirms exactly one, so this
/// is the moment a server decides what the session will look like -- including, for a client
/// that says so, a codec Microsoft never allocated.
pub fn parse_caps_advertise(mut body: &[u8]) -> anyhow::Result<Vec<CapabilitySet>> {
    use bytes::Buf as _;

    if body.len() < 2 {
        anyhow::bail!("not enough data for a caps advertisement");
    }

    let count = usize::from(body.get_u16_le());
    let mut sets = Vec::with_capacity(count);

    for _ in 0..count {
        let set = CapabilitySet::from_bytes(body)?;
        let len = set.serialized_len();
        if body.len() < len {
            anyhow::bail!("a capability set ran past the end of the advertisement");
        }
        body.advance(len);
        sets.push(set);
    }

    Ok(sets)
}

/// Picks the newest version both sides understand.
///
/// Newest rather than any: the later versions are what carry the surface commands worth
/// having, and a client that advertises one has already said it can decode everything the
/// earlier ones could.
pub fn best_capability<'a>(
    advertised: &'a [CapabilitySet],
    supported: &[u32],
) -> Option<&'a CapabilitySet> {
    advertised
        .iter()
        .filter(|set| supported.contains(&set.version))
        .max_by_key(|set| set.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdu::{CreateSurface, DeleteSurface, EndFrame, StartFrame, WireToSurface1};

    /// Reads back a PDU's header and hands over the body, checking the length field means what
    /// it says: everything, header included.
    fn body<'a>(encoded: &'a [u8], expected: CmdId) -> &'a [u8] {
        let mut cursor = encoded;
        let header = Header::parse(&mut cursor).expect("header");
        assert_eq!(header.cmd_id as u16, expected as u16);
        assert_eq!(header.pdu_length as usize, encoded.len());
        cursor
    }

    #[test]
    fn create_surface_round_trips() {
        let encoded = create_surface(1, 1920, 1080, 32);
        let mut rest = body(&encoded, CmdId::CreateSurface);

        let parsed = CreateSurface::parse(&mut rest).expect("parse");
        assert_eq!(parsed.surface_id, 1);
        assert_eq!(parsed.width, 1920);
        assert_eq!(parsed.height, 1080);
        assert_eq!(parsed.pixel_format, 32);
    }

    #[test]
    fn delete_surface_round_trips() {
        let encoded = delete_surface(7);
        let mut rest = body(&encoded, CmdId::DeleteSurface);
        assert_eq!(DeleteSurface::parse(&mut rest).expect("parse").surface_id, 7);
    }

    #[test]
    fn frames_round_trip() {
        let encoded = start_frame(42, 0);
        let mut rest = body(&encoded, CmdId::StartFrame);
        assert_eq!(StartFrame::parse(&mut rest).expect("parse").frame_id, 42);

        let encoded = end_frame(42);
        let mut rest = body(&encoded, CmdId::EndFrame);
        assert_eq!(EndFrame::parse(&mut rest).expect("parse").frame_id, 42);
    }

    #[test]
    fn wire_to_surface_round_trips() {
        let bitstream = [1u8, 2, 3, 4, 5];
        let dest = Rectangle {
            left: 0,
            top: 0,
            right: 64,
            bottom: 32,
        };

        let encoded = wire_to_surface_1(3, 9, 32, dest, &bitstream);
        let mut rest = body(&encoded, CmdId::WireToSurface1);

        let parsed = WireToSurface1::parse(&mut rest).expect("parse");
        assert_eq!(parsed.surface_id, 3);
        assert_eq!(parsed.codec_id, 9);
        assert_eq!(parsed.dest_rect.right, 64);
        assert_eq!(parsed.bitmap_data, bitstream);
    }

    #[test]
    fn reset_graphics_is_always_340_bytes() {
        for monitors in 1..=4 {
            let defs = vec![MonitorDefinition::primary(1920, 1080); monitors];
            let encoded = reset_graphics(1920, 1080, &defs);

            assert_eq!(encoded.len(), RESET_GRAPHICS_SIZE);

            let mut cursor = encoded.as_slice();
            let header = Header::parse(&mut cursor).expect("header");
            assert_eq!(header.pdu_length as usize, RESET_GRAPHICS_SIZE);
        }
    }

    #[test]
    fn caps_advertise_round_trips() {
        use crate::caps::cap_version as versions;

        // Built the way the client builds it: a count, then the sets.
        let sets = vec![
            CapabilitySet::new(versions::V8, 0),
            CapabilitySet::new(versions::V107, 0x20),
        ];

        let mut body = Vec::new();
        body.extend_from_slice(&(sets.len() as u16).to_le_bytes());
        for set in &sets {
            body.extend_from_slice(&set.to_bytes());
        }

        let parsed = parse_caps_advertise(&body).expect("parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].version, versions::V8);
        assert_eq!(parsed[1].version, versions::V107);
        assert_eq!(parsed[1].flags, 0x20);

        // And the newest one both sides know is the one that gets confirmed.
        let best = best_capability(&parsed, &[versions::V8, versions::V107]).expect("a match");
        assert_eq!(best.version, versions::V107);

        // A server that only speaks the old one says so instead.
        let best = best_capability(&parsed, &[versions::V8]).expect("a match");
        assert_eq!(best.version, versions::V8);

        // Nothing in common is not a negotiation.
        assert!(best_capability(&parsed, &[versions::V102]).is_none());
    }

    #[test]
    fn caps_confirm_carries_one_set() {
        use crate::caps::cap_version as versions;

        let set = CapabilitySet::new(versions::V107, 0x20);
        let encoded = caps_confirm(&set);
        let rest = body(&encoded, CmdId::CapsConfirm);

        let parsed = CapabilitySet::from_bytes(rest).expect("parse");
        assert_eq!(parsed.version, versions::V107);
        assert_eq!(parsed.flags, 0x20);
    }

    #[test]
    fn a_segment_says_it_is_not_compressed() {
        let framed = segment(&[0xAA, 0xBB]);
        assert_eq!(framed[0], ZGFX_SEGMENTED_SINGLE);
        assert_eq!(framed[1] & 0x01, 0, "the compressed bit must be clear");
        assert_eq!(&framed[2..], &[0xAA, 0xBB]);
    }

    /// The framing this crate writes has to be the framing this crate reads.
    #[test]
    fn a_segment_decompresses_back() {
        let payload = create_surface(1, 800, 600, 32);
        let framed = segment(&payload);

        let out = zgfx::decompress(&framed).expect("decompress");
        assert_eq!(out, payload);
    }
}
