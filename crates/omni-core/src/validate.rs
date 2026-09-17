//! Validation logic shared between the Rust backend and the browser (via the
//! `omni-wasm` bindings) so the same rules apply in the admin UI's form
//! validation and the server's API validation - one source of truth.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("camera name must not be empty")]
    EmptyName,
    #[error("camera name must be 64 characters or fewer")]
    NameTooLong,
    #[error("resolution must be non-zero in both dimensions")]
    InvalidResolution,
    #[error("framerate must be between 1 and 120")]
    InvalidFramerate,
}

pub fn validate_camera_name(name: &str) -> Result<(), ValidationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::EmptyName);
    }
    if trimmed.chars().count() > 64 {
        return Err(ValidationError::NameTooLong);
    }
    Ok(())
}

pub fn validate_resolution(width: u32, height: u32) -> Result<(), ValidationError> {
    if width == 0 || height == 0 {
        return Err(ValidationError::InvalidResolution);
    }
    Ok(())
}

pub fn validate_framerate(fps: u32) -> Result<(), ValidationError> {
    if fps == 0 || fps > 120 {
        return Err(ValidationError::InvalidFramerate);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_name() {
        assert_eq!(validate_camera_name("   "), Err(ValidationError::EmptyName));
    }

    #[test]
    fn accepts_normal_name() {
        assert!(validate_camera_name("Front Door").is_ok());
    }

    #[test]
    fn rejects_zero_resolution() {
        assert_eq!(
            validate_resolution(0, 720),
            Err(ValidationError::InvalidResolution)
        );
    }

    #[test]
    fn rejects_absurd_framerate() {
        assert_eq!(
            validate_framerate(240),
            Err(ValidationError::InvalidFramerate)
        );
    }
}
