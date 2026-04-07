/// Background HLS transcoding pipeline
///
/// Design decision: transcoding runs *after* upload returns to the client.
/// This minimises time-to-stream — the raw file is immediately servable
/// via byte-range streaming. HLS kicks in asynchronously for better
/// playback compatibility (bonus requirement).
///
/// We shell out to `ffmpeg` rather than using ffmpeg-sys Rust bindings because:
///   1. ffmpeg-sys adds hours to build time and ~300MB of C deps
///   2. A subprocess crash is isolated — a codec bug cannot segfault Axum
///   3. `-c:v copy` makes the operation I/O-bound, not CPU-bound, so
///      subprocess overhead is negligible relative to disk I/O time
///
/// FFmpeg flag rationale:
///   -c:v copy         Remux video bitstream as-is — no decode/encode cycle.
///                     This is the key flag for time-to-stream. A 1GB file
///                     remuxes in seconds; re-encoding the same file at
///                     libx264 medium preset takes minutes.
///   -c:a aac          AAC audio is universally supported in HLS/MPEG-TS.
///                     Most source files use AAC already; this re-encodes
///                     only when the source codec differs (e.g. FLAC, Opus).
///   -hls_time 2       2-second segments. Chosen to balance:
///                       - Seek latency: shorter = player can jump faster
///                       - Request overhead: too short = too many HTTP GETs
///                       - Buffer start: player needs ~2 segments to begin
///                     Industry standard is 4-6s; 2s is better for seek UX.
///   -hls_list_size 0  VOD mode — keep ALL segments in the playlist file.
///                     Without this, only the last N segments appear, which
///                     breaks seeking to the beginning of long videos.
///   -hls_segment_type mpegts
///                     MPEG-TS container per segment. More compatible than
///                     fMP4 (the alternative), especially on older Safari/iOS.
///
/// Failure mode: if FFmpeg is not installed or exits non-zero, the function
/// returns Ok(()) — the raw byte-range stream remains the fallback. HLS is
/// an enhancement, not a hard dependency.

use std::path::Path;
use anyhow::Result;
use tracing::{info, warn};

use crate::AppState;

pub async fn transcode_to_hls(
    state: &AppState,
    token: &str,
    input_path: &Path,
) -> Result<()> {
    let hls_dir = state.upload_dir.join("hls").join(token);
    tokio::fs::create_dir_all(&hls_dir).await?;

    let playlist = hls_dir.join("playlist.m3u8");
    let segment_pattern = hls_dir.join("seg%03d.ts");

    let output = tokio::process::Command::new("ffmpeg")
        .args([
            "-i",  input_path.to_str().unwrap(),
            "-c:v", "copy",
            "-c:a", "aac",
            "-hls_time", "2",
            "-hls_list_size", "0",
            "-hls_segment_type", "mpegts",
            "-hls_segment_filename", segment_pattern.to_str().unwrap(),
            "-f", "hls",
            playlist.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        // Capture stderr so we can log it on failure instead of silently
        // discarding it. Previously, ffmpeg exits with code 1 produced only
        // "ffmpeg exited with Some(1)" — no hint as to why. Now we log the
        // actual ffmpeg error message (codec unsupported, corrupt file, etc).
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            info!("HLS transcode complete for token={}", token);
            state.db.mark_hls_ready(token).await?;
        }
        Ok(o) => {
            // FFmpeg ran but exited non-zero. Log stderr for debuggability.
            // System degrades gracefully — raw stream still works.
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "ffmpeg exited {:?} for token={} — falling back to byte-range stream\nffmpeg stderr: {}",
                o.status.code(),
                token,
                stderr.trim()
            );
            // Clean up any partial HLS directory to avoid serving corrupt segments
            let _ = tokio::fs::remove_dir_all(&hls_dir).await;
        }
        Err(e) => {
            // FFmpeg binary not found. This is expected in local dev without Docker.
            // Not an error — byte-range streaming works without it.
            warn!(
                "ffmpeg not available ({}), HLS skipped for token={} — byte-range stream active",
                e, token
            );
        }
    }

    Ok(())
}