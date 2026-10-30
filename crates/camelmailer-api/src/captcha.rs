//! Captcha verification for self-registration.
//!
//! Provider `cap` speaks the siteverify contract of Cap
//! (https://trycap.dev): `POST {endpoint}siteverify` with the JSON body
//! `{ "secret": …, "response": <token> }`, answered by `{ "success": true }`
//! for a valid token. Any service with the same contract works. The widget
//! in the browser talks to `endpoint` directly; the secret never leaves the
//! server.

use camelmailer_config::{Captcha, CaptchaProvider};
use serde_json::json;
use std::time::Duration;

/// Why a token was not accepted.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptchaError {
    /// The verifier answered and said no.
    Rejected,
    /// The verifier could not be reached or answered nonsense.
    Unavailable(String),
}

/// Verify `token` against the configured provider. With provider `none`
/// every request passes.
pub async fn verify(config: &Captcha, token: &str) -> Result<(), CaptchaError> {
    match config.provider {
        CaptchaProvider::None => Ok(()),
        CaptchaProvider::Cap => verify_cap(config, token).await,
    }
}

async fn verify_cap(config: &Captcha, token: &str) -> Result<(), CaptchaError> {
    if token.trim().is_empty() {
        return Err(CaptchaError::Rejected);
    }
    let url = config
        .siteverify_url()
        .ok_or_else(|| CaptchaError::Unavailable("captcha.endpoint is not set".into()))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| CaptchaError::Unavailable(error.to_string()))?;
    let response = client
        .post(&url)
        .json(&json!({
            "secret": config.secret.as_deref().unwrap_or_default(),
            "response": token,
        }))
        .send()
        .await
        .map_err(|error| CaptchaError::Unavailable(error.to_string()))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| CaptchaError::Unavailable(format!("{status}: {error}")))?;
    if body["success"].as_bool() == Some(true) {
        return Ok(());
    }
    if status.is_server_error() {
        return Err(CaptchaError::Unavailable(format!(
            "verifier returned {status}"
        )));
    }
    Err(CaptchaError::Rejected)
}
