use serde::{Deserialize, Serialize, de};
use url::Url;

#[derive(Deserialize)]
pub struct PdfRequest {
    pub name: String,
    #[serde(deserialize_with = "validate_url")]
    pub url: String,
}

impl PdfRequest {
    pub fn validate(&self) -> Result<(), String> {
        // Additional validation if needed
        if self.name.is_empty() {
            return Err("Name cannot be empty".to_string());
        }
        Ok(())
    }
}

fn validate_url<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: de::Deserializer<'de>,
{
    let url_str = String::deserialize(deserializer)?;
    
    // Parse URL to validate format
    let url = Url::parse(&url_str)
        .map_err(|_| de::Error::custom("Invalid URL format"))?;
    
    // Validate scheme - only allow http and https
    match url.scheme() {
        "http" | "https" => Ok(url_str),
        scheme => Err(de::Error::custom(format!(
            "Invalid URL scheme '{}'. Only 'http' and 'https' are allowed",
            scheme
        ))),
    }
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}