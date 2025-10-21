//! Desktop Composition State Machine
//!
//! Client-side implementation for managing Desktop Composition state
//! as specified in MS-RDPEDC section 3.2

use std::collections::HashMap;

use super::desktop_composition::*;
use ironrdp_core::{invalid_field_err, DecodeError};

/// Drawing mode of the graphics subsystem
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingMode {
    /// Non-composed mode - drawing operations rendered directly to screen
    NonComposited,
    /// Composed mode - drawing operations rendered to in-memory surfaces,
    /// then composed by the compositor
    Composited,
}

/// Desktop mode indicating which desktop is active
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopMode {
    /// Non-composed desktop is active
    NonComposeDesktop,
    /// Composed desktop is active
    ComposeDesktop,
}

/// Logical Surface tracked by the client
///
/// Per spec section 3.1.1: "logical surface: A 64-bit numerical ID that uniquely
/// identifies a surface that is meant to be composed with the rest of the desktop"
#[derive(Debug, Clone)]
pub struct LogicalSurface {
    /// Unique handle for this logical surface
    pub handle: u64,
    /// Flags indicating surface properties
    pub flags: LSurfaceFlags,
    /// Window handle associated with this surface
    pub hwnd: u64,
    /// Currently associated redirection surface handle (if any)
    pub associated_redir_surface: Option<u64>,
    /// True if compositor has referenced this surface
    pub compositor_referenced: bool,
}

/// Redirection Surface tracked by the client
///
/// Per spec section 3.1.1: "redirection surface: The component responsible for
/// maintaining a second set of display data for a GUI"
#[derive(Debug, Clone)]
pub struct RedirectionSurface {
    /// Unique handle for this redirection surface
    pub handle: u64,
    /// Cache ID used to identify this surface (31-bit identifier)
    pub cache_id: u32,
    /// Bits per pixel
    pub bits_per_pixel: u8,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Surface bitmap data (if needed for rendering)
    /// Note: This could be large - implementations may want to use a different storage strategy
    pub data: Vec<u8>,
}

/// Desktop Composition State Manager
///
/// Implements the client-side state machine per MS-RDPEDC section 3.2
pub struct DesktopCompositionState {
    /// Current drawing mode
    pub drawing_mode: DrawingMode,
    /// Current desktop mode
    pub desktop_mode: DesktopMode,
    /// Logical surfaces indexed by handle
    logical_surfaces: HashMap<u64, LogicalSurface>,
    /// Redirection surfaces indexed by handle
    redir_surfaces: HashMap<u64, RedirectionSurface>,
    /// Mapping from cache ID to redirection surface handle
    cache_id_to_handle: HashMap<u32, u64>,
    /// Currently targeted redirection surface for drawing operations
    current_target: Option<u32>,
    /// Track whether we've initialized
    initialized: bool,
}

impl Default for DesktopCompositionState {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopCompositionState {
    pub fn new() -> Self {
        Self {
            drawing_mode: DrawingMode::NonComposited,
            desktop_mode: DesktopMode::NonComposeDesktop,
            logical_surfaces: HashMap::new(),
            redir_surfaces: HashMap::new(),
            cache_id_to_handle: HashMap::new(),
            current_target: None,
            initialized: false,
        }
    }

    /// Process a Desktop Composition order and update state
    ///
    /// Returns Result indicating success or validation error
    pub fn process_order(&mut self, order: &DesktopCompositionOrder) -> Result<(), DecodeError> {
        match order {
            DesktopCompositionOrder::Toggle(toggle) => self.process_toggle(&toggle),
            DesktopCompositionOrder::LSurface(lsurf) => self.process_lsurface(&lsurf),
            DesktopCompositionOrder::SurfObj(surf) => self.process_surfobj(&surf),
            DesktopCompositionOrder::RedirSurfAssoc(assoc) => self.process_assoc(&assoc),
            DesktopCompositionOrder::LSurfaceCompRef(compref) => self.process_compref(&compref),
            DesktopCompositionOrder::SwitchSurfObj(switch) => self.process_switch(&switch),
            DesktopCompositionOrder::FlushComposeOnce(flush) => self.process_flush(&flush),
        }
    }

    /// Process TS_COMPDESK_TOGGLE - Desktop Composition Mode Management
    ///
    /// Per spec section 3.2.5.1.1
    fn process_toggle(&mut self, toggle: &CompDeskToggle) -> Result<(), DecodeError> {
        match toggle.event_type {
            CompDeskToggleEventType::CompositionOn => {
                // The surface manager proxy transitions the DrawingMode to composited
                self.drawing_mode = DrawingMode::Composited;
                self.desktop_mode = DesktopMode::ComposeDesktop;
                self.initialized = true;
                Ok(())
            }
            CompDeskToggleEventType::CompositionOff => {
                // The surface manager proxy transitions the DrawingMode to noncomposited
                if self.drawing_mode != DrawingMode::Composited {
                    // Out of order - ignore per spec section 3.2.5
                    return Ok(());
                }
                self.drawing_mode = DrawingMode::NonComposited;
                self.desktop_mode = DesktopMode::NonComposeDesktop;
                // Clean up surfaces on composition off
                self.logical_surfaces.clear();
                self.redir_surfaces.clear();
                self.cache_id_to_handle.clear();
                self.current_target = None;
                Ok(())
            }
            CompDeskToggleEventType::DwmDeskEnter => {
                // Surface manager proxy transitions DesktopMode to compose desktop
                // DrawingMode remains composited
                if self.drawing_mode != DrawingMode::Composited {
                    // Out of order - ignore
                    return Ok(());
                }
                self.desktop_mode = DesktopMode::ComposeDesktop;
                Ok(())
            }
            CompDeskToggleEventType::DwmDeskLeave => {
                // Surface manager proxy transitions DesktopMode to non-compose desktop
                // DrawingMode remains composited
                if self.drawing_mode != DrawingMode::Composited {
                    // Out of order - ignore
                    return Ok(());
                }
                self.desktop_mode = DesktopMode::NonComposeDesktop;
                Ok(())
            }
            CompDeskToggleEventType::Reserved00 | CompDeskToggleEventType::Reserved01 => {
                // Reserved values - should be ignored per spec
                Ok(())
            }
        }
    }

    /// Process TS_COMPDESK_LSURFACE - Logical Surface Lifetime Management
    ///
    /// Per spec section 3.2.5.2.1
    fn process_lsurface(&mut self, lsurf: &CompDeskLSurface) -> Result<(), DecodeError> {
        if lsurf.create {
            // Create logical surface
            if self.logical_surfaces.contains_key(&lsurf.h_lsurface) {
                // Already exists - this is an error, but we'll ignore per spec behavior
                return Ok(());
            }

            self.logical_surfaces.insert(
                lsurf.h_lsurface,
                LogicalSurface {
                    handle: lsurf.h_lsurface,
                    flags: lsurf.flags,
                    hwnd: lsurf.hwnd,
                    associated_redir_surface: None,
                    compositor_referenced: false,
                },
            );
        } else {
            // Destroy logical surface
            if let Some(lsurface) = self.logical_surfaces.get(&lsurf.h_lsurface) {
                // Per spec: cannot destroy if redirection surface is attached
                if lsurface.associated_redir_surface.is_some() {
                    return Err(invalid_field_err!(
                        "hLSurface",
                        "cannot destroy logical surface with attached redirection surface"
                    ));
                }
            }
            self.logical_surfaces.remove(&lsurf.h_lsurface);
        }
        Ok(())
    }

    /// Process TS_COMPDESK_SURFOBJ - Redirection Surface Lifetime Management
    ///
    /// Per spec section 3.2.5.2.2
    fn process_surfobj(&mut self, surf: &CompDeskSurfObj) -> Result<(), DecodeError> {
        if surf.is_create() {
            // Create redirection surface
            let cache_id = surf.get_cache_id();

            if self.cache_id_to_handle.contains_key(&cache_id) {
                // Already exists - ignore
                return Ok(());
            }

            // Calculate data size (bpp * width * height / 8)
            let data_size = (u64::from(surf.surface_bpp) * u64::from(surf.cx) * u64::from(surf.cy)
                / 8) as usize;

            self.redir_surfaces.insert(
                surf.h_surf,
                RedirectionSurface {
                    handle: surf.h_surf,
                    cache_id,
                    bits_per_pixel: surf.surface_bpp,
                    width: surf.cx,
                    height: surf.cy,
                    data: vec![0; data_size],
                },
            );

            self.cache_id_to_handle.insert(cache_id, surf.h_surf);
        } else {
            // Destroy redirection surface
            let cache_id = surf.get_cache_id();

            if let Some(&handle) = self.cache_id_to_handle.get(&cache_id) {
                self.redir_surfaces.remove(&handle);
                self.cache_id_to_handle.remove(&cache_id);

                // If this was the current target, clear it
                if self.current_target == Some(cache_id) {
                    self.current_target = None;
                }
            }
        }
        Ok(())
    }

    /// Process TS_COMPDESK_REDIRSURF_ASSOC_LSURFACE - Association Management
    ///
    /// Per spec section 3.2.5.2.3
    fn process_assoc(&mut self, assoc: &CompDeskRedirSurfAssocLSurface) -> Result<(), DecodeError> {
        if assoc.associate {
            // Associate redirection surface with logical surface
            let lsurface =
                self.logical_surfaces
                    .get_mut(&assoc.h_lsurface)
                    .ok_or(invalid_field_err!(
                        "hLSurface",
                        "logical surface does not exist"
                    ))?;

            // Per spec: at most one redirection surface can be attached at a time
            if lsurface.associated_redir_surface.is_some() {
                return Err(invalid_field_err!(
                    "hLSurface",
                    "logical surface already has associated redirection surface"
                ));
            }

            // Verify redirection surface exists
            if !self.redir_surfaces.contains_key(&assoc.h_surf) {
                return Err(invalid_field_err!(
                    "hSurf",
                    "redirection surface does not exist"
                ));
            }

            lsurface.associated_redir_surface = Some(assoc.h_surf);
        } else {
            // Disassociate redirection surface from logical surface
            if let Some(lsurface) = self.logical_surfaces.get_mut(&assoc.h_lsurface) {
                lsurface.associated_redir_surface = None;
            }
        }
        Ok(())
    }

    /// Process TS_COMPDESK_LSURFACE_COMPREF_PENDING - Compositor Reference
    ///
    /// Per spec section 3.2.5.2.4
    fn process_compref(
        &mut self,
        compref: &CompDeskLSurfaceCompRefPending,
    ) -> Result<(), DecodeError> {
        // Mark that compositor has referenced this logical surface
        // Client should not release the surface until compositor retrieves it
        if let Some(lsurface) = self.logical_surfaces.get_mut(&compref.h_lsurface) {
            lsurface.compositor_referenced = true;
        }
        Ok(())
    }

    /// Process TS_COMPDESK_SWITCH_SURFOBJ - Retargeting Drawing Order
    ///
    /// Per spec section 3.2.5.3.1
    fn process_switch(&mut self, switch: &CompDeskSwitchSurfObj) -> Result<(), DecodeError> {
        // Set the current drawing target to the specified redirection surface
        // Subsequent drawing orders will be applied to this surface
        self.current_target = Some(switch.cache_id);
        Ok(())
    }

    /// Process TS_COMPDESK_FLUSH_COMPOSEONCE - Flush Compose-Once Surface
    ///
    /// Per spec section 3.2.5.3.2
    fn process_flush(&mut self, _flush: &CompDeskFlushComposeOnce) -> Result<(), DecodeError> {
        // Notify compositor that logical drawing operation is complete
        // The compositor should run a composition pass
        // This is typically an event to the rendering system
        Ok(())
    }

    /// Get the current drawing target redirection surface
    pub fn get_current_target(&self) -> Option<&RedirectionSurface> {
        self.current_target
            .and_then(|cache_id| self.cache_id_to_handle.get(&cache_id))
            .and_then(|handle| self.redir_surfaces.get(handle))
    }

    /// Get a logical surface by handle
    pub fn get_logical_surface(&self, handle: u64) -> Option<&LogicalSurface> {
        self.logical_surfaces.get(&handle)
    }

    /// Get a redirection surface by handle
    pub fn get_redirection_surface(&self, handle: u64) -> Option<&RedirectionSurface> {
        self.redir_surfaces.get(&handle)
    }

    /// Get a redirection surface by cache ID
    pub fn get_redirection_surface_by_cache_id(
        &self,
        cache_id: u32,
    ) -> Option<&RedirectionSurface> {
        self.cache_id_to_handle
            .get(&cache_id)
            .and_then(|handle| self.redir_surfaces.get(handle))
    }

    /// Iterate over all logical surfaces
    pub fn logical_surfaces(&self) -> impl Iterator<Item = &LogicalSurface> {
        self.logical_surfaces.values()
    }

    /// Iterate over all redirection surfaces
    pub fn redirection_surfaces(&self) -> impl Iterator<Item = &RedirectionSurface> {
        self.redir_surfaces.values()
    }

    /// Check if Desktop Composition is active
    pub fn is_compositing(&self) -> bool {
        self.drawing_mode == DrawingMode::Composited
            && self.desktop_mode == DesktopMode::ComposeDesktop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composition_lifecycle() {
        let mut state = DesktopCompositionState::new();

        // Initially non-composited
        assert_eq!(state.drawing_mode, DrawingMode::NonComposited);
        assert!(!state.is_compositing());

        // Turn on composition
        let toggle_on = CompDeskToggle::new(CompDeskToggleEventType::CompositionOn);
        state
            .process_order(&DesktopCompositionOrder::Toggle(toggle_on))
            .unwrap();

        assert_eq!(state.drawing_mode, DrawingMode::Composited);
        assert_eq!(state.desktop_mode, DesktopMode::ComposeDesktop);
        assert!(state.is_compositing());

        // Turn off composition
        let toggle_off = CompDeskToggle::new(CompDeskToggleEventType::CompositionOff);
        state
            .process_order(&DesktopCompositionOrder::Toggle(toggle_off))
            .unwrap();

        assert_eq!(state.drawing_mode, DrawingMode::NonComposited);
        assert!(!state.is_compositing());
    }

    #[test]
    fn test_logical_surface_lifecycle() {
        let mut state = DesktopCompositionState::new();

        // Create logical surface
        let lsurf = CompDeskLSurface::new_create(0x12345, LSurfaceFlags::REDIRECTION, 0xABCD);
        state
            .process_order(&DesktopCompositionOrder::LSurface(lsurf))
            .unwrap();

        assert!(state.get_logical_surface(0x12345).is_some());
        assert_eq!(state.get_logical_surface(0x12345).unwrap().hwnd, 0xABCD);

        // Destroy logical surface
        let lsurf_destroy = CompDeskLSurface::new_destroy(0x12345);
        state
            .process_order(&DesktopCompositionOrder::LSurface(lsurf_destroy))
            .unwrap();

        assert!(state.get_logical_surface(0x12345).is_none());
    }

    #[test]
    fn test_redirection_surface_lifecycle() {
        let mut state = DesktopCompositionState::new();

        // Create redirection surface
        let surf = CompDeskSurfObj::new_create(1, 32, 0x7050184, 64, 64);
        state
            .process_order(&DesktopCompositionOrder::SurfObj(surf))
            .unwrap();

        assert!(state.get_redirection_surface(0x7050184).is_some());
        assert_eq!(
            state.get_redirection_surface_by_cache_id(1).unwrap().width,
            64
        );

        // Destroy redirection surface
        let surf_destroy = CompDeskSurfObj::new_destroy(1, 0x7050184);
        state
            .process_order(&DesktopCompositionOrder::SurfObj(surf_destroy))
            .unwrap();

        assert!(state.get_redirection_surface(0x7050184).is_none());
    }

    #[test]
    fn test_surface_association() {
        let mut state = DesktopCompositionState::new();

        // Create logical and redirection surfaces
        let lsurf = CompDeskLSurface::new_create(0x100, LSurfaceFlags::REDIRECTION, 0x200);
        state
            .process_order(&DesktopCompositionOrder::LSurface(lsurf))
            .unwrap();

        let rsurf = CompDeskSurfObj::new_create(1, 32, 0x300, 64, 64);
        state
            .process_order(&DesktopCompositionOrder::SurfObj(rsurf))
            .unwrap();

        // Associate them
        let assoc = CompDeskRedirSurfAssocLSurface::new_associate(0x100, 0x300);
        state
            .process_order(&DesktopCompositionOrder::RedirSurfAssoc(assoc))
            .unwrap();

        let lsurface = state.get_logical_surface(0x100).unwrap();
        assert_eq!(lsurface.associated_redir_surface, Some(0x300));

        // Disassociate
        let disassoc = CompDeskRedirSurfAssocLSurface::new_disassociate(0x100, 0x300);
        state
            .process_order(&DesktopCompositionOrder::RedirSurfAssoc(disassoc))
            .unwrap();

        let lsurface = state.get_logical_surface(0x100).unwrap();
        assert_eq!(lsurface.associated_redir_surface, None);
    }

    #[test]
    fn test_switch_target() {
        let mut state = DesktopCompositionState::new();

        // Create redirection surface
        let surf = CompDeskSurfObj::new_create(5, 32, 0x500, 128, 128);
        state
            .process_order(&DesktopCompositionOrder::SurfObj(surf))
            .unwrap();

        // Switch to this surface
        let switch = CompDeskSwitchSurfObj::new(5);
        state
            .process_order(&DesktopCompositionOrder::SwitchSurfObj(switch))
            .unwrap();

        assert_eq!(state.current_target, Some(5));
        assert!(state.get_current_target().is_some());
        assert_eq!(state.get_current_target().unwrap().cache_id, 5);
    }
}
