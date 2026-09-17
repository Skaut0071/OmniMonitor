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
    #[error("recording segment length must be between 10 and 3600 seconds")]
    InvalidSegmentSeconds,
    #[error("RTSP URL must start with rtsp://")]
    InvalidRtspUrl,
    #[error("enable recording requires a max age and/or a max size limit, so it doesn't fill the disk forever")]
    RecordingWithoutRetentionLimit,
    #[error("motion sensitivity must be between 1 and 100")]
    InvalidSensitivity,
    #[error("webhook URL must start with http:// or https://")]
    InvalidWebhookUrl,
    #[error("password must be at least 8 characters")]
    PasswordTooShort,
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

pub fn validate_segment_seconds(secs: u32) -> Result<(), ValidationError> {
    if !(10..=3600).contains(&secs) {
        return Err(ValidationError::InvalidSegmentSeconds);
    }
    Ok(())
}

pub fn validate_rtsp_url(url: &str) -> Result<(), ValidationError> {
    if !url.trim().starts_with("rtsp://") {
        return Err(ValidationError::InvalidRtspUrl);
    }
    Ok(())
}

/// A retention policy with neither limit set would record forever without
/// ever deleting anything, silently filling the disk - require at least
/// one bound whenever recording is turned on.
pub fn validate_retention(
    recording_enabled: bool,
    max_age_secs: Option<u64>,
    max_size_bytes: Option<u64>,
) -> Result<(), ValidationError> {
    if recording_enabled && max_age_secs.is_none() && max_size_bytes.is_none() {
        return Err(ValidationError::RecordingWithoutRetentionLimit);
    }
    Ok(())
}

pub fn validate_sensitivity(sensitivity: u8) -> Result<(), ValidationError> {
    if !(1..=100).contains(&sensitivity) {
        return Err(ValidationError::InvalidSensitivity);
    }
    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), ValidationError> {
    if password.chars().count() < 8 {
        return Err(ValidationError::PasswordTooShort);
    }
    Ok(())
}

pub fn validate_webhook_url(url: &str) -> Result<(), ValidationError> {
    let trimmed = url.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ValidationError::InvalidWebhookUrl);
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

    #[test]
    fn rejects_recording_without_any_retention_limit() {
        assert_eq!(
            validate_retention(true, None, None),
            Err(ValidationError::RecordingWithoutRetentionLimit)
        );
    }

    #[test]
    fn accepts_recording_with_one_retention_limit() {
        assert!(validate_retention(true, Some(86_400), None).is_ok());
        assert!(validate_retention(true, None, Some(10_000_000_000)).is_ok());
    }

    #[test]
    fn retention_limits_irrelevant_when_recording_disabled() {
        assert!(validate_retention(false, None, None).is_ok());
    }

    #[test]
    fn rejects_non_rtsp_url() {
        assert_eq!(
            validate_rtsp_url("http://example.com"),
            Err(ValidationError::InvalidRtspUrl)
        );
    }

    #[test]
    fn rejects_out_of_range_sensitivity() {
        assert_eq!(
            validate_sensitivity(0),
            Err(ValidationError::InvalidSensitivity)
        );
        assert!(validate_sensitivity(50).is_ok());
    }

    #[test]
    fn rejects_non_http_webhook() {
        assert_eq!(
            validate_webhook_url("ftp://example.com"),
            Err(ValidationError::InvalidWebhookUrl)
        );
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
    }
}
