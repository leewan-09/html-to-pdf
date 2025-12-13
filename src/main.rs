mod config;
mod handlers;
mod models;
mod pdf_service;

use config::Config;
use handlers::{generate_pdf, generate_pdf_from_html};
use pdf_service::PdfService;

use axum::{
    routing::{post, get},
    Router,
    response::Json,
    http::{Method, HeaderValue},
};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::cors::{CorsLayer, Any};
use tokio::signal;
use tracing::{info, warn, error};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing subscriber with env filter (respects RUST_LOG)
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

    info!("Initializing PDF service...");
    info!(chrome_path = ?config.chrome_path, "Chrome path from environment");
    info!(min = config.browser_pool_min, max = config.browser_pool_max, max_pdf_size_mb = config.max_pdf_size_mb, "Browser pool configuration");
    let pdf_service = Arc::new(
        PdfService::new(
            config.chrome_path.clone(),
            config.browser_pool_min,
            config.browser_pool_max,
            config.max_pdf_size_mb,
        ).await?
    );
    info!("PDF service initialized successfully!");

    // Clone pdf_service for shutdown handler before moving it
    let pdf_service_for_shutdown = Arc::clone(&pdf_service);

    // Configure CORS based on allowed_origins
    let cors = if config.allowed_origins.iter().any(|o| o == "*") {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(Any)
    } else {
        let origins: Vec<HeaderValue> = config.allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers(Any)
    };

    let app = Router::new()
        .route("/", post(generate_pdf))
        .route("/html", post(generate_pdf_from_html))
        .layer(ServiceBuilder::new().layer(cors))
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
    info!(address = %addr, "Server running");

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
        match signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                error!(error = %e, "Failed to install Ctrl+C handler");
                // Wait forever if we can't install the handler
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => { sig.recv().await; }
            Err(e) => {
                error!(error = %e, "Failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal");
        },
        _ = terminate => {
            info!("Received SIGTERM signal");
        },
    }

    info!("Starting graceful shutdown...");

    // Cleanup PDF service resources
    pdf_service.shutdown().await;

    info!("Graceful shutdown complete");
}
