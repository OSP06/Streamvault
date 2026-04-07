use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tracing::debug;

use crate::{error::AppError, models::VideoResponse, AppState};

pub async fn video_info(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<VideoResponse>, AppError> {
    let video = state.db.get_video_by_token(&token).await?
        .ok_or_else(|| AppError::NotFound(format!("Video {} not found", token)))?;
    Ok(Json(VideoResponse::from_video(&video, &state.base_url)))
}

pub async fn stream_video(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let video = state.db.get_video_by_token(&token).await?
        .ok_or_else(|| AppError::NotFound(format!("Video {} not found", token)))?;

    let file_path = state.upload_dir.join(&video.filename);
    let file_size = tokio::fs::metadata(&file_path).await
        .map_err(|e| AppError::Internal(e.into()))?.len();

    let range = headers.get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_range(s, file_size));

    let (start, end) = range.unwrap_or((0, file_size - 1));
    let chunk_size = end - start + 1;

    debug!("Streaming {} bytes {}-{}/{}", token, start, end, file_size);

    let mut file = File::open(&file_path).await
        .map_err(|e| AppError::Internal(e.into()))?;

    if start > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(start)).await
            .map_err(|e| AppError::Internal(e.into()))?;
    }

    let stream = ReaderStream::new(tokio::io::AsyncReadExt::take(file, chunk_size));
    let body = Body::from_stream(stream);

    let status = if start > 0 || end < file_size - 1 {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, &video.content_type)
        .header(header::CONTENT_LENGTH, chunk_size)
        .header(header::CONTENT_RANGE, format!("bytes {}-{}/{}", start, end, file_size))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(body)
        .unwrap();

    Ok(response)
}

pub async fn hls_playlist(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let video = state.db.get_video_by_token(&token).await?
        .ok_or_else(|| AppError::NotFound(format!("Video {} not found", token)))?;

    if !video.hls_ready {
        return Ok(Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header(header::LOCATION, format!("/api/stream/{}", token))
            .body(Body::empty())
            .unwrap());
    }

    let playlist_path = state.upload_dir.join("hls").join(&token).join("playlist.m3u8");
    let content = tokio::fs::read_to_string(&playlist_path).await
        .map_err(|e| AppError::Internal(e.into()))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(content))
        .unwrap())
}

pub async fn hls_segment(
    State(state): State<AppState>,
    Path((token, segment)): Path<(String, String)>,
) -> Result<Response, AppError> {
    if segment.contains('/') || segment.contains("..") {
        return Err(AppError::BadRequest("Invalid segment".to_string()));
    }

    let segment_path = state.upload_dir.join("hls").join(&token).join(&segment);
    let file = File::open(&segment_path).await
        .map_err(|_| AppError::NotFound(format!("Segment {} not found", segment)))?;

    let stream = ReaderStream::new(file);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp2t")
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(Body::from_stream(stream))
        .unwrap())
}

fn parse_range(range: &str, file_size: u64) -> Option<(u64, u64)> {
    let bytes = range.strip_prefix("bytes=")?;
    let mut parts = bytes.splitn(2, '-');
    let start: u64 = parts.next()?.parse().ok()?;
    let end: u64 = parts.next()
        .and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
        .unwrap_or(file_size - 1)
        .min(file_size - 1);
    if start > end { return None; }
    Some((start, end))
}
