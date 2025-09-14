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
}

/// Browser instance with health tracking
struct BrowserInstance {
    browser: Arc<Browser>,
    created_at: Instant,
    last_used: Mutex<Instant>,
    usage_count: Mutex<usize>,
    is_healthy: Arc<RwLock<bool>>,
}

impl BrowserInstance {
    async fn new(chrome_path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (browser, handler) = Self::launch_browser(chrome_path).await?;

        // Spawn handler with health monitoring
        let browser_arc = Arc::new(browser);
        let browser_health = Arc::new(RwLock::new(true));
        let health_clone = browser_health.clone();

        tokio::spawn(async move {
            let mut handler = handler;
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    eprintln!("Browser handler error detected: {}", e);
                    // Mark browser as unhealthy on connection errors
                    if e.to_string().contains("closed connection") ||
                       e.to_string().contains("WebSocket") {
                        *health_clone.write().await = false;
                        break;
                    }
                }
            }
            eprintln!("Browser handler loop ended - marking as unhealthy");
            *health_clone.write().await = false;
        });

        Ok(BrowserInstance {
            browser: browser_arc,
            created_at: Instant::now(),
            last_used: Mutex::new(Instant::now()),
            usage_count: Mutex::new(0),
            is_healthy: browser_health,
        })
    }

    async fn launch_browser(chrome_path: &str) -> Result<(Browser, chromiumoxide::Handler), Box<dyn std::error::Error + Send + Sync>> {
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
                "--disable-web-security".to_string(),
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
            ]);

        Browser::launch(config.build()?).await.map_err(Into::into)
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
        // Retire after 1 hour or 100 uses
        self.age() > Duration::from_secs(3600) ||
        *self.usage_count.lock().await > 100
    }
}

/// Connection pool for browser instances
struct BrowserPool {
    instances: Arc<Mutex<VecDeque<Arc<BrowserInstance>>>>,
    chrome_path: String,
    max_instances: usize,
    min_instances: usize,
}

impl BrowserPool {
    async fn new(chrome_path: String, min_instances: usize, max_instances: usize) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut instances = VecDeque::new();

        // Create initial instances
        for i in 0..min_instances {
            println!("Creating browser instance {} of {}", i + 1, min_instances);
            match BrowserInstance::new(&chrome_path).await {
                Ok(instance) => instances.push_back(Arc::new(instance)),
                Err(e) => eprintln!("Failed to create initial browser instance: {}", e),
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
        })
    }

    async fn get_healthy_instance(&self) -> Result<Arc<BrowserInstance>, PdfError> {
        const MAX_RETRIES: usize = 3;

        for attempt in 0..MAX_RETRIES {
            let mut instances = self.instances.lock().await;

            // Remove unhealthy instances
            instances.retain(|instance| {
                let is_healthy = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(instance.is_healthy())
                });
                if !is_healthy {
                    eprintln!("Removing unhealthy browser instance");
                }
                is_healthy
            });

            // Try to find a healthy instance that shouldn't retire
            for instance in instances.iter() {
                if instance.is_healthy().await && !instance.should_retire().await {
                    instance.mark_used().await;
                    return Ok(Arc::clone(instance));
                }
            }

            // Create new instance if needed and allowed
            if instances.len() < self.max_instances {
                println!("Creating new browser instance (attempt {})", attempt + 1);
                match BrowserInstance::new(&self.chrome_path).await {
                    Ok(new_instance) => {
                        let instance_arc = Arc::new(new_instance);
                        instance_arc.mark_used().await;
                        instances.push_back(Arc::clone(&instance_arc));
                        return Ok(instance_arc);
                    }
                    Err(e) => {
                        eprintln!("Failed to create new browser instance: {}", e);
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
        let mut instances = self.instances.lock().await;

        // Remove unhealthy and old instances
        let initial_count = instances.len();
        instances.retain(|instance| {
            let should_keep = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    instance.is_healthy().await && !instance.should_retire().await
                })
            });
            should_keep
        });

        let removed = initial_count - instances.len();
        if removed > 0 {
            println!("Maintenance: removed {} unhealthy/old browser instances", removed);
        }

        // Ensure minimum instances
        while instances.len() < self.min_instances {
            drop(instances); // Release lock for instance creation
            match BrowserInstance::new(&self.chrome_path).await {
                Ok(new_instance) => {
                    instances = self.instances.lock().await;
                    instances.push_back(Arc::new(new_instance));
                    println!("Maintenance: added new browser instance");
                }
                Err(e) => {
                    eprintln!("Maintenance: failed to create browser instance: {}", e);
                    break;
                }
            }
        }
    }
}

pub struct PdfService {
    pool: Arc<BrowserPool>,
    maintenance_handle: Option<tokio::task::JoinHandle<()>>,
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
        tokio::spawn(async move {
            if let Err(e) = page.close().await {
                eprintln!("Failed to close page: {}", e);
            }
        });
    }
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
                    println!("Resolved Chrome path: {} -> {}", path, p.display());
                    p
                },
                Err(e) => {
                    println!("Warning: Could not resolve symlink for {}: {}", path, e);
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

        println!("Attempting to validate Chrome at: {:?}", resolved_path);

        match cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    println!("Chrome validation successful: {}", version);
                    Ok(resolved_path.to_string_lossy().to_string())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("Chrome execution failed with status: {:?}", output.status);
                    eprintln!("stderr: {}", stderr);
                    Err(ChromePathError::NotExecutable {
                        path: path.to_string(),
                    })
                }
            }
            Err(e) => {
                eprintln!("Failed to execute Chrome binary: {}", e);
                // Try with just the original path as a fallback
                if path != resolved_path.to_string_lossy() {
                    println!("Retrying with original path: {}", path);
                    match Command::new(path).arg("--version").output() {
                        Ok(output) if output.status.success() => {
                            println!("Chrome validation successful with original path");
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
            println!("Checking provided Chrome path: {}", path);
            tried_paths.push(path.clone());
            match Self::validate_chrome_path(&path) {
                Ok(validated_path) => {
                    println!("Using provided Chrome path: {}", validated_path);
                    return Ok(validated_path);
                }
                Err(e) => {
                    eprintln!("Provided Chrome path failed validation: {}", e);
                }
            }
        }

        // Try to find Chrome using 'which' command as a fallback
        println!("Attempting to find Chrome using 'which' command...");
        if let Ok(output) = Command::new("which")
            .arg("google-chrome-stable")
            .output()
        {
            if output.status.success() {
                let chrome_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !chrome_path.is_empty() {
                    println!("Found Chrome via 'which': {}", chrome_path);
                    tried_paths.push(chrome_path.clone());
                    if let Ok(validated_path) = Self::validate_chrome_path(&chrome_path) {
                        println!("Using Chrome found via 'which': {}", validated_path);
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
                        println!("Found {} via 'which': {}", browser, browser_path);
                        tried_paths.push(browser_path.clone());
                        if let Ok(validated_path) = Self::validate_chrome_path(&browser_path) {
                            println!("Using {} found via 'which': {}", browser, validated_path);
                            return Ok(validated_path);
                        }
                    }
                }
            }
        }

        // Try fallback paths
        println!("Trying fallback Chrome paths...");
        for &fallback_path in Self::CHROME_FALLBACK_PATHS {
            if !tried_paths.contains(&fallback_path.to_string()) {
                tried_paths.push(fallback_path.to_string());
                match Self::validate_chrome_path(fallback_path) {
                    Ok(validated_path) => {
                        println!("Using fallback Chrome path: {}", validated_path);
                        return Ok(validated_path);
                    }
                    Err(e) => {
                        eprintln!("Fallback path {} failed: {}", fallback_path, e);
                    }
                }
            }
        }

        eprintln!("No valid Chrome installation found. Tried paths: {:?}", tried_paths);
        Err(ChromePathError::NoValidInstallation { paths: tried_paths })
    }

    pub async fn new(
        chrome_path: Option<String>,
        min_instances: usize,
        max_instances: usize,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Find and validate Chrome executable
        let validated_chrome_path = Self::find_chrome_executable(chrome_path)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        // Create browser pool with configured instances
        let pool = Arc::new(BrowserPool::new(validated_chrome_path, min_instances, max_instances).await?);

        // Start maintenance task
        let pool_clone = Arc::clone(&pool);
        let maintenance_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                pool_clone.maintain().await;
            }
        });

        Ok(Self {
            pool,
            maintenance_handle: Some(maintenance_handle),
        })
    }

    pub async fn generate_pdf(&self, url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        const PDF_TIMEOUT: Duration = Duration::from_secs(30);
        const MAX_RETRIES: usize = 3;

        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                println!("Retrying PDF generation (attempt {})", attempt + 1);
                // Exponential backoff
                sleep(Duration::from_millis(100 * (2_u64.pow(attempt as u32)))).await;
            }

            // Get a healthy browser instance from the pool
            let instance = match self.pool.get_healthy_instance().await {
                Ok(inst) => inst,
                Err(e) => {
                    last_error = Some(e.to_string());
                    continue;
                }
            };

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

                let pdf_params = PrintToPdfParams::builder()
                    .paper_width(8.27) // A4 width in inches
                    .paper_height(11.69) // A4 height in inches
                    .print_background(true)
                    .margin_top(0.4)
                    .margin_bottom(0.4)
                    .margin_left(0.4)
                    .margin_right(0.4)
                    .build();

                // Generate PDF using the guard which ensures cleanup
                page_guard.generate_pdf(pdf_params).await
                    .map_err(|e| PdfError::GenerationFailed(e.to_string()))
            };

            match timeout(PDF_TIMEOUT, generate_with_timeout).await {
                Ok(Ok(pdf_data)) => {
                    println!("PDF generated successfully on attempt {}", attempt + 1);
                    return Ok(pdf_data);
                }
                Ok(Err(e)) => {
                    last_error = Some(e.to_string());
                    eprintln!("PDF generation failed: {}", e);

                    // If it's a browser connection issue, mark instance as unhealthy
                    if matches!(e, PdfError::BrowserConnectionLost | PdfError::PageCreationFailed(_)) {
                        *instance.is_healthy.write().await = false;
                    }
                }
                Err(_) => {
                    last_error = Some("PDF generation timed out".to_string());
                    eprintln!("PDF generation timed out");
                }
            }
        }

        Err(format!("PDF generation failed after {} attempts. Last error: {}",
                   MAX_RETRIES,
                   last_error.unwrap_or_else(|| "Unknown error".to_string())).into())
    }
}

impl Drop for PdfService {
    fn drop(&mut self) {
        // Cancel the maintenance task when service is dropped
        if let Some(handle) = self.maintenance_handle.take() {
            handle.abort();
        }
    }
}