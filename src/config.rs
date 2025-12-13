use std::env;
use thiserror::Error;
use tracing::warn;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid port number: {0}")]
    InvalidPort(String),
}

#[derive(Clone)]
pub struct Config {
    pub port: u16,
    pub chrome_path: Option<String>,
    pub rust_log: String,
    pub allowed_origins: Vec<String>,
    pub max_pdf_size_mb: usize,
    pub request_timeout_seconds: u64,
    pub browser_pool_min: usize,
    pub browser_pool_max: usize,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        dotenv::dotenv().ok();
        
        let port_str = env::var("PORT").unwrap_or_else(|_| "5000".to_string());
        let port = port_str
            .parse::<u16>()
            .map_err(|_| ConfigError::InvalidPort(port_str))?;
        
        // Validate port range
        if port < 1024 && port != 0 {
            return Err(ConfigError::InvalidPort(format!(
                "Port {} is in reserved range. Use port >= 1024",
                port
            )));
        }
            
        let chrome_path = env::var("CHROME_PATH").ok();
        let rust_log = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
        
        let allowed_origins = env::var("ALLOWED_ORIGINS")
            .unwrap_or_else(|_| "*".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        
        let max_pdf_size_mb = match env::var("MAX_PDF_SIZE_MB") {
            Ok(val) => val.parse().unwrap_or_else(|_| {
                warn!(value = %val, default = 10, "Invalid MAX_PDF_SIZE_MB, using default");
                10
            }),
            Err(_) => 10,
        };

        let request_timeout_seconds = match env::var("REQUEST_TIMEOUT_SECONDS") {
            Ok(val) => val.parse().unwrap_or_else(|_| {
                warn!(value = %val, default = 30, "Invalid REQUEST_TIMEOUT_SECONDS, using default");
                30
            }),
            Err(_) => 30,
        };

        let browser_pool_min = match env::var("BROWSER_POOL_MIN") {
            Ok(val) => val.parse().unwrap_or_else(|_| {
                warn!(value = %val, default = 2, "Invalid BROWSER_POOL_MIN, using default");
                2
            }).max(1),
            Err(_) => 2,
        };

        let browser_pool_max = match env::var("BROWSER_POOL_MAX") {
            Ok(val) => val.parse().unwrap_or_else(|_| {
                warn!(value = %val, default = 5, "Invalid BROWSER_POOL_MAX, using default");
                5
            }).max(browser_pool_min),
            Err(_) => 5.max(browser_pool_min),
        };

        Ok(Self {
            port,
            chrome_path,
            rust_log,
            allowed_origins,
            max_pdf_size_mb,
            request_timeout_seconds,
            browser_pool_min,
            browser_pool_max,
        })
    }
}