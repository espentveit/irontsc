//! Geometry DVC Channel
//!
//! Handles the Microsoft::Windows::RDS::Geometry::v08.01 channel
//! for tracking video region positions on the remote desktop.

use ironrdp_core::AsAny;
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_geometry::{MappedGeometry, GEOMETRY_CHANNEL_NAME};
use ironrdp_pdu::PduResult;
use tracing::{info, warn};

use crate::video_redirect::SharedVideoRedirectionManager;

/// Geometry DVC Processor
pub struct GeometryProcessor {
    manager: SharedVideoRedirectionManager,
    channel_id: Option<u32>,
}

impl GeometryProcessor {
    pub fn new(manager: SharedVideoRedirectionManager) -> Self {
        Self {
            manager,
            channel_id: None,
        }
    }
}

impl AsAny for GeometryProcessor {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl DvcProcessor for GeometryProcessor {
    fn channel_name(&self) -> &str {
        GEOMETRY_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!("📐 Geometry channel opened! channel_id={}", channel_id);
        self.channel_id = Some(channel_id);
        Ok(Vec::new())
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        info!(
            "📥 Geometry: Received {} bytes on channel {}",
            payload.len(),
            channel_id
        );

        if payload.is_empty() {
            return Ok(Vec::new());
        }

        // Parse mapped geometry
        let geometry = match MappedGeometry::parse(payload) {
            Ok(g) => g,
            Err(e) => {
                warn!("❌ Geometry: Failed to parse geometry: {}", e);
                return Ok(Vec::new());
            }
        };

        // Handle in manager
        if let Ok(mut manager) = self.manager.lock() {
            if let Err(e) = manager.handle_geometry(geometry) {
                warn!("❌ Geometry: Failed to handle geometry: {}", e);
            }
        } else {
            warn!("❌ Geometry: Failed to lock manager");
        }

        // No response needed for geometry updates
        Ok(Vec::new())
    }

    fn close(&mut self, channel_id: u32) {
        info!("🔌 Geometry channel closed (ID: {})", channel_id);
        self.channel_id = None;
    }
}
