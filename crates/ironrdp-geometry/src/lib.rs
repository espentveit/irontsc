//! MS-RDPEVOR: Remote Desktop Protocol: Geometry Tracking Virtual Channel Extension
//!
//! This module implements the Geometry channel for Video Redirection, which tracks
//! the position and shape of video regions on the remote desktop.

use anyhow::{bail, Result};
use bytes::{Buf, Bytes};

/// Geometry DVC channel name
pub const GEOMETRY_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Geometry::v08.01";

/// Update types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum UpdateType {
    Update = 1,
    Clear = 2,
}

impl UpdateType {
    pub fn from_u32(value: u32) -> Result<Self> {
        match value {
            1 => Ok(Self::Update),
            2 => Ok(Self::Clear),
            _ => bail!("Invalid update type: {}", value),
        }
    }
}

/// Geometry types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum GeometryType {
    Rectangle = 1,
}

impl GeometryType {
    pub fn from_u32(value: u32) -> Result<Self> {
        match value {
            1 => Ok(Self::Rectangle),
            _ => bail!("Invalid geometry type: {}", value),
        }
    }
}

/// Rectangle structure
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rectangle {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rectangle {
    pub fn width(&self) -> u32 {
        (self.right - self.left).abs() as u32
    }

    pub fn height(&self) -> u32 {
        (self.bottom - self.top).abs() as u32
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Region data (collection of rectangles)
#[derive(Debug, Clone)]
pub struct RegionData {
    pub bounding_rect: Rectangle,
    pub rects: Vec<Rectangle>,
}

/// MAPPED_GEOMETRY packet (server → client)
#[derive(Debug, Clone)]
pub struct MappedGeometry {
    pub version: u32,
    pub mapping_id: u64,
    pub update_type: UpdateType,
    pub top_level_id: u64,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub top_level_left: i32,
    pub top_level_top: i32,
    pub top_level_right: i32,
    pub top_level_bottom: i32,
    pub geometry_type: GeometryType,
    pub geometry: RegionData,
}

impl MappedGeometry {
    const MIN_SIZE: usize = 4 + 8 + 4 + 8 + 4 * 4 + 4 * 4 + 4; // 76 bytes min

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < Self::MIN_SIZE {
            bail!(
                "Not enough data for MappedGeometry: need {}, have {}",
                Self::MIN_SIZE,
                data.len()
            );
        }

        let mut buf = Bytes::copy_from_slice(data);

        let version = buf.get_u32_le();
        let mapping_id = buf.get_u64_le();
        let update_type = UpdateType::from_u32(buf.get_u32_le())?;
        let top_level_id = buf.get_u64_le();

        let left = buf.get_i32_le();
        let top = buf.get_i32_le();
        let right = buf.get_i32_le();
        let bottom = buf.get_i32_le();

        let top_level_left = buf.get_i32_le();
        let top_level_top = buf.get_i32_le();
        let top_level_right = buf.get_i32_le();
        let top_level_bottom = buf.get_i32_le();

        let geometry_type = GeometryType::from_u32(buf.get_u32_le())?;

        // Parse region data
        if buf.remaining() < 20 {
            // bounding rect (16 bytes) + rect count (4 bytes)
            bail!("Not enough data for region data");
        }

        let bounding_rect = Rectangle {
            left: buf.get_i32_le(),
            top: buf.get_i32_le(),
            right: buf.get_i32_le(),
            bottom: buf.get_i32_le(),
        };

        let rect_count = buf.get_u32_le() as usize;

        if buf.remaining() < rect_count * 16 {
            bail!(
                "Not enough data for rectangles: need {}, have {}",
                rect_count * 16,
                buf.remaining()
            );
        }

        let mut rects = Vec::with_capacity(rect_count);
        for _ in 0..rect_count {
            rects.push(Rectangle {
                left: buf.get_i32_le(),
                top: buf.get_i32_le(),
                right: buf.get_i32_le(),
                bottom: buf.get_i32_le(),
            });
        }

        Ok(Self {
            version,
            mapping_id,
            update_type,
            top_level_id,
            left,
            top,
            right,
            bottom,
            top_level_left,
            top_level_top,
            top_level_right,
            top_level_bottom,
            geometry_type,
            geometry: RegionData {
                bounding_rect,
                rects,
            },
        })
    }

    /// Get the main bounding rectangle
    pub fn bounds(&self) -> Rectangle {
        Rectangle {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    /// Get the top-level window bounds
    pub fn top_level_bounds(&self) -> Rectangle {
        Rectangle {
            left: self.top_level_left,
            top: self.top_level_top,
            right: self.top_level_right,
            bottom: self.top_level_bottom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rectangle() {
        let rect = Rectangle {
            left: 100,
            top: 200,
            right: 500,
            bottom: 600,
        };

        assert_eq!(rect.width(), 400);
        assert_eq!(rect.height(), 400);
        assert!(rect.contains_point(300, 400));
        assert!(!rect.contains_point(50, 100));
    }
}
