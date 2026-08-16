//! LemonSqueezy license-key validation.
//!
//! Endpoints used (keyless — the license key itself is the credential):
//!   POST /v1/licenses/activate    — first activation on this machine
//!   POST /v1/licenses/validate    — periodic re-check (at most every 30 days)
//!   POST /v1/licenses/deactivate  — free a machine slot
//!
//! No API key is compiled into the binary.

use serde::{Deserialize, Serialize};

const LS_API_BASE: &str = "https://api.lemonsqueezy.com/v1/licenses";

/// Re-validate at most every 30 days.
pub const REVALIDATION_INTERVAL_SECS: u64 = 30 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// Public types (returned to frontend via Tauri commands)
// ---------------------------------------------------------------------------

/// Startup license check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum LicenseStatus {
    /// No stored activation — must enter a key.
    NotActivated,
    /// Stored activation is valid, last check < 30 days ago. No network call.
    Valid,
    /// Re-validation succeeded, timestamp updated.
    Revalidated,
    /// LS explicitly rejected the key (revoked / disabled / invalid).
    Rejected { message: String },
    /// Re-validation failed due to network / 5xx, but a stored activation
    /// exists. App opens normally (fail-open).
    FailOpen,
}

/// Result of an activation attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationResult {
    pub success: bool,
    pub error: Option<String>,
    pub instance_id: Option<String>,
    pub is_limit_reached: bool,
    pub activation_limit: Option<u32>,
    pub activation_usage: Option<u32>,
}

// ---------------------------------------------------------------------------
// LS API response shapes (private)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LsActivateResponse {
    activated: bool,
    error: Option<String>,
    license_key: Option<LsLicenseKey>,
    instance: Option<LsInstance>,
}

#[derive(Debug, Deserialize)]
struct LsValidateResponse {
    valid: bool,
    error: Option<String>,
    #[allow(dead_code)]
    license_key: Option<LsLicenseKey>,
}

#[derive(Debug, Deserialize)]
struct LsDeactivateResponse {
    deactivated: bool,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LsLicenseKey {
    activation_limit: Option<u32>,
    activation_usage: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct LsInstance {
    id: String,
}

// ---------------------------------------------------------------------------
// Startup decision logic
// ---------------------------------------------------------------------------

pub enum StartupAction {
    /// Valid stored activation, checked < 30 days ago. Open immediately.
    Proceed,
    /// Stored activation exists but > 30 days stale. Re-validate.
    Revalidate { key: String, instance_id: String },
    /// No stored activation. Block until the user enters a key.
    NeedActivation,
}

/// Decide what to do at startup based on stored activation state.
pub fn check_startup(
    license_key: &Option<String>,
    instance_id: &Option<String>,
    last_validated: u64,
) -> StartupAction {
    match (license_key.as_deref(), instance_id.as_deref()) {
        (Some(key), Some(iid)) if !key.is_empty() && !iid.is_empty() => {
            let now = now_secs();
            if now.saturating_sub(last_validated) < REVALIDATION_INTERVAL_SECS {
                StartupAction::Proceed
            } else {
                StartupAction::Revalidate {
                    key: key.to_string(),
                    instance_id: iid.to_string(),
                }
            }
        }
        _ => StartupAction::NeedActivation,
    }
}

// ---------------------------------------------------------------------------
// Machine identity
// ---------------------------------------------------------------------------

/// Returns the macOS IOPlatformUUID (hardware UUID).
/// Stable across OS reinstalls and user-account changes.
pub fn get_machine_id() -> Result<String, String> {
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .map_err(|e| format!("Failed to read hardware UUID: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("IOPlatformUUID") {
            if let Some(uuid) = line.split('"').nth(3) {
                return Ok(uuid.to_string());
            }
        }
    }
    Err("Could not determine machine identity".to_string())
}

// ---------------------------------------------------------------------------
// API calls
// ---------------------------------------------------------------------------

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

/// Activate a license key on this machine.
///
/// This is the FIRST activation — no stored activation exists yet.
/// Network failure here is a hard block (not fail-open) because
/// there is nothing to fall back on.
pub async fn activate(license_key: &str, instance_name: &str) -> ActivationResult {
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return activation_err(&e),
    };

    let resp = match client
        .post(format!("{}/activate", LS_API_BASE))
        .header("Accept", "application/json")
        .form(&[
            ("license_key", license_key),
            ("instance_name", instance_name),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            return activation_err(
                "Unable to reach the license server. Check your internet connection and try again.",
            );
        }
    };

    let status = resp.status();

    let body: LsActivateResponse = match resp.json().await {
        Ok(b) => b,
        Err(_) => {
            return activation_err(&format!(
                "Unexpected response from license server (HTTP {})",
                status
            ));
        }
    };

    if body.activated {
        ActivationResult {
            success: true,
            error: None,
            instance_id: body.instance.map(|i| i.id),
            is_limit_reached: false,
            activation_limit: body.license_key.as_ref().and_then(|lk| lk.activation_limit),
            activation_usage: body.license_key.as_ref().and_then(|lk| lk.activation_usage),
        }
    } else {
        let err = body
            .error
            .unwrap_or_else(|| "Activation failed".to_string());
        let is_limit = err.to_lowercase().contains("activation limit");
        ActivationResult {
            success: false,
            error: Some(err),
            instance_id: None,
            is_limit_reached: is_limit,
            activation_limit: body.license_key.as_ref().and_then(|lk| lk.activation_limit),
            activation_usage: body.license_key.as_ref().and_then(|lk| lk.activation_usage),
        }
    }
}

/// Re-validation result.
pub enum ValidateResult {
    /// Key + instance are still valid.
    Valid,
    /// LS explicitly says invalid (revoked, disabled, etc.). Block entry.
    Invalid { message: String },
    /// Network unreachable, timeout, or LS 5xx. Fail open.
    NetworkError,
}

/// Re-validate a previously activated key.
///
/// Fail-open: network errors and 5xx responses return `NetworkError`,
/// and the caller lets the user in. Only an explicit `valid: false`
/// from LS blocks entry.
pub async fn validate(license_key: &str, instance_id: &str) -> ValidateResult {
    let client = match http_client() {
        Ok(c) => c,
        Err(_) => return ValidateResult::NetworkError,
    };

    let resp = match client
        .post(format!("{}/validate", LS_API_BASE))
        .header("Accept", "application/json")
        .form(&[
            ("license_key", license_key),
            ("instance_id", instance_id),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return ValidateResult::NetworkError,
    };

    // 5xx from LS → fail open (their problem, not the customer's)
    if resp.status().is_server_error() {
        return ValidateResult::NetworkError;
    }

    let body: LsValidateResponse = match resp.json().await {
        Ok(b) => b,
        // Unparseable response → fail open
        Err(_) => return ValidateResult::NetworkError,
    };

    if body.valid {
        ValidateResult::Valid
    } else {
        ValidateResult::Invalid {
            message: body
                .error
                .unwrap_or_else(|| "License is no longer valid".to_string()),
        }
    }
}

/// Deactivate this machine's instance, freeing a slot for another device.
pub async fn deactivate(license_key: &str, instance_id: &str) -> Result<(), String> {
    let client = http_client()?;

    let resp = client
        .post(format!("{}/deactivate", LS_API_BASE))
        .header("Accept", "application/json")
        .form(&[
            ("license_key", license_key),
            ("instance_id", instance_id),
        ])
        .send()
        .await
        .map_err(|_| {
            "Unable to reach the license server. Check your connection and try again.".to_string()
        })?;

    let body: LsDeactivateResponse = resp
        .json()
        .await
        .map_err(|_| "Unexpected response from license server".to_string())?;

    if body.deactivated {
        Ok(())
    } else {
        Err(body
            .error
            .unwrap_or_else(|| "Deactivation failed".to_string()))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn activation_err(msg: &str) -> ActivationResult {
    ActivationResult {
        success: false,
        error: Some(msg.to_string()),
        instance_id: None,
        is_limit_reached: false,
        activation_limit: None,
        activation_usage: None,
    }
}
