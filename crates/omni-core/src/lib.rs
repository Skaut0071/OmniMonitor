//! Shared types and pure logic used by both the backend (native) and the
//! frontend (compiled to wasm via the `omni-wasm` crate). Keeping this crate
//! free of tokio/gstreamer/etc. dependencies is what makes it compilable to
//! `wasm32-unknown-unknown`.

pub mod camera;
pub mod config;
pub mod validate;

pub use camera::{Camera, CameraKind, CameraStatus, RecordingSettings, StreamCodec};
pub use config::AppConfig;
