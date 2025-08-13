use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use futures_util::stream::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct PdfService {
    browser: Arc<Mutex<Browser>>,
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
            browser: Arc::new(Mutex::new(browser)),
        })
    }

    pub async fn generate_pdf(&self, url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let browser = self.browser.lock().await;
        let page = browser.new_page(url).await?;
        
        page.goto(url).await?;
        page.wait_for_navigation().await?;
        
        let pdf_params = PrintToPdfParams::builder()
            .paper_width(8.27) // A4 width in inches
            .paper_height(11.69) // A4 height in inches
            .print_background(true)
            .build();

        let pdf_data = page.pdf(pdf_params).await?;
        
        page.close().await?;
        
        Ok(pdf_data)
    }
}