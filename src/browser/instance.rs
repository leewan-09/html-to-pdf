use chromiumoxide::{Browser, BrowserConfig};
use futures_util::stream::StreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tracing::{debug, warn};

/// Browser instance with health tracking
pub struct BrowserInstance {
    pub browser: Arc<Browser>,
    created_at: Instant,
    last_used: Mutex<Instant>,
    usage_count: Mutex<usize>,
    pub is_healthy: Arc<RwLock<bool>>,
    user_data_dir: std::path::PathBuf,
}

impl BrowserInstance {
    pub async fn new(chrome_path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
                    if e.to_string().contains("closed connection")
                        || e.to_string().contains("WebSocket")
                    {
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

    async fn launch_browser(
        chrome_path: &str,
    ) -> Result<
        (Browser, chromiumoxide::Handler, std::path::PathBuf),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        use std::env;
        use std::time::{SystemTime, UNIX_EPOCH};

        // Generate unique ID for this browser instance
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let thread_id = std::thread::current().id();
        let random_num = format!("{:?}", thread_id)
            .chars()
            .filter(|c| c.is_numeric())
            .collect::<String>();
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

    pub async fn is_healthy(&self) -> bool {
        *self.is_healthy.read().await
    }

    pub async fn mark_unhealthy(&self) {
        *self.is_healthy.write().await = false;
    }

    pub async fn mark_used(&self) {
        *self.last_used.lock().await = Instant::now();
        *self.usage_count.lock().await += 1;
    }

    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    pub async fn should_retire(&self) -> bool {
        // Retire after 20 minutes or 30 uses (aggressive recycling for stability)
        self.age() > Duration::from_secs(1200) || *self.usage_count.lock().await > 30
    }

    pub async fn idle_time(&self) -> Duration {
        self.last_used.lock().await.elapsed()
    }

    pub async fn is_idle_too_long(&self) -> bool {
        // Recycle browsers idle for more than 5 minutes
        self.idle_time().await > Duration::from_secs(300)
    }

    /// Quick health ping - verify browser is responsive via CDP
    pub async fn health_ping(&self) -> bool {
        // Try to get browser version - fast CDP call to verify connection
        match tokio::time::timeout(Duration::from_secs(5), self.browser.version()).await {
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
                        eprintln!(
                            "Failed to clean up temp directory {:?}: {}",
                            user_data_dir, e
                        );
                    }
                }
            }
        }
    }
}
