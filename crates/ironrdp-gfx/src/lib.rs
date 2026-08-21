//! RDPEGFX (MS-RDPEGFX) client implementation
//!
//! This crate implements the Graphics Virtual Channel Extension for Microsoft RDP,
//! providing support for advanced graphics codecs including H.264 (AVC420/AVC444),
//! RFX Progressive, and others.
//!
//! # Architecture
//!
//! The crate is organized into:
//! - `client`: Main GfxClient state machine
//! - `pdu`: PDU structures and parsing
//! - `codec`: Codec IDs and helpers
//! - `caps`: Capability negotiation
//!
//! # Example
//! ```ignore
//! use ironrdp_gfx::{GfxClient, GfxContext};
//!
//! struct MyContext { /* ... */ }
//!
//! impl GfxContext for MyContext {
//!     fn send(&mut self, data: &[u8]) -> anyhow::Result<()> {
//!         // Send via DRDYNVC channel
//!         Ok(())
//!     }
//!
//!     fn on_create_surface(
//!         &mut self,
//!         surface_id: u16,
//!         width: u16,
//!         height: u16,
//!         pixel_format: u8,
//!     ) -> anyhow::Result<()> {
//!         // Create rendering surface
//!         Ok(())
//!     }
//!
//!     // ... implement the remaining required callbacks
//! }
//!
//! let mut gfx = GfxClient::new(MyContext { /* ... */ }, false, false);
//! gfx.send_caps_advertise()?;
//!
//! // Process incoming data
//! let decompressed_data = vec![...]; // ZGFX decompressed
//! gfx.process_pdu_stream(&decompressed_data)?;
//! ```

pub mod caps;
pub mod client;
pub mod codec;
pub mod pdu;
pub mod server;

pub use client::{GfxClient, GfxContext};
pub use codec::*;
