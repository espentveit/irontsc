//! Desktop Composition Integration
//!
//! This module provides integration between the Desktop Composition state machine
//! (from ironrdp-pdu) and the compositor (from ironrdp-graphics).

use ironrdp_graphics::desktop_composition_compositor::{
    BitmapPixelFormat, CompositorBitmap, DesktopCompositor,
};
use ironrdp_pdu::basic_output::desktop_composition::{
    CompDeskFlushComposeOnce, DesktopCompositionOrder,
};
use ironrdp_pdu::basic_output::desktop_composition_state::DesktopCompositionState;

/// Desktop Composition Handler
///
/// Integrates the Desktop Composition state machine with the compositor
/// to provide complete rendering pipeline support.
pub struct DesktopCompositionHandler {
    /// The state machine tracking surfaces and associations
    state: DesktopCompositionState,
    /// The compositor that performs actual rendering
    compositor: DesktopCompositor,
    /// Whether composition has changed and needs flush
    needs_flush: bool,
}

impl DesktopCompositionHandler {
    /// Create a new handler with specified output dimensions
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            state: DesktopCompositionState::new(),
            compositor: DesktopCompositor::new(width, height),
            needs_flush: false,
        }
    }

    /// Set the output dimensions
    pub fn set_output_size(&mut self, width: u32, height: u32) {
        self.compositor.set_output_size(width, height);
    }

    /// Process a Desktop Composition order
    ///
    /// This updates both the state machine and the compositor
    pub fn process_order(
        &mut self,
        order: &DesktopCompositionOrder,
    ) -> Result<(), ironrdp_core::DecodeError> {
        // Process the order in the state machine first
        self.state.process_order(order)?;

        // Sync the compositor with the state changes
        match order {
            DesktopCompositionOrder::Toggle(_) => {
                // Mode change - may need to clear compositor
                if !self.state.is_compositing() {
                    self.compositor.clear();
                }
            }
            DesktopCompositionOrder::SurfObj(surf) => {
                if surf.is_create() {
                    // Create bitmap in compositor
                    let cache_id = surf.get_cache_id();
                    if let Some(redir_surf) =
                        self.state.get_redirection_surface_by_cache_id(cache_id)
                    {
                        let format = BitmapPixelFormat::from_bpp(redir_surf.bits_per_pixel)
                            .unwrap_or(BitmapPixelFormat::Bpp32);

                        let bitmap =
                            CompositorBitmap::new(redir_surf.width, redir_surf.height, format);

                        let _ = self
                            .compositor
                            .store_bitmap(redir_surf.handle, cache_id, bitmap);

                        // Add layer for this surface (initially at position 0,0)
                        self.compositor.set_layer(
                            redir_surf.handle,
                            cache_id,
                            (0, 0),
                            cache_id as i32, // Use cache_id as z-order
                            true,
                        );
                    }
                } else {
                    // Destroy bitmap in compositor
                    let cache_id = surf.get_cache_id();
                    self.compositor.remove_bitmap(surf.h_surf, cache_id);
                }
                self.needs_flush = true;
            }
            DesktopCompositionOrder::LSurface(_) => {
                // Logical surface changes may affect composition
                self.needs_flush = true;
            }
            DesktopCompositionOrder::RedirSurfAssoc(_) => {
                // Association changes may affect layer visibility
                self.needs_flush = true;
            }
            DesktopCompositionOrder::SwitchSurfObj(_) => {
                // Drawing target changed - current target updates handled by state
            }
            DesktopCompositionOrder::FlushComposeOnce(_) => {
                // Explicit flush request
                self.needs_flush = true;
            }
            DesktopCompositionOrder::LSurfaceCompRef(_) => {
                // Compositor reference - may trigger flush
                self.needs_flush = true;
            }
        }

        Ok(())
    }

    /// Update bitmap data for the current drawing target
    ///
    /// Call this when drawing operations produce new pixel data
    pub fn update_target_bitmap(&mut self, data: &[u8]) -> Result<(), String> {
        if let Some(target) = self.state.get_current_target() {
            self.compositor
                .update_bitmap(target.handle, data.to_vec())
                .map_err(|e| format!("Failed to update bitmap: {}", e))?;
            self.needs_flush = true;
        }
        Ok(())
    }

    /// Get the current drawing target surface
    pub fn get_current_target(
        &self,
    ) -> Option<&ironrdp_pdu::basic_output::desktop_composition_state::RedirectionSurface> {
        self.state.get_current_target()
    }

    /// Get mutable access to target bitmap for drawing
    pub fn get_target_bitmap_mut(&mut self) -> Option<&mut CompositorBitmap> {
        self.state
            .get_current_target()
            .and_then(|target| self.compositor.get_bitmap_mut(target.handle))
    }

    /// Check if composition is needed
    pub fn needs_flush(&self) -> bool {
        self.needs_flush
    }

    /// Perform composition and return the output bitmap
    ///
    /// Returns None if composition is not needed or fails
    pub fn flush(&mut self) -> Option<&CompositorBitmap> {
        if !self.needs_flush || !self.state.is_compositing() {
            return None;
        }

        match self.compositor.compose() {
            Ok(output) => {
                self.needs_flush = false;
                Some(output)
            }
            Err(e) => {
                tracing::warn!("Composition failed: {}", e);
                None
            }
        }
    }

    /// Get access to the state machine
    pub fn state(&self) -> &DesktopCompositionState {
        &self.state
    }

    /// Get access to the compositor
    pub fn compositor(&self) -> &DesktopCompositor {
        &self.compositor
    }

    /// Check if currently in compositing mode
    pub fn is_compositing(&self) -> bool {
        self.state.is_compositing()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_pdu::basic_output::desktop_composition::{
        CompDeskLSurface, CompDeskSurfObj, CompDeskToggle, CompDeskToggleEventType, LSurfaceFlags,
    };

    #[test]
    fn test_desktop_composition_handler_creation() {
        let handler = DesktopCompositionHandler::new(1920, 1080);
        assert!(!handler.is_compositing());
        assert!(!handler.needs_flush());
    }

    #[test]
    fn test_desktop_composition_toggle() {
        let mut handler = DesktopCompositionHandler::new(1920, 1080);

        // Enable composition
        let toggle = CompDeskToggle::new(CompDeskToggleEventType::CompositionOn);
        handler
            .process_order(&DesktopCompositionOrder::Toggle(toggle))
            .unwrap();

        assert!(handler.is_compositing());
    }

    #[test]
    fn test_surface_creation_and_composition() {
        let mut handler = DesktopCompositionHandler::new(1920, 1080);

        // Enable composition
        let toggle = CompDeskToggle::new(CompDeskToggleEventType::CompositionOn);
        handler
            .process_order(&DesktopCompositionOrder::Toggle(toggle))
            .unwrap();

        // Create logical surface
        let lsurf = CompDeskLSurface::new_create(0x100, LSurfaceFlags::REDIRECTION, 0x200);
        handler
            .process_order(&DesktopCompositionOrder::LSurface(lsurf))
            .unwrap();

        // Create redirection surface
        let rsurf = CompDeskSurfObj::new_create(1, 32, 0x300, 64, 64);
        handler
            .process_order(&DesktopCompositionOrder::SurfObj(rsurf))
            .unwrap();

        assert!(handler.needs_flush());

        // Perform composition
        let output = handler.flush();
        assert!(output.is_some());
        assert!(!handler.needs_flush());
    }
}
