//! Device flow login for Waddle CLI.
//!
//! Implements OAuth Device Flow (RFC 8628) to authenticate users
//! through their web browser.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use crate::config::Config;

/// Saved credentials from a successful login
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCredentials {
    /// User's UUID principal
    pub user_id: String,
    /// User's immutable username
    pub username: String,
    /// Auth provider ID used during login
    pub provider_id: String,
    /// Session ID / XMPP token
    pub token: String,
    /// JID for XMPP connection
    pub jid: String,
    /// XMPP server host
    pub xmpp_host: String,
    /// XMPP server port
    pub xmpp_port: u16,
    /// HTTP API server URL (for fetching waddles/channels)
    pub server_url: String,
    /// When these credentials were saved
    pub saved_at: String,
}

/// Response from POST /api/auth/device/start
#[derive(Debug, Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[allow(dead_code)]
    verification_uri_complete: String,
    interval: u32,
    #[allow(dead_code)]
    expires_in: u32,
}

/// Response from POST /api/auth/device/poll when pending
#[derive(Debug, Deserialize)]
struct DevicePollPendingResponse {
    status: String,
    #[allow(dead_code)]
    expires_in: u32,
}

/// Response from POST /api/auth/device/poll when complete
#[derive(Debug, Deserialize)]
struct DevicePollCompleteResponse {
    status: String,
    session_id: String,
    user_id: String,
    username: String,
    provider_id: String,
    jid: String,
    xmpp_host: String,
    xmpp_port: u16,
}

/// Error response from the API
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

/// Run the device flow login process
pub async fn run_login(provider: &str, server_url: &str) -> Result<()> {
    println!();
    println!("  Starting device login with provider '{}'", provider);
    println!();

    let client = reqwest::Client::new();

    // Step 1: Request device authorization
    print!("  Requesting authorization...");
    std::io::Write::flush(&mut std::io::stdout())?;

    let device_auth: DeviceAuthResponse = client
        .post(format!("{}/api/auth/device/start", server_url))
        .json(&serde_json::json!({ "provider": provider }))
        .send()
        .await
        .context("Failed to connect to server")?
        .error_for_status()
        .map_err(|e| {
            anyhow::anyhow!(
                "Server error: {}. Is the server running at {}?",
                e,
                server_url
            )
        })?
        .json()
        .await
        .context("Failed to parse server response")?;

    println!(" done!");
    println!();

    // Step 2: Show user the code and URL
    println!("  +-----------------------------------------+");
    println!("  |                                         |");
    println!(
        "  |   Go to: {}   ",
        format_url(&device_auth.verification_uri)
    );
    println!("  |                                         |");
    println!("  |   Enter code:  {}            |", device_auth.user_code);
    println!("  |                                         |");
    println!("  +-----------------------------------------+");
    println!();

    // Try to open the browser automatically
    if open_browser(&device_auth.verification_uri) {
        println!("  (Browser opened automatically)");
    } else {
        println!("  (Please open the URL above in your browser)");
    }
    println!();
    println!("  Waiting for authorization...");

    // Step 3: Poll until complete
    let poll_interval = Duration::from_secs(device_auth.interval as u64);
    let mut attempts = 0;
    let max_attempts = 180; // 15 minutes at 5 second intervals

    loop {
        attempts += 1;
        if attempts > max_attempts {
            return Err(anyhow::anyhow!(
                "Authorization timed out. Please try again."
            ));
        }

        tokio::time::sleep(poll_interval).await;

        let response = client
            .post(format!("{}/api/auth/device/poll", server_url))
            .json(&serde_json::json!({ "device_code": device_auth.device_code }))
            .send()
            .await
            .context("Failed to poll for authorization")?;

        let status = response.status();

        if status.is_success() {
            // Try to parse as complete response first
            let text = response.text().await?;

            // Check if it's a pending response
            if let Ok(pending) = serde_json::from_str::<DevicePollPendingResponse>(&text) {
                if pending.status == "authorization_pending" {
                    print!(".");
                    std::io::Write::flush(&mut std::io::stdout())?;
                    continue;
                }
            }

            // Try to parse as complete response
            if let Ok(complete) = serde_json::from_str::<DevicePollCompleteResponse>(&text) {
                if complete.status == "complete" {
                    println!();
                    println!();

                    // Save credentials
                    let creds = SavedCredentials {
                        user_id: complete.user_id.clone(),
                        username: complete.username.clone(),
                        provider_id: complete.provider_id.clone(),
                        token: complete.session_id.clone(),
                        jid: complete.jid.clone(),
                        xmpp_host: complete.xmpp_host.clone(),
                        xmpp_port: complete.xmpp_port,
                        server_url: server_url.to_string(),
                        saved_at: chrono::Utc::now().to_rfc3339(),
                    };

                    save_credentials(&creds)?;

                    println!("  Success! Logged in as @{}", complete.username);
                    println!();
                    println!("  You can now run 'waddle' to start the TUI.");
                    println!();

                    return Ok(());
                }
            }

            // Unknown response
            return Err(anyhow::anyhow!("Unexpected response from server: {}", text));
        } else if status.as_u16() == 400 {
            // Could be expired or invalid
            let error: ErrorResponse =
                serde_json::from_str(&response.text().await?).unwrap_or(ErrorResponse {
                    error: "unknown".to_string(),
                    message: "Unknown error".to_string(),
                });

            if error.error == "expired_token" {
                println!();
                return Err(anyhow::anyhow!("Authorization expired. Please try again."));
            }

            return Err(anyhow::anyhow!("Authorization failed: {}", error.message));
        } else if status.as_u16() == 403 {
            println!();
            return Err(anyhow::anyhow!("Authorization denied."));
        } else {
            return Err(anyhow::anyhow!("Server error: {}", status));
        }
    }
}

/// Get the path to the credentials file
fn credentials_path() -> Result<PathBuf> {
    let data_dir = Config::data_dir()?;
    Ok(data_dir.join("credentials.json"))
}

/// Save credentials to disk
fn save_credentials(creds: &SavedCredentials) -> Result<()> {
    let path = credentials_path()?;
    let content = serde_json::to_string_pretty(creds)?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to save credentials to {:?}", path))?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o600); // Owner read/write only
        std::fs::set_permissions(&path, perms)?;
    }

    Ok(())
}

/// Load saved credentials from disk
pub fn load_credentials() -> Result<SavedCredentials> {
    let path = credentials_path()?;
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("No credentials found at {:?}", path))?;
    let creds: SavedCredentials =
        serde_json::from_str(&content).context("Failed to parse credentials file")?;
    Ok(creds)
}

/// Clear saved credentials
pub fn clear_credentials() -> Result<()> {
    let path = credentials_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Try to open a URL in the default browser
fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn().is_ok()
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .is_ok()
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

/// Format URL for display (pad to consistent width)
fn format_url(url: &str) -> String {
    let display = if url.len() > 30 {
        format!("{}...", &url[..27])
    } else {
        url.to_string()
    };
    format!("{:<30}", display)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_url_short() {
        let result = format_url("http://localhost:3000");
        assert_eq!(result.len(), 30);
    }

    #[test]
    fn test_format_url_long() {
        let result = format_url("http://localhost:3000/api/auth/device/verify");
        assert!(result.contains("..."));
        assert_eq!(result.len(), 30);
    }
}
