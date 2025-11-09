#![allow(dead_code)]

use std::collections::HashMap;
use std::convert::TryInto;

use ironrdp_pdu::codecs::rfx::{EntropyAlgorithm, Quant, RfxRectangle};
use thiserror::Error;

#[allow(unused_imports)]
use crate::{color_conversion, dwt, rlgr, subband_reconstruction};

const PROGRESSIVE_MAGIC: u32 = 0xCACCACCA;
const TILE_SIZE: usize = 64;
const TILE_PIXELS: usize = TILE_SIZE * TILE_SIZE;
const COMPONENT_COUNT: usize = 3;
const RFX_TILE_DIFFERENCE: u8 = 0x01;
const RFX_DWT_REDUCE_EXTRAPOLATE: u8 = 0x01;

// Block type constants
const PROGRESSIVE_WBT_SYNC: u16 = 0xCCC0;
const PROGRESSIVE_WBT_FRAME_BEGIN: u16 = 0xCCC1;
const PROGRESSIVE_WBT_FRAME_END: u16 = 0xCCC2;
const PROGRESSIVE_WBT_CONTEXT: u16 = 0xCCC3;
const PROGRESSIVE_WBT_REGION: u16 = 0xCCC4;
const PROGRESSIVE_WBT_TILE_SIMPLE: u16 = 0xCCC5;
const PROGRESSIVE_WBT_TILE_FIRST: u16 = 0xCCC6;
const PROGRESSIVE_WBT_TILE_UPGRADE: u16 = 0xCCC7;

type Result<T> = std::result::Result<T, ProgressiveError>;

#[derive(Debug, Error)]
pub enum ProgressiveError {
    #[error("{0}")]
    Invalid(String),
    #[error("missing {0} block")]
    MissingBlock(&'static str),
    #[error("truncated {0}")]
    Truncated(&'static str),
    #[error("unknown surface {0}")]
    UnknownSurface(u16),
    #[error("unsupported progressive version {0:#06x}")]
    UnsupportedVersion(u16),
}

macro_rules! ensure_progressive {
    ($cond:expr, $err:expr $(,)?) => {
        if !$cond {
            return Err($err);
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockType {
    Sync,
    FrameBegin,
    FrameEnd,
    Context,
    Region,
    TileSimple,
    TileFirst,
    TileUpgrade,
}

impl BlockType {
    fn from_u16(value: u16) -> Result<Self> {
        match value {
            PROGRESSIVE_WBT_SYNC => Ok(BlockType::Sync),
            PROGRESSIVE_WBT_FRAME_BEGIN => Ok(BlockType::FrameBegin),
            PROGRESSIVE_WBT_FRAME_END => Ok(BlockType::FrameEnd),
            PROGRESSIVE_WBT_CONTEXT => Ok(BlockType::Context),
            PROGRESSIVE_WBT_REGION => Ok(BlockType::Region),
            PROGRESSIVE_WBT_TILE_SIMPLE => Ok(BlockType::TileSimple),
            PROGRESSIVE_WBT_TILE_FIRST => Ok(BlockType::TileFirst),
            PROGRESSIVE_WBT_TILE_UPGRADE => Ok(BlockType::TileUpgrade),
            _ => Err(ProgressiveError::Invalid(format!(
                "unknown block type 0x{:04X}",
                value
            ))),
        }
    }
}

#[derive(Debug)]
struct BlockHeader {
    block_type: BlockType,
    length: usize,
}

impl BlockHeader {
    fn parse(data: &mut &[u8]) -> Result<Self> {
        ensure_progressive!(data.len() >= 6, ProgressiveError::Truncated("block header"),);
        let block_type_val = u16::from_le_bytes([data[0], data[1]]);
        let length = u32::from_le_bytes(data[2..6].try_into().unwrap()) as usize;
        *data = &data[6..];

        let block_type = BlockType::from_u16(block_type_val)?;
        ensure_progressive!(
            length >= 6,
            ProgressiveError::Invalid(format!("block length {} too short", length)),
        );

        Ok(Self { block_type, length })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuantLevels {
    hl1: i16,
    lh1: i16,
    hh1: i16,
    hl2: i16,
    lh2: i16,
    hh2: i16,
    hl3: i16,
    lh3: i16,
    hh3: i16,
    ll3: i16,
}

impl QuantLevels {
    fn from_quant(quant: &Quant) -> Self {
        Self {
            hl1: i16::from(quant.hl1),
            lh1: i16::from(quant.lh1),
            hh1: i16::from(quant.hh1),
            hl2: i16::from(quant.hl2),
            lh2: i16::from(quant.lh2),
            hh2: i16::from(quant.hh2),
            hl3: i16::from(quant.hl3),
            lh3: i16::from(quant.lh3),
            hh3: i16::from(quant.hh3),
            ll3: i16::from(quant.ll3),
        }
    }

    fn add(&self, other: &QuantLevels) -> QuantLevels {
        QuantLevels {
            hl1: self.hl1 + other.hl1,
            lh1: self.lh1 + other.lh1,
            hh1: self.hh1 + other.hh1,
            hl2: self.hl2 + other.hl2,
            lh2: self.lh2 + other.lh2,
            hh2: self.hh2 + other.hh2,
            hl3: self.hl3 + other.hl3,
            lh3: self.lh3 + other.lh3,
            hh3: self.hh3 + other.hh3,
            ll3: self.ll3 + other.ll3,
        }
    }

    fn sub(&self, other: &QuantLevels) -> QuantLevels {
        QuantLevels {
            hl1: self.hl1 - other.hl1,
            lh1: self.lh1 - other.lh1,
            hh1: self.hh1 - other.hh1,
            hl2: self.hl2 - other.hl2,
            lh2: self.lh2 - other.lh2,
            hh2: self.hh2 - other.hh2,
            hl3: self.hl3 - other.hl3,
            lh3: self.lh3 - other.lh3,
            hh3: self.hh3 - other.hh3,
            ll3: self.ll3 - other.ll3,
        }
    }

    fn sub_scalar(&self, val: i16) -> QuantLevels {
        QuantLevels {
            hl1: self.hl1 - val,
            lh1: self.lh1 - val,
            hh1: self.hh1 - val,
            hl2: self.hl2 - val,
            lh2: self.lh2 - val,
            hh2: self.hh2 - val,
            hl3: self.hl3 - val,
            lh3: self.lh3 - val,
            hh3: self.hh3 - val,
            ll3: self.ll3 - val,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProgressiveCodecQuant {
    quality: u8,
    y: QuantLevels,
    cb: QuantLevels,
    cr: QuantLevels,
}

impl ProgressiveCodecQuant {
    fn parse(data: &mut &[u8]) -> Result<Self> {
        ensure_progressive!(
            data.len() >= 1 + 3 * 5,
            ProgressiveError::Truncated("progressive quant block"),
        );
        let quality = data[0];
        *data = &data[1..];
        let y = parse_quant_levels(data)?;
        let cb = parse_quant_levels(data)?;
        let cr = parse_quant_levels(data)?;
        Ok(Self { quality, y, cb, cr })
    }
}

impl Default for ProgressiveCodecQuant {
    fn default() -> Self {
        Self {
            quality: 100,
            y: QuantLevels::default(),
            cb: QuantLevels::default(),
            cr: QuantLevels::default(),
        }
    }
}

fn parse_quant_levels(data: &mut &[u8]) -> Result<QuantLevels> {
    ensure_progressive!(
        data.len() >= 5,
        ProgressiveError::Truncated("progressive quant levels"),
    );
    let ll3_lh3 = data[0];
    let hl3_hh3 = data[1];
    let lh2_hl2 = data[2];
    let hh2_lh1 = data[3];
    let hl1_hh1 = data[4];
    *data = &data[5..];

    let quant = Quant {
        ll3: ll3_lh3 & 0x0F,
        lh3: ll3_lh3 >> 4,
        hl3: hl3_hh3 & 0x0F,
        hh3: hl3_hh3 >> 4,
        lh2: lh2_hl2 & 0x0F,
        hl2: lh2_hl2 >> 4,
        hh2: hh2_lh1 & 0x0F,
        lh1: hh2_lh1 >> 4,
        hl1: hl1_hh1 & 0x0F,
        hh1: hl1_hh1 >> 4,
    };

    Ok(QuantLevels::from_quant(&quant))
}

#[derive(Debug)]
pub struct SyncBlock {
    pub version: u16,
}

impl SyncBlock {
    fn parse(data: &[u8]) -> Result<Self> {
        ensure_progressive!(data.len() >= 6, ProgressiveError::Truncated("SYNC block"),);
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        ensure_progressive!(
            magic == PROGRESSIVE_MAGIC,
            ProgressiveError::Invalid(format!("invalid progressive magic 0x{magic:08X}")),
        );
        let version = u16::from_le_bytes(data[4..6].try_into().unwrap());
        Ok(Self { version })
    }
}

#[derive(Debug, Default, Clone)]
pub struct ContextBlock {
    pub context_id: u8,
    pub tile_size: u16,
    pub flags: u8,
}

impl ContextBlock {
    fn parse(data: &[u8]) -> Result<Self> {
        ensure_progressive!(
            data.len() >= 4,
            ProgressiveError::Truncated("CONTEXT block"),
        );
        let context_id = data[0];
        let tile_size = u16::from_le_bytes([data[1], data[2]]);
        let flags = data[3];
        Ok(Self {
            context_id,
            tile_size,
            flags,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FrameBeginBlock {
    pub frame_index: u32,
    pub region_count: u16,
}

impl FrameBeginBlock {
    fn parse(data: &[u8]) -> Result<Self> {
        ensure_progressive!(
            data.len() >= 6,
            ProgressiveError::Truncated("FRAME_BEGIN block"),
        );
        let frame_index = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let region_count = u16::from_le_bytes([data[4], data[5]]);
        Ok(Self {
            frame_index,
            region_count,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FrameEndBlock;

#[derive(Debug, Clone)]
pub struct RegionBlock<'a> {
    pub tile_size: u8,
    pub num_rects: u16,
    pub num_tiles: u16,
    pub flags: u8,
    pub quant_values: Vec<QuantLevels>,
    pub progressive_quants: Vec<ProgressiveCodecQuant>,
    pub rects: Vec<RfxRectangle>,
    pub tile_data: &'a [u8],
}

impl<'a> RegionBlock<'a> {
    fn parse(mut data: &'a [u8]) -> Result<Self> {
        ensure_progressive!(
            data.len() >= 1 + 2 + 1 + 1 + 1 + 2 + 4,
            ProgressiveError::Truncated("REGION block header"),
        );
        let tile_size = data[0];
        let num_rects = u16::from_le_bytes([data[1], data[2]]);
        let num_quant = data[3] as usize;
        let num_prog_quant = data[4] as usize;
        let flags = data[5];
        let num_tiles = u16::from_le_bytes([data[6], data[7]]);
        let tile_data_size = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        data = &data[12..];

        let mut rects = Vec::with_capacity(num_rects as usize);
        for _ in 0..num_rects {
            ensure_progressive!(
                data.len() >= 8,
                ProgressiveError::Truncated("region rectangle"),
            );
            let x = u16::from_le_bytes([data[0], data[1]]);
            let y = u16::from_le_bytes([data[2], data[3]]);
            let width = u16::from_le_bytes([data[4], data[5]]);
            let height = u16::from_le_bytes([data[6], data[7]]);
            rects.push(RfxRectangle {
                x,
                y,
                width,
                height,
            });
            data = &data[8..];
        }

        let mut quant_values = Vec::with_capacity(num_quant);
        for _ in 0..num_quant {
            let mut slice = data;
            let quant = parse_quant_levels(&mut slice)?;
            quant_values.push(quant);
            data = slice;
        }

        let mut progressive_quants = Vec::with_capacity(num_prog_quant);
        for _ in 0..num_prog_quant {
            let mut slice = data;
            let quant = ProgressiveCodecQuant::parse(&mut slice)?;
            progressive_quants.push(quant);
            data = slice;
        }

        ensure_progressive!(
            data.len() >= tile_data_size,
            ProgressiveError::Truncated("REGION tile data"),
        );
        let tile_data = &data[..tile_data_size];

        Ok(Self {
            tile_size,
            num_rects,
            num_tiles,
            flags,
            quant_values,
            progressive_quants,
            rects,
            tile_data,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoordinate {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone)]
pub struct TileUpdate {
    pub coordinate: TileCoordinate,
    pub rect: RfxRectangle,
    pub pixels: Vec<u8>,
}

struct TileDecodeScratch {
    channels: [Vec<i16>; COMPONENT_COUNT],
    temp: Vec<i16>,
    // DWT scratch buffers to avoid allocations in hot path
    // Max size needed is for level 1: ~1100 elements per buffer
    dwt_hl: Vec<i16>,
    dwt_lh: Vec<i16>,
    dwt_hh: Vec<i16>,
    dwt_ll: Vec<i16>,
}

impl TileDecodeScratch {
    fn new() -> Self {
        const DWT_SCRATCH_SIZE: usize = 1200; // Slightly larger than max needed (1089 for level 1)
        Self {
            channels: std::array::from_fn(|_| vec![0i16; TILE_PIXELS]),
            temp: vec![0i16; TILE_PIXELS],
            dwt_hl: vec![0i16; DWT_SCRATCH_SIZE],
            dwt_lh: vec![0i16; DWT_SCRATCH_SIZE],
            dwt_hh: vec![0i16; DWT_SCRATCH_SIZE],
            dwt_ll: vec![0i16; DWT_SCRATCH_SIZE],
        }
    }

    fn channel_mut(&mut self, index: usize) -> &mut [i16] {
        self.channels[index].as_mut_slice()
    }

    fn channels_mut(&mut self) -> [&mut [i16]; COMPONENT_COUNT] {
        let [ref mut c0, ref mut c1, ref mut c2] = self.channels;
        [c0.as_mut_slice(), c1.as_mut_slice(), c2.as_mut_slice()]
    }

    fn channels(&self) -> [&[i16]; COMPONENT_COUNT] {
        let [ref c0, ref c1, ref c2] = self.channels;
        [c0.as_slice(), c1.as_slice(), c2.as_slice()]
    }

    fn temp_mut(&mut self) -> &mut [i16] {
        self.temp.as_mut_slice()
    }

    fn split_mut(&mut self) -> ([&mut [i16]; COMPONENT_COUNT], &mut [i16]) {
        let [ref mut c0, ref mut c1, ref mut c2] = self.channels;
        let temp = self.temp.as_mut_slice();
        (
            [c0.as_mut_slice(), c1.as_mut_slice(), c2.as_mut_slice()],
            temp,
        )
    }

    fn dwt_scratch_mut(&mut self) -> (&mut [i16], &mut [i16], &mut [i16], &mut [i16]) {
        (
            self.dwt_hl.as_mut_slice(),
            self.dwt_lh.as_mut_slice(),
            self.dwt_hh.as_mut_slice(),
            self.dwt_ll.as_mut_slice(),
        )
    }

    /// Split all scratch buffers for DWT operations
    fn split_all_mut(
        &mut self,
    ) -> (
        [&mut [i16]; COMPONENT_COUNT],
        &mut [i16],
        &mut [i16],
        &mut [i16],
        &mut [i16],
        &mut [i16],
    ) {
        let [ref mut c0, ref mut c1, ref mut c2] = self.channels;
        (
            [c0.as_mut_slice(), c1.as_mut_slice(), c2.as_mut_slice()],
            self.temp.as_mut_slice(),
            self.dwt_hl.as_mut_slice(),
            self.dwt_lh.as_mut_slice(),
            self.dwt_hh.as_mut_slice(),
            self.dwt_ll.as_mut_slice(),
        )
    }
}

#[derive(Debug)]
struct TileSimpleBlock<'a> {
    block_type: BlockType,
    quant_idx: [u8; COMPONENT_COUNT],
    x_idx: u16,
    y_idx: u16,
    flags: u8,
    quality: u8,
    y_data: &'a [u8],
    cb_data: &'a [u8],
    cr_data: &'a [u8],
    tail_data: &'a [u8],
}

#[derive(Debug)]
struct TileUpgradeBlock<'a> {
    block_type: BlockType,
    quant_idx: [u8; COMPONENT_COUNT],
    x_idx: u16,
    y_idx: u16,
    quality: u8,
    y_srl: &'a [u8],
    y_raw: &'a [u8],
    cb_srl: &'a [u8],
    cb_raw: &'a [u8],
    cr_srl: &'a [u8],
    cr_raw: &'a [u8],
}

struct TileDecoder<'a> {
    entropy: EntropyAlgorithm,
    context_flags: u8,
    full_quality_quant: &'a ProgressiveCodecQuant,
    scratch: TileDecodeScratch,
}

impl<'a> TileDecoder<'a> {
    fn new(
        entropy: EntropyAlgorithm,
        context_flags: u8,
        full_quality_quant: &'a ProgressiveCodecQuant,
    ) -> Self {
        Self {
            entropy,
            context_flags,
            full_quality_quant,
            scratch: TileDecodeScratch::new(),
        }
    }

    fn process_region(
        &mut self,
        surface: &mut SurfaceState,
        region: &RegionBlock<'_>,
        tiles: &mut Vec<TileUpdate>,
    ) -> Result<()> {
        let mut cursor = region.tile_data;
        let mut processed = 0usize;

        while !cursor.is_empty() {
            ensure_progressive!(
                processed < usize::from(region.num_tiles),
                ProgressiveError::Invalid("tile count exceeds region.num_tiles".into()),
            );

            let header = BlockHeader::parse(&mut cursor)?;
            ensure_progressive!(
                cursor.len() + header.length >= header.length,
                ProgressiveError::Invalid("tile block overflow".into()),
            );
            ensure_progressive!(
                header.length >= 6,
                ProgressiveError::Invalid("tile block length too short".into()),
            );

            ensure_progressive!(
                cursor.len() >= header.length - 6,
                ProgressiveError::Truncated("tile block body"),
            );
            let (mut body, rest) = cursor.split_at(header.length - 6);
            cursor = rest;

            match header.block_type {
                BlockType::TileSimple | BlockType::TileFirst => {
                    let block = parse_tile_simple_block(&mut body, header.block_type)?;
                    self.decode_tile_initial(surface, region, block, tiles)?;
                }
                BlockType::TileUpgrade => {
                    let block = parse_tile_upgrade_block(&mut body)?;
                    self.decode_tile_upgrade(surface, region, block, tiles)?;
                }
                other => {
                    return Err(ProgressiveError::Invalid(format!(
                        "unexpected block {:?} inside REGION",
                        other
                    )));
                }
            }

            ensure_progressive!(
                body.is_empty(),
                ProgressiveError::Invalid("tile parser did not consume entire block".into()),
            );

            processed += 1;
        }

        ensure_progressive!(
            processed == usize::from(region.num_tiles),
            ProgressiveError::Invalid("region.num_tiles mismatch".into()),
        );

        Ok(())
    }

    fn decode_tile_initial(
        &mut self,
        surface: &mut SurfaceState,
        region: &RegionBlock<'_>,
        block: TileSimpleBlock<'_>,
        tiles: &mut Vec<TileUpdate>,
    ) -> Result<()> {
        let coord = TileCoordinate {
            x: block.x_idx,
            y: block.y_idx,
        };

        ensure_progressive!(
            coord.x < surface.grid_width as u16 && coord.y < surface.grid_height as u16,
            ProgressiveError::Invalid("tile index out of surface bounds".into()),
        );

        let surface_width = surface.width;
        let surface_height = surface.height;
        let extrapolate = (region.flags & RFX_DWT_REDUCE_EXTRAPOLATE) != 0;

        {
            let tile_state = surface.tile_mut(coord);
            self.decode_initial_components(tile_state, region, &block)?;
            self.reconstruct_rgba(tile_state, extrapolate)?;
            tile_state.dirty = false;
        }

        surface.updated_tiles.push(coord);

        if let Some(tile_state) = surface.tiles.get(&coord) {
            Self::emit_tile_update(surface_width, surface_height, coord, tile_state, tiles);
        }

        Ok(())
    }

    fn decode_tile_upgrade(
        &mut self,
        surface: &mut SurfaceState,
        region: &RegionBlock<'_>,
        block: TileUpgradeBlock<'_>,
        tiles: &mut Vec<TileUpdate>,
    ) -> Result<()> {
        let coord = TileCoordinate {
            x: block.x_idx,
            y: block.y_idx,
        };

        ensure_progressive!(
            coord.x < surface.grid_width as u16 && coord.y < surface.grid_height as u16,
            ProgressiveError::Invalid("upgrade tile index out of bounds".into()),
        );

        let surface_width = surface.width;
        let surface_height = surface.height;
        let extrapolate = (region.flags & RFX_DWT_REDUCE_EXTRAPOLATE) != 0;

        {
            let tile_state = surface.tile_mut(coord);
            self.decode_upgrade_components(tile_state, region, &block)?;
            self.reconstruct_rgba(tile_state, extrapolate)?;
            tile_state.dirty = false;
        }

        surface.updated_tiles.push(coord);

        if let Some(tile_state) = surface.tiles.get(&coord) {
            Self::emit_tile_update(surface_width, surface_height, coord, tile_state, tiles);
        }

        Ok(())
    }

    fn process_component_coefficients(buffer: &mut [i16], quant: &QuantLevels, extrapolate: bool) {
        if !extrapolate {
            Self::apply_subband_diff_standard(buffer);
        } else {
            Self::apply_subband_diff_extrapolate(buffer);
        }
        Self::apply_quant_shift(buffer, quant, extrapolate);
    }

    fn decode_initial_components(
        &mut self,
        tile_state: &mut TileState,
        region: &RegionBlock<'_>,
        block: &TileSimpleBlock<'_>,
    ) -> Result<()> {
        let coeff_diff = (block.flags & RFX_TILE_DIFFERENCE) != 0;
        let extrapolate = (region.flags & RFX_DWT_REDUCE_EXTRAPOLATE) != 0;

        tile_state.pass = 1;
        tile_state.flags = block.flags;
        tile_state.quality = block.quality;
        tile_state.quant_idx = block.quant_idx;

        let prog_quant = self.resolve_progressive_quant(region, block.quality)?;

        for (component, data) in [block.y_data, block.cb_data, block.cr_data]
            .into_iter()
            .enumerate()
        {
            let base_quant = self.resolve_base_quant(region, block.quant_idx[component])?;
            let component_prog = &prog_quant[component];
            self.decode_initial_component(
                tile_state,
                component,
                base_quant,
                component_prog,
                data,
                coeff_diff,
                // **THE FIX**: Pass the correct extrapolate flag down.
                extrapolate,
            )?;
        }

        Ok(())
    }

    fn decode_initial_component(
        &mut self,
        tile_state: &mut TileState,
        component: usize,
        base_quant: &QuantLevels,
        prog_quant: &QuantLevels,
        data: &[u8],
        coeff_diff: bool,
        extrapolate: bool,
    ) -> Result<()> {
        let buffer = self.scratch.channel_mut(component);
        rlgr::decode(self.entropy, data, buffer)
            .map_err(|_| ProgressiveError::Invalid("RLGR decode failed".into()))?;

        tile_state.sign[component].copy_from_slice(buffer);

        // **THE FIX**: Store the correct persistent state for bit_pos (`base + prog`),
        // but calculate a separate temporary value for the initial shift (`base + prog - 1`).
        let combined_quant = base_quant.add(prog_quant);
        let shift_quant = combined_quant.sub_scalar(1);

        tile_state.quant[component] = *base_quant;
        tile_state.progressive_quant[component] = *prog_quant;
        tile_state.bit_pos[component] = combined_quant; // Store the correct state.

        // Process the buffer using the temporary shift value.
        Self::process_component_coefficients(buffer, &shift_quant, extrapolate);

        // Store the fully processed coefficients as the new persistent state.
        Self::store_coefficients(buffer, &mut tile_state.coefficients[component], coeff_diff);

        Ok(())
    }

    fn decode_upgrade_components(
        &mut self,
        tile_state: &mut TileState,
        region: &RegionBlock<'_>,
        block: &TileUpgradeBlock<'_>,
    ) -> Result<()> {
        let prog_quant = self.resolve_progressive_quant(region, block.quality)?;

        let channels = [
            (block.y_srl, block.y_raw),
            (block.cb_srl, block.cb_raw),
            (block.cr_srl, block.cr_raw),
        ];

        for (component, (srl, raw)) in channels.into_iter().enumerate() {
            self.decode_upgrade_component(
                tile_state,
                region,
                component,
                block.quant_idx[component],
                &prog_quant[component],
                srl,
                raw,
            )?;
        }

        Ok(())
    }

    fn decode_upgrade_component(
        &mut self,
        tile_state: &mut TileState,
        region: &RegionBlock<'_>,
        component: usize,
        quant_idx: u8,
        prog_quant: &QuantLevels,
        srl_data: &[u8],
        raw_data: &[u8],
    ) -> Result<()> {
        let base_quant = self.resolve_base_quant(region, quant_idx)?;
        let previous_bitpos = tile_state.bit_pos[component];

        // **THE FIX**: The logic for calculating the state and deltas is now correct
        // in the context of an un-shifted persistent state.
        let new_bitpos = base_quant.add(prog_quant);
        let bitpos_delta = previous_bitpos.sub(&new_bitpos);
        let shift = base_quant.add(prog_quant).sub_scalar(1);

        tile_state.bit_pos[component] = new_bitpos;
        tile_state.progressive_quant[component] = *prog_quant;
        tile_state.quant[component] = *base_quant;

        let extrapolate = (region.flags & RFX_DWT_REDUCE_EXTRAPOLATE) != 0;

        // Apply the upgrade deltas to the persistent `coefficients` buffer.
        self.apply_upgrade_rlgr(
            &mut tile_state.coefficients[component],
            &mut tile_state.sign[component],
            &shift,
            &bitpos_delta,
            extrapolate,
            prog_quant,
            srl_data,
            raw_data,
        )?;

        Ok(())
    }

    fn reconstruct_rgba(&mut self, tile_state: &mut TileState, extrapolate: bool) -> Result<()> {
        {
            let (mut channels, temp, hl, lh, hh, ll) = self.scratch.split_all_mut();

            for (component, buffer_ref) in channels.as_mut_slice().iter_mut().enumerate() {
                let buffer = &mut **buffer_ref;
                buffer.copy_from_slice(&tile_state.coefficients[component]);
                Self::inverse_dwt(buffer, temp, extrapolate, hl, lh, hh, ll);
            }
        }

        let channels = self.scratch.channels();
        let ycbcr = color_conversion::YCbCrBuffer {
            y: channels[0],
            cb: channels[1],
            cr: channels[2],
        };

        color_conversion::ycbcr_to_bgra(ycbcr, tile_state.reconstruction.as_mut_slice()).map_err(
            |err| ProgressiveError::Invalid(format!("YCbCr to BGRA conversion failed: {err}")),
        )?;

        Ok(())
    }

    fn inverse_dwt(
        buffer: &mut [i16],
        temp: &mut [i16],
        extrapolate: bool,
        hl_scratch: &mut [i16],
        lh_scratch: &mut [i16],
        hh_scratch: &mut [i16],
        ll_scratch: &mut [i16],
    ) {
        if !extrapolate {
            dwt::decode(buffer, temp);
        } else {
            dwt_extrapolate_decode(buffer, temp, hl_scratch, lh_scratch, hh_scratch, ll_scratch);
        }
    }

    fn emit_tile_update(
        surface_width: u32,
        surface_height: u32,
        coord: TileCoordinate,
        tile_state: &TileState,
        tiles: &mut Vec<TileUpdate>,
    ) {
        let x = coord.x as u32 * TILE_SIZE as u32;
        let y = coord.y as u32 * TILE_SIZE as u32;
        let width = ((x + TILE_SIZE as u32).min(surface_width) - x) as u16;
        let height = ((y + TILE_SIZE as u32).min(surface_height) - y) as u16;
        let rect = RfxRectangle {
            x: x as u16,
            y: y as u16,
            width,
            height,
        };
        let width = usize::from(rect.width);
        let height = usize::from(rect.height);
        let mut pixels = vec![0u8; width * height * 4];

        for row in 0..height {
            let src_offset = row * TILE_SIZE * 4;
            let dst_offset = row * width * 4;
            let src_slice = &tile_state.reconstruction[src_offset..src_offset + width * 4];
            pixels[dst_offset..dst_offset + width * 4].copy_from_slice(src_slice);
        }

        tiles.push(TileUpdate {
            coordinate: coord,
            rect,
            pixels,
        });
    }

    fn resolve_base_quant<'b>(
        &self,
        region: &'b RegionBlock<'_>,
        idx: u8,
    ) -> Result<&'b QuantLevels> {
        region
            .quant_values
            .get(idx as usize)
            .ok_or_else(|| ProgressiveError::Invalid("quant index out of range".into()))
    }

    fn resolve_progressive_quant(
        &self,
        region: &RegionBlock<'_>,
        quality: u8,
    ) -> Result<[QuantLevels; COMPONENT_COUNT]> {
        if quality == 0xFF {
            Ok([
                self.full_quality_quant.y,
                self.full_quality_quant.cb,
                self.full_quality_quant.cr,
            ])
        } else {
            let quant = region
                .progressive_quants
                .get(quality as usize)
                .ok_or_else(|| {
                    ProgressiveError::Invalid("progressive quant index out of range".into())
                })?;
            Ok([quant.y, quant.cb, quant.cr])
        }
    }

    fn store_coefficients(buffer: &[i16], coeffs: &mut [i16], coeff_diff: bool) {
        if coeff_diff {
            for (dst, src) in coeffs.iter_mut().zip(buffer.iter()) {
                *dst = dst.wrapping_add(*src);
            }
        } else {
            coeffs.copy_from_slice(buffer);
        }
    }

    fn apply_subband_diff_standard(buffer: &mut [i16]) {
        subband_reconstruction::decode(&mut buffer[4032..]);
    }

    fn apply_subband_diff_extrapolate(buffer: &mut [i16]) {
        rfx_differential_decode_extrapolate(buffer);
    }

    fn apply_quant_shift(buffer: &mut [i16], quant: &QuantLevels, extrapolate: bool) {
        if !extrapolate {
            apply_quant_shift_standard(buffer, quant);
        } else {
            apply_quant_shift_extrapolate(buffer, quant);
        }
    }

    fn apply_upgrade_rlgr(
        &self,
        coefficients: &mut [i16],
        sign: &mut [i16],
        shift: &QuantLevels,
        bitpos_delta: &QuantLevels,
        extrapolate: bool,
        prog_quant: &QuantLevels,
        srl_data: &[u8],
        raw_data: &[u8],
    ) -> Result<()> {
        progressive_upgrade_decode(
            coefficients,
            sign,
            shift,
            bitpos_delta,
            extrapolate,
            prog_quant,
            srl_data,
            raw_data,
        )
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Band {
    Hl1,
    Lh1,
    Hh1,
    Hl2,
    Lh2,
    Hh2,
    Hl3,
    Lh3,
    Hh3,
    Ll3,
}

impl Band {
    fn value(self, levels: &QuantLevels) -> i16 {
        match self {
            Band::Hl1 => levels.hl1,
            Band::Lh1 => levels.lh1,
            Band::Hh1 => levels.hh1,
            Band::Hl2 => levels.hl2,
            Band::Lh2 => levels.lh2,
            Band::Hh2 => levels.hh2,
            Band::Hl3 => levels.hl3,
            Band::Lh3 => levels.lh3,
            Band::Hh3 => levels.hh3,
            Band::Ll3 => levels.ll3,
        }
    }
}

struct SubbandMeta {
    band: Band,
    offset: usize,
    len: usize,
}

const STANDARD_SUBBANDS: [SubbandMeta; 10] = [
    SubbandMeta {
        band: Band::Hl1,
        offset: 0,
        len: 1024,
    },
    SubbandMeta {
        band: Band::Lh1,
        offset: 1024,
        len: 1024,
    },
    SubbandMeta {
        band: Band::Hh1,
        offset: 2048,
        len: 1024,
    },
    SubbandMeta {
        band: Band::Hl2,
        offset: 3072,
        len: 256,
    },
    SubbandMeta {
        band: Band::Lh2,
        offset: 3328,
        len: 256,
    },
    SubbandMeta {
        band: Band::Hh2,
        offset: 3584,
        len: 256,
    },
    SubbandMeta {
        band: Band::Hl3,
        offset: 3840,
        len: 64,
    },
    SubbandMeta {
        band: Band::Lh3,
        offset: 3904,
        len: 64,
    },
    SubbandMeta {
        band: Band::Hh3,
        offset: 3968,
        len: 64,
    },
    SubbandMeta {
        band: Band::Ll3,
        offset: 4032,
        len: 64,
    },
];

const EXTRAPOLATE_SUBBANDS: [SubbandMeta; 10] = [
    SubbandMeta {
        band: Band::Hl1,
        offset: 0,
        len: 1023,
    },
    SubbandMeta {
        band: Band::Lh1,
        offset: 1023,
        len: 1023,
    },
    SubbandMeta {
        band: Band::Hh1,
        offset: 2046,
        len: 961,
    },
    SubbandMeta {
        band: Band::Hl2,
        offset: 3007,
        len: 272,
    },
    SubbandMeta {
        band: Band::Lh2,
        offset: 3279,
        len: 272,
    },
    SubbandMeta {
        band: Band::Hh2,
        offset: 3551,
        len: 256,
    },
    SubbandMeta {
        band: Band::Hl3,
        offset: 3807,
        len: 72,
    },
    SubbandMeta {
        band: Band::Lh3,
        offset: 3879,
        len: 72,
    },
    SubbandMeta {
        band: Band::Hh3,
        offset: 3951,
        len: 64,
    },
    SubbandMeta {
        band: Band::Ll3,
        offset: 4015,
        len: 81,
    },
];

#[inline(always)]
fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[inline(always)]
fn shift_block(data: &mut [i16], shift: i16) {
    if shift == 0 {
        return;
    }

    if shift > 0 {
        let shift = shift as u32;
        for value in data {
            let shifted = (*value as i32) << shift;
            *value = clamp_i16(shifted);
        }
    } else {
        let shift = (-shift) as u32;
        for value in data {
            *value >>= shift;
        }
    }
}

fn apply_quant_shift_impl(buffer: &mut [i16], quant: &QuantLevels, subbands: &[SubbandMeta]) {
    for meta in subbands {
        let shift = meta.band.value(quant);
        if shift == 0 {
            continue;
        }

        let start = meta.offset;
        let end = start + meta.len;
        if end > buffer.len() {
            continue;
        }

        shift_block(&mut buffer[start..end], shift);
    }
}

fn apply_quant_shift_standard(buffer: &mut [i16], quant: &QuantLevels) {
    apply_quant_shift_impl(buffer, quant, &STANDARD_SUBBANDS);
}

fn apply_quant_shift_extrapolate(buffer: &mut [i16], quant: &QuantLevels) {
    apply_quant_shift_impl(buffer, quant, &EXTRAPOLATE_SUBBANDS);
}

fn rfx_differential_decode_extrapolate(buffer: &mut [i16]) {
    const LL3_OFFSET: usize = 4015;
    const LL3_LEN: usize = 81;

    if buffer.len() < LL3_OFFSET + LL3_LEN {
        return;
    }

    subband_reconstruction::decode(&mut buffer[LL3_OFFSET..LL3_OFFSET + LL3_LEN]);
}

fn progressive_get_band_l_count(level: usize) -> usize {
    (TILE_SIZE >> level) + 1
}

fn progressive_get_band_h_count(level: usize) -> usize {
    if level == 1 {
        (TILE_SIZE >> 1) - 1
    } else {
        (TILE_SIZE + (1 << (level - 1))) >> level
    }
}

fn progressive_idwt_x(
    low_band: &[i16],
    low_step: usize,
    high_band: &[i16],
    high_step: usize,
    dst_band: &mut [i16],
    dst_step: usize,
    low_count: usize,
    high_count: usize,
    dst_count: usize,
) {
    for row in 0..dst_count {
        let mut low_idx = row * low_step;
        let mut high_idx = row * high_step;
        let mut dst_idx = row * dst_step;

        let mut h0 = high_band.get(high_idx).copied().unwrap_or_default();
        high_idx += 1;

        // **THE FIX IS HERE**: `l0` is now `mut` and is updated inside the loop.
        let mut l0 = low_band.get(low_idx).copied().unwrap_or_default();
        low_idx += 1;

        let mut x0 = clamp_i16(i32::from(l0) - i32::from(h0));
        let mut x2 = x0;

        for _ in 0..high_count.saturating_sub(1) {
            let h1 = high_band.get(high_idx).copied().unwrap_or_default();
            high_idx += 1;

            l0 = low_band.get(low_idx).copied().unwrap_or_default();
            low_idx += 1;

            x2 = clamp_i16(i32::from(l0) - ((i32::from(h0) + i32::from(h1)) / 2));
            let x1 = clamp_i16(((i32::from(x0) + i32::from(x2)) / 2) + 2 * i32::from(h0));
            if let Some(slice) = dst_band.get_mut(dst_idx..dst_idx + 2) {
                slice[0] = x0;
                slice[1] = x1;
            }
            dst_idx += 2;
            x0 = x2;
            h0 = h1;
        }

        if low_count <= high_count + 1 {
            if low_count <= high_count {
                if let Some(slice) = dst_band.get_mut(dst_idx..dst_idx + 2) {
                    slice[0] = x2;
                    slice[1] = clamp_i16(i32::from(x2) + 2 * i32::from(h0));
                }
            } else {
                l0 = low_band.get(low_idx).copied().unwrap_or_default();
                let x_next = clamp_i16(i32::from(l0) - i32::from(h0));
                if let Some(slice) = dst_band.get_mut(dst_idx..dst_idx + 3) {
                    slice[0] = x2;
                    slice[1] =
                        clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                    slice[2] = x_next;
                }
            }
        } else {
            let l0_first = low_band.get(low_idx).copied().unwrap_or_default();
            low_idx += 1;
            let x_next = clamp_i16(i32::from(l0_first) - (i32::from(h0) / 2));

            let l0_second = low_band.get(low_idx).copied().unwrap_or_default();

            if let Some(slice) = dst_band.get_mut(dst_idx..dst_idx + 4) {
                slice[0] = x2;
                slice[1] = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                slice[2] = x_next;
                slice[3] = clamp_i16((i32::from(x_next) + i32::from(l0_second)) / 2);
            }
        }
    }
}

fn progressive_idwt_y(
    low_band: &[i16],
    low_step: usize,
    high_band: &[i16],
    high_step: usize,
    dst_band: &mut [i16],
    dst_step: usize,
    low_count: usize,
    high_count: usize,
    dst_count: usize,
) {
    for col in 0..dst_count {
        let mut low_idx = col;
        let mut high_idx = col;
        let mut dst_idx = col;

        let mut h0 = high_band.get(high_idx).copied().unwrap_or_default();
        high_idx += high_step;

        let mut l0 = low_band.get(low_idx).copied().unwrap_or_default();
        low_idx += low_step;

        let mut x0 = clamp_i16(i32::from(l0) - i32::from(h0));
        let mut x2 = x0;

        for _ in 0..high_count.saturating_sub(1) {
            let h1 = high_band.get(high_idx).copied().unwrap_or_default();
            high_idx += high_step;

            l0 = low_band.get(low_idx).copied().unwrap_or_default();
            low_idx += low_step;

            x2 = clamp_i16(i32::from(l0) - ((i32::from(h0) + i32::from(h1)) / 2));
            let x1 = clamp_i16(((i32::from(x0) + i32::from(x2)) / 2) + 2 * i32::from(h0));

            if let Some(val) = dst_band.get_mut(dst_idx) {
                *val = x0;
            }
            dst_idx += dst_step;
            if let Some(val) = dst_band.get_mut(dst_idx) {
                *val = x1;
            }
            dst_idx += dst_step;

            x0 = x2;
            h0 = h1;
        }

        if low_count <= high_count + 1 {
            if low_count <= high_count {
                if let Some(val) = dst_band.get_mut(dst_idx) {
                    *val = x2;
                }
                dst_idx += dst_step;
                if let Some(val) = dst_band.get_mut(dst_idx) {
                    *val = clamp_i16(i32::from(x2) + 2 * i32::from(h0));
                }
            } else {
                // Here we use the final `l0` value updated from the loop.
                l0 = low_band.get(low_idx).copied().unwrap_or_default();
                let x_next = clamp_i16(i32::from(l0) - i32::from(h0));

                if let Some(val) = dst_band.get_mut(dst_idx) {
                    *val = x2;
                }
                dst_idx += dst_step;
                if let Some(val) = dst_band.get_mut(dst_idx) {
                    *val = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                }
                dst_idx += dst_step;
                if let Some(val) = dst_band.get_mut(dst_idx) {
                    *val = x_next;
                }
            }
        } else {
            let l0_first = low_band.get(low_idx).copied().unwrap_or_default();
            low_idx += low_step;
            let x_next = clamp_i16(i32::from(l0_first) - (i32::from(h0) / 2));

            if let Some(val) = dst_band.get_mut(dst_idx) {
                *val = x2;
            }
            dst_idx += dst_step;
            if let Some(val) = dst_band.get_mut(dst_idx) {
                *val = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
            }
            dst_idx += dst_step;
            if let Some(val) = dst_band.get_mut(dst_idx) {
                *val = x_next;
            }
            dst_idx += dst_step;

            let l0_second = low_band.get(low_idx).copied().unwrap_or_default();
            if let Some(val) = dst_band.get_mut(dst_idx) {
                *val = clamp_i16((i32::from(x_next) + i32::from(l0_second)) / 2);
            }
        }
    }
}

fn progressive_dwt_decode_block(
    buffer: &mut [i16],
    temp: &mut [i16],
    level: usize,
    hl_scratch: &mut [i16],
    lh_scratch: &mut [i16],
    hh_scratch: &mut [i16],
    ll_scratch: &mut [i16],
) {
    let n_band_l = progressive_get_band_l_count(level);
    let n_band_h = progressive_get_band_h_count(level);

    let hl_len = n_band_h * n_band_l;
    let lh_len = n_band_l * n_band_h;
    let hh_len = n_band_h * n_band_h;
    let ll_len = n_band_l * n_band_l;
    let required = hl_len + lh_len + hh_len + ll_len;

    if buffer.len() < required {
        return;
    }

    let dst_step = n_band_l + n_band_h;
    let dst_len = dst_step * dst_step;

    if buffer.len() < dst_len || temp.len() < dst_step * (n_band_l + n_band_h) {
        return;
    }

    // Use provided scratch buffers instead of allocating
    let ll_actual_len = ll_len
        .min(buffer.len() - (hl_len + lh_len + hh_len))
        .min(ll_scratch.len());

    let hl_copy = &mut hl_scratch[..hl_len];
    let lh_copy = &mut lh_scratch[..lh_len];
    let hh_copy = &mut hh_scratch[..hh_len];
    let ll_copy = &mut ll_scratch[..ll_actual_len];

    hl_copy.copy_from_slice(&buffer[0..hl_len]);
    lh_copy.copy_from_slice(&buffer[hl_len..hl_len + lh_len]);
    hh_copy.copy_from_slice(&buffer[hl_len + lh_len..hl_len + lh_len + hh_len]);
    ll_copy.copy_from_slice(
        &buffer[hl_len + lh_len + hh_len..hl_len + lh_len + hh_len + ll_actual_len],
    );

    let (l_temp, h_temp) = temp.split_at_mut(n_band_l * dst_step);

    progressive_idwt_x(
        &ll_copy, n_band_l, &hl_copy, n_band_h, l_temp, dst_step, n_band_l, n_band_h, n_band_l,
    );
    progressive_idwt_x(
        &lh_copy, n_band_l, &hh_copy, n_band_h, h_temp, dst_step, n_band_l, n_band_h, n_band_h,
    );

    let llx = &mut buffer[0..dst_len];
    progressive_idwt_y(
        l_temp, dst_step, h_temp, dst_step, llx, dst_step, n_band_l, n_band_h, dst_step,
    );
}

fn dwt_extrapolate_decode(
    buffer: &mut [i16],
    temp: &mut [i16],
    hl_scratch: &mut [i16],
    lh_scratch: &mut [i16],
    hh_scratch: &mut [i16],
    ll_scratch: &mut [i16],
) {
    if buffer.len() < 4096 {
        return;
    }

    progressive_dwt_decode_block(
        &mut buffer[3807..],
        temp,
        3,
        hl_scratch,
        lh_scratch,
        hh_scratch,
        ll_scratch,
    );
    progressive_dwt_decode_block(
        &mut buffer[3007..],
        temp,
        2,
        hl_scratch,
        lh_scratch,
        hh_scratch,
        ll_scratch,
    );
    progressive_dwt_decode_block(
        &mut buffer[0..],
        temp,
        1,
        hl_scratch,
        lh_scratch,
        hh_scratch,
        ll_scratch,
    );
}

struct BitStream<'a> {
    data: &'a [u8],
    position: usize,
    accumulator: u32,
    mask: u32,
}

impl<'a> BitStream<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut bs = Self {
            data,
            position: 0,
            accumulator: 0,
            mask: 0,
        };
        bs.fetch();
        bs
    }

    fn fetch(&mut self) {
        let byte_offset = self.position / 8;
        if byte_offset + 4 <= self.data.len() {
            self.accumulator = u32::from_be_bytes([
                self.data[byte_offset],
                self.data[byte_offset + 1],
                self.data[byte_offset + 2],
                self.data[byte_offset + 3],
            ]);
        } else {
            self.accumulator = 0;
            for i in 0..4 {
                if byte_offset + i < self.data.len() {
                    self.accumulator |= (self.data[byte_offset + i] as u32) << (24 - i * 8);
                }
            }
        }
    }

    fn shift(&mut self, bits: u32) {
        self.position += bits as usize;
        if (self.position % 8) == 0 {
            self.fetch();
        } else {
            self.accumulator <<= bits;
        }
    }

    fn peek_bit(&self) -> bool {
        (self.accumulator & 0x80000000) != 0
    }

    fn read_bits(&mut self, num_bits: u32) -> u32 {
        self.mask = if num_bits == 32 {
            0xFFFFFFFF
        } else {
            (1 << num_bits) - 1
        };
        let value = (self.accumulator >> (32 - num_bits)) & self.mask;
        self.shift(num_bits);
        value
    }
}

struct UpgradeState<'a> {
    non_ll: bool,
    srl: BitStream<'a>,
    raw: BitStream<'a>,
    kp: u32,
    nz: i32,
    mode: bool,
}

impl<'a> UpgradeState<'a> {
    fn new(srl_data: &'a [u8], raw_data: &'a [u8]) -> Self {
        Self {
            non_ll: true,
            srl: BitStream::new(srl_data),
            raw: BitStream::new(raw_data),
            kp: 8,
            nz: 0,
            mode: false,
        }
    }

    fn srl_read(&mut self, num_bits: u32) -> i16 {
        if self.nz > 0 {
            self.nz -= 1;
            return 0;
        }

        let k = self.kp / 8;

        if !self.mode {
            // zero encoding
            let bit = self.srl.peek_bit();
            self.srl.shift(1);

            if !bit {
                // '0' bit, nz >= (1 << k), nz = (1 << k)
                self.nz = (1 << k) as i32;
                self.kp += 4;
                if self.kp > 80 {
                    self.kp = 80;
                }
                self.nz -= 1;
                return 0;
            } else {
                // '1' bit, nz < (1 << k), nz = next k bits
                self.nz = 0;
                self.mode = true; // unary encoding is next

                if k > 0 {
                    self.nz = self.srl.read_bits(k) as i32;
                }

                if self.nz > 0 {
                    self.nz -= 1;
                    return 0;
                }
            }
        }

        self.mode = false; // zero encoding is next

        // unary encoding - read sign bit
        let sign = self.srl.peek_bit();
        self.srl.shift(1);

        if self.kp < 6 {
            self.kp = 0;
        } else {
            self.kp -= 6;
        }

        if num_bits == 1 {
            return if sign { -1 } else { 1 };
        }

        let mut mag = 1u32;
        let max = (1 << num_bits) - 1;

        while mag < max {
            let bit = self.srl.peek_bit();
            self.srl.shift(1);
            if bit {
                break;
            }
            mag += 1;
        }

        let mag = mag.min(i16::MAX as u32) as i16;
        if sign {
            -mag
        } else {
            mag
        }
    }

    fn upgrade_block(
        &mut self,
        buffer: &mut [i16],
        sign: &mut [i16],
        shift: i16,
        num_bits: i16,
    ) -> Result<()> {
        if num_bits == 0 {
            return Ok(());
        }

        let num_bits_u32 = num_bits as u32;

        if !self.non_ll {
            // LL3 block - read directly from raw
            for i in 0..buffer.len() {
                let input = self.raw.read_bits(num_bits_u32) as i16;
                let shifted = if shift >= 0 {
                    (input as i32) << shift
                } else {
                    (input as i32) >> -shift
                };
                buffer[i] = clamp_i16((buffer[i] as i32) + shifted);
            }
        } else {
            // Non-LL blocks - use sign array to determine source
            for i in 0..buffer.len() {
                let input = if sign[i] > 0 {
                    // sign > 0, read from raw
                    self.raw.read_bits(num_bits_u32) as i16
                } else if sign[i] < 0 {
                    // sign < 0, read from raw and negate
                    -(self.raw.read_bits(num_bits_u32) as i16)
                } else {
                    // sign == 0, read from srl
                    let val = self.srl_read(num_bits_u32);
                    sign[i] = val;
                    val
                };

                let shifted = if shift >= 0 {
                    (input as i32) << shift
                } else {
                    (input as i32) >> -shift
                };
                buffer[i] = clamp_i16((buffer[i] as i32) + shifted);
            }
        }

        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        // Read trailing bits from RAW/SRL bit streams
        let raw_pad = if self.raw.position % 8 != 0 {
            8 - (self.raw.position % 8)
        } else {
            0
        };
        if raw_pad > 0 {
            self.raw.shift(raw_pad as u32);
        }

        let srl_pad = if self.srl.position % 8 != 0 {
            8 - (self.srl.position % 8)
        } else {
            0
        };
        if srl_pad > 0 {
            self.srl.shift(srl_pad as u32);
        }

        // Skip final alignment byte if present
        if (self.srl.position / 8) + 1 == self.srl.data.len() {
            self.srl.shift(8);
        }

        Ok(())
    }
}

fn progressive_upgrade_decode(
    coefficients: &mut [i16],
    sign: &mut [i16],
    shift: &QuantLevels,
    bitpos_delta: &QuantLevels,
    extrapolate: bool,
    _prog_quant: &QuantLevels,
    srl_data: &[u8],
    raw_data: &[u8],
) -> Result<()> {
    let mut state = UpgradeState::new(srl_data, raw_data);

    let subbands: &[SubbandMeta] = if extrapolate {
        &EXTRAPOLATE_SUBBANDS
    } else {
        &STANDARD_SUBBANDS
    };

    for meta in subbands {
        let band_shift = meta.band.value(shift);
        let band_num_bits = meta.band.value(bitpos_delta);

        if band_num_bits <= 0 {
            continue;
        }

        state.non_ll = meta.band != Band::Ll3;
        let start = meta.offset;
        let end = start + meta.len;

        if end > coefficients.len() || end > sign.len() {
            return Err(ProgressiveError::Invalid(format!(
                "Subband slice [{start}..{end}] is out of bounds for coefficient/sign buffers"
            )));
        }

        state.upgrade_block(
            &mut coefficients[start..end],
            &mut sign[start..end],
            band_shift,
            band_num_bits,
        )?;
    }

    state.finish()?;
    Ok(())
}

fn parse_tile_simple_block<'a>(
    data: &mut &'a [u8],
    block_type: BlockType,
) -> Result<TileSimpleBlock<'a>> {
    ensure_progressive!(
        data.len() >= 1 + 1 + 1 + 2 + 2 + 1 + 2 + 2 + 2 + 2,
        ProgressiveError::Truncated("tile block"),
    );

    let quant_idx_y = data[0];
    let quant_idx_cb = data[1];
    let quant_idx_cr = data[2];
    let x_idx = u16::from_le_bytes([data[3], data[4]]);
    let y_idx = u16::from_le_bytes([data[5], data[6]]);
    let flags = data[7];
    let mut offset = 8;
    let quality = if block_type == BlockType::TileSimple {
        0xFF
    } else {
        ensure_progressive!(
            data.len() >= offset + 1,
            ProgressiveError::Truncated("tile quality"),
        );
        let q = data[offset];
        offset += 1;
        q
    };

    ensure_progressive!(
        data.len() >= offset + 8,
        ProgressiveError::Truncated("tile channel lengths"),
    );
    let y_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
    let cb_len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as usize;
    let cr_len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]) as usize;
    let tail_len = u16::from_le_bytes([data[offset + 6], data[offset + 7]]) as usize;
    offset += 8;

    ensure_progressive!(
        data.len() >= offset + y_len + cb_len + cr_len + tail_len,
        ProgressiveError::Truncated("tile channel data"),
    );

    let (y_data, rest) = data[offset..].split_at(y_len);
    let (cb_data, rest) = rest.split_at(cb_len);
    let (cr_data, rest) = rest.split_at(cr_len);
    let (tail_data, remaining) = rest.split_at(tail_len);

    *data = &data[data.len() - remaining.len()..];

    Ok(TileSimpleBlock {
        block_type,
        quant_idx: [quant_idx_y, quant_idx_cb, quant_idx_cr],
        x_idx,
        y_idx,
        flags,
        quality,
        y_data,
        cb_data,
        cr_data,
        tail_data,
    })
}

fn parse_tile_upgrade_block<'a>(data: &mut &'a [u8]) -> Result<TileUpgradeBlock<'a>> {
    ensure_progressive!(
        data.len() >= 1 + 1 + 1 + 2 + 2 + 1 + 2 * 6,
        ProgressiveError::Truncated("tile upgrade block"),
    );

    let quant_idx_y = data[0];
    let quant_idx_cb = data[1];
    let quant_idx_cr = data[2];
    let x_idx = u16::from_le_bytes([data[3], data[4]]);
    let y_idx = u16::from_le_bytes([data[5], data[6]]);
    let quality = data[7];
    let y_srl_len = u16::from_le_bytes([data[8], data[9]]) as usize;
    let y_raw_len = u16::from_le_bytes([data[10], data[11]]) as usize;
    let cb_srl_len = u16::from_le_bytes([data[12], data[13]]) as usize;
    let cb_raw_len = u16::from_le_bytes([data[14], data[15]]) as usize;
    let cr_srl_len = u16::from_le_bytes([data[16], data[17]]) as usize;
    let cr_raw_len = u16::from_le_bytes([data[18], data[19]]) as usize;

    let offset = 20;
    ensure_progressive!(
        data.len()
            >= offset + y_srl_len + y_raw_len + cb_srl_len + cb_raw_len + cr_srl_len + cr_raw_len,
        ProgressiveError::Truncated("tile upgrade channel data"),
    );

    let (y_srl, rest) = data[offset..].split_at(y_srl_len);
    let (y_raw, rest) = rest.split_at(y_raw_len);
    let (cb_srl, rest) = rest.split_at(cb_srl_len);
    let (cb_raw, rest) = rest.split_at(cb_raw_len);
    let (cr_srl, rest) = rest.split_at(cr_srl_len);
    let (cr_raw, remaining) = rest.split_at(cr_raw_len);

    *data = &data[data.len() - remaining.len()..];

    Ok(TileUpgradeBlock {
        block_type: BlockType::TileUpgrade,
        quant_idx: [quant_idx_y, quant_idx_cb, quant_idx_cr],
        x_idx,
        y_idx,
        quality,
        y_srl,
        y_raw,
        cb_srl,
        cb_raw,
        cr_srl,
        cr_raw,
    })
}

#[derive(Debug)]
struct TileState {
    quant: [QuantLevels; COMPONENT_COUNT],
    progressive_quant: [QuantLevels; COMPONENT_COUNT],
    bit_pos: [QuantLevels; COMPONENT_COUNT],
    coefficients: [[i16; TILE_PIXELS]; COMPONENT_COUNT],
    sign: [[i16; TILE_PIXELS]; COMPONENT_COUNT],
    reconstruction: Vec<u8>,
    dirty: bool,
    pass: u16,
    flags: u8,
    quality: u8,
    quant_idx: [u8; COMPONENT_COUNT],
}

impl TileState {
    fn new() -> Self {
        Self {
            quant: [QuantLevels::default(); COMPONENT_COUNT],
            progressive_quant: [QuantLevels::default(); COMPONENT_COUNT],
            bit_pos: [QuantLevels::default(); COMPONENT_COUNT],
            coefficients: [[0i16; TILE_PIXELS]; COMPONENT_COUNT],
            sign: [[0i16; TILE_PIXELS]; COMPONENT_COUNT],
            reconstruction: vec![0u8; TILE_PIXELS * 4],
            dirty: true,
            pass: 0,
            flags: 0,
            quality: 0xFF,
            quant_idx: [0; COMPONENT_COUNT],
        }
    }
}

#[derive(Debug)]
struct SurfaceState {
    width: u32,
    height: u32,
    grid_width: u32,
    grid_height: u32,
    tiles: HashMap<TileCoordinate, TileState>,
    frame_id: u32,
    updated_tiles: Vec<TileCoordinate>,
}

impl SurfaceState {
    fn new(width: u32, height: u32) -> Self {
        let grid_width = ((width + (TILE_SIZE as u32 - 1)) / TILE_SIZE as u32).max(1);
        let grid_height = ((height + (TILE_SIZE as u32 - 1)) / TILE_SIZE as u32).max(1);
        Self {
            width,
            height,
            grid_width,
            grid_height,
            tiles: HashMap::new(),
            frame_id: 0,
            updated_tiles: Vec::new(),
        }
    }

    fn tile_mut(&mut self, coord: TileCoordinate) -> &mut TileState {
        self.tiles.entry(coord).or_insert_with(TileState::new)
    }

    fn surface_rect_for_tile(&self, coord: TileCoordinate) -> RfxRectangle {
        let x = coord.x as u32 * TILE_SIZE as u32;
        let y = coord.y as u32 * TILE_SIZE as u32;
        let width = (((x + TILE_SIZE as u32).min(self.width)) - x) as u16;
        let height = (((y + TILE_SIZE as u32).min(self.height)) - y) as u16;
        RfxRectangle {
            x: x as u16,
            y: y as u16,
            width,
            height,
        }
    }
}

pub struct ProgressiveDecoder {
    surfaces: HashMap<u16, SurfaceState>,
    entropy: EntropyAlgorithm,
    context_flags: u8,
    full_quality_quant: ProgressiveCodecQuant,
}

impl ProgressiveDecoder {
    pub fn new(entropy: EntropyAlgorithm) -> Self {
        Self {
            surfaces: HashMap::new(),
            entropy,
            context_flags: 0,
            full_quality_quant: ProgressiveCodecQuant::default(),
        }
    }

    pub fn reset_surface(&mut self, surface_id: u16, width: u32, height: u32) {
        self.surfaces
            .insert(surface_id, SurfaceState::new(width, height));
    }

    pub fn remove_surface(&mut self, surface_id: u16) {
        self.surfaces.remove(&surface_id);
    }

    #[allow(dead_code)]
    pub fn decode_surface_update(
        &mut self,
        surface_id: u16,
        frame_index_hint: u32,
        data: &[u8],
    ) -> Result<ProgressiveSurfaceUpdate> {
        let surface = self
            .surfaces
            .get_mut(&surface_id)
            .ok_or(ProgressiveError::UnknownSurface(surface_id))?;
        surface.frame_id = frame_index_hint;
        surface.updated_tiles.clear();

        let mut cursor = data;
        let mut _sync_seen = false;
        let mut _context_seen = false;
        let mut frame_begin: Option<FrameBeginBlock> = None;
        let mut tiles = Vec::new();

        while !cursor.is_empty() {
            let header = BlockHeader::parse(&mut cursor)?;
            ensure_progressive!(
                cursor.len() >= header.length - 6,
                ProgressiveError::Truncated("progressive block body"),
            );
            let (body, rest) = cursor.split_at(header.length - 6);
            cursor = rest;

            match header.block_type {
                BlockType::Sync => {
                    let block = SyncBlock::parse(body)?;
                    _sync_seen = true;
                    if block.version < 0x0100 {
                        return Err(ProgressiveError::UnsupportedVersion(block.version));
                    }
                }
                BlockType::Context => {
                    let block = ContextBlock::parse(body)?;
                    self.context_flags = block.flags;
                    _context_seen = true;
                }
                BlockType::FrameBegin => {
                    let block = FrameBeginBlock::parse(body)?;
                    frame_begin = Some(block);
                    surface.updated_tiles.clear();
                }
                BlockType::Region => {
                    let region = RegionBlock::parse(body)?;
                    let mut tile_decoder = TileDecoder::new(
                        self.entropy,
                        self.context_flags,
                        &self.full_quality_quant,
                    );
                    tile_decoder.process_region(surface, &region, &mut tiles)?;
                }
                BlockType::FrameEnd => {
                    break;
                }
                _ => {
                    // Tile blocks should be embedded within region blocks only.
                }
            }
        }

        // SYNC and CONTEXT are only required in the first frame, not in incremental updates
        // For incremental updates, we use the previously stored context_flags

        // Return the frame index from frame_begin, or use the hint if frame_begin wasn't present
        let frame_index = frame_begin
            .map(|fb| fb.frame_index)
            .unwrap_or(frame_index_hint);

        Ok(ProgressiveSurfaceUpdate {
            surface_id,
            frame_index,
            tiles,
        })
    }
}

#[derive(Debug)]
pub struct ProgressiveSurfaceUpdate {
    pub surface_id: u16,
    pub frame_index: u32,
    pub tiles: Vec<TileUpdate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Phase 1: Block Parsing Tests

    #[test]
    fn test_parse_sync_block_valid() {
        let data = [
            0xC0, 0xCC, // blockType = SYNC (0xCCC0)
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0xCA, 0xAC, 0xCC, 0xCA, // magic = 0xCACCACCA (little-endian)
            0x00, 0x01, // version = 0x0100
        ];

        let sync = SyncBlock::parse(&data[6..]).expect("Should parse valid SYNC block");
        assert_eq!(sync.version, 0x0100, "Version should be 0x0100");
    }

    #[test]
    fn test_parse_sync_block_invalid_magic() {
        let data = [
            0xDE, 0xAD, 0xBE, 0xEF, // wrong magic
            0x00, 0x01, // version
        ];

        let result = SyncBlock::parse(&data);
        assert!(result.is_err(), "Should fail with invalid magic");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ProgressiveError::Invalid(_)),
            "Should be Invalid error"
        );
    }

    #[test]
    fn test_parse_sync_block_truncated() {
        let data = [
            0xCA, 0xCC, 0xAC, 0xCA, // magic only
        ];

        let result = SyncBlock::parse(&data);
        assert!(result.is_err(), "Should fail with truncated data");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_parse_context_block_valid() {
        let data = [
            0x01, // contextId = 1
            0x40, 0x00, // tileSize = 64
            0x01, // flags = RFX_DWT_REDUCE_EXTRAPOLATE
        ];

        let context = ContextBlock::parse(&data).expect("Should parse valid CONTEXT block");
        assert_eq!(context.context_id, 1);
        assert_eq!(context.tile_size, 64);
        assert_eq!(context.flags, 1);
    }

    #[test]
    fn test_parse_context_block_truncated() {
        let data = [
            0x01, // contextId only
        ];

        let result = ContextBlock::parse(&data);
        assert!(result.is_err(), "Should fail with truncated data");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_parse_frame_begin_block_valid() {
        let data = [
            0x01, 0x00, 0x00, 0x00, // frameIndex = 1
            0x02, 0x00, // regionCount = 2
        ];

        let frame_begin =
            FrameBeginBlock::parse(&data).expect("Should parse valid FRAME_BEGIN block");
        assert_eq!(frame_begin.frame_index, 1);
        assert_eq!(frame_begin.region_count, 2);
    }

    #[test]
    fn test_parse_frame_begin_block_truncated() {
        let data = [
            0x01, 0x00, 0x00, 0x00, // frameIndex only
        ];

        let result = FrameBeginBlock::parse(&data);
        assert!(result.is_err(), "Should fail with truncated data");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_parse_region_block_valid() {
        let data = [
            0x40, // tileSize = 64
            0x01, 0x00, // numRects = 1
            0x01, // numQuant = 1
            0x00, // numProgQuant = 0
            0x00, // flags = 0
            0x01, 0x00, // numTiles = 1
            0x00, 0x00, 0x00, 0x00, // tileDataSize = 0
            // rect
            0x00, 0x00, // x = 0
            0x00, 0x00, // y = 0
            0x40, 0x00, // width = 64
            0x40, 0x00, // height = 64
            // quant (5 bytes)
            0x66, // LL3/LH3
            0x66, // HL3/HH3
            0x66, // LH2/HL2
            0x66, // HH2/LH1
            0x66, // HL1/HH1
        ];

        let region = RegionBlock::parse(&data).expect("Should parse valid REGION block");
        assert_eq!(region.tile_size, 64);
        assert_eq!(region.num_rects, 1);
        assert_eq!(region.num_tiles, 1);
        assert_eq!(region.rects.len(), 1);
        assert_eq!(region.quant_values.len(), 1);
        assert_eq!(region.progressive_quants.len(), 0);
    }

    #[test]
    fn test_parse_region_block_zero_rects() {
        let data = [
            0x40, // tileSize = 64
            0x00, 0x00, // numRects = 0
            0x01, // numQuant = 1
            0x00, // numProgQuant = 0
            0x00, // flags = 0
            0x01, 0x00, // numTiles = 1
            0x00, 0x00, 0x00, 0x00, // tileDataSize = 0
            // quant (5 bytes)
            0x66, 0x66, 0x66, 0x66, 0x66,
        ];

        let region = RegionBlock::parse(&data).expect("Should parse region with zero rects");
        assert_eq!(region.num_rects, 0);
        assert_eq!(region.rects.len(), 0);
    }

    #[test]
    fn test_parse_region_block_truncated_rects() {
        let data = [
            0x40, // tileSize = 64
            0x02, 0x00, // numRects = 2
            0x01, // numQuant = 1
            0x00, // numProgQuant = 0
            0x00, // flags = 0
            0x01, 0x00, // numTiles = 1
            0x00, 0x00, 0x00, 0x00, // tileDataSize = 0
            // Only 1 rect provided, but claimed 2
            0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x40, 0x00,
        ];

        let result = RegionBlock::parse(&data);
        assert!(result.is_err(), "Should fail with truncated rects");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_parse_tile_simple_block_valid() {
        let mut data = vec![
            0x00, // quantIdxY = 0
            0x00, // quantIdxCb = 0
            0x00, // quantIdxCr = 0
            0x00, 0x00, // xIdx = 0
            0x00, 0x00, // yIdx = 0
            0x00, // flags = 0
            // No quality field for TILE_SIMPLE
            0x0A, 0x00, // yLen = 10
            0x05, 0x00, // cbLen = 5
            0x05, 0x00, // crLen = 5
            0x00, 0x00, // tailLen = 0
        ];
        // Add y data (10 bytes)
        data.extend_from_slice(&[0u8; 10]);
        // Add cb data (5 bytes)
        data.extend_from_slice(&[0u8; 5]);
        // Add cr data (5 bytes)
        data.extend_from_slice(&[0u8; 5]);

        let mut slice = data.as_slice();
        let block = parse_tile_simple_block(&mut slice, BlockType::TileSimple)
            .expect("Should parse valid TILE_SIMPLE block");
        assert_eq!(block.quant_idx, [0, 0, 0]);
        assert_eq!(block.x_idx, 0);
        assert_eq!(block.y_idx, 0);
        assert_eq!(block.flags, 0);
        assert_eq!(block.quality, 0xFF); // Default for TILE_SIMPLE
        assert_eq!(block.y_data.len(), 10);
        assert_eq!(block.cb_data.len(), 5);
        assert_eq!(block.cr_data.len(), 5);
    }

    #[test]
    fn test_parse_tile_first_block_valid() {
        let mut data = vec![
            0x00, // quantIdxY = 0
            0x00, // quantIdxCb = 0
            0x00, // quantIdxCr = 0
            0x01, 0x00, // xIdx = 1
            0x02, 0x00, // yIdx = 2
            0x00, // flags = 0
            0x32, // quality = 50 (TILE_FIRST has quality field)
            0x0A, 0x00, // yLen = 10
            0x05, 0x00, // cbLen = 5
            0x05, 0x00, // crLen = 5
            0x00, 0x00, // tailLen = 0
        ];
        data.extend_from_slice(&[0u8; 20]); // y + cb + cr data

        let mut slice = data.as_slice();
        let block = parse_tile_simple_block(&mut slice, BlockType::TileFirst)
            .expect("Should parse valid TILE_FIRST block");
        assert_eq!(block.x_idx, 1);
        assert_eq!(block.y_idx, 2);
        assert_eq!(block.quality, 50);
    }

    #[test]
    fn test_parse_tile_simple_block_truncated() {
        let data = [
            0x00, 0x00, 0x00, // quantIdx
            0x00, 0x00, // xIdx
            0x00, 0x00, // yIdx
            0x00, // flags
            // Missing length fields
        ];

        let mut slice = data.as_slice();
        let result = parse_tile_simple_block(&mut slice, BlockType::TileSimple);
        assert!(result.is_err(), "Should fail with truncated data");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_parse_tile_upgrade_block_valid() {
        let mut data = vec![
            0x00, // quantIdxY = 0
            0x00, // quantIdxCb = 0
            0x00, // quantIdxCr = 0
            0x00, 0x00, // xIdx = 0
            0x00, 0x00, // yIdx = 0
            0x64, // quality = 100
            0x05, 0x00, // ySrlLen = 5
            0x05, 0x00, // yRawLen = 5
            0x03, 0x00, // cbSrlLen = 3
            0x03, 0x00, // cbRawLen = 3
            0x03, 0x00, // crSrlLen = 3
            0x03, 0x00, // crRawLen = 3
        ];
        // Add all data streams
        data.extend_from_slice(&[0u8; 5]); // ySrl
        data.extend_from_slice(&[0u8; 5]); // yRaw
        data.extend_from_slice(&[0u8; 3]); // cbSrl
        data.extend_from_slice(&[0u8; 3]); // cbRaw
        data.extend_from_slice(&[0u8; 3]); // crSrl
        data.extend_from_slice(&[0u8; 3]); // crRaw

        let mut slice = data.as_slice();
        let block =
            parse_tile_upgrade_block(&mut slice).expect("Should parse valid TILE_UPGRADE block");
        assert_eq!(block.quant_idx, [0, 0, 0]);
        assert_eq!(block.x_idx, 0);
        assert_eq!(block.y_idx, 0);
        assert_eq!(block.quality, 100);
        assert_eq!(block.y_srl.len(), 5);
        assert_eq!(block.y_raw.len(), 5);
        assert_eq!(block.cb_srl.len(), 3);
        assert_eq!(block.cb_raw.len(), 3);
        assert_eq!(block.cr_srl.len(), 3);
        assert_eq!(block.cr_raw.len(), 3);
    }

    #[test]
    fn test_parse_tile_upgrade_block_truncated() {
        let data = [
            0x00, 0x00, 0x00, // quantIdx
            0x00, 0x00, // xIdx
            0x00, 0x00, // yIdx
            0x64, // quality
            0x05, 0x00, // ySrlLen = 5
            // Missing rest of length fields
        ];

        let mut slice = data.as_slice();
        let result = parse_tile_upgrade_block(&mut slice);
        assert!(result.is_err(), "Should fail with truncated data");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_block_header_parse_valid() {
        let mut data: &[u8] = &[
            0xC0, 0xCC, // blockType = SYNC
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0xCA, 0xCC, 0xAC, 0xCA, // payload (magic)
            0x00, 0x01, // payload (version)
        ];

        let header = BlockHeader::parse(&mut data).expect("Should parse valid block header");
        assert_eq!(header.block_type, BlockType::Sync);
        assert_eq!(header.length, 12);
        assert_eq!(data.len(), 6, "Should consume 6 bytes from header");
    }

    #[test]
    fn test_block_header_parse_invalid_type() {
        let mut data: &[u8] = &[
            0xFF, 0xFF, // invalid blockType
            0x0C, 0x00, 0x00, 0x00, // blockLen
        ];

        let result = BlockHeader::parse(&mut data);
        assert!(result.is_err(), "Should fail with invalid block type");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Invalid(_)),
            "Should be Invalid error"
        );
    }

    #[test]
    fn test_block_header_parse_too_short_length() {
        let mut data: &[u8] = &[
            0xC0, 0xCC, // blockType = SYNC
            0x05, 0x00, 0x00, 0x00, // blockLen = 5 (< 6, invalid)
        ];

        let result = BlockHeader::parse(&mut data);
        assert!(result.is_err(), "Should fail with length < 6");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Invalid(_)),
            "Should be Invalid error"
        );
    }

    #[test]
    fn test_block_type_from_u16() {
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_SYNC).unwrap(),
            BlockType::Sync
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_FRAME_BEGIN).unwrap(),
            BlockType::FrameBegin
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_FRAME_END).unwrap(),
            BlockType::FrameEnd
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_CONTEXT).unwrap(),
            BlockType::Context
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_REGION).unwrap(),
            BlockType::Region
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_TILE_SIMPLE).unwrap(),
            BlockType::TileSimple
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_TILE_FIRST).unwrap(),
            BlockType::TileFirst
        );
        assert_eq!(
            BlockType::from_u16(PROGRESSIVE_WBT_TILE_UPGRADE).unwrap(),
            BlockType::TileUpgrade
        );

        assert!(BlockType::from_u16(0xFFFF).is_err(), "Should fail for unknown type");
    }

    #[test]
    fn test_progressive_codec_quant_parse() {
        let data = [
            0x64, // quality = 100
            // Y component
            0x66, 0x66, 0x66, 0x66, 0x66, // Cb component
            0x55, 0x55, 0x55, 0x55, 0x55, // Cr component
            0x44, 0x44, 0x44, 0x44, 0x44,
        ];

        let mut slice = data.as_slice();
        let quant = ProgressiveCodecQuant::parse(&mut slice)
            .expect("Should parse progressive codec quant");
        assert_eq!(quant.quality, 100);
        // Verify some quant values were parsed
        assert_eq!(quant.y.ll3, 6); // 0x66 & 0x0F
        assert_eq!(quant.y.lh3, 6); // 0x66 >> 4
    }

    #[test]
    fn test_progressive_codec_quant_parse_truncated() {
        let data = [
            0x64, // quality only
        ];

        let mut slice = data.as_slice();
        let result = ProgressiveCodecQuant::parse(&mut slice);
        assert!(result.is_err(), "Should fail with truncated data");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    // ========================================================================
    // Phase 2: Decoding Logic Tests
    // ========================================================================

    #[test]
    fn test_quant_levels_from_quant() {
        let quant = Quant {
            ll3: 5,
            lh3: 7,
            hl3: 6,
            hh3: 8,
            lh2: 4,
            hl2: 3,
            hh2: 5,
            lh1: 2,
            hl1: 1,
            hh1: 3,
        };

        let levels = QuantLevels::from_quant(&quant);
        assert_eq!(levels.ll3, 5, "LL3 should match");
        assert_eq!(levels.lh3, 7, "LH3 should match");
        assert_eq!(levels.hl3, 6, "HL3 should match");
        assert_eq!(levels.hh3, 8, "HH3 should match");
        assert_eq!(levels.lh2, 4, "LH2 should match");
        assert_eq!(levels.hl2, 3, "HL2 should match");
        assert_eq!(levels.hh2, 5, "HH2 should match");
        assert_eq!(levels.lh1, 2, "LH1 should match");
        assert_eq!(levels.hl1, 1, "HL1 should match");
        assert_eq!(levels.hh1, 3, "HH1 should match");
    }

    #[test]
    fn test_quant_levels_add() {
        let a = QuantLevels {
            ll3: 5,
            lh3: 7,
            hl3: 6,
            hh3: 8,
            lh2: 4,
            hl2: 3,
            hh2: 5,
            lh1: 2,
            hl1: 1,
            hh1: 3,
        };

        let b = QuantLevels {
            ll3: 2,
            lh3: 3,
            hl3: 1,
            hh3: 4,
            lh2: 1,
            hl2: 2,
            hh2: 3,
            lh1: 1,
            hl1: 2,
            hh1: 1,
        };

        let result = a.add(&b);
        assert_eq!(result.ll3, 7, "LL3 should be 5 + 2");
        assert_eq!(result.lh3, 10, "LH3 should be 7 + 3");
        assert_eq!(result.hl3, 7, "HL3 should be 6 + 1");
        assert_eq!(result.hh3, 12, "HH3 should be 8 + 4");
        assert_eq!(result.lh2, 5, "LH2 should be 4 + 1");
        assert_eq!(result.hl2, 5, "HL2 should be 3 + 2");
        assert_eq!(result.hh2, 8, "HH2 should be 5 + 3");
        assert_eq!(result.lh1, 3, "LH1 should be 2 + 1");
        assert_eq!(result.hl1, 3, "HL1 should be 1 + 2");
        assert_eq!(result.hh1, 4, "HH1 should be 3 + 1");
    }

    #[test]
    fn test_quant_levels_sub() {
        let a = QuantLevels {
            ll3: 10,
            lh3: 12,
            hl3: 8,
            hh3: 15,
            lh2: 9,
            hl2: 7,
            hh2: 11,
            lh1: 6,
            hl1: 5,
            hh1: 8,
        };

        let b = QuantLevels {
            ll3: 2,
            lh3: 3,
            hl3: 1,
            hh3: 4,
            lh2: 1,
            hl2: 2,
            hh2: 3,
            lh1: 1,
            hl1: 2,
            hh1: 1,
        };

        let result = a.sub(&b);
        assert_eq!(result.ll3, 8, "LL3 should be 10 - 2");
        assert_eq!(result.lh3, 9, "LH3 should be 12 - 3");
        assert_eq!(result.hl3, 7, "HL3 should be 8 - 1");
        assert_eq!(result.hh3, 11, "HH3 should be 15 - 4");
        assert_eq!(result.lh2, 8, "LH2 should be 9 - 1");
        assert_eq!(result.hl2, 5, "HL2 should be 7 - 2");
        assert_eq!(result.hh2, 8, "HH2 should be 11 - 3");
        assert_eq!(result.lh1, 5, "LH1 should be 6 - 1");
        assert_eq!(result.hl1, 3, "HL1 should be 5 - 2");
        assert_eq!(result.hh1, 7, "HH1 should be 8 - 1");
    }

    #[test]
    fn test_quant_levels_sub_scalar() {
        let a = QuantLevels {
            ll3: 10,
            lh3: 12,
            hl3: 8,
            hh3: 15,
            lh2: 9,
            hl2: 7,
            hh2: 11,
            lh1: 6,
            hl1: 5,
            hh1: 8,
        };

        let result = a.sub_scalar(3);
        assert_eq!(result.ll3, 7, "LL3 should be 10 - 3");
        assert_eq!(result.lh3, 9, "LH3 should be 12 - 3");
        assert_eq!(result.hl3, 5, "HL3 should be 8 - 3");
        assert_eq!(result.hh3, 12, "HH3 should be 15 - 3");
        assert_eq!(result.lh2, 6, "LH2 should be 9 - 3");
        assert_eq!(result.hl2, 4, "HL2 should be 7 - 3");
        assert_eq!(result.hh2, 8, "HH2 should be 11 - 3");
        assert_eq!(result.lh1, 3, "LH1 should be 6 - 3");
        assert_eq!(result.hl1, 2, "HL1 should be 5 - 3");
        assert_eq!(result.hh1, 5, "HH1 should be 8 - 3");
    }

    #[test]
    fn test_apply_quant_shift_standard() {
        // Create a buffer with known values
        let mut buffer = vec![0i16; 4096];
        
        // Set some test values in different subbands based on actual layout
        // HL1: offset 0, len 1024
        buffer[0] = 16;
        buffer[100] = 32;
        
        // LH1: offset 1024, len 1024
        buffer[1024] = 8;
        buffer[1025] = 16;
        
        // LL3: offset 4032, len 64
        buffer[4032] = 4;
        buffer[4033] = 8;

        let quant = QuantLevels {
            hl1: 1, // shift by 1
            lh1: 2, // shift by 2
            hh1: 0, // no shift
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            ll3: 3, // shift by 3
        };

        apply_quant_shift_standard(&mut buffer, &quant);

        // Verify shifts were applied correctly
        assert_eq!(buffer[0], 16 << 1, "HL1 should be shifted left by 1");
        assert_eq!(buffer[100], 32 << 1, "HL1 should be shifted left by 1");
        
        assert_eq!(buffer[1024], 8 << 2, "LH1 should be shifted left by 2");
        assert_eq!(buffer[1025], 16 << 2, "LH1 should be shifted left by 2");
        
        assert_eq!(buffer[4032], 4 << 3, "LL3 should be shifted left by 3");
        assert_eq!(buffer[4033], 8 << 3, "LL3 should be shifted left by 3");
    }

    #[test]
    fn test_apply_quant_shift_extrapolate() {
        // Create a buffer with known values
        let mut buffer = vec![0i16; 4096];
        
        // Set some test values in different subbands for extrapolate layout
        // HL1: offset 0, len 1023
        buffer[0] = 16;
        buffer[100] = 32;
        
        // LL3: offset 4015, len 81 (extrapolate has larger LL3)
        buffer[4015] = 4;
        buffer[4016] = 8;
        buffer[4095] = 12; // last coefficient

        let quant = QuantLevels {
            hl1: 1, // shift by 1
            lh1: 0,
            hh1: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            ll3: 2, // shift by 2
        };

        apply_quant_shift_extrapolate(&mut buffer, &quant);

        // Verify shifts were applied correctly
        assert_eq!(buffer[0], 16 << 1, "HL1 should be shifted left by 1");
        assert_eq!(buffer[100], 32 << 1, "HL1 should be shifted left by 1");
        
        assert_eq!(buffer[4015], 4 << 2, "LL3 should be shifted left by 2");
        assert_eq!(buffer[4016], 8 << 2, "LL3 should be shifted left by 2");
        assert_eq!(buffer[4095], 12 << 2, "LL3 last coeff should be shifted");
    }

    #[test]
    fn test_subband_metadata_standard() {
        // Verify STANDARD_SUBBANDS metadata is correct
        let total_coeffs: usize = STANDARD_SUBBANDS.iter().map(|m| m.len).sum();
        assert_eq!(total_coeffs, 4096, "Standard layout should have 4096 coefficients");

        // Check HL1 is first
        assert_eq!(STANDARD_SUBBANDS[0].offset, 0, "HL1 should start at 0");
        assert_eq!(STANDARD_SUBBANDS[0].len, 1024, "HL1 should have 1024 coeffs");

        // Check LL3 is last and 64 coefficients
        let ll3 = STANDARD_SUBBANDS.iter().find(|m| matches!(m.band, Band::Ll3)).unwrap();
        assert_eq!(ll3.len, 64, "LL3 should have 64 coeffs in standard layout");
        assert_eq!(ll3.offset, 4032, "LL3 should start at offset 4032");
    }

    #[test]
    fn test_subband_metadata_extrapolate() {
        // Verify EXTRAPOLATE_SUBBANDS metadata is correct
        let total_coeffs: usize = EXTRAPOLATE_SUBBANDS.iter().map(|m| m.len).sum();
        assert_eq!(total_coeffs, 4096, "Extrapolate layout should have 4096 coefficients");

        // Check HL1 is first
        assert_eq!(EXTRAPOLATE_SUBBANDS[0].offset, 0, "HL1 should start at 0");
        assert_eq!(EXTRAPOLATE_SUBBANDS[0].len, 1023, "HL1 should have 1023 coeffs in extrapolate");

        // Check LL3 is last and 81 coefficients (larger than standard)
        let ll3 = EXTRAPOLATE_SUBBANDS.iter().find(|m| matches!(m.band, Band::Ll3)).unwrap();
        assert_eq!(ll3.len, 81, "LL3 should have 81 coeffs in extrapolate layout");
        assert_eq!(ll3.offset, 4015, "LL3 should start at offset 4015");
    }

    #[test]
    fn test_rfx_differential_decode_standard() {
        // Test differential decoding for standard layout (LL3 = 64 coeffs)
        let mut buffer = vec![0i16; 4096];
        
        // Set LL3 subband with differential values
        // LL3 starts at offset 4032, length 64
        buffer[4032] = 10; // First value
        buffer[4033] = 5;  // Diff from previous
        buffer[4034] = -3; // Diff from previous
        buffer[4035] = 7;  // Diff from previous

        subband_reconstruction::decode(&mut buffer[4032..]);

        // After differential decode:
        // buffer[4032] = 10 (unchanged)
        // buffer[4033] = 10 + 5 = 15
        // buffer[4034] = 15 + (-3) = 12
        // buffer[4035] = 12 + 7 = 19
        assert_eq!(buffer[4032], 10, "First LL3 value unchanged");
        assert_eq!(buffer[4033], 15, "Second LL3 value accumulated");
        assert_eq!(buffer[4034], 12, "Third LL3 value accumulated");
        assert_eq!(buffer[4035], 19, "Fourth LL3 value accumulated");
    }

    #[test]
    fn test_rfx_differential_decode_extrapolate() {
        // Test differential decoding for extrapolate layout (LL3 = 81 coeffs)
        let mut buffer = vec![0i16; 4096];
        
        // Set LL3 subband with differential values
        // LL3 starts at offset 4015, length 81
        buffer[4015] = 20; // First value
        buffer[4016] = 8;  // Diff from previous
        buffer[4017] = -5; // Diff from previous
        buffer[4018] = 10; // Diff from previous

        rfx_differential_decode_extrapolate(&mut buffer);

        // After differential decode:
        // buffer[4015] = 20 (unchanged)
        // buffer[4016] = 20 + 8 = 28
        // buffer[4017] = 28 + (-5) = 23
        // buffer[4018] = 23 + 10 = 33
        assert_eq!(buffer[4015], 20, "First LL3 value unchanged");
        assert_eq!(buffer[4016], 28, "Second LL3 value accumulated");
        assert_eq!(buffer[4017], 23, "Third LL3 value accumulated");
        assert_eq!(buffer[4018], 33, "Fourth LL3 value accumulated");
    }

    #[test]
    fn test_progressive_codec_quant_quality() {
        let data = [
            0x64, // quality = 100
            0x00, 0x00, 0x00, 0x00, 0x00, // Y quant (all zeros)
            0x00, 0x00, 0x00, 0x00, 0x00, // Cb quant (all zeros)
            0x00, 0x00, 0x00, 0x00, 0x00, // Cr quant (all zeros)
        ];

        let mut slice = data.as_slice();
        let quant = ProgressiveCodecQuant::parse(&mut slice).expect("Should parse quality 100");
        assert_eq!(quant.quality, 100, "Quality should be 100");

        // Test with different quality value
        let data2 = [
            0x32, // quality = 50
            0x12, 0x34, 0x56, 0x78, 0x9A, // Y quant
            0x00, 0x00, 0x00, 0x00, 0x00, // Cb quant
            0x00, 0x00, 0x00, 0x00, 0x00, // Cr quant
        ];

        let mut slice2 = data2.as_slice();
        let quant2 = ProgressiveCodecQuant::parse(&mut slice2).expect("Should parse quality 50");
        assert_eq!(quant2.quality, 50, "Quality should be 50");
    }

    #[test]
    fn test_block_type_all_variants() {
        // Test all valid block types
        assert_eq!(BlockType::from_u16(0xCCC0).unwrap(), BlockType::Sync);
        assert_eq!(BlockType::from_u16(0xCCC1).unwrap(), BlockType::FrameBegin);
        assert_eq!(BlockType::from_u16(0xCCC2).unwrap(), BlockType::FrameEnd);
        assert_eq!(BlockType::from_u16(0xCCC3).unwrap(), BlockType::Context);
        assert_eq!(BlockType::from_u16(0xCCC4).unwrap(), BlockType::Region);
        assert_eq!(BlockType::from_u16(0xCCC5).unwrap(), BlockType::TileSimple);
        assert_eq!(BlockType::from_u16(0xCCC6).unwrap(), BlockType::TileFirst);
        assert_eq!(BlockType::from_u16(0xCCC7).unwrap(), BlockType::TileUpgrade);

        // Test invalid block types
        assert!(BlockType::from_u16(0x0000).is_err(), "0x0000 should be invalid");
        assert!(BlockType::from_u16(0xFFFF).is_err(), "0xFFFF should be invalid");
        assert!(BlockType::from_u16(0xCCBF).is_err(), "0xCCBF should be invalid");
        assert!(BlockType::from_u16(0xCCC8).is_err(), "0xCCC8 should be invalid");
    }

    #[test]
    fn test_progressive_magic_constant() {
        // Verify the PROGRESSIVE_MAGIC constant is correct
        assert_eq!(PROGRESSIVE_MAGIC, 0xCACCACCA, "Magic should be 0xCACCACCA");
    }

    #[test]
    fn test_region_flags() {
        // Test RFX_TILE_DIFFERENCE flag
        const RFX_TILE_DIFFERENCE: u8 = 0x01;
        let flags_with_diff = RFX_TILE_DIFFERENCE;
        assert_eq!(
            flags_with_diff & RFX_TILE_DIFFERENCE,
            RFX_TILE_DIFFERENCE,
            "Flag should be set"
        );

        let flags_without_diff = 0x00;
        assert_eq!(
            flags_without_diff & RFX_TILE_DIFFERENCE,
            0,
            "Flag should not be set"
        );
    }

    #[test]
    fn test_context_flags_extrapolate() {
        // Test RFX_DWT_REDUCE_EXTRAPOLATE flag (bit 0 of flags)
        const RFX_DWT_REDUCE_EXTRAPOLATE: u8 = 0x01;
        
        let flags_with_extrapolate = RFX_DWT_REDUCE_EXTRAPOLATE;
        assert_eq!(
            flags_with_extrapolate & RFX_DWT_REDUCE_EXTRAPOLATE,
            RFX_DWT_REDUCE_EXTRAPOLATE,
            "Extrapolate flag should be set"
        );

        let flags_without_extrapolate = 0x00;
        assert_eq!(
            flags_without_extrapolate & RFX_DWT_REDUCE_EXTRAPOLATE,
            0,
            "Extrapolate flag should not be set"
        );
    }

    // ========================================================================
    // Phase 3: Integration & Error Handling Tests
    // ========================================================================

    #[test]
    fn test_progressive_decoder_creation() {
        let decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        // Should create successfully with no surfaces
        assert!(decoder.surfaces.is_empty(), "New decoder should have no surfaces");
    }

    #[test]
    fn test_progressive_decoder_add_remove_surface() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        
        // Add a surface
        decoder.reset_surface(1, 1024, 768);
        assert!(decoder.surfaces.contains_key(&1), "Surface 1 should exist");
        
        // Add another surface
        decoder.reset_surface(2, 800, 600);
        assert!(decoder.surfaces.contains_key(&2), "Surface 2 should exist");
        
        // Remove surface
        decoder.remove_surface(1);
        assert!(!decoder.surfaces.contains_key(&1), "Surface 1 should be removed");
        assert!(decoder.surfaces.contains_key(&2), "Surface 2 should still exist");
    }

    #[test]
    fn test_decode_unknown_surface() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        
        // Try to decode on non-existent surface
        let data = [
            0xC0, 0xCC, // SYNC block type
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0xCA, 0xAC, 0xCC, 0xCA, // magic
            0x00, 0x01, // version
        ];
        
        let result = decoder.decode_surface_update(999, 0, &data);
        assert!(result.is_err(), "Should fail with unknown surface");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::UnknownSurface(999)),
            "Should be UnknownSurface error"
        );
    }

    #[test]
    fn test_decode_sync_block_only() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // Minimal valid stream with just SYNC block
        let data = [
            0xC0, 0xCC, // SYNC block type
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0xCA, 0xAC, 0xCC, 0xCA, // magic (little-endian)
            0x00, 0x01, // version = 0x0100
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_ok(), "Should parse SYNC block successfully");
        
        let update = result.unwrap();
        assert_eq!(update.surface_id, 1, "Surface ID should match");
        assert_eq!(update.tiles.len(), 0, "Should have no tiles");
    }

    #[test]
    fn test_decode_sync_unsupported_version() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // SYNC block with old version
        let data = [
            0xC0, 0xCC, // SYNC block type
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0xCA, 0xAC, 0xCC, 0xCA, // magic
            0x00, 0x00, // version = 0x0000 (unsupported)
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_err(), "Should fail with unsupported version");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::UnsupportedVersion(0)),
            "Should be UnsupportedVersion error"
        );
    }

    #[test]
    fn test_decode_sync_and_context() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // SYNC + CONTEXT blocks
        let data = [
            // SYNC block
            0xC0, 0xCC, // block type
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0xCA, 0xAC, 0xCC, 0xCA, // magic
            0x00, 0x01, // version
            
            // CONTEXT block
            0xC3, 0xCC, // block type = CONTEXT (0xCCC3)
            0x0A, 0x00, 0x00, 0x00, // blockLen = 10
            0x01, // contextId
            0x40, // tileSize = 64
            0x00, 0x00, // flags = 0 (standard DWT)
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_ok(), "Should parse SYNC + CONTEXT successfully");
        
        // Context flags should be stored
        assert_eq!(decoder.context_flags, 0, "Context flags should be 0");
    }

    #[test]
    fn test_decode_context_with_extrapolate_flag() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // CONTEXT block with extrapolate flag (flags is 1 byte, not 2)
        let data = [
            0xC3, 0xCC, // block type = CONTEXT
            0x0A, 0x00, 0x00, 0x00, // blockLen = 10
            0x01, // contextId
            0x40, 0x00, // tileSize = 64 (little-endian)
            0x01, // flags = 0x01 (RFX_DWT_REDUCE_EXTRAPOLATE)
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_ok(), "Should parse CONTEXT with extrapolate flag");
        
        // Context flags should be stored
        assert_eq!(decoder.context_flags, 1, "Context flags should be 1");
    }

    #[test]
    fn test_decode_frame_begin_and_end() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // FRAME_BEGIN + FRAME_END
        let data = [
            // FRAME_BEGIN
            0xC1, 0xCC, // block type = FRAME_BEGIN (0xCCC1)
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12
            0x2A, 0x00, 0x00, 0x00, // frameIndex = 42
            0x01, 0x00, // regionCount = 1
            
            // FRAME_END
            0xC2, 0xCC, // block type = FRAME_END (0xCCC2)
            0x06, 0x00, 0x00, 0x00, // blockLen = 6
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_ok(), "Should parse FRAME_BEGIN + FRAME_END");
        
        let update = result.unwrap();
        assert_eq!(update.frame_index, 42, "Frame index should be 42");
    }

    #[test]
    fn test_decode_truncated_block_header() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // Truncated block header (only 4 bytes instead of 6)
        let data = [
            0xC0, 0xCC, // block type
            0x0C, 0x00, // incomplete length field
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_err(), "Should fail with truncated header");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_decode_truncated_block_body() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // Block header claims 12 bytes, but only 6 bytes of body provided
        let data = [
            0xC0, 0xCC, // block type = SYNC
            0x0C, 0x00, 0x00, 0x00, // blockLen = 12 (but only 6 bytes follow)
            0xCA, 0xAC, 0xCC, 0xCA, // magic (4 bytes)
            0x00, // only 1 byte of version instead of 2
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_err(), "Should fail with truncated body");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Truncated(_)),
            "Should be Truncated error"
        );
    }

    #[test]
    fn test_decode_invalid_block_type() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // Invalid block type (0xCCBF, before SYNC range)
        let data = [
            0xBF, 0xCC, // invalid block type
            0x06, 0x00, 0x00, 0x00, // blockLen = 6
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_err(), "Should fail with invalid block type");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Invalid(_)),
            "Should be Invalid error"
        );
    }

    #[test]
    fn test_decode_multiple_blocks_sequence() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        // SYNC + CONTEXT + FRAME_BEGIN + FRAME_END sequence
        let data = [
            // SYNC
            0xC0, 0xCC,
            0x0C, 0x00, 0x00, 0x00,
            0xCA, 0xAC, 0xCC, 0xCA,
            0x00, 0x01,
            
            // CONTEXT
            0xC3, 0xCC,
            0x0A, 0x00, 0x00, 0x00,
            0x01,
            0x40,
            0x00, 0x00,
            
            // FRAME_BEGIN
            0xC1, 0xCC,
            0x0C, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00,
            0x00, 0x00,
            
            // FRAME_END
            0xC2, 0xCC,
            0x06, 0x00, 0x00, 0x00,
        ];
        
        let result = decoder.decode_surface_update(1, 0, &data);
        assert!(result.is_ok(), "Should parse complete frame sequence");
        
        let update = result.unwrap();
        assert_eq!(update.frame_index, 1, "Frame index should be 1");
        assert_eq!(update.surface_id, 1, "Surface ID should be 1");
    }

    #[test]
    fn test_decode_empty_data() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        decoder.reset_surface(1, 1024, 768);
        
        let data: &[u8] = &[];
        let result = decoder.decode_surface_update(1, 0, data);
        
        // Empty data should succeed with no tiles
        assert!(result.is_ok(), "Empty data should be valid");
        let update = result.unwrap();
        assert_eq!(update.tiles.len(), 0, "Should have no tiles");
    }

    #[test]
    fn test_block_header_minimum_length_check() {
        // Test that blockLen must be at least 6
        let data = [
            0xC0, 0xCC, // block type = SYNC
            0x05, 0x00, 0x00, 0x00, // blockLen = 5 (too short!)
        ];
        
        let mut slice = &data[..];
        let result = BlockHeader::parse(&mut slice);
        assert!(result.is_err(), "Should reject blockLen < 6");
        assert!(
            matches!(result.unwrap_err(), ProgressiveError::Invalid(_)),
            "Should be Invalid error for short length"
        );
    }

    #[test]
    fn test_surface_state_reset() {
        let mut decoder = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        
        // Create surface with one size
        decoder.reset_surface(1, 800, 600);
        assert!(decoder.surfaces.contains_key(&1), "Surface 1 should exist");
        
        // Reset same surface with different size
        decoder.reset_surface(1, 1024, 768);
        assert!(decoder.surfaces.contains_key(&1), "Surface 1 should still exist");
        
        // Surface should be reset - tiles HashMap starts empty and grows on demand
        let surface = decoder.surfaces.get(&1).unwrap();
        assert_eq!(surface.grid_width, 16, "Should have 16 tile columns for 1024 width");
        assert_eq!(surface.grid_height, 12, "Should have 12 tile rows for 768 height");
        assert_eq!(surface.width, 1024, "Width should be 1024");
        assert_eq!(surface.height, 768, "Height should be 768");
    }

    #[test]
    fn test_entropy_algorithm_storage() {
        let decoder_rlgr1 = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr1);
        assert!(
            matches!(decoder_rlgr1.entropy, EntropyAlgorithm::Rlgr1),
            "Should store Rlgr1"
        );
        
        let decoder_rlgr3 = ProgressiveDecoder::new(EntropyAlgorithm::Rlgr3);
        assert!(
            matches!(decoder_rlgr3.entropy, EntropyAlgorithm::Rlgr3),
            "Should store Rlgr3"
        );
    }
}
