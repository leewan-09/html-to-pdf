use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use chromiumoxide::page::Page;
use futures_util::stream::StreamExt;
use std::sync::Arc;
use tokio::time::timeout;
use std::time::Duration;

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
    pub async fn new(chrome_path: Option<String>) -> Result<Self, Box<dyn std::error::Error>> {
        use std::time::{SystemTime, UNIX_EPOCH};
        
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let user_data_dir = format!("/tmp/chrome-rust-pdf-{}", timestamp);
        
        let mut config = BrowserConfig::builder()
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

        if let Some(path) = chrome_path {
            config = config.chrome_executable(&path);
        }

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