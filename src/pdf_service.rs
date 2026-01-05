use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use chromiumoxide::page::Page;
use futures_util::stream::StreamExt;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{timeout, sleep};
use std::time::{Duration, Instant};
use std::path::Path;
use std::process::Command;
use std::collections::VecDeque;
use thiserror::Error;
use tracing::{info, warn, error, debug};

use crate::models::PdfOptions;

#[derive(Error, Debug)]
pub enum ChromePathError {
    #[error("Chrome binary not found at path: {path}")]
    NotFound { path: String },
    #[error("Chrome binary is not executable: {path}")]
    NotExecutable { path: String },
    #[error("Chrome binary validation failed: {path}, error: {error}")]
    ValidationFailed { path: String, error: String },
    #[error("No valid Chrome installation found. Tried paths: {paths:?}")]
    NoValidInstallation { paths: Vec<String> },
}

#[derive(Error, Debug)]
pub enum PdfError {
    #[error("Browser connection lost")]
    BrowserConnectionLost,
    #[error("Failed to create browser page: {0}")]
    PageCreationFailed(String),
    #[error("PDF generation failed: {0}")]
    GenerationFailed(String),
    #[error("Timeout while generating PDF")]
    Timeout,
    #[error("All browser instances failed")]
    AllInstancesFailed,
    #[error("Chrome path error: {0}")]
    ChromePath(#[from] ChromePathError),
    #[error("PDF size {size_mb:.2}MB exceeds limit of {limit_mb}MB")]
    SizeExceeded { size_mb: f64, limit_mb: usize },
}

/// Browser instance with health tracking
struct BrowserInstance {
    browser: Arc<Browser>,
    created_at: Instant,
    last_used: Mutex<Instant>,
    usage_count: Mutex<usize>,
    is_healthy: Arc<RwLock<bool>>,
    user_data_dir: std::path::PathBuf,
}

impl BrowserInstance {
    async fn new(chrome_path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (browser, handler, user_data_dir) = Self::launch_browser(chrome_path).await?;

        // Spawn handler with health monitoring
        let browser_arc = Arc::new(browser);
        let browser_health = Arc::new(RwLock::new(true));
        let health_clone = browser_health.clone();

        tokio::spawn(async move {
            let mut handler = handler;
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    warn!(error = %e, "Browser handler error detected");
                    // Mark browser as unhealthy on connection errors
                    if e.to_string().contains("closed connection") ||
                       e.to_string().contains("WebSocket") {
                        *health_clone.write().await = false;
                        break;
                    }
                }
            }
            warn!("Browser handler loop ended - marking as unhealthy");
            *health_clone.write().await = false;
        });

        Ok(BrowserInstance {
            browser: browser_arc,
            created_at: Instant::now(),
            last_used: Mutex::new(Instant::now()),
            usage_count: Mutex::new(0),
            is_healthy: browser_health,
            user_data_dir,
        })
    }

    async fn launch_browser(chrome_path: &str) -> Result<(Browser, chromiumoxide::Handler, std::path::PathBuf), Box<dyn std::error::Error + Send + Sync>> {
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::env;

        // Generate unique ID for this browser instance
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let thread_id = std::thread::current().id();
        let random_num = format!("{:?}", thread_id).chars().filter(|c| c.is_numeric()).collect::<String>();
        let temp_dir = env::temp_dir();
        let user_data_dir = temp_dir.join(format!("chrome-rust-pdf-{}-{}", timestamp, random_num));

        // Ensure the directory exists
        std::fs::create_dir_all(&user_data_dir)?;

        let config = BrowserConfig::builder()
            .chrome_executable(chrome_path)
            .user_data_dir(&user_data_dir)
            .no_sandbox()
            .launch_timeout(Duration::from_secs(60))
            .args(vec![
                "--disable-setuid-sandbox".to_string(),
                "--disable-dev-shm-usage".to_string(),
                "--disable-features=VizDisplayCompositor".to_string(),
                "--disable-gpu".to_string(),
                "--disable-background-timer-throttling".to_string(),
                "--disable-backgrounding-occluded-windows".to_string(),
                "--disable-renderer-backgrounding".to_string(),
                "--disable-extensions".to_string(),
                "--disable-default-apps".to_string(),
                "--no-first-run".to_string(),
                "--no-default-browser-check".to_string(),
                "--headless".to_string(),
                "--timezone=Indian/Maldives".to_string(),
                // Resource limits
                "--max-old-space-size=512".to_string(), // Limit V8 heap to 512MB
                "--js-flags=--max-old-space-size=512".to_string(),
            ]);

        let (browser, handler) = Browser::launch(config.build()?).await?;
        Ok((browser, handler, user_data_dir))
    }

    async fn is_healthy(&self) -> bool {
        *self.is_healthy.read().await
    }

    async fn mark_used(&self) {
        *self.last_used.lock().await = Instant::now();
        *self.usage_count.lock().await += 1;
    }

    fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    async fn should_retire(&self) -> bool {
        // Retire after 20 minutes or 30 uses (aggressive recycling for stability)
        self.age() > Duration::from_secs(1200) ||
        *self.usage_count.lock().await > 30
    }

    async fn idle_time(&self) -> Duration {
        self.last_used.lock().await.elapsed()
    }

    async fn is_idle_too_long(&self) -> bool {
        // Recycle browsers idle for more than 5 minutes
        self.idle_time().await > Duration::from_secs(300)
    }

    /// Quick health ping - verify browser is responsive via CDP
    async fn health_ping(&self) -> bool {
        // Try to get browser version - fast CDP call to verify connection
        match timeout(Duration::from_secs(5), self.browser.version()).await {
            Ok(Ok(_)) => true,
            Ok(Err(e)) => {
                warn!(error = %e, "Browser health ping failed");
                false
            }
            Err(_) => {
                warn!("Browser health ping timed out");
                false
            }
        }
    }
}

impl Drop for BrowserInstance {
    fn drop(&mut self) {
        let user_data_dir = self.user_data_dir.clone();

        debug!("Dropping browser instance - cleaning up resources");

        // Try to spawn cleanup task, fall back to sync cleanup if runtime unavailable
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    // Wait a bit to ensure browser process has fully terminated
                    sleep(Duration::from_secs(2)).await;

                    // Remove the user data directory
                    if user_data_dir.exists() {
                        match std::fs::remove_dir_all(&user_data_dir) {
                            Ok(_) => debug!(path = ?user_data_dir, "Cleaned up browser temp directory"),
                            Err(e) => warn!(path = ?user_data_dir, error = %e, "Failed to clean up temp directory"),
                        }
                    }
                });
            }
            Err(_) => {
                // Runtime not available, do sync cleanup
                if user_data_dir.exists() {
                    // Small sync sleep to let browser terminate
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Err(e) = std::fs::remove_dir_all(&user_data_dir) {
                        // Can't use tracing here - runtime is gone, use eprintln as fallback
                        eprintln!("Failed to clean up temp directory {:?}: {}", user_data_dir, e);
                    }
                }
            }
        }
    }
}

/// Circuit breaker state - combined into single struct to prevent deadlock from inconsistent lock ordering
struct CircuitBreakerState {
    consecutive_failures: usize,
    /// Timestamp when circuit breaker was last tripped (for half-open recovery)
    tripped_at: Option<Instant>,
}

impl CircuitBreakerState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            tripped_at: None,
        }
    }

    fn record_failure(&mut self, max_failures: usize) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= max_failures && self.tripped_at.is_none() {
            self.tripped_at = Some(Instant::now());
        }
    }

    fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.tripped_at = None;
    }

    fn is_open(&self, max_failures: usize) -> bool {
        self.consecutive_failures >= max_failures
    }

    fn should_attempt_recovery(&self, cooldown_secs: u64) -> bool {
        if let Some(trip_time) = self.tripped_at {
            trip_time.elapsed() >= Duration::from_secs(cooldown_secs)
        } else {
            false
        }
    }

    fn reset_cooldown(&mut self) {
        self.tripped_at = Some(Instant::now());
    }
}

/// Connection pool for browser instances
struct BrowserPool {
    instances: Arc<Mutex<VecDeque<Arc<BrowserInstance>>>>,
    chrome_path: String,
    max_instances: usize,
    min_instances: usize,
    /// Circuit breaker state - single lock to prevent deadlock
    circuit_breaker: Arc<Mutex<CircuitBreakerState>>,
}

impl BrowserPool {
    async fn new(chrome_path: String, min_instances: usize, max_instances: usize) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut instances = VecDeque::new();

        // Create initial instances
        for i in 0..min_instances {
            info!(current = i + 1, total = min_instances, "Creating browser instance");
            match BrowserInstance::new(&chrome_path).await {
                Ok(instance) => instances.push_back(Arc::new(instance)),
                Err(e) => error!(error = %e, "Failed to create initial browser instance"),
            }
        }

        if instances.is_empty() {
            return Err("Failed to create any browser instances".into());
        }

        Ok(BrowserPool {
            instances: Arc::new(Mutex::new(instances)),
            chrome_path,
            max_instances,
            min_instances,
            circuit_breaker: Arc::new(Mutex::new(CircuitBreakerState::new())),
        })
    }

    async fn get_healthy_instance(&self) -> Result<Arc<BrowserInstance>, PdfError> {
        const MAX_RETRIES: usize = 3;

        for attempt in 0..MAX_RETRIES {
            let mut instances = self.instances.lock().await;

            // Remove unhealthy instances - collect indices to remove first
            let mut to_remove = Vec::new();
            for (idx, instance) in instances.iter().enumerate() {
                if !instance.is_healthy().await {
                    warn!("Removing unhealthy browser instance");
                    to_remove.push(idx);
                }
            }
            // Remove in reverse order to preserve indices
            for idx in to_remove.into_iter().rev() {
                instances.remove(idx);
            }

            // Try to find a healthy instance that shouldn't retire
            for instance in instances.iter() {
                if instance.is_healthy().await && !instance.should_retire().await {
                    // Pre-use health ping - verify browser is actually responsive
                    if !instance.health_ping().await {
                        warn!("Browser failed pre-use health ping, marking unhealthy");
                        *instance.is_healthy.write().await = false;
                        continue;
                    }
                    instance.mark_used().await;
                    // Reset circuit breaker on success
                    self.circuit_breaker.lock().await.reset();
                    return Ok(Arc::clone(instance));
                }
            }

            // Create new instance if needed and allowed
            if instances.len() < self.max_instances {
                info!(attempt = attempt + 1, "Creating new browser instance");
                match BrowserInstance::new(&self.chrome_path).await {
                    Ok(new_instance) => {
                        let instance_arc = Arc::new(new_instance);
                        instance_arc.mark_used().await;
                        instances.push_back(Arc::clone(&instance_arc));
                        // Reset circuit breaker on success
                        self.circuit_breaker.lock().await.reset();
                        return Ok(instance_arc);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to create new browser instance");
                        // Record failure in circuit breaker
                        self.circuit_breaker.lock().await.record_failure(10);
                        if attempt < MAX_RETRIES - 1 {
                            // Wait before retry
                            drop(instances); // Release lock before sleeping
                            sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
                        }
                    }
                }
            } else if instances.is_empty() {
                // No instances available and can't create more
                return Err(PdfError::AllInstancesFailed);
            }
        }

        Err(PdfError::AllInstancesFailed)
    }

    async fn maintain(&self) {
        const MAX_CONSECUTIVE_FAILURES: usize = 10;
        const CIRCUIT_BREAKER_RECOVERY_SECS: u64 = 120; // 2 minutes before attempting recovery

        // Check circuit breaker state - acquire lock once and release before any async work
        {
            let cb = self.circuit_breaker.lock().await;
            if cb.is_open(MAX_CONSECUTIVE_FAILURES) {
                let pool_size = self.instances.lock().await.len();

                if !cb.should_attempt_recovery(CIRCUIT_BREAKER_RECOVERY_SECS) {
                    // Still in cooldown period, skip maintenance
                    let remaining = if let Some(trip_time) = cb.tripped_at {
                        CIRCUIT_BREAKER_RECOVERY_SECS.saturating_sub(trip_time.elapsed().as_secs())
                    } else {
                        CIRCUIT_BREAKER_RECOVERY_SECS
                    };
                    warn!(
                        failures = cb.consecutive_failures,
                        pool_size = pool_size,
                        recovery_in_secs = remaining,
                        "Maintenance: circuit breaker open"
                    );

                    // If no trip time recorded, set it now
                    if cb.tripped_at.is_none() {
                        drop(cb);
                        self.circuit_breaker.lock().await.reset_cooldown();
                    }
                    return;
                }

                // Half-open state: will attempt recovery after releasing lock
                let elapsed = cb.tripped_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                drop(cb);

                info!(
                    cooldown_secs = elapsed,
                    "Maintenance: circuit breaker half-open, attempting recovery"
                );

                match BrowserInstance::new(&self.chrome_path).await {
                    Ok(new_instance) => {
                        let mut instances = self.instances.lock().await;
                        instances.push_back(Arc::new(new_instance));
                        drop(instances);
                        // Reset circuit breaker on success
                        self.circuit_breaker.lock().await.reset();
                        info!("Maintenance: circuit breaker RESET - recovery successful!");
                        return;
                    }
                    Err(e) => {
                        error!(error = %e, "Maintenance: half-open recovery failed");
                        // Reset the trip time to start a new cooldown period
                        self.circuit_breaker.lock().await.reset_cooldown();
                        
                        // We can't easily track persistent state across loop iterations without changing the struct
                        // But we CAN check if this is happening repeatedly by looking at the circuit breaker stats or just failing hard now.
                        // For simplicity in this patch: If we are in this half-open state and fail, it's serious. 
                        // But let's look at how to implement the counter properly.
                        // The loop is in `maintain`, which is called every 15s. We need state persistence.
                        // Since `maintain` is called from a loop in `new`, and `maintain` returns `()`, we don't have local state persistence across calls easily.
                        // ACTUALLY: The maintain function is called repeatedly. 
                        
                        // Let's force exit if we are in a really bad state. 
                        // If we are half-open, it means we've already failed MAX_CONSECUTIVE_FAILURES (10) times + waited 120s.
                        // If we fail again now, that's essentially 11 failures + wait.
                        // If we want to be aggressive: exit now. 
                        // If we want to try a few times: we need a counter in PdfService or CircuitBreakerState.
                        
                        // Given the user wants "restart if bad stuff happens", exiting on a failed recovery (which is already a rare, bad state) is safe.
                        // It ensures we don't loop forever in "fail -> wait 120s -> fail -> wait 120s"
                         
                        error!("Fatal: Failed to recover browser pool from half-open state. Initiating self-healing restart.");
                        std::process::exit(1);
                    }
                }
            }
        }

        let mut instances = self.instances.lock().await;

        // Remove unhealthy, old, and idle instances - collect indices first
        let initial_count = instances.len();
        let mut to_remove = Vec::new();
        for (idx, instance) in instances.iter().enumerate() {
            let is_unhealthy = !instance.is_healthy().await;
            let should_retire = instance.should_retire().await;
            // Only remove idle instances if we're above minimum pool size
            let is_idle = instances.len() > self.min_instances && instance.is_idle_too_long().await;

            if is_unhealthy || should_retire || is_idle {
                if is_unhealthy {
                    debug!("Marking instance for removal: unhealthy");
                } else if should_retire {
                    debug!("Marking instance for removal: should retire (age/uses)");
                } else if is_idle {
                    debug!("Marking instance for removal: idle too long");
                }
                to_remove.push(idx);
            }
        }
        // Remove in reverse order to preserve indices
        for idx in to_remove.into_iter().rev() {
            instances.remove(idx);
        }

        let removed = initial_count - instances.len();
        if removed > 0 {
            info!(count = removed, "Maintenance: removed unhealthy/old/idle browser instances");
        }

        // Ensure minimum instances with circuit breaker
        let mut maintenance_failures = 0;
        const MAX_MAINTENANCE_ATTEMPTS: usize = 3;
        drop(instances); // Release lock before creating instances

        loop {
            // Check current pool size
            let current_len = self.instances.lock().await.len();

            if current_len >= self.min_instances || maintenance_failures >= MAX_MAINTENANCE_ATTEMPTS {
                break;
            }

            match BrowserInstance::new(&self.chrome_path).await {
                Ok(new_instance) => {
                    let mut instances = self.instances.lock().await;
                    instances.push_back(Arc::new(new_instance));
                    info!("Maintenance: added new browser instance");
                    drop(instances);
                    // Reset circuit breaker on success
                    self.circuit_breaker.lock().await.reset();
                    maintenance_failures = 0; // Reset local counter too
                }
                Err(e) => {
                    error!(error = %e, "Maintenance: failed to create browser instance");
                    maintenance_failures += 1;
                    // Record failure in circuit breaker
                    self.circuit_breaker.lock().await.record_failure(MAX_CONSECUTIVE_FAILURES);

                    if maintenance_failures >= MAX_MAINTENANCE_ATTEMPTS {
                        let current_pool_size = self.instances.lock().await.len();
                        error!(
                            failures = maintenance_failures,
                            pool_size = current_pool_size,
                            "Maintenance: stopping after consecutive failures"
                        );
                    }
                    break;
                }
            }
        }
    }
}

pub struct PdfService {
    pool: Arc<BrowserPool>,
    /// Maintenance task handle - wrapped in Mutex to allow taking ownership during shutdown
    maintenance_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_signal: Arc<tokio::sync::Notify>,
    /// Maximum PDF size in bytes (0 = unlimited)
    max_pdf_size_bytes: usize,
}

struct PageGuard {
    page: Page,
}

impl PageGuard {
    fn new(page: Page) -> Self {
        Self { page }
    }

    async fn generate_pdf(&self, pdf_params: PrintToPdfParams) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.page.pdf(pdf_params).await.map_err(Into::into)
    }
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        let page = self.page.clone();
        // Try to spawn cleanup task, silently skip if runtime unavailable
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(e) = page.close().await {
                    warn!(error = %e, "Failed to close page");
                }
            });
        }
        // If runtime is gone, page will be cleaned up when browser closes
    }
}

/// Parse margin string to inches (supports px, cm, mm, in, or plain numbers)
fn parse_margin(value: Option<String>) -> f64 {
    match value {
        None => 0.0,
        Some(s) if s.is_empty() => 0.0,
        Some(s) => {
            let s = s.trim().to_lowercase();
            if s.ends_with("in") {
                s.trim_end_matches("in").trim().parse().unwrap_or(0.0)
            } else if s.ends_with("cm") {
                let cm: f64 = s.trim_end_matches("cm").trim().parse().unwrap_or(0.0);
                cm / 2.54 // Convert cm to inches
            } else if s.ends_with("mm") {
                let mm: f64 = s.trim_end_matches("mm").trim().parse().unwrap_or(0.0);
                mm / 25.4 // Convert mm to inches
            } else if s.ends_with("px") {
                let px: f64 = s.trim_end_matches("px").trim().parse().unwrap_or(0.0);
                px / 96.0 // Convert pixels to inches (96 DPI)
            } else {
                // Plain number - treat as pixels if > 10, otherwise inches
                let num: f64 = s.parse().unwrap_or(0.0);
                if num > 10.0 { num / 96.0 } else { num }
            }
        }
    }
}

/// Build PrintToPdfParams from optional PdfOptions
fn build_pdf_params(options: Option<&PdfOptions>) -> PrintToPdfParams {
    let opts = options.cloned().unwrap_or_default();

    // Paper dimensions by format (in inches)
    let (width, height) = match opts.format.to_uppercase().as_str() {
        "LETTER" => (8.5, 11.0),
        "LEGAL" => (8.5, 14.0),
        "A3" => (11.69, 16.54),
        "A5" => (5.83, 8.27),
        _ => (8.27, 11.69), // A4 default
    };

    let margin = opts.margin.unwrap_or_default();

    PrintToPdfParams::builder()
        .paper_width(width)
        .paper_height(height)
        .print_background(opts.print_background)
        .margin_top(parse_margin(margin.top))
        .margin_right(parse_margin(margin.right))
        .margin_bottom(parse_margin(margin.bottom))
        .margin_left(parse_margin(margin.left))
        .build()
}

impl PdfService {
    /// Common Chrome installation paths to try as fallbacks
    const CHROME_FALLBACK_PATHS: &'static [&'static str] = &[
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
                },
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
    fn find_chrome_executable(provided_path: Option<String>) -> Result<String, ChromePathError> {
        let mut tried_paths = Vec::new();

        // First, try the provided path if available
        if let Some(path) = provided_path.clone() {
            debug!(path = %path, "Checking provided Chrome path");
            tried_paths.push(path.clone());
            match Self::validate_chrome_path(&path) {
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
        if let Ok(output) = Command::new("which")
            .arg("google-chrome-stable")
            .output()
        {
            if output.status.success() {
                let chrome_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !chrome_path.is_empty() {
                    debug!(path = %chrome_path, "Found Chrome via 'which'");
                    tried_paths.push(chrome_path.clone());
                    if let Ok(validated_path) = Self::validate_chrome_path(&chrome_path) {
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
                        if let Ok(validated_path) = Self::validate_chrome_path(&browser_path) {
                            info!(browser = %browser, path = %validated_path, "Using browser found via 'which'");
                            return Ok(validated_path);
                        }
                    }
                }
            }
        }

        // Try fallback paths
        debug!("Trying fallback Chrome paths");
        for &fallback_path in Self::CHROME_FALLBACK_PATHS {
            if !tried_paths.contains(&fallback_path.to_string()) {
                tried_paths.push(fallback_path.to_string());
                match Self::validate_chrome_path(fallback_path) {
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

    pub async fn new(
        chrome_path: Option<String>,
        min_instances: usize,
        max_instances: usize,
        max_pdf_size_mb: usize,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Find and validate Chrome executable
        let validated_chrome_path = Self::find_chrome_executable(chrome_path)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        // Create browser pool with configured instances
        let pool = Arc::new(BrowserPool::new(validated_chrome_path, min_instances, max_instances).await?);

        // Create shutdown signal
        let shutdown_signal = Arc::new(tokio::sync::Notify::new());

        // Start maintenance task with shutdown awareness (15 second interval for aggressive recycling)
        let pool_clone = Arc::clone(&pool);
        let shutdown_clone = Arc::clone(&shutdown_signal);
        let maintenance_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        pool_clone.maintain().await;
                    }
                    _ = shutdown_clone.notified() => {
                        info!("Maintenance task received shutdown signal");
                        break;
                    }
                }
            }
        });

        // Convert MB to bytes (0 means unlimited)
        let max_pdf_size_bytes = max_pdf_size_mb * 1024 * 1024;

        Ok(Self {
            pool,
            maintenance_handle: Mutex::new(Some(maintenance_handle)),
            shutdown_signal,
            max_pdf_size_bytes,
        })
    }

    /// Gracefully shutdown the PDF service and cleanup all resources
    pub async fn shutdown(&self) {
        info!("Shutting down PDF service...");

        // Signal maintenance task to stop
        self.shutdown_signal.notify_one();

        // Take ownership of the maintenance handle and await it
        if let Some(handle) = self.maintenance_handle.lock().await.take() {
            debug!("Waiting for maintenance task to complete...");
            match timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(())) => info!("Maintenance task completed successfully"),
                Ok(Err(e)) => error!(error = %e, "Maintenance task panicked"),
                Err(_) => {
                    warn!("Maintenance task did not complete within timeout, aborting");
                    // Handle was already consumed by timeout, so we can't abort it
                }
            }
        }

        // Clear all browser instances from the pool
        let mut instances = self.pool.instances.lock().await;
        let count = instances.len();
        instances.clear();
        drop(instances);

        info!(count = count, "Cleared browser instances - cleanup via Drop trait");

        // Give some time for Drop cleanup to complete
        sleep(Duration::from_secs(3)).await;

        info!("PDF service shutdown complete");
    }

    pub async fn generate_pdf(&self, url: &str, options: Option<&PdfOptions>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        const PDF_TIMEOUT: Duration = Duration::from_secs(30);
        const MAX_RETRIES: usize = 3;

        let mut last_error = None;
        let pdf_params = build_pdf_params(options);

        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                debug!(attempt = attempt + 1, "Retrying PDF generation");
                // Exponential backoff
                sleep(Duration::from_millis(100 * (2_u64.pow(attempt as u32)))).await;
            }

            // Get a healthy browser instance from the pool
            let instance = match self.pool.get_healthy_instance().await {
                Ok(inst) => inst,
                Err(e) => {
                    error!(attempt = attempt + 1, error = %e, "Failed to get browser instance");
                    last_error = Some(e.to_string());
                    continue;
                }
            };

            let pdf_params_clone = pdf_params.clone();
            let generate_with_timeout = async {
                // Create a new page with retry logic
                let page = match instance.browser.new_page(url).await {
                    Ok(p) => p,
                    Err(e) => {
                        // Mark instance as unhealthy if page creation fails
                        *instance.is_healthy.write().await = false;
                        return Err(PdfError::PageCreationFailed(e.to_string()));
                    }
                };

                let page_guard = PageGuard::new(page);

                // Wait for the page to fully load with error handling
                if let Err(e) = page_guard.page.wait_for_navigation().await {
                    return Err(PdfError::GenerationFailed(format!("Navigation failed: {}", e)));
                }

                // Additional wait for dynamic content
                sleep(Duration::from_millis(500)).await;

                // Generate PDF using the guard which ensures cleanup
                page_guard.generate_pdf(pdf_params_clone).await
                    .map_err(|e| PdfError::GenerationFailed(e.to_string()))
            };

            match timeout(PDF_TIMEOUT, generate_with_timeout).await {
                Ok(Ok(pdf_data)) => {
                    // Check PDF size against limit
                    if self.max_pdf_size_bytes > 0 && pdf_data.len() > self.max_pdf_size_bytes {
                        let size_mb = pdf_data.len() as f64 / (1024.0 * 1024.0);
                        let limit_mb = self.max_pdf_size_bytes / (1024 * 1024);
                        error!(
                            size_mb = size_mb,
                            limit_mb = limit_mb,
                            "PDF size exceeds configured limit"
                        );
                        return Err(format!(
                            "PDF size {:.2}MB exceeds limit of {}MB",
                            size_mb, limit_mb
                        ).into());
                    }
                    info!(attempt = attempt + 1, size_bytes = pdf_data.len(), "PDF generated successfully");
                    return Ok(pdf_data);
                }
                Ok(Err(e)) => {
                    last_error = Some(e.to_string());
                    error!(error = %e, "PDF generation failed");

                    // If it's a browser connection issue, mark instance as unhealthy
                    if matches!(e, PdfError::BrowserConnectionLost | PdfError::PageCreationFailed(_)) {
                        *instance.is_healthy.write().await = false;
                    }
                }
                Err(_) => {
                    // Timeout - mark browser as unhealthy since it's likely stuck
                    warn!("PDF generation timed out - marking browser instance as unhealthy");
                    *instance.is_healthy.write().await = false;
                    last_error = Some("PDF generation timed out".to_string());
                }
            }
        }

        let error_msg = format!("PDF generation failed after {} attempts. Last error: {}",
                   MAX_RETRIES,
                   last_error.unwrap_or_else(|| "Unknown error".to_string()));
        error!(retries = MAX_RETRIES, error = %error_msg, "PDF generation failed after all attempts");
        Err(error_msg.into())
    }

    /// Generate PDF from raw HTML content
    pub async fn generate_pdf_from_html(&self, html: &str, options: Option<&PdfOptions>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        const PDF_TIMEOUT: Duration = Duration::from_secs(30);
        const MAX_RETRIES: usize = 3;

        let mut last_error = None;
        let pdf_params = build_pdf_params(options);

        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                debug!(attempt = attempt + 1, "Retrying HTML PDF generation");
                sleep(Duration::from_millis(100 * (2_u64.pow(attempt as u32)))).await;
            }

            let instance = match self.pool.get_healthy_instance().await {
                Ok(inst) => inst,
                Err(e) => {
                    error!(attempt = attempt + 1, error = %e, "Failed to get browser instance");
                    last_error = Some(e.to_string());
                    continue;
                }
            };

            let html_owned = html.to_string();
            let pdf_params_clone = pdf_params.clone();
            let generate_with_timeout = async {
                // Create a blank page
                let page = match instance.browser.new_page("about:blank").await {
                    Ok(p) => p,
                    Err(e) => {
                        *instance.is_healthy.write().await = false;
                        return Err(PdfError::PageCreationFailed(e.to_string()));
                    }
                };

                let page_guard = PageGuard::new(page);

                // Set HTML content
                if let Err(e) = page_guard.page.set_content(&html_owned).await {
                    return Err(PdfError::GenerationFailed(format!("Failed to set HTML content: {}", e)));
                }

                // Wait for fonts to load
                if let Err(e) = page_guard.page.evaluate("document.fonts.ready").await {
                    warn!(error = %e, "Font loading check failed, continuing anyway");
                }

                // Wait for all images to load
                let wait_images_script = r#"
                    Promise.all(
                        Array.from(document.images)
                            .filter(img => !img.complete)
                            .map(img => new Promise(resolve => {
                                img.onload = img.onerror = resolve;
                            }))
                    )
                "#;
                if let Err(e) = page_guard.page.evaluate(wait_images_script).await {
                    warn!(error = %e, "Image loading check failed, continuing anyway");
                }

                // Small delay for any remaining async resources
                sleep(Duration::from_millis(100)).await;

                // Generate PDF
                page_guard.generate_pdf(pdf_params_clone).await
                    .map_err(|e| PdfError::GenerationFailed(e.to_string()))
            };

            match timeout(PDF_TIMEOUT, generate_with_timeout).await {
                Ok(Ok(pdf_data)) => {
                    // Check PDF size against limit
                    if self.max_pdf_size_bytes > 0 && pdf_data.len() > self.max_pdf_size_bytes {
                        let size_mb = pdf_data.len() as f64 / (1024.0 * 1024.0);
                        let limit_mb = self.max_pdf_size_bytes / (1024 * 1024);
                        error!(size_mb = size_mb, limit_mb = limit_mb, "PDF size exceeds configured limit");
                        return Err(format!("PDF size {:.2}MB exceeds limit of {}MB", size_mb, limit_mb).into());
                    }
                    info!(attempt = attempt + 1, size_bytes = pdf_data.len(), "HTML PDF generated successfully");
                    return Ok(pdf_data);
                }
                Ok(Err(e)) => {
                    last_error = Some(e.to_string());
                    error!(error = %e, "HTML PDF generation failed");
                    if matches!(e, PdfError::BrowserConnectionLost | PdfError::PageCreationFailed(_)) {
                        *instance.is_healthy.write().await = false;
                    }
                }
                Err(_) => {
                    // Timeout - mark browser as unhealthy since it's likely stuck
                    warn!("HTML PDF generation timed out - marking browser instance as unhealthy");
                    *instance.is_healthy.write().await = false;
                    last_error = Some("HTML PDF generation timed out".to_string());
                }
            }
        }

        let error_msg = format!("HTML PDF generation failed after {} attempts. Last error: {}",
                   MAX_RETRIES,
                   last_error.unwrap_or_else(|| "Unknown error".to_string()));
        error!(retries = MAX_RETRIES, error = %error_msg, "HTML PDF generation failed after all attempts");
        Err(error_msg.into())
    }
}

impl Drop for PdfService {
    fn drop(&mut self) {
        // Signal shutdown to maintenance task
        self.shutdown_signal.notify_one();

        // Try to abort the maintenance task if it hasn't been awaited via shutdown()
        // We can use get_mut() since we have &mut self, avoiding async lock
        if let Some(handle) = self.maintenance_handle.get_mut().take() {
            handle.abort();
            debug!("Aborted maintenance task during PdfService drop");
        }
    }
}