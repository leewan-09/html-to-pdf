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
    #[error("PDF size {size_mb:.2}MB exceeds limit of {limit_mb}MB")]
    SizeExceeded { size_mb: f64, limit_mb: usize },
}
