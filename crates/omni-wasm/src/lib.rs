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

#[wasm_bindgen]
pub fn validate_segment_seconds(secs: u32) -> Result<(), JsError> {
    validate::validate_segment_seconds(secs).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_rtsp_url(url: &str) -> Result<(), JsError> {
    validate::validate_rtsp_url(url).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_retention(
    recording_enabled: bool,
    max_age_secs: Option<u64>,
    max_size_bytes: Option<u64>,
) -> Result<(), JsError> {
    validate::validate_retention(recording_enabled, max_age_secs, max_size_bytes)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_sensitivity(sensitivity: u8) -> Result<(), JsError> {
    validate::validate_sensitivity(sensitivity).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_webhook_url(url: &str) -> Result<(), JsError> {
    validate::validate_webhook_url(url).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_password(password: &str) -> Result<(), JsError> {
    validate::validate_password(password).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn validate_group_name(name: &str) -> Result<(), JsError> {
    validate::validate_group_name(name).map_err(|e| JsError::new(&e.to_string()))
}
