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
        
        Ok(Self { port, chrome_path })
    }
}