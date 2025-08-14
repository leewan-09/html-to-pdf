use crate::models::{PdfRequest, ErrorResponse};
use crate::pdf_service::PdfService;
use axum::{extract::State, http::StatusCode, response::Response, Json};
use axum::body::Body;
use axum::response::IntoResponse;
use std::sync::Arc;
use regex::Regex;
use thiserror::Error;
use once_cell::sync::Lazy;

static FILENAME_SANITIZER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[^a-zA-Z0-9._-]").expect("Invalid regex pattern for filename sanitization")
});

#[derive(Error, Debug)]
pub enum AppError {
    #[error("PDF generation failed: {0}")]
    PdfGeneration(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Response building failed")]
    ResponseBuilding,
    #[error("Validation error: {0}")]
    Validation(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AppError::PdfGeneration(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Serialization(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization error".to_string()),
            AppError::ResponseBuilding => (StatusCode::INTERNAL_SERVER_ERROR, "Response building error".to_string()),
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
        };
        
        let error_response = ErrorResponse { error: error_message };
        match serde_json::to_string(&error_response) {
            Ok(body) => {
                // Try to build the JSON response
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Body::from(body))
                    .unwrap_or_else(|_| create_fallback_response())
            },
            Err(_) => create_fallback_response(),
        }
    }
}

fn create_fallback_response() -> Response {
    // This is a safe fallback that cannot fail
    let mut response = Response::new(Body::from("Internal server error"));
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    // Use a safe method to insert headers without unwrap
    if let Ok(header_value) = "text/plain".parse() {
        response.headers_mut().insert("Content-Type", header_value);
    }
    response
}

pub fn sanitize_filename(name: &str) -> String {
    let sanitized = FILENAME_SANITIZER.replace_all(name, "_").to_string();
    
    // Limit length and ensure it's not empty
    let mut result = sanitized.chars().take(50).collect::<String>();
    if result.is_empty() || result.chars().all(|c| c == '_') {
        result = "document".to_string();
    }
    
    result
}

pub async fn generate_pdf(
    State(pdf_service): State<Arc<PdfService>>,
    Json(request): Json<PdfRequest>,
) -> Result<Response, AppError> {
    println!("Generating PDF for {} from {}", request.name, request.url);
    
    // Validate request
    request.validate().map_err(AppError::Validation)?;
    
    // Generate PDF
    let pdf_data = pdf_service
        .generate_pdf(&request.url)
        .await
        .map_err(|e| AppError::PdfGeneration(e.to_string()))?;
    
    // Sanitize filename to prevent injection
    let safe_filename = sanitize_filename(&request.name);
    let filename = format!("{}.pdf", safe_filename);
    
    // Build response
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/pdf")
        .header("Content-Disposition", format!("inline; filename=\"{}\"", filename))
        .body(Body::from(pdf_data))
        .map_err(|_| AppError::ResponseBuilding)
}