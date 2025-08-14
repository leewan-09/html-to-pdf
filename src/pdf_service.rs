use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use chromiumoxide::page::Page;
use futures_util::stream::StreamExt;
use std::sync::Arc;
use tokio::time::timeout;
use std::time::Duration;
use std::path::Path;
use std::process::Command;
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

pub struct PdfService {
    browser: Arc<Browser>,
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
        
        // Check if the path exists
        if !path_buf.exists() {
            return Err(ChromePathError::NotFound {
                path: path.to_string(),
            });
        }

        // Check if it's executable by trying to run --version
        match Command::new(path).arg("--version").output() {
            Ok(output) => {
                if output.status.success() {
                    println!("Chrome validation successful: {}", String::from_utf8_lossy(&output.stdout).trim());
                    Ok(path.to_string())
                } else {
                    Err(ChromePathError::NotExecutable {
                        path: path.to_string(),
                    })
                }
            }
            Err(e) => Err(ChromePathError::ValidationFailed {
                path: path.to_string(),
                error: e.to_string(),
            }),
        }
    }

    /// Finds a valid Chrome installation by checking provided path and fallbacks
    fn find_chrome_executable(provided_path: Option<String>) -> Result<String, ChromePathError> {
        let mut tried_paths = Vec::new();

        // First, try the provided path if available
        if let Some(path) = provided_path {
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

        // Try fallback paths
        println!("Trying fallback Chrome paths...");
        for &fallback_path in Self::CHROME_FALLBACK_PATHS {
            tried_paths.push(fallback_path.to_string());
            match Self::validate_chrome_path(fallback_path) {
                Ok(validated_path) => {
                    println!("Using fallback Chrome path: {}", validated_path);
                    return Ok(validated_path);
                }
                Err(_) => {
                    // Continue to next fallback, don't log every failure
                    continue;
                }
            }
        }

        eprintln!("No valid Chrome installation found. Tried paths: {:?}", tried_paths);
        Err(ChromePathError::NoValidInstallation { paths: tried_paths })
    }
    pub async fn new(chrome_path: Option<String>) -> Result<Self, Box<dyn std::error::Error>> {
        use std::time::{SystemTime, UNIX_EPOCH};
        
        // Find and validate Chrome executable
        let validated_chrome_path = Self::find_chrome_executable(chrome_path)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let user_data_dir = format!("/tmp/chrome-rust-pdf-{}", timestamp);
        
        let config = BrowserConfig::builder()
            .chrome_executable(&validated_chrome_path)
            .no_sandbox()
            .launch_timeout(std::time::Duration::from_secs(60))
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
                format!("--user-data-dir={}", user_data_dir),
                "--timezone=Indian/Maldives".to_string(),
            ]);

        let (browser, mut handler) = Browser::launch(config.build()?).await?;
        
        tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    eprintln!("Browser handler error: {}", e);
                }
            }
        });

        Ok(Self {
            browser: Arc::new(browser),
        })
    }

    pub async fn generate_pdf(&self, url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        const PDF_TIMEOUT: Duration = Duration::from_secs(30);
        
        let generate_with_timeout = async {
            // Create a new page directly with the target URL
            let page = self.browser.new_page(url).await?;
            let page_guard = PageGuard::new(page);
            
            // Wait for the page to fully load
            page_guard.page.wait_for_navigation().await?;
            
            let pdf_params = PrintToPdfParams::builder()
                .paper_width(8.27) // A4 width in inches
                .paper_height(11.69) // A4 height in inches
                .print_background(true)
                .build();

            // Generate PDF using the guard which ensures cleanup
            page_guard.generate_pdf(pdf_params).await
        };

        timeout(PDF_TIMEOUT, generate_with_timeout)
            .await
            .map_err(|_| -> Box<dyn std::error::Error> { "PDF generation timed out".into() })?

    }
}