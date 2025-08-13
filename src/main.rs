mod config;
mod handlers;
mod models;
mod pdf_service;

use config::Config;
use handlers::generate_pdf;
use pdf_service::PdfService;

use axum::{
    routing::{post, get},
    Router,
};
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    
    println!("Initializing PDF service...");
    let pdf_service = Arc::new(PdfService::new(config.chrome_path).await?);
    
    let app = Router::new()
        .route("/", post(generate_pdf))
        .layer(
            ServiceBuilder::new()
                .layer(CorsLayer::permissive())
        )
        .with_state(pdf_service);

    // Add a health check route
    let app = app.route("/health", get(|| async { "OK" }));


    let addr = format!("0.0.0.0:{}", config.port);
    println!("Server running on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}
