//! The IronTSC client as a library, so that more than one shell can be built on it.
//!
//! Everything below the frontend -- the connector, the graphics pipeline, the transports, the
//! settings file and the input translation -- is the same code whichever window it is driven
//! from. It lives here rather than inside the binary so a second shell can depend on it as a
//! crate instead of compiling the same files again behind `#[path]`, which is what having two
//! frontends cost the last time.
//!
//! The binary in `main.rs` is a thin wrapper over [`egui_app::run`]; nothing here knows about
//! it, and nothing here draws a window of its own.

// Re-exported so that anything built on this crate is guaranteed to be speaking to the same
// protocol types, rather than a second copy resolved through its own dependency graph.
pub use ironrdp;
pub use smallvec;

pub mod agent;
pub mod cliprdr_channel;
pub mod config;
pub mod console;
pub mod core_input_channel;
pub mod dtls_udp;
pub mod dvc_compression;
pub mod gfx;
pub mod gfx_channel;
pub mod h264_codec_caps;
pub mod mouse_cursor_channel;
pub mod preferences;
pub mod rdp;
pub mod stub_dvc;
pub mod transport_rules;
pub mod udp_gfx;
pub mod udp_transport;

#[cfg(feature = "video-redirection")]
pub mod geometry_channel;
#[cfg(feature = "video-redirection")]
pub mod video_control_channel;
#[cfg(feature = "video-redirection")]
pub mod video_data_channel;
#[cfg(feature = "video-redirection")]
pub mod video_redirect;

pub mod egui_app;
pub mod egui_scancode;
pub mod egui_shortcuts;
pub mod settings;
