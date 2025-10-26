#![allow(dead_code)]

use std::collections::HashMap;
use std::convert::TryInto;

use ironrdp_pdu::codecs::rfx::{EntropyAlgorithm, Quant, RfxRectangle};
use thiserror::Error;

#[allow(unused_imports)]
use crate::{
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
        debug_assert!(dst_band.len() >= dst_step * dst_count);
        debug_assert!(low_band.len() >= low_step * low_count);
        debug_assert!(high_band.len() >= high_step * high_count);

        for row in 0..dst_count {
            let mut p_l = row * low_step;
            let mut p_h = row * high_step;
            let mut p_x = row * dst_step;

            let mut h0 = high_band[p_h];
            p_h += 1;
            let mut l0 = low_band[p_l];
            p_l += 1;
            let mut x0 = clamp_i16(i32::from(l0) - i32::from(h0));
            let mut x2 = x0;

            for _ in 0..high_count.saturating_sub(1) {
                let h1 = high_band[p_h];
                p_h += 1;
                l0 = low_band[p_l];
                p_l += 1;
                x2 = clamp_i16(i32::from(l0) - ((i32::from(h0) + i32::from(h1)) / 2));
                let x1 = clamp_i16(((i32::from(x0) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                dst_band[p_x] = x0;
                p_x += 1;
                dst_band[p_x] = x1;
                p_x += 1;
                x0 = x2;
                h0 = h1;
            }

            if low_count <= high_count + 1 {
                if low_count <= high_count {
                    dst_band[p_x] = x2;
                    p_x += 1;
                    dst_band[p_x] = clamp_i16(i32::from(x2) + 2 * i32::from(h0));
                } else {
                    let l0 = low_band[p_l];
                    let x_next = clamp_i16(i32::from(l0) - i32::from(h0));
                    dst_band[p_x] = x2;
                    p_x += 1;
                    dst_band[p_x] = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                    p_x += 1;
                    dst_band[p_x] = x_next;
                }
            } else {
                let l0 = low_band[p_l];
                let x_next = clamp_i16(i32::from(l0) - (i32::from(h0) / 2));
                dst_band[p_x] = x2;
                p_x += 1;
                dst_band[p_x] = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                p_x += 1;
                dst_band[p_x] = x_next;
                p_x += 1;
                let l0 = low_band[p_l + 1];
                dst_band[p_x] = clamp_i16((i32::from(x_next) + i32::from(l0)) / 2);
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
        debug_assert!(dst_band.len() >= dst_step * dst_count);
        debug_assert!(low_band.len() >= low_step * low_count);
        debug_assert!(high_band.len() >= high_step * high_count);

        for col in 0..dst_count {
            let mut p_l = col;
            let mut p_h = col;
            let mut p_x = col;

            let mut h0 = high_band[p_h];
            p_h += high_step;
            let mut l0 = low_band[p_l];
            p_l += low_step;
            let mut x0 = clamp_i16(i32::from(l0) - i32::from(h0));
            let mut x2 = x0;

            for _ in 0..high_count.saturating_sub(1) {
                let h1 = high_band[p_h];
                p_h += high_step;
                l0 = low_band[p_l];
                p_l += low_step;
                x2 = clamp_i16(i32::from(l0) - ((i32::from(h0) + i32::from(h1)) / 2));
                let x1 = clamp_i16(((i32::from(x0) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                dst_band[p_x] = x0;
                p_x += dst_step;
                dst_band[p_x] = x1;
                p_x += dst_step;
                x0 = x2;
                h0 = h1;
            }

            if low_count <= high_count + 1 {
                if low_count <= high_count {
                    dst_band[p_x] = x2;
                    p_x += dst_step;
                    dst_band[p_x] = clamp_i16(i32::from(x2) + 2 * i32::from(h0));
                } else {
                    let l0 = low_band[p_l];
                    let x_next = clamp_i16(i32::from(l0) - i32::from(h0));
                    dst_band[p_x] = x2;
                    p_x += dst_step;
                    dst_band[p_x] = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                    p_x += dst_step;
                    dst_band[p_x] = x_next;
                }
            } else {
                let l0 = low_band[p_l];
                let x_next = clamp_i16(i32::from(l0) - (i32::from(h0) / 2));
                dst_band[p_x] = x2;
                p_x += dst_step;
                dst_band[p_x] = clamp_i16(((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0));
                p_x += dst_step;
                dst_band[p_x] = x_next;
                p_x += dst_step;
                let l0 = low_band[p_l + low_step];
                dst_band[p_x] = clamp_i16((i32::from(x_next) + i32::from(l0)) / 2);
            }
        }
    }

    fn progressive_dwt_decode_block(buffer: &mut [i16], temp: &mut [i16], level: usize) {
        let n_band_l = progressive_get_band_l_count(level);
        let n_band_h = progressive_get_band_h_count(level);

        let hl_len = n_band_h * n_band_l;
        let lh_len = n_band_l * n_band_h;
        let hh_len = n_band_h * n_band_h;
        let ll_len = n_band_l * n_band_l;
        let required = hl_len + lh_len + hh_len + ll_len;

        debug_assert!(buffer.len() >= required);

        let dst_step = n_band_l + n_band_h;
        let dst_len = dst_step * dst_step;
        debug_assert!(buffer.len() >= dst_len);

        let temp_required = dst_step * (n_band_l + n_band_h);
        debug_assert!(temp.len() >= temp_required);

        let hl_range = 0..hl_len;
        let lh_range = hl_len..hl_len + lh_len;
        let hh_range = lh_range.end..lh_range.end + hh_len;
        let ll_range = hh_range.end..hh_range.end + ll_len;

        {
            let ll = &buffer[ll_range.clone()];
            let hl = &buffer[hl_range.clone()];
            let lh = &buffer[lh_range.clone()];
            let hh = &buffer[hh_range.clone()];

            let (l_temp, rest_temp) = temp.split_at_mut(n_band_l * dst_step);
            let (h_temp, _) = rest_temp.split_at_mut(n_band_h * dst_step);

            progressive_idwt_x(
                ll,
                n_band_l,
                hl,
                n_band_h,
                l_temp,
                dst_step,
                n_band_l,
                n_band_h,
                n_band_l,
            );
            progressive_idwt_x(
                lh,
                n_band_l,
                hh,
                n_band_h,
                h_temp,
                dst_step,
                n_band_l,
                n_band_h,
                n_band_h,
            );

            let llx = &mut buffer[..dst_len];
            progressive_idwt_y(
                l_temp,
                dst_step,
                h_temp,
                dst_step,
                llx,
                dst_step,
                n_band_l,
                n_band_h,
                dst_step,
            );
        }
    }

    fn dwt_extrapolate_decode(buffer: &mut [i16], temp: &mut [i16]) {
        if buffer.len() < 4096 {
            return;
        }

        if buffer.len() >= 4015 + 289 {
            progressive_dwt_decode_block(&mut buffer[3807..], temp, 3);
        }
        if buffer.len() >= 3007 + 961 {
            progressive_dwt_decode_block(&mut buffer[3007..], temp, 2);
        }
        progressive_dwt_decode_block(buffer, temp, 1);
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
            data.len() >= 1 + 2 + 1 + 1 + 1 + 2 + 2 + 4,
            ProgressiveError::Truncated("REGION block header"),
        );
        let tile_size = data[0];
        let num_rects = u16::from_le_bytes([data[1], data[2]]);
        let num_quant = data[3] as usize;
        let num_prog_quant = data[4] as usize;
        let flags = data[5];
        let num_tiles = u16::from_le_bytes([data[6], data[7]]);
        let used_tiles = u16::from_le_bytes([data[8], data[9]]);
        let tile_data_size = u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize;
        data = &data[14..];

        ensure_progressive!(
            used_tiles <= num_tiles,
            ProgressiveError::Invalid("usedTiles exceeds numTiles".into()),
        );

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
}

impl TileDecodeScratch {
    fn new() -> Self {
        Self {
            channels: std::array::from_fn(|_| vec![0i16; TILE_PIXELS]),
            temp: vec![0i16; TILE_PIXELS],
        }
    }

    fn channel_mut(&mut self, index: usize) -> &mut [i16] {
        self.channels[index].as_mut_slice()
    }

    fn channels_mut(&mut self) -> [&mut [i16]; COMPONENT_COUNT] {
        let [ref mut c0, ref mut c1, ref mut c2] = self.channels;
        [
            c0.as_mut_slice(),
            c1.as_mut_slice(),
            c2.as_mut_slice(),
        ]
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
}

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

    fn decode_initial_components(
        &mut self,
        tile_state: &mut TileState,
        region: &RegionBlock<'_>,
        block: &TileSimpleBlock<'_>,
    ) -> Result<()> {
        let coeff_diff = (block.flags & RFX_TILE_DIFFERENCE) != 0;

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
    ) -> Result<()> {
        let buffer = self.scratch.channel_mut(component);
        rlgr::decode(self.entropy, data, buffer)
            .map_err(|_| ProgressiveError::Invalid("RLGR decode failed".into()))?;

        tile_state.sign[component].copy_from_slice(buffer);

        let combined_quant = base_quant.add(prog_quant);
        tile_state.quant[component] = *base_quant;
        tile_state.progressive_quant[component] = *prog_quant;
        tile_state.bit_pos[component] = combined_quant;

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
        let new_bitpos = base_quant.add(prog_quant);

        let bitpos_delta = previous_bitpos.sub(&new_bitpos);
        tile_state.bit_pos[component] = new_bitpos;
        tile_state.progressive_quant[component] = *prog_quant;
        tile_state.quant[component] = *base_quant;

        let extrapolate = (region.flags & RFX_DWT_REDUCE_EXTRAPOLATE) != 0;

        self.apply_upgrade_rlgr(
            &mut tile_state.coefficients[component],
            &mut tile_state.sign[component],
            &new_bitpos,
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
            let (mut channels, temp) = self.scratch.split_mut();

            for (component, buffer_ref) in channels.as_mut_slice().iter_mut().enumerate() {
                let buffer = &mut **buffer_ref;
                buffer.copy_from_slice(&tile_state.coefficients[component]);

                if !extrapolate {
                    Self::apply_subband_diff_standard(buffer);
                } else {
                    Self::apply_subband_diff_extrapolate(buffer);
                }

                Self::apply_quant_shift(buffer, &tile_state.bit_pos[component], extrapolate);
                Self::inverse_dwt(buffer, temp, extrapolate);
            }
        }

        let channels = self.scratch.channels();
        let ycbcr = color_conversion::YCbCrBuffer {
            y: channels[0],
            cb: channels[1],
            cr: channels[2],
        };

        color_conversion::ycbcr_to_rgba(ycbcr, tile_state.reconstruction.as_mut_slice())
            .map_err(|err| ProgressiveError::Invalid(format!(
                "YCbCr to RGBA conversion failed: {err}"
            )))?;

        Ok(())
    }

    fn inverse_dwt(buffer: &mut [i16], temp: &mut [i16], extrapolate: bool) {
        if !extrapolate {
            dwt::decode(buffer, temp);
        } else {
            dwt_extrapolate_decode(buffer, temp);
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
                .ok_or_else(|| ProgressiveError::Invalid("progressive quant index out of range".into()))?;
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

#[derive(Clone, Copy)]
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

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

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

        let mut h0 = *high_band.get(high_idx).unwrap_or(&0);
        high_idx += 1;
        let mut l0 = *low_band.get(low_idx).unwrap_or(&0);
        low_idx += 1;
        let mut x0 = clamp_i16(i32::from(l0) - i32::from(h0));
        let mut x2 = x0;

        for _ in 0..high_count.saturating_sub(1) {
            let h1 = *high_band.get(high_idx).unwrap_or(&h0);
            high_idx += 1;
            l0 = *low_band.get(low_idx).unwrap_or(&l0);
            low_idx += 1;
            x2 = clamp_i16(i32::from(l0) - ((i32::from(h0) + i32::from(h1)) / 2));
            let x1 = clamp_i16(((i32::from(x0) + i32::from(x2)) / 2) + 2 * i32::from(h0));
            if dst_idx + 1 < dst_band.len() {
                dst_band[dst_idx] = x0;
                dst_band[dst_idx + 1] = x1;
            }
            dst_idx += 2;
            x0 = x2;
            h0 = h1;
        }

        if low_count <= high_count + 1 {
            if low_count <= high_count {
                if dst_idx + 1 < dst_band.len() {
                    dst_band[dst_idx] = x2;
                    dst_band[dst_idx + 1] = clamp_i16(i32::from(x2) + 2 * i32::from(h0));
                }
            } else {
                let l0 = *low_band.get(low_idx).unwrap_or(&0);
                low_idx += 1;
                let x_next = clamp_i16(i32::from(l0) - i32::from(h0));
                if dst_idx + 2 < dst_band.len() {
                    dst_band[dst_idx] = x2;
                    dst_band[dst_idx + 1] = clamp_i16(
                        ((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0),
                    );
                    dst_band[dst_idx + 2] = x_next;
                }
            }
        } else {
            let l0 = *low_band.get(low_idx).unwrap_or(&0);
            low_idx += 1;
            let x_next = clamp_i16(i32::from(l0) - (i32::from(h0) / 2));
            if dst_idx + 2 < dst_band.len() {
                dst_band[dst_idx] = x2;
                dst_band[dst_idx + 1] = clamp_i16(
                    ((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0),
                );
                dst_band[dst_idx + 2] = x_next;
                let l0 = *low_band.get(low_idx).unwrap_or(&l0);
                if dst_idx + 3 < dst_band.len() {
                    dst_band[dst_idx + 3] = clamp_i16((i32::from(x_next) + i32::from(l0)) / 2);
                }
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

        let mut h0 = *high_band.get(high_idx).unwrap_or(&0);
        high_idx += high_step;
        let mut l0 = *low_band.get(low_idx).unwrap_or(&0);
        low_idx += low_step;
        let mut x0 = clamp_i16(i32::from(l0) - i32::from(h0));
        let mut x2 = x0;

        for _ in 0..high_count.saturating_sub(1) {
            let h1 = *high_band.get(high_idx).unwrap_or(&h0);
            high_idx += high_step;
            l0 = *low_band.get(low_idx).unwrap_or(&l0);
            low_idx += low_step;
            x2 = clamp_i16(i32::from(l0) - ((i32::from(h0) + i32::from(h1)) / 2));
            let x1 = clamp_i16(((i32::from(x0) + i32::from(x2)) / 2) + 2 * i32::from(h0));
            if dst_idx < dst_band.len() {
                dst_band[dst_idx] = x0;
            }
            dst_idx += dst_step;
            if dst_idx < dst_band.len() {
                dst_band[dst_idx] = x1;
            }
            dst_idx += dst_step;
            x0 = x2;
            h0 = h1;
        }

        if low_count <= high_count + 1 {
            if low_count <= high_count {
                if dst_idx < dst_band.len() {
                    dst_band[dst_idx] = x2;
                }
                dst_idx += dst_step;
                if dst_idx < dst_band.len() {
                    dst_band[dst_idx] = clamp_i16(i32::from(x2) + 2 * i32::from(h0));
                }
            } else {
                let l0 = *low_band.get(low_idx).unwrap_or(&0);
                let x_next = clamp_i16(i32::from(l0) - i32::from(h0));
                if dst_idx < dst_band.len() {
                    dst_band[dst_idx] = x2;
                }
                dst_idx += dst_step;
                if dst_idx < dst_band.len() {
                    dst_band[dst_idx] = clamp_i16(
                        ((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0),
                    );
                }
                dst_idx += dst_step;
                if dst_idx < dst_band.len() {
                    dst_band[dst_idx] = x_next;
                }
            }
        } else {
            let l0 = *low_band.get(low_idx).unwrap_or(&0);
            let x_next = clamp_i16(i32::from(l0) - (i32::from(h0) / 2));
            if dst_idx < dst_band.len() {
                dst_band[dst_idx] = x2;
            }
            dst_idx += dst_step;
            if dst_idx < dst_band.len() {
                dst_band[dst_idx] = clamp_i16(
                    ((i32::from(x_next) + i32::from(x2)) / 2) + 2 * i32::from(h0),
                );
            }
            dst_idx += dst_step;
            if dst_idx < dst_band.len() {
                dst_band[dst_idx] = x_next;
            }
            dst_idx += dst_step;
            let l0 = *low_band.get(low_idx + low_step).unwrap_or(&l0);
            if dst_idx < dst_band.len() {
                dst_band[dst_idx] = clamp_i16((i32::from(x_next) + i32::from(l0)) / 2);
            }
        }
    }
}

fn progressive_dwt_decode_block(buffer: &mut [i16], temp: &mut [i16], level: usize) {
    let n_band_l = progressive_get_band_l_count(level);
    let n_band_h = progressive_get_band_h_count(level);

    let hl_len = n_band_h * n_band_l;
    let lh_len = n_band_l * n_band_h;
    let hh_len = n_band_h * n_band_h;
    let ll_len = n_band_l * n_band_l;
    let required = hl_len + lh_len + hh_len + ll_len;

    debug_assert!(buffer.len() >= required);

    let (hl, rest) = buffer.split_at_mut(hl_len);
    let (lh, rest) = rest.split_at_mut(lh_len);
    let (hh, ll) = rest.split_at_mut(hh_len);
    let ll = &mut ll[..ll_len.min(ll.len())];

    let dst_step = n_band_l + n_band_h;
    let dst_len = dst_step * dst_step;
    debug_assert!(buffer.len() >= dst_len);
    let (llx, _) = buffer.split_at_mut(dst_len);

    let temp_required = dst_step * (n_band_l + n_band_h);
    debug_assert!(temp.len() >= temp_required);
    let (l_temp, rest_temp) = temp.split_at_mut(n_band_l * dst_step);
    let (h_temp, _) = rest_temp.split_at_mut(n_band_h * dst_step);

    progressive_idwt_x(
        ll,
        n_band_l,
        hl,
        n_band_h,
        l_temp,
        dst_step,
        n_band_l,
        n_band_h,
        n_band_l,
    );
    progressive_idwt_x(
        lh,
        n_band_l,
        hh,
        n_band_h,
        h_temp,
        dst_step,
        n_band_l,
        n_band_h,
        n_band_h,
    );
    progressive_idwt_y(
        l_temp,
        dst_step,
        h_temp,
        dst_step,
        llx,
        dst_step,
        n_band_l,
        n_band_h,
        dst_step,
    );
}

fn dwt_extrapolate_decode(buffer: &mut [i16], temp: &mut [i16]) {
    if buffer.len() < 4096 {
        return;
    }

    {
        let (tail, _,) = buffer.partition_at_index_mut(3807);
        // compiler doesn't expose partition_at_index_mut stable; fallback manual
    }
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

fn parse_tile_upgrade_block<'a>(
    data: &mut &'a [u8],
) -> Result<TileUpgradeBlock<'a>> {
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
        let mut sync_seen = false;
        let mut context_seen = false;
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
                    sync_seen = true;
                    if block.version < 0x0100 {
                        return Err(ProgressiveError::UnsupportedVersion(block.version));
                    }
                }
                BlockType::Context => {
                    let block = ContextBlock::parse(body)?;
                    self.context_flags = block.flags;
                    context_seen = true;
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

        ensure_progressive!(sync_seen, ProgressiveError::MissingBlock("SYNC"));
        ensure_progressive!(context_seen, ProgressiveError::MissingBlock("CONTEXT"));
        let frame_begin = frame_begin.ok_or(ProgressiveError::MissingBlock("FRAME_BEGIN"))?;
        Ok(ProgressiveSurfaceUpdate {
            surface_id,
            frame_index: frame_begin.frame_index,
            tiles,
        })
    }

}

pub struct ProgressiveSurfaceUpdate {
    pub surface_id: u16,
    pub frame_index: u32,
    pub tiles: Vec<TileUpdate>,
}
