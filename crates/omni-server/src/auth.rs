//! Single-admin-account authentication: login/logout with a server-side
//! session token in an HttpOnly cookie. No multi-user/per-camera
//! permissions yet - see docs/ROADMAP.md.
//!
//! Deliberately simple for a single-box home NVR: one account, argon2
//! password hashing, opaque random session tokens stored in SQLite
//! (`sessions` table) rather than a signed/stateless token - a plain
//! `DELETE` on logout is enough, and there's no need for JWT machinery
//! at this scale.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rand::distributions::Alphanumeric;
use rand::Rng;
use std::sync::Arc;

use crate::state::AppState;

pub const SESSION_COOKIE: &str = "omni_session";
const SESSION_LIFETIME: chrono::Duration = chrono::Duration::days(30);

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    // `.expect()`: the only failure mode is a password containing a NUL
    // byte or similar encoding issue, effectively unreachable for
    // anything typed into a login form.
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("hashing a plain password should not fail")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn generate_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

/// Ensures an admin account exists. If one doesn't, creates it with
/// `OMNI_ADMIN_PASSWORD` if set, otherwise a freshly generated random
/// password printed once to the log - the standard self-hosted-app
/// pattern for "there is no sensible default password to ship".
pub async fn bootstrap_admin(db: &omni_db::Db) -> anyhow::Result<()> {
    if db.admin_user().await?.is_some() {
        return Ok(());
    }
    let password = match std::env::var("OMNI_ADMIN_PASSWORD") {
        Ok(p) if !p.is_empty() => p,
        _ => generate_token(),
    };
    db.set_admin_user("admin", &hash_password(&password)).await?;
    tracing::warn!(
        "No admin account existed - created one. Username: admin  Password: {password}\n\
         This is only printed once. Log in and change it from the UI, or set OMNI_ADMIN_PASSWORD \
         before first boot next time to control it yourself."
    );
    Ok(())
}

/// Ensures the RTSP server's Basic-auth credential exists (separate from
/// the HTTP admin account - see `Db::rtsp_credentials`'s docs for why).
/// Same bootstrap pattern: `OMNI_RTSP_PASSWORD` if set, else random and
/// printed once.
pub async fn bootstrap_rtsp_credentials(db: &omni_db::Db) -> anyhow::Result<(String, String)> {
    if let Some(creds) = db.rtsp_credentials().await? {
        return Ok(creds);
    }
    let password = match std::env::var("OMNI_RTSP_PASSWORD") {
        Ok(p) if !p.is_empty() => p,
        _ => generate_token(),
    };
    db.set_rtsp_credentials("rtsp", &password).await?;
    tracing::warn!(
        "No RTSP credential existed - created one. Username: rtsp  Password: {password}\n\
         This is only printed once. View it from the UI (Settings), or set OMNI_RTSP_PASSWORD \
         before first boot next time to control it yourself."
    );
    Ok(("rtsp".to_string(), password))
}

/// New session lifetime, used both by the login handler (to know when to
/// expire it in the DB) and when writing the `Set-Cookie` header (whose
/// `Max-Age` should match).
pub fn session_expiry() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + SESSION_LIFETIME
}

pub fn set_cookie_header(token: &str) -> (header::HeaderName, String) {
    (
        header::SET_COOKIE,
        format!(
            "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
            SESSION_LIFETIME.num_seconds()
        ),
    )
}

pub fn clear_cookie_header() -> (header::HeaderName, String) {
    (
        header::SET_COOKIE,
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    )
}

fn session_token_from_request(req: &Request) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    cookie_header.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(&format!("{SESSION_COOKIE}="))
            .map(|v| v.to_string())
    })
}

/// Periodically deletes expired session rows. Not required for
/// correctness (`session_valid` already rejects an expired-but-present
/// token), just housekeeping so the table doesn't grow forever.
pub async fn run_session_sweeper(db: omni_db::Db) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        ticker.tick().await;
        if let Err(err) = db.delete_expired_sessions().await {
            tracing::warn!(%err, "failed to sweep expired sessions");
        }
    }
}

/// Rejects any request without a valid session cookie. Applied to every
/// `/api/*` route except `/api/auth/login` - see `routes::api_routes`.
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = session_token_from_request(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let valid = state
        .db
        .session_valid(&token)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}
