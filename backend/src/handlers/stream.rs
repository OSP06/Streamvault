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
    let video = state
        .db
        .get_video_by_token(&token)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Video {} not found", token)))?;
    Ok(Json(VideoResponse::from_video(&video, &state.base_url)))
}

pub async fn stream_video(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let video = state
        .db
        .get_video_by_token(&token)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Video {} not found", token)))?;

    let file_path = state.upload_dir.join(&video.filename);
    let file_size = tokio::fs::metadata(&file_path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .len();

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_range(s, file_size));

    let mut file = File::open(&file_path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    // If a Range header was provided, respond with 206 Partial Content.
    // If not, respond with 200 OK and stream the entire file.
    // RFC 7233 §4.1: Content-Range MUST NOT appear on 200 responses.
    // Previously the code always sent Content-Range, which is a protocol
    // violation (harmless in browsers, fails strict HTTP validators).
    if let Some((start, end)) = range {
        let chunk_size = end - start + 1;

        if start > 0 {
            use tokio::io::AsyncSeekExt;
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
        }

        debug!("Streaming {} range bytes={}-{}/{}", token, start, end, file_size);

        let stream = ReaderStream::new(tokio::io::AsyncReadExt::take(file, chunk_size));
        let body = Body::from_stream(stream);

        Ok(Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, &video.content_type)
            .header(header::CONTENT_LENGTH, chunk_size)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", start, end, file_size),
            )
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(body)
            .unwrap())
    } else {
        // No Range header — serve the whole file with 200 OK.
        // Still set Accept-Ranges so the browser knows it can seek later.
        debug!("Streaming {} full file ({} bytes)", token, file_size);

        let stream = ReaderStream::new(file);
        let body = Body::from_stream(stream);

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &video.content_type)
            .header(header::CONTENT_LENGTH, file_size)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(body)
            .unwrap())
    }
}

pub async fn hls_playlist(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let video = state
        .db
        .get_video_by_token(&token)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Video {} not found", token)))?;

    if !video.hls_ready {
        // Transcode not yet complete — redirect transparently to the raw
        // byte-range stream. The browser/player follows the 307 automatically.
        // 307 (not 301/302) preserves the GET method and is temporary —
        // the browser will re-check this endpoint after the page reloads.
        return Ok(Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header(header::LOCATION, format!("/api/stream/{}", token))
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .unwrap());
    }

    let playlist_path = state
        .upload_dir
        .join("hls")
        .join(&token)
        .join("playlist.m3u8");

    let content = tokio::fs::read_to_string(&playlist_path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
        // Playlist must never be cached — it changes as segments are added
        // during transcoding, and a stale playlist breaks the player.
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .body(Body::from(content))
        .unwrap())
}

pub async fn hls_segment(
    State(state): State<AppState>,
    Path((token, segment)): Path<(String, String)>,
) -> Result<Response, AppError> {
    // Sanitise segment name — reject path traversal attempts.
    // Without this, a request for `../../../etc/passwd` could escape the
    // upload directory. We allow only simple filenames (no slashes, no dots
    // at the start, no parent references).
    if segment.contains('/') || segment.contains("..") || segment.starts_with('.') {
        return Err(AppError::BadRequest("Invalid segment name".to_string()));
    }

    let segment_path = state
        .upload_dir
        .join("hls")
        .join(&token)
        .join(&segment);

    let file = File::open(&segment_path)
        .await
        .map_err(|_| AppError::NotFound(format!("Segment {} not found", segment)))?;

    let stream = ReaderStream::new(file);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp2t")
        // Segments are immutable once written — safe to cache aggressively.
        // CDN/browser cache hit rate is ~100% for repeat viewers.
        .header(header::CACHE_CONTROL, "public, max-age=86400, immutable")
        .body(Body::from_stream(stream))
        .unwrap())
}

/// Parse an HTTP Range header value into (start, end) byte offsets.
///
/// Handles the common cases:
///   "bytes=0-1048575"    → Some((0, 1048575))
///   "bytes=1048576-"     → Some((1048576, file_size - 1))   ← open-ended
///   "bytes=0-"           → Some((0, file_size - 1))
///
/// Returns None for malformed headers, which triggers a 200 full-file response.
fn parse_range(range: &str, file_size: u64) -> Option<(u64, u64)> {
    let bytes = range.strip_prefix("bytes=")?;
    let mut parts = bytes.splitn(2, '-');
    let start: u64 = parts.next()?.parse().ok()?;
    let end: u64 = parts
        .next()
        .and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
        .unwrap_or(file_size - 1)
        .min(file_size - 1);

    if start > end {
        return None;
    }
    Some((start, end))
}