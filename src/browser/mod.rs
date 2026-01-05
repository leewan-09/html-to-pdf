pub mod instance;
pub mod pool;

use crate::error::ChromePathError;
use std::path::Path;
use std::process::Command;
use tracing::{debug, error, info, warn};

/// Common Chrome installation paths to try as fallbacks
const CHROME_FALLBACK_PATHS: &[&str] = &[
    "/usr/bin/google-chrome-stable",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium-browser",
    "/usr/bin/chromium",
    "/opt/google/chrome/chrome",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", // macOS
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",   // Windows
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe", // Windows 32-bit
];

/// Validates that a Chrome binary path is accessible and executable
fn validate_chrome_path(path: &str) -> Result<String, ChromePathError> {
    let path_buf = Path::new(path);

    // Try to resolve symlinks
    let resolved_path = if path_buf.exists() {
        match std::fs::canonicalize(path_buf) {
            Ok(p) => {
                debug!(original = %path, resolved = %p.display(), "Resolved Chrome path");
                p
            }
            Err(e) => {
                warn!(path = %path, error = %e, "Could not resolve symlink");
                path_buf.to_path_buf()
            }
        }
    } else {
        return Err(ChromePathError::NotFound {
            path: path.to_string(),
        });
    };

    // Check if it's executable by trying to run --version with no-sandbox for containers
    let mut cmd = Command::new(&resolved_path);
    cmd.arg("--version")
        .arg("--no-sandbox")
        .arg("--disable-setuid-sandbox");

    debug!(path = ?resolved_path, "Attempting to validate Chrome");

    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                info!(version = %version, "Chrome validation successful");
                Ok(resolved_path.to_string_lossy().to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(status = ?output.status, stderr = %stderr, "Chrome execution failed");
                Err(ChromePathError::NotExecutable {
                    path: path.to_string(),
                })
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to execute Chrome binary");
            // Try with just the original path as a fallback
            if path != resolved_path.to_string_lossy() {
                debug!(path = %path, "Retrying with original path");
                match Command::new(path).arg("--version").output() {
                    Ok(output) if output.status.success() => {
                        info!("Chrome validation successful with original path");
                        return Ok(path.to_string());
                    }
                    _ => {}
                }
            }
            Err(ChromePathError::ValidationFailed {
                path: path.to_string(),
                error: e.to_string(),
            })
        }
    }
}

/// Finds a valid Chrome installation by checking provided path and fallbacks
pub fn find_chrome_executable(provided_path: Option<String>) -> Result<String, ChromePathError> {
    let mut tried_paths = Vec::new();

    // First, try the provided path if available
    if let Some(path) = provided_path.clone() {
        debug!(path = %path, "Checking provided Chrome path");
        tried_paths.push(path.clone());
        match validate_chrome_path(&path) {
            Ok(validated_path) => {
                info!(path = %validated_path, "Using provided Chrome path");
                return Ok(validated_path);
            }
            Err(e) => {
                warn!(error = %e, "Provided Chrome path failed validation");
            }
        }
    }

    // Try to find Chrome using 'which' command as a fallback
    debug!("Attempting to find Chrome using 'which' command");
    if let Ok(output) = Command::new("which").arg("google-chrome-stable").output() {
        if output.status.success() {
            let chrome_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !chrome_path.is_empty() {
                debug!(path = %chrome_path, "Found Chrome via 'which'");
                tried_paths.push(chrome_path.clone());
                if let Ok(validated_path) = validate_chrome_path(&chrome_path) {
                    info!(path = %validated_path, "Using Chrome found via 'which'");
                    return Ok(validated_path);
                }
            }
        }
    }

    // Also try 'which chromium' and 'which chromium-browser'
    for browser in &["chromium", "chromium-browser", "google-chrome"] {
        if let Ok(output) = Command::new("which").arg(browser).output() {
            if output.status.success() {
                let browser_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !browser_path.is_empty() && !tried_paths.contains(&browser_path) {
                    debug!(browser = %browser, path = %browser_path, "Found browser via 'which'");
                    tried_paths.push(browser_path.clone());
                    if let Ok(validated_path) = validate_chrome_path(&browser_path) {
                        info!(browser = %browser, path = %validated_path, "Using browser found via 'which'");
                        return Ok(validated_path);
                    }
                }
            }
        }
    }

    // Try fallback paths
    debug!("Trying fallback Chrome paths");
    for &fallback_path in CHROME_FALLBACK_PATHS {
        if !tried_paths.contains(&fallback_path.to_string()) {
            tried_paths.push(fallback_path.to_string());
            match validate_chrome_path(fallback_path) {
                Ok(validated_path) => {
                    info!(path = %validated_path, "Using fallback Chrome path");
                    return Ok(validated_path);
                }
                Err(e) => {
                    debug!(path = %fallback_path, error = %e, "Fallback path failed");
                }
            }
        }
    }

    error!(tried_paths = ?tried_paths, "No valid Chrome installation found");
    Err(ChromePathError::NoValidInstallation { paths: tried_paths })
}
