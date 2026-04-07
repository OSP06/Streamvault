use serde::{Deserialize, Serialize};

// Note: no sqlx::FromRow derive — we map rows manually in db.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub id: Option<i64>,
    pub token: String,
    pub filename: String,
    pub original_name: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub duration_secs: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub hls_ready: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct VideoResponse {
    pub token: String,
    pub original_name: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub duration_secs: Option<f64>,
    pub hls_ready: bool,
    pub created_at: String,
    pub stream_url: String,
    pub share_url: String,
}

impl VideoResponse {
    pub fn from_video(v: &Video, base_url: &str) -> Self {
        VideoResponse {
            token: v.token.clone(),
            original_name: v.original_name.clone(),
            content_type: v.content_type.clone(),
            size_bytes: v.size_bytes,
            duration_secs: v.duration_secs,
            hls_ready: v.hls_ready,
            created_at: v.created_at.clone(),
            stream_url: format!("{}/api/stream/{}", base_url, v.token),
            share_url: format!("{}/watch/{}", base_url, v.token),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub token: String,
    pub share_url: String,
    pub stream_url: String,
    pub original_name: String,
    pub size_bytes: i64,
}
