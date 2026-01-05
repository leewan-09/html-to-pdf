use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use chromiumoxide::page::Page;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Duration};
use tracing::{debug, error, info, warn};

use crate::browser::find_chrome_executable;
use crate::browser::pool::BrowserPool;
use crate::error::PdfError;
use crate::models::PdfOptions;

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

    async fn generate_pdf(
        &self,
        pdf_params: PrintToPdfParams,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
                if num > 10.0 {
                    num / 96.0
                } else {
                    num
                }
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
    pub async fn new(
        chrome_path: Option<String>,
        min_instances: usize,
        max_instances: usize,
        max_pdf_size_mb: usize,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Find and validate Chrome executable
        let validated_chrome_path = find_chrome_executable(chrome_path)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        // Create browser pool with configured instances
        let pool: Arc<BrowserPool> =
            Arc::new(BrowserPool::new(validated_chrome_path, min_instances, max_instances).await?);

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
        let count = self.pool.clear().await;

        info!(
            count = count,
            "Cleared browser instances - cleanup via Drop trait"
        );

        // Give some time for Drop cleanup to complete
        sleep(Duration::from_secs(3)).await;

        info!("PDF service shutdown complete");
    }

    pub async fn generate_pdf(
        &self,
        url: &str,
        options: Option<&PdfOptions>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
                // instance.browser is accessible as pub field
                let page = match instance.browser.new_page(url).await {
                    Ok(p) => p,
                    Err(e) => {
                        // Mark instance as unhealthy if page creation fails
                        instance.mark_unhealthy().await;
                        return Err(PdfError::PageCreationFailed(e.to_string()));
                    }
                };

                let page_guard = PageGuard::new(page);

                // Wait for the page to fully load with error handling
                if let Err(e) = page_guard.page.wait_for_navigation().await {
                    return Err(PdfError::GenerationFailed(format!(
                        "Navigation failed: {}",
                        e
                    )));
                }

                // Additional wait for dynamic content
                sleep(Duration::from_millis(500)).await;

                // Generate PDF using the guard which ensures cleanup
                page_guard
                    .generate_pdf(pdf_params_clone)
                    .await
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
                        )
                        .into());
                    }
                    info!(
                        attempt = attempt + 1,
                        size_bytes = pdf_data.len(),
                        "PDF generated successfully"
                    );
                    return Ok(pdf_data);
                }
                Ok(Err(e)) => {
                    last_error = Some(e.to_string());
                    error!(error = %e, "PDF generation failed");

                    // If it's a browser connection issue, mark instance as unhealthy
                    if matches!(
                        e,
                        PdfError::BrowserConnectionLost | PdfError::PageCreationFailed(_)
                    ) {
                        instance.mark_unhealthy().await;
                    }
                }
                Err(_) => {
                    // Timeout - mark browser as unhealthy since it's likely stuck
                    warn!("PDF generation timed out - marking browser instance as unhealthy");
                    instance.mark_unhealthy().await;
                    last_error = Some("PDF generation timed out".to_string());
                }
            }
        }

        let error_msg = format!(
            "PDF generation failed after {} attempts. Last error: {}",
            MAX_RETRIES,
            last_error.unwrap_or_else(|| "Unknown error".to_string())
        );
        error!(retries = MAX_RETRIES, error = %error_msg, "PDF generation failed after all attempts");
        Err(error_msg.into())
    }

    /// Generate PDF from raw HTML content
    pub async fn generate_pdf_from_html(
        &self,
        html: &str,
        options: Option<&PdfOptions>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
                        instance.mark_unhealthy().await;
                        return Err(PdfError::PageCreationFailed(e.to_string()));
                    }
                };

                let page_guard = PageGuard::new(page);

                // Set HTML content
                if let Err(e) = page_guard.page.set_content(&html_owned).await {
                    return Err(PdfError::GenerationFailed(format!(
                        "Failed to set HTML content: {}",
                        e
                    )));
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
                page_guard
                    .generate_pdf(pdf_params_clone)
                    .await
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
                        )
                        .into());
                    }
                    info!(
                        attempt = attempt + 1,
                        size_bytes = pdf_data.len(),
                        "HTML PDF generated successfully"
                    );
                    return Ok(pdf_data);
                }
                Ok(Err(e)) => {
                    last_error = Some(e.to_string());
                    error!(error = %e, "HTML PDF generation failed");
                    if matches!(
                        e,
                        PdfError::BrowserConnectionLost | PdfError::PageCreationFailed(_)
                    ) {
                        instance.mark_unhealthy().await;
                    }
                }
                Err(_) => {
                    // Timeout - mark browser as unhealthy since it's likely stuck
                    warn!("HTML PDF generation timed out - marking browser instance as unhealthy");
                    instance.mark_unhealthy().await;
                    last_error = Some("HTML PDF generation timed out".to_string());
                }
            }
        }

        let error_msg = format!(
            "HTML PDF generation failed after {} attempts. Last error: {}",
            MAX_RETRIES,
            last_error.unwrap_or_else(|| "Unknown error".to_string())
        );
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
