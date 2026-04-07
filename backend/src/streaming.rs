/// Background HLS transcoding pipeline
///
/// Design decision: transcoding runs *after* upload returns to the client.
/// This minimises time-to-stream — the raw file is immediately servable
/// via byte-range streaming. HLS kicks in asynchronously for adaptive
/// playback quality (bonus requirement).
///
/// We shell out to `ffmpeg` because:
///   1. It is the industry standard for video transcoding
///   2. Rust bindings (ffmpeg-sys) add significant build complexity
///   3. FFmpeg subprocess isolation means a codec crash won't kill the server

use std::path::Path;
use anyhow::Result;
use tracing::{info, warn};

use crate::AppState;

/// Transcode a video to HLS format with 2-second segments.
///
/// Output layout:
///   uploads/hls/<token>/playlist.m3u8
///   uploads/hls/<token>/seg000.ts
///   uploads/hls/<token>/seg001.ts ...
///
/// HLS segment duration of 2s is chosen to balance:
///   - Seek latency (shorter = faster seeks)
///   - Request overhead (longer = fewer HTTP requests)
///   - Buffer size (2s is well within typical player buffer)
pub async fn transcode_to_hls(
    state: &AppState,
    token: &str,
    input_path: &Path,
) -> Result<()> {
    let hls_dir = state.upload_dir.join("hls").join(token);
    tokio::fs::create_dir_all(&hls_dir).await?;

    let playlist = hls_dir.join("playlist.m3u8");
    let segment_pattern = hls_dir.join("seg%03d.ts");

    // ffmpeg invocation:
    //   -c:v copy     — remux without re-encoding for speed (time-to-stream priority)
    //   -c:a aac      — AAC audio is universally supported in HLS
    //   -hls_time 2   — 2-second segments
    //   -hls_list_size 0 — keep all segments in playlist (VOD mode)
    //   -hls_segment_filename — explicit segment naming
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-i", input_path.to_str().unwrap(),
            "-c:v", "copy",         // no re-encode — fast!
            "-c:a", "aac",
            "-hls_time", "2",
            "-hls_list_size", "0",
            "-hls_segment_type", "mpegts",
            "-hls_segment_filename", segment_pattern.to_str().unwrap(),
            "-f", "hls",
            playlist.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            info!("HLS transcode complete for token={}", token);
            state.db.mark_hls_ready(token).await?;
            Ok(())
        }
        Ok(s) => {
            warn!("ffmpeg exited with {:?} for token={}", s.code(), token);
            // Not a fatal error — raw stream still works
            Ok(())
        }
        Err(e) => {
            // ffmpeg not installed — silently degrade to raw streaming
            warn!("ffmpeg not available ({}), HLS skipped for token={}", e, token);
            Ok(())
        }
    }
}
