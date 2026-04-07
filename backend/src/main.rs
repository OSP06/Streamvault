mod db;
mod error;
mod handlers;
mod models;
mod streaming;

use axum::{Router, routing::{get, post}};
use axum::extract::DefaultBodyLimit;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use crate::db::Database;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub upload_dir: std::path::PathBuf,
    pub base_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "streamvault=debug,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let upload_dir = std::env::var("UPLOAD_DIR").unwrap_or_else(|_| "/data/uploads".to_string());
    let upload_path = std::path::PathBuf::from(&upload_dir);
    tokio::fs::create_dir_all(&upload_path).await?;
    info!("Upload dir: {:?}", upload_path);

    let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "/app/streamvault.db".to_string());
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let db_url = format!("sqlite://{}?mode=rwc", db_path);
    info!("Database: {}", db_url);

    let db = Database::new(&db_url).await?;
    db.migrate().await?;
    info!("Database ready");

    let state = AppState {
        db: Arc::new(db),
        upload_dir: upload_path,
        base_url,
    };

    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let app = Router::new()
        // Disable body limit on upload so large video files aren't rejected
        .route("/api/upload", post(handlers::upload::upload_video)
            .layer(DefaultBodyLimit::disable()))
        .route("/api/videos", get(handlers::upload::list_videos))
        .route("/api/videos/:token", get(handlers::stream::video_info))
        .route("/api/stream/:token", get(handlers::stream::stream_video))
        .route("/api/hls/:token/playlist.m3u8", get(handlers::stream::hls_playlist))
        .route("/api/hls/:token/:segment", get(handlers::stream::hls_segment))
        .route("/health", get(handlers::health::health_check))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    let listener = TcpListener::bind(&bind_addr).await?;
    info!("StreamVault listening on {}", bind_addr);

    axum::serve(listener, app).await?;
    Ok(())
}
