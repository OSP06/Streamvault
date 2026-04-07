use axum::{
    body::Body,
    extract::{Multipart, State},
    http::Request,
    Json,
};
use axum::extract::DefaultBodyLimit;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};
use uuid::Uuid;
use futures::StreamExt;

use crate::{
    error::AppError,
    models::{UploadResponse, Video, VideoResponse},
    AppState,
};

const MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024; // 1GB

const ALLOWED_TYPES: &[&str] = &[
    "video/mp4",
    "video/webm",
    "video/quicktime",
    "video/x-msvideo",
    "video/x-matroska",
    "video/mp2t",
    "video/mpeg",
    "video/ogg",
    "application/octet-stream",
];

pub async fn upload_video(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, AppError> {
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        AppError::BadRequest(format!("Multipart error: {e}"))
    })? {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name != "video" {
            continue;
        }

        let original_name = field.file_name().unwrap_or("upload").to_string();
        let content_type = field.content_type().unwrap_or("application/octet-stream").to_string();

        let effective_ct = infer_content_type(&original_name, &content_type);
        if !ALLOWED_TYPES.contains(&effective_ct.as_str()) {
            return Err(AppError::UnsupportedFormat);
        }

        let ext = extension_from_content_type(&effective_ct, &original_name);
        let token = generate_token();
        let stored_filename = format!("{}.{}", Uuid::new_v4(), ext);
        let file_path = state.upload_dir.join(&stored_filename);

        let mut file = tokio::fs::File::create(&file_path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

        let mut total_bytes: usize = 0;
        let mut data = field;

        while let Some(chunk) = data.chunk().await.map_err(|e| {
            AppError::BadRequest(format!("Read error: {e}"))
        })? {
            total_bytes += chunk.len();
            if total_bytes > MAX_UPLOAD_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&file_path).await;
                return Err(AppError::FileTooLarge);
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }

        file.flush().await.map_err(|e| AppError::Internal(e.into()))?;
        info!("Uploaded {} ({} bytes) → token={}", original_name, total_bytes, token);

        let video = Video {
            id: None,
            token: token.clone(),
            filename: stored_filename,
            original_name: original_name.clone(),
            content_type: effective_ct,
            size_bytes: total_bytes as i64,
            duration_secs: None,
            width: None,
            height: None,
            hls_ready: false,
            created_at: String::new(),
        };

        state.db.insert_video(&video).await?;

        let state_clone = state.clone();
        let token_clone = token.clone();
        let path_clone = file_path.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::streaming::transcode_to_hls(&state_clone, &token_clone, &path_clone).await {
                warn!("HLS transcode failed for {}: {}", token_clone, e);
            }
        });

        return Ok(Json(UploadResponse {
            share_url: format!("{}/watch/{}", state.base_url, token),
            stream_url: format!("{}/api/stream/{}", state.base_url, token),
            token,
            original_name,
            size_bytes: total_bytes as i64,
        }));
    }

    Err(AppError::BadRequest("No `video` field in form".to_string()))
}

pub async fn list_videos(
    State(state): State<AppState>,
) -> Result<Json<Vec<VideoResponse>>, AppError> {
    let videos = state.db.list_videos().await?;
    let resp: Vec<VideoResponse> = videos.iter().map(|v| VideoResponse::from_video(v, &state.base_url)).collect();
    Ok(Json(resp))
}

fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..8).map(|_| {
        let idx = rng.gen_range(0..36usize);
        if idx < 10 { (b'0' + idx as u8) as char } else { (b'a' + (idx - 10) as u8) as char }
    }).collect()
}

fn infer_content_type(filename: &str, declared: &str) -> String {
    if declared != "application/octet-stream" {
        return declared.to_string();
    }
    match filename.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "ts" => "video/mp2t",
        "mpeg" | "mpg" => "video/mpeg",
        _ => declared,
    }.to_string()
}

fn extension_from_content_type(ct: &str, fallback_name: &str) -> String {
    match ct {
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        "video/x-matroska" => "mkv",
        "video/mp2t" => "ts",
        "video/mpeg" => "mpeg",
        _ => fallback_name.rsplit('.').next().unwrap_or("bin"),
    }.to_string()
}
