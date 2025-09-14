use std::env;
use thiserror::Error;

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
        
        let max_pdf_size_mb = env::var("MAX_PDF_SIZE_MB")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .unwrap_or(10);
        
        let request_timeout_seconds = env::var("REQUEST_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .unwrap_or(30);

        let browser_pool_min = env::var("BROWSER_POOL_MIN")
            .unwrap_or_else(|_| "2".to_string())
            .parse()
            .unwrap_or(2)
            .max(1); // At least 1 instance

        let browser_pool_max = env::var("BROWSER_POOL_MAX")
            .unwrap_or_else(|_| "5".to_string())
            .parse()
            .unwrap_or(5)
            .max(browser_pool_min); // Max must be >= min

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