//! Reading things out of the framebuffer: pixels, and a rough sense of layout.
//!
//! What is here is deliberately modest. Finding *controls* in a screenshot is a vision
//! problem, and a model looking at the image is better at it than any heuristic in this file.
//! What a heuristic can do cheaply and exactly, which a model cannot, is answer questions
//! about specific pixels: what colour is this, and where are the edges of the flat block it
//! sits in. That is what [`sample`] and [`find_regions`] do, and neither claims more.
//!
//! [`find_regions`] finds axis-aligned runs of near-uniform colour. Windows chrome is largely
//! made of those -- buttons, entry fields, title bars, panels -- so the boxes it returns often
//! *are* controls. They are candidates to check against the screenshot, never a widget tree.

/// One pixel, as the desktop holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pixel {
    pub x: u16,
    pub y: u16,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Pixel {
    /// `#rrggbb`, which is how a person or a model will want to read it.
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

/// Reads one pixel out of a BGRA framebuffer.
pub fn sample(bgra: &[u8], width: u16, height: u16, x: u16, y: u16) -> Option<Pixel> {
    if x >= width || y >= height {
        return None;
    }
    let offset = (usize::from(y) * usize::from(width) + usize::from(x)) * 4;
    let pixel = bgra.get(offset..offset + 4)?;
    Some(Pixel {
        x,
        y,
        red: pixel[2],
        green: pixel[1],
        blue: pixel[0],
    })
}

/// A block of near-uniform colour, as a rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    /// How much of the bounding box the block actually fills, as a percentage. A true
    /// rectangle is 100; anything lower is an L, a ring, or something with a hole in it.
    pub fill_percent: u8,
}

impl Region {
    /// The middle, which is where a click on this block should go.
    pub fn centre(&self) -> (u16, u16) {
        (
            self.x + self.width / 2,
            self.y + self.height / 2,
        )
    }

    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

/// How many bits of each channel are kept when deciding whether two pixels are "the same
/// colour". Three bits tolerates the gentle gradients and anti-aliasing that flat Windows
/// chrome is full of, without merging genuinely different greys.
const QUANTISE_SHIFT: u8 = 3;

/// Search settings, so the caller can ask for big panels or small buttons.
#[derive(Debug, Clone, Copy)]
pub struct RegionQuery {
    pub min_width: u16,
    pub min_height: u16,
    pub max_width: u16,
    pub max_height: u16,
    /// Blocks squarer than this fraction of their bounding box are kept.
    pub min_fill_percent: u8,
    pub limit: usize,
}

impl Default for RegionQuery {
    fn default() -> Self {
        Self {
            // Smaller than a checkbox is noise; larger than most of the screen is the
            // background or a window body rather than a control.
            min_width: 16,
            min_height: 12,
            max_width: u16::MAX,
            max_height: u16::MAX,
            min_fill_percent: 85,
            limit: 40,
        }
    }
}

/// Finds blocks of near-uniform colour, largest first.
///
/// One pass of union-find over the framebuffer, joining each pixel to its left and upper
/// neighbours when they quantise to the same colour, then a pass to collect bounding boxes.
pub fn find_regions(
    bgra: &[u8],
    width: u16,
    height: u16,
    query: RegionQuery,
) -> Vec<Region> {
    let width_usize = usize::from(width);
    let height_usize = usize::from(height);
    let count = width_usize * height_usize;

    if count == 0 || bgra.len() < count * 4 {
        return Vec::new();
    }

    // Quantised colour per pixel, packed into one byte per channel triple.
    let mut key = vec![0u32; count];
    for (index, pixel) in bgra.chunks_exact(4).take(count).enumerate() {
        let blue = u32::from(pixel[0] >> QUANTISE_SHIFT);
        let green = u32::from(pixel[1] >> QUANTISE_SHIFT);
        let red = u32::from(pixel[2] >> QUANTISE_SHIFT);
        key[index] = (red << 16) | (green << 8) | blue;
    }

    let mut parent: Vec<u32> = (0..count as u32).collect();

    for y in 0..height_usize {
        for x in 0..width_usize {
            let index = y * width_usize + x;
            if x > 0 && key[index] == key[index - 1] {
                union(&mut parent, index as u32, (index - 1) as u32);
            }
            if y > 0 && key[index] == key[index - width_usize] {
                union(&mut parent, index as u32, (index - width_usize) as u32);
            }
        }
    }

    // Bounding box and pixel count per component, kept in a map because most roots are tiny
    // and allocating a full-size array of boxes would dwarf the framebuffer.
    let mut boxes: std::collections::HashMap<u32, Accumulator> = std::collections::HashMap::new();
    for y in 0..height_usize {
        for x in 0..width_usize {
            let index = y * width_usize + x;
            let root = find(&mut parent, index as u32);
            let entry = boxes.entry(root).or_insert_with(|| Accumulator::new(x, y));
            entry.add(x, y);
        }
    }

    let mut regions: Vec<Region> = boxes
        .into_iter()
        .filter_map(|(root, accumulator)| {
            let box_width = (accumulator.max_x - accumulator.min_x + 1) as u16;
            let box_height = (accumulator.max_y - accumulator.min_y + 1) as u16;

            if box_width < query.min_width
                || box_height < query.min_height
                || box_width > query.max_width
                || box_height > query.max_height
            {
                return None;
            }

            let area = u64::from(box_width) * u64::from(box_height);
            let fill = (accumulator.count as u64 * 100 / area.max(1)) as u8;
            if fill < query.min_fill_percent {
                return None;
            }

            let offset = root as usize * 4;
            let pixel = bgra.get(offset..offset + 4)?;

            Some(Region {
                x: accumulator.min_x as u16,
                y: accumulator.min_y as u16,
                width: box_width,
                height: box_height,
                red: pixel[2],
                green: pixel[1],
                blue: pixel[0],
                fill_percent: fill,
            })
        })
        .collect();

    // Largest first: the big panels give a model its bearings before the small controls do.
    regions.sort_by_key(|region| {
        std::cmp::Reverse(u64::from(region.width) * u64::from(region.height))
    });
    regions.truncate(query.limit);
    regions
}

struct Accumulator {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    count: usize,
}

impl Accumulator {
    fn new(x: usize, y: usize) -> Self {
        Self {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
            count: 0,
        }
    }

    fn add(&mut self, x: usize, y: usize) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
        self.count += 1;
    }
}

fn find(parent: &mut [u32], mut node: u32) -> u32 {
    while parent[node as usize] != node {
        // Path halving, so a long chain does not have to be walked twice.
        let grandparent = parent[parent[node as usize] as usize];
        parent[node as usize] = grandparent;
        node = grandparent;
    }
    node
}

fn union(parent: &mut [u32], left: u32, right: u32) {
    let left_root = find(parent, left);
    let right_root = find(parent, right);
    if left_root != right_root {
        // Lower index wins, which keeps the root at the block's first pixel in scan order and
        // makes the root's own colour a fair sample of the block.
        let (keep, drop) = if left_root < right_root {
            (left_root, right_root)
        } else {
            (right_root, left_root)
        };
        parent[drop as usize] = keep;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a BGRA canvas with an optional filled rectangle painted on it.
    fn canvas(width: u16, height: u16, fill: [u8; 3], rect: Option<(u16, u16, u16, u16, [u8; 3])>) -> Vec<u8> {
        let mut bgra = Vec::with_capacity(usize::from(width) * usize::from(height) * 4);
        for y in 0..height {
            for x in 0..width {
                let colour = match rect {
                    Some((rx, ry, rw, rh, block))
                        if x >= rx && x < rx + rw && y >= ry && y < ry + rh =>
                    {
                        block
                    }
                    _ => fill,
                };
                bgra.extend_from_slice(&[colour[2], colour[1], colour[0], 255]);
            }
        }
        bgra
    }

    #[test]
    fn samples_a_pixel_in_rgb_order() {
        let bgra = canvas(4, 4, [10, 20, 30], None);
        let pixel = sample(&bgra, 4, 4, 1, 1).expect("in bounds");
        assert_eq!((pixel.red, pixel.green, pixel.blue), (10, 20, 30));
        assert_eq!(pixel.hex(), "#0a141e");
    }

    #[test]
    fn refuses_a_pixel_outside_the_desktop() {
        let bgra = canvas(4, 4, [0, 0, 0], None);
        assert!(sample(&bgra, 4, 4, 4, 0).is_none());
        assert!(sample(&bgra, 4, 4, 0, 4).is_none());
    }

    #[test]
    fn finds_a_painted_rectangle() {
        // A 40x24 block of one colour on a background of another.
        let bgra = canvas(120, 80, [240, 240, 240], Some((20, 16, 40, 24, [0, 90, 200])));
        let regions = find_regions(&bgra, 120, 80, RegionQuery::default());

        let block = regions
            .iter()
            .find(|region| (region.width, region.height) == (40, 24))
            .unwrap_or_else(|| panic!("the painted block should be found in {regions:?}"));

        assert_eq!((block.x, block.y), (20, 16));
        assert_eq!((block.red, block.green, block.blue), (0, 90, 200));
        assert_eq!(block.fill_percent, 100);
        assert_eq!(block.centre(), (40, 28));
    }

    #[test]
    fn ignores_blocks_below_the_minimum_size() {
        // A 4x4 speck, well under the default 16x12 floor.
        let bgra = canvas(120, 80, [240, 240, 240], Some((10, 10, 4, 4, [0, 0, 0])));
        let regions = find_regions(&bgra, 120, 80, RegionQuery::default());
        assert!(
            !regions.iter().any(|region| region.width == 4),
            "the speck should have been filtered out: {regions:?}"
        );
    }

    #[test]
    fn returns_the_largest_blocks_first() {
        let bgra = canvas(120, 80, [240, 240, 240], Some((20, 16, 40, 24, [0, 90, 200])));
        let regions = find_regions(&bgra, 120, 80, RegionQuery::default());
        assert!(regions.len() >= 2, "expected background and block");

        let areas: Vec<u64> = regions
            .iter()
            .map(|region| u64::from(region.width) * u64::from(region.height))
            .collect();
        let mut sorted = areas.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(areas, sorted);
    }

    #[test]
    fn tolerates_a_short_framebuffer() {
        assert!(find_regions(&[0; 16], 64, 64, RegionQuery::default()).is_empty());
    }
}
