# H.264 Codec Support Patch for IronRDP

This is a patched version of `ironrdp-pdu` that adds H.264/AVC444 codec support to the BitmapCodecs capability negotiation.

## Changes Made

### Modified File: `src/rdp/capability_sets/bitmap_codecs/mod.rs`

1. **Added H.264 GUID constant** (line 50):
   ```rust
   const GUID_H264: Guid = Guid(0x3f8b_4284, 0x64ad, 0x4f55, 0x88, 0xfb, 0x18, 0xd2, 0xe8, 0xc2, 0xa5, 0x28);
   ```

2. **Extended CodecProperty enum** (line 324):
   ```rust
   pub enum CodecProperty {
       // ... existing variants
       H264(u8), // H.264/AVC444 codec with flags
       None,
   }
   ```

3. **Updated Codec::encode()** to handle H.264 GUID (line 182) and properties (lines 224-227):
   ```rust
   CodecProperty::H264(_) => GUID_H264,  // GUID mapping
   // ...
   CodecProperty::H264(flags) => {
       dst.write_u16(1); // Properties length: 1 byte
       dst.write_u8(*flags); // H264_CAPSET_FLAGS
   }
   ```

4. **Updated Codec::size()** to include H.264 property size (line 255):
   ```rust
   CodecProperty::H264(_) => 1, // 1 byte for flags
   ```

5. **Updated Codec::decode()** to parse H.264 (lines 307-312):
   ```rust
   GUID_H264 => {
       if property_buffer.len() != 1 {
           return Err(invalid_field_err!("h264 property", "must be 1 byte"));
       }
       CodecProperty::H264(property_buffer[0])
   }
   ```

## Usage

The H.264 codec can now be added to BitmapCodecs:

```rust
use ironrdp::pdu::rdp::capability_sets::{Codec, CodecProperty};

const AVC444_SUPPORT: u8 = 0x02;  // Enable AVC444 (hardware encoding)

codecs.0.push(Codec {
    id: 4,  // H.264 codec ID
    property: CodecProperty::H264(AVC444_SUPPORT),
});
```

## Why This Patch Is Needed

Upstream IronRDP doesn't support H.264 in BitmapCodecs capability negotiation. Without this:
- Windows Server defaults to Video Redirection (MS-RDPEVOR) mode
- Hardware-accelerated H.264 encoding (AVC444) is not available
- The RDPEGFX Graphics channel is never created

With this patch:
- Client advertises H.264/AVC444 support in BitmapCodecs
- Windows Server can choose RDPEGFX with hardware encoding
- Better performance for graphics-intensive applications

## Upstream Status

This patch should be submitted upstream to the IronRDP project. Until then, we maintain a local patched version.

## Original Source

Based on IronRDP commit: a0a3e75
Repository: https://github.com/Devolutions/IronRDP
