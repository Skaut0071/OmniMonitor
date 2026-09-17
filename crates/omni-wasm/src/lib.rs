//! Compiled to WebAssembly (via `wasm-pack`) and imported directly by the
//! Svelte admin UI, so the "add camera" form validates against the exact
//! same rules the server enforces in `omni-db`/`omni-server` - both sides
//! call straight into `omni_core::validate`, so there is exactly one
//! implementation of each rule, not a JS copy that can drift from Rust.

use omni_core::validate;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn validate_camera_name(name: &str) -> Result<(), JsError> {
    validate::validate_camera_name(name).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_resolution(width: u32, height: u32) -> Result<(), JsError> {
    validate::validate_resolution(width, height).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_framerate(fps: u32) -> Result<(), JsError> {
    validate::validate_framerate(fps).map_err(|e| JsError::new(&e.to_string()))
}
