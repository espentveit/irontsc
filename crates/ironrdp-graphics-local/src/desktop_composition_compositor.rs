//! Desktop Composition Compositor
//!
//! Provides bitmap storage and composition services for the Desktop Composition
//! Virtual Channel Extension (MS-RDPEDC).
//!
//! This module integrates with the Desktop Composition state machine to provide:
//! - Efficient bitmap storage for redirection surfaces
//! - Composition of multiple surfaces into a final output
//! - Hardware-accelerated rendering support (via RGB format)

use std::collections::HashMap;

/// Pixel format for stored bitmaps
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitmapPixelFormat {
    /// 8 bits per pixel (256 colors)
    Bpp8,
    /// 16 bits per pixel (RGB565 or similar)
    Bpp16,
    /// 24 bits per pixel (RGB888)
    Bpp24,
    /// 32 bits per pixel (RGBA8888 or BGRX8888)
    Bpp32,
}

impl BitmapPixelFormat {
    /// Get the number of bytes per pixel
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            BitmapPixelFormat::Bpp8 => 1,
            BitmapPixelFormat::Bpp16 => 2,
            BitmapPixelFormat::Bpp24 => 3,
            BitmapPixelFormat::Bpp32 => 4,
        }
    }

    /// Create from bits per pixel
    pub fn from_bpp(bpp: u8) -> Option<Self> {
        match bpp {
            8 => Some(BitmapPixelFormat::Bpp8),
            16 => Some(BitmapPixelFormat::Bpp16),
            24 => Some(BitmapPixelFormat::Bpp24),
            32 => Some(BitmapPixelFormat::Bpp32),
            _ => None,
        }
    }
}

/// A bitmap stored in the compositor
#[derive(Debug, Clone)]
pub struct CompositorBitmap {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Pixel format
    pub format: BitmapPixelFormat,
    /// Bitmap data in row-major order (top to bottom, left to right)
    /// Length must equal width * height * bytes_per_pixel
    pub data: Vec<u8>,
}

impl CompositorBitmap {
    /// Create a new bitmap with allocated storage
    pub fn new(width: u32, height: u32, format: BitmapPixelFormat) -> Self {
        let size = (width * height) as usize * format.bytes_per_pixel();
        Self {
            width,
            height,
            format,
            data: vec![0; size],
        }
    }

    /// Create from existing data
    pub fn from_data(
        width: u32,
        height: u32,
        format: BitmapPixelFormat,
        data: Vec<u8>,
    ) -> Result<Self, CompositorError> {
        let expected_size = (width * height) as usize * format.bytes_per_pixel();
        if data.len() != expected_size {
            return Err(CompositorError::InvalidBitmapSize {
                expected: expected_size,
                actual: data.len(),
            });
        }
        Ok(Self {
            width,
            height,
            format,
            data,
        })
    }

    /// Get the stride (bytes per row)
    pub fn stride(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel()
    }

    /// Clear the bitmap to a solid color (black by default)
    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    /// Copy a region from another bitmap
    pub fn blit_from(
        &mut self,
        source: &CompositorBitmap,
        src_x: u32,
        src_y: u32,
        dst_x: u32,
        dst_y: u32,
        width: u32,
        height: u32,
    ) -> Result<(), CompositorError> {
        // Validate formats match
        if self.format != source.format {
            return Err(CompositorError::FormatMismatch {
                expected: self.format,
                actual: source.format,
            });
        }

        // Validate bounds
        if src_x + width > source.width
            || src_y + height > source.height
            || dst_x + width > self.width
            || dst_y + height > self.height
        {
            return Err(CompositorError::OutOfBounds);
        }

        let bpp = self.format.bytes_per_pixel();
        let src_stride = source.stride();
        let dst_stride = self.stride();

        // Copy row by row
        for y in 0..height {
            let src_row = (src_y + y) as usize * src_stride + src_x as usize * bpp;
            let dst_row = (dst_y + y) as usize * dst_stride + dst_x as usize * bpp;
            let row_bytes = width as usize * bpp;

            self.data[dst_row..dst_row + row_bytes]
                .copy_from_slice(&source.data[src_row..src_row + row_bytes]);
        }

        Ok(())
    }
}

/// Errors that can occur during composition
#[derive(Debug, thiserror::Error)]
pub enum CompositorError {
    #[error("Invalid bitmap size: expected {expected}, got {actual}")]
    InvalidBitmapSize { expected: usize, actual: usize },

    #[error("Format mismatch: expected {expected:?}, got {actual:?}")]
    FormatMismatch {
        expected: BitmapPixelFormat,
        actual: BitmapPixelFormat,
    },

    #[error("Bitmap region out of bounds")]
    OutOfBounds,

    #[error("Bitmap not found: handle {0}")]
    BitmapNotFound(u64),

    #[error("Surface not found: cache_id {0}")]
    SurfaceNotFound(u32),
}

/// Surface layer for composition
#[derive(Debug, Clone)]
pub struct CompositionLayer {
    /// Handle to the redirection surface
    pub surface_handle: u64,
    /// Cache ID for quick lookup
    pub cache_id: u32,
    /// Position on screen (x, y)
    pub position: (i32, i32),
    /// Z-order (higher values are on top)
    pub z_order: i32,
    /// Visibility flag
    pub visible: bool,
}

/// Desktop Composition Compositor
///
/// Manages bitmap storage and performs composition of multiple surfaces
pub struct DesktopCompositor {
    /// Stored bitmaps indexed by surface handle
    bitmaps: HashMap<u64, CompositorBitmap>,
    /// Cache ID to surface handle mapping
    cache_id_to_handle: HashMap<u32, u64>,
    /// Composition layers (ordered by z-order)
    layers: Vec<CompositionLayer>,
    /// Output bitmap (the final composed result)
    output: Option<CompositorBitmap>,
    /// Output dimensions
    output_width: u32,
    output_height: u32,
}

impl DesktopCompositor {
    /// Create a new compositor with specified output dimensions
    pub fn new(output_width: u32, output_height: u32) -> Self {
        Self {
            bitmaps: HashMap::new(),
            cache_id_to_handle: HashMap::new(),
            layers: Vec::new(),
            output: None,
            output_width,
            output_height,
        }
    }

    /// Set the output dimensions
    pub fn set_output_size(&mut self, width: u32, height: u32) {
        self.output_width = width;
        self.output_height = height;
        self.output = None; // Force recreation on next compose
    }

    /// Store a bitmap for a redirection surface
    pub fn store_bitmap(
        &mut self,
        handle: u64,
        cache_id: u32,
        bitmap: CompositorBitmap,
    ) -> Result<(), CompositorError> {
        self.bitmaps.insert(handle, bitmap);
        self.cache_id_to_handle.insert(cache_id, handle);
        Ok(())
    }

    /// Update bitmap data for an existing surface
    pub fn update_bitmap(&mut self, handle: u64, data: Vec<u8>) -> Result<(), CompositorError> {
        let bitmap = self
            .bitmaps
            .get_mut(&handle)
            .ok_or(CompositorError::BitmapNotFound(handle))?;

        if data.len() != bitmap.data.len() {
            return Err(CompositorError::InvalidBitmapSize {
                expected: bitmap.data.len(),
                actual: data.len(),
            });
        }

        bitmap.data.copy_from_slice(&data);
        Ok(())
    }

    /// Get a bitmap by surface handle
    pub fn get_bitmap(&self, handle: u64) -> Option<&CompositorBitmap> {
        self.bitmaps.get(&handle)
    }

    /// Get a mutable bitmap by surface handle
    pub fn get_bitmap_mut(&mut self, handle: u64) -> Option<&mut CompositorBitmap> {
        self.bitmaps.get_mut(&handle)
    }

    /// Get a bitmap by cache ID
    pub fn get_bitmap_by_cache_id(&self, cache_id: u32) -> Option<&CompositorBitmap> {
        self.cache_id_to_handle
            .get(&cache_id)
            .and_then(|handle| self.bitmaps.get(handle))
    }

    /// Get a mutable bitmap by cache ID
    pub fn get_bitmap_by_cache_id_mut(&mut self, cache_id: u32) -> Option<&mut CompositorBitmap> {
        self.cache_id_to_handle
            .get(&cache_id)
            .and_then(|handle| self.bitmaps.get_mut(handle))
    }

    /// Remove a bitmap
    pub fn remove_bitmap(&mut self, handle: u64, cache_id: u32) {
        self.bitmaps.remove(&handle);
        self.cache_id_to_handle.remove(&cache_id);
        self.layers.retain(|layer| layer.surface_handle != handle);
    }

    /// Add or update a composition layer
    pub fn set_layer(
        &mut self,
        surface_handle: u64,
        cache_id: u32,
        position: (i32, i32),
        z_order: i32,
        visible: bool,
    ) {
        // Remove existing layer for this surface if present
        self.layers
            .retain(|layer| layer.surface_handle != surface_handle);

        // Add new layer
        self.layers.push(CompositionLayer {
            surface_handle,
            cache_id,
            position,
            z_order,
            visible,
        });

        // Sort by z-order (lower values first)
        self.layers.sort_by_key(|layer| layer.z_order);
    }

    /// Remove a layer
    pub fn remove_layer(&mut self, surface_handle: u64) {
        self.layers
            .retain(|layer| layer.surface_handle != surface_handle);
    }

    /// Perform composition and return the output bitmap
    ///
    /// This composites all visible layers in z-order and returns a reference
    /// to the composed output.
    pub fn compose(&mut self) -> Result<&CompositorBitmap, CompositorError> {
        // Create output bitmap if needed
        if self.output.is_none() {
            self.output = Some(CompositorBitmap::new(
                self.output_width,
                self.output_height,
                BitmapPixelFormat::Bpp32, // Use 32bpp for output
            ));
        }

        let output = self.output.as_mut().unwrap();
        output.clear();

        // Compose each visible layer in z-order
        for layer in &self.layers {
            if !layer.visible {
                continue;
            }

            let bitmap = match self.bitmaps.get(&layer.surface_handle) {
                Some(bmp) => bmp,
                None => continue, // Skip if bitmap not found
            };

            // Calculate clipped region
            let output_width = self.output_width;
            let output_height = self.output_height;
            let (dst_x, dst_y, src_x, src_y, width, height) = Self::calculate_blit_region_static(
                layer.position,
                bitmap,
                output_width,
                output_height,
            );

            if width == 0 || height == 0 {
                continue; // Completely clipped
            }

            // Blit the surface to the output
            let output = self.output.as_mut().unwrap();
            if let Err(e) = output.blit_from(bitmap, src_x, src_y, dst_x, dst_y, width, height) {
                tracing::warn!(
                    "Failed to blit surface {} to output: {}",
                    layer.surface_handle,
                    e
                );
            }
        }

        Ok(self.output.as_ref().unwrap())
    }

    /// Calculate the clipped blit region (static version to avoid borrow issues)
    fn calculate_blit_region_static(
        position: (i32, i32),
        source: &CompositorBitmap,
        dest_width: u32,
        dest_height: u32,
    ) -> (u32, u32, u32, u32, u32, u32) {
        let (pos_x, pos_y) = position;

        // Determine source and destination rectangles
        let src_x = if pos_x < 0 { -pos_x as u32 } else { 0 };
        let src_y = if pos_y < 0 { -pos_y as u32 } else { 0 };
        let dst_x = if pos_x >= 0 { pos_x as u32 } else { 0 };
        let dst_y = if pos_y >= 0 { pos_y as u32 } else { 0 };

        // Calculate available width and height
        let available_width = source.width.saturating_sub(src_x);
        let available_height = source.height.saturating_sub(src_y);

        let max_dst_width = dest_width.saturating_sub(dst_x);
        let max_dst_height = dest_height.saturating_sub(dst_y);

        let width = available_width.min(max_dst_width);
        let height = available_height.min(max_dst_height);

        (dst_x, dst_y, src_x, src_y, width, height)
    }

    /// Calculate the clipped blit region
    #[deprecated(note = "Use calculate_blit_region_static instead")]
    fn calculate_blit_region(
        &self,
        position: (i32, i32),
        source: &CompositorBitmap,
        dest: &CompositorBitmap,
    ) -> (u32, u32, u32, u32, u32, u32) {
        Self::calculate_blit_region_static(position, source, dest.width, dest.height)
    }

    /// Get the current output bitmap without composing
    pub fn get_output(&self) -> Option<&CompositorBitmap> {
        self.output.as_ref()
    }

    /// Clear all stored bitmaps and layers
    pub fn clear(&mut self) {
        self.bitmaps.clear();
        self.cache_id_to_handle.clear();
        self.layers.clear();
        self.output = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compositor_bitmap_creation() {
        let bitmap = CompositorBitmap::new(64, 64, BitmapPixelFormat::Bpp32);
        assert_eq!(bitmap.width, 64);
        assert_eq!(bitmap.height, 64);
        assert_eq!(bitmap.data.len(), 64 * 64 * 4);
    }

    #[test]
    fn test_compositor_bitmap_blit() {
        let mut source = CompositorBitmap::new(64, 64, BitmapPixelFormat::Bpp32);
        source.data.fill(255); // Fill with white

        let mut dest = CompositorBitmap::new(128, 128, BitmapPixelFormat::Bpp32);
        dest.clear(); // Fill with black

        // Blit 32x32 region from source to dest
        dest.blit_from(&source, 0, 0, 10, 10, 32, 32).unwrap();

        // Verify the blitted region
        for y in 10..42 {
            for x in 10..42 {
                let offset = (y * 128 + x) * 4;
                assert_eq!(dest.data[offset], 255); // Should be white
            }
        }
    }

    #[test]
    fn test_desktop_compositor_basic() {
        let mut compositor = DesktopCompositor::new(1920, 1080);

        // Create and store a bitmap
        let bitmap = CompositorBitmap::new(64, 64, BitmapPixelFormat::Bpp32);
        compositor.store_bitmap(1, 1, bitmap).unwrap();

        // Add a layer
        compositor.set_layer(1, 1, (0, 0), 0, true);

        // Compose
        let output = compositor.compose().unwrap();
        assert_eq!(output.width, 1920);
        assert_eq!(output.height, 1080);
    }

    #[test]
    fn test_desktop_compositor_multiple_layers() {
        let mut compositor = DesktopCompositor::new(256, 256);

        // Create two bitmaps
        let mut bitmap1 = CompositorBitmap::new(64, 64, BitmapPixelFormat::Bpp32);
        bitmap1.data.fill(255); // White

        let mut bitmap2 = CompositorBitmap::new(64, 64, BitmapPixelFormat::Bpp32);
        bitmap2.data.fill(128); // Gray

        compositor.store_bitmap(1, 1, bitmap1).unwrap();
        compositor.store_bitmap(2, 2, bitmap2).unwrap();

        // Add layers with different z-orders
        compositor.set_layer(1, 1, (0, 0), 0, true);
        compositor.set_layer(2, 2, (32, 32), 1, true);

        // Compose
        let output = compositor.compose().unwrap();
        assert_eq!(output.width, 256);
        assert_eq!(output.height, 256);
    }
}
