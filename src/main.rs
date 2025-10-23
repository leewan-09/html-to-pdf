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
    response::Json,
};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
    
    println!("Initializing PDF service...");
    println!("Chrome path from environment: {:?}", config.chrome_path);
    println!("Browser pool configuration: min={}, max={}", config.browser_pool_min, config.browser_pool_max);
    let pdf_service = Arc::new(
        PdfService::new(
            config.chrome_path.clone(),
            config.browser_pool_min,
            config.browser_pool_max
        ).await?
    );
    println!("PDF service initialized successfully!");

    // Clone pdf_service for shutdown handler before moving it
    let pdf_service_for_shutdown = Arc::clone(&pdf_service);

    let app = Router::new()
        .route("/", post(generate_pdf))
        .layer(
            ServiceBuilder::new()
                .layer(CorsLayer::permissive())
        )
        .with_state(pdf_service);

    // Add a health check route with JSON response
    let app = app.route("/health", get(|| async {
        Json(json!({
            "status": "healthy",
            "service": "html-to-pdf-rust",
            "timestamp": chrono::Utc::now().to_rfc3339()
        }))
    }));


    let addr = format!("0.0.0.0:{}", config.port);
    println!("Server running on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Run server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(pdf_service_for_shutdown))
        .await?;

    Ok(())
}

/// Handle shutdown signals (SIGTERM, SIGINT) and cleanup resources
async fn shutdown_signal(pdf_service: Arc<PdfService>) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            println!("\nReceived Ctrl+C signal");
        },
        _ = terminate => {
            println!("Received SIGTERM signal");
        },
    }

    println!("Starting graceful shutdown...");

    // Cleanup PDF service resources
    pdf_service.shutdown().await;

    println!("Graceful shutdown complete");
}
