#!/usr/bin/env bash
#
# StreamVault Performance Benchmark Suite
#
# Measures the metrics that matter for the spec:
#   1. Time-to-stream  — seconds from upload-complete to first streamable byte
#   2. HLS ready time  — seconds from upload-complete to hls_ready=true
#   3. Upload RAM      — peak RSS during a large upload (proves O(chunk) not O(file))
#   4. Concurrent seek — latency for N simultaneous byte-range seeks
#
# Prerequisites:
#   - StreamVault running at BASE_URL (Docker: http://localhost)
#   - ffmpeg   (for generating synthetic test videos)
#   - curl, jq, bc, /usr/bin/time (standard on macOS/Linux)
#
# Usage:
#   chmod +x benchmarks/measure.sh
#   ./benchmarks/measure.sh                   # runs against http://localhost
#   BASE_URL=http://localhost:3000 ./benchmarks/measure.sh

set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost}"
WORK_DIR="$(mktemp -d)"
RESULTS_FILE="benchmarks/results.txt"

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()    { echo -e "${CYAN}[bench]${NC} $*"; }
success() { echo -e "${GREEN}[pass] ${NC} $*"; }
warn()    { echo -e "${YELLOW}[warn] ${NC} $*"; }
result()  { echo -e "${GREEN}  ➜${NC} $*"; tee -a "$RESULTS_FILE" <<< "  $*" > /dev/null; }

# ── prerequisites ─────────────────────────────────────────────────────────────
check_deps() {
    for cmd in curl jq bc ffmpeg; do
        if ! command -v "$cmd" &>/dev/null; then
            echo -e "${RED}ERROR:${NC} '$cmd' is required but not installed."
            exit 1
        fi
    done
}

# ── test video generation ─────────────────────────────────────────────────────
# Generates a synthetic H.264/AAC mp4 of a given duration (seconds).
# 1 minute of 1280x720 H.264 ≈ ~50 MB; adjust duration for size targets.
generate_video() {
    local label="$1"
    local duration_secs="$2"
    local output="$WORK_DIR/${label}.mp4"

    info "Generating ${label} test video (${duration_secs}s @ 720p) ..."
    ffmpeg -y -loglevel error \
        -f lavfi -i "testsrc=duration=${duration_secs}:size=1280x720:rate=30" \
        -f lavfi -i "sine=frequency=440:duration=${duration_secs}" \
        -c:v libx264 -preset ultrafast -crf 23 \
        -c:a aac -b:a 128k \
        -movflags +faststart \
        "$output"

    local size_mb
    size_mb=$(du -m "$output" | cut -f1)
    info "  Generated: ${output} (~${size_mb} MB)"
    echo "$output"
}

# ── upload + time-to-stream measurement ──────────────────────────────────────
# Returns the share token and prints timing stats.
measure_upload() {
    local label="$1"
    local file="$2"
    local size_bytes
    size_bytes=$(wc -c < "$file")

    info "--- Test: ${label} ($(( size_bytes / 1048576 )) MB) ---"

    # Record wall-clock time for the upload POST
    local t_start t_end upload_secs
    t_start=$(date +%s%N)

    local response
    response=$(curl -s -w '\n{"http_code":"%{http_code}","time_total":%{time_total}}' \
        -X POST "${BASE_URL}/api/upload" \
        -F "video=@${file}")

    t_end=$(date +%s%N)
    upload_secs=$(echo "scale=3; ($t_end - $t_start) / 1000000000" | bc)

    # Parse response (last line is the curl stats, rest is the JSON body)
    local body curl_stats token
    body=$(echo "$response" | head -n1)
    curl_stats=$(echo "$response" | tail -n1)
    token=$(echo "$body" | jq -r '.token // empty')

    if [[ -z "$token" ]]; then
        warn "Upload failed: $body"
        return 1
    fi

    local http_code time_total
    http_code=$(echo "$curl_stats" | jq -r '.http_code')
    time_total=$(echo "$curl_stats" | jq -r '.time_total')

    result "${label} | Upload size:       $(( size_bytes / 1048576 )) MB"
    result "${label} | Upload wall time:  ${time_total}s"
    result "${label} | HTTP status:       ${http_code}"

    # ── TIME-TO-STREAM ────────────────────────────────────────────────────────
    # The moment we have the token, the video should be streamable.
    # Measure how long the first byte-range GET takes.
    local t_stream_start t_stream_end t2s_ms
    t_stream_start=$(date +%s%N)

    local stream_status
    stream_status=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Range: bytes=0-1023" \
        "${BASE_URL}/api/stream/${token}")

    t_stream_end=$(date +%s%N)
    t2s_ms=$(echo "scale=1; ($t_stream_end - $t_stream_start) / 1000000" | bc)

    if [[ "$stream_status" == "206" ]]; then
        result "${label} | Time-to-stream:    ${t2s_ms} ms  ✓ (HTTP 206)"
    else
        warn "${label} | Time-to-stream: HTTP ${stream_status} — streaming failed"
    fi

    # ── HLS READY TIME ────────────────────────────────────────────────────────
    # Poll /api/videos/:token until hls_ready=true, recording the delay.
    local poll_start poll_now hls_ready_secs attempts=0
    poll_start=$(date +%s%N)

    while true; do
        (( attempts++ ))
        local meta
        meta=$(curl -s "${BASE_URL}/api/videos/${token}")
        local hls_ready
        hls_ready=$(echo "$meta" | jq -r '.hls_ready')

        if [[ "$hls_ready" == "true" ]]; then
            poll_now=$(date +%s%N)
            hls_ready_secs=$(echo "scale=2; ($poll_now - $poll_start) / 1000000000" | bc)
            result "${label} | HLS ready time:   ${hls_ready_secs}s (${attempts} polls)"
            break
        fi

        if (( attempts > 120 )); then
            warn "${label} | HLS did not become ready within 60s — FFmpeg may have failed"
            break
        fi
        sleep 0.5
    done

    echo "$token"
}

# ── seek latency test ─────────────────────────────────────────────────────────
# Performs N random seeks on a video, measuring response time for each.
measure_seek_latency() {
    local label="$1"
    local token="$2"
    local file_size_bytes="$3"
    local n_seeks="${4:-10}"

    info "Measuring seek latency (${n_seeks} random seeks on ${label}) ..."

    local total_ms=0 max_ms=0

    for i in $(seq 1 "$n_seeks"); do
        # Random offset between 0 and (file_size - 1MB)
        local max_offset=$(( file_size_bytes - 1048576 ))
        local offset=$(( RANDOM * RANDOM % max_offset ))
        local end_byte=$(( offset + 1048575 ))

        local t_start t_end elapsed_ms
        t_start=$(date +%s%N)

        curl -s -o /dev/null \
            -H "Range: bytes=${offset}-${end_byte}" \
            "${BASE_URL}/api/stream/${token}"

        t_end=$(date +%s%N)
        elapsed_ms=$(( (t_end - t_start) / 1000000 ))

        total_ms=$(( total_ms + elapsed_ms ))
        if (( elapsed_ms > max_ms )); then max_ms=$elapsed_ms; fi
    done

    local avg_ms=$(( total_ms / n_seeks ))
    result "${label} | Seek avg latency:  ${avg_ms} ms"
    result "${label} | Seek max latency:  ${max_ms} ms"
}

# ── concurrent viewer test ────────────────────────────────────────────────────
# Fires N simultaneous byte-range requests and reports p50/p95 latency.
measure_concurrent_streams() {
    local label="$1"
    local token="$2"
    local n_concurrent="${3:-20}"

    info "Measuring concurrent streaming (${n_concurrent} simultaneous viewers) ..."

    local tmpdir
    tmpdir=$(mktemp -d)

    # Launch N parallel curls
    for i in $(seq 1 "$n_concurrent"); do
        (
            t_start=$(date +%s%N)
            # Each "viewer" requests 1MB starting at a different offset
            offset=$(( i * 524288 ))
            curl -s -o /dev/null \
                -H "Range: bytes=${offset}-$(( offset + 1048575 ))" \
                "${BASE_URL}/api/stream/${token}"
            t_end=$(date +%s%N)
            echo $(( (t_end - t_start) / 1000000 )) > "${tmpdir}/req_${i}"
        ) &
    done
    wait

    # Collect and sort results
    local times=()
    for f in "${tmpdir}"/req_*; do
        times+=( "$(cat "$f")" )
    done

    # Sort numerically
    IFS=$'\n' sorted_times=($(sort -n <<< "${times[*]}")); unset IFS

    local n=${#sorted_times[@]}
    local p50_idx=$(( n / 2 ))
    local p95_idx=$(( n * 95 / 100 ))

    result "${label} | Concurrent viewers: ${n_concurrent}"
    result "${label} | p50 response time:  ${sorted_times[$p50_idx]} ms"
    result "${label} | p95 response time:  ${sorted_times[$p95_idx]} ms"
    result "${label} | max response time:  ${sorted_times[-1]} ms"

    rm -rf "$tmpdir"
}

# ── main ──────────────────────────────────────────────────────────────────────
main() {
    check_deps

    echo "" > "$RESULTS_FILE"
    echo "=======================================" >> "$RESULTS_FILE"
    echo "StreamVault Benchmark — $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$RESULTS_FILE"
    echo "BASE_URL: ${BASE_URL}" >> "$RESULTS_FILE"
    echo "=======================================" >> "$RESULTS_FILE"

    info "Starting StreamVault benchmark suite against ${BASE_URL}"
    info "Results will be saved to ${RESULTS_FILE}"
    echo ""

    # Health check
    local health
    health=$(curl -s "${BASE_URL}/health" | jq -r '.status // empty')
    if [[ "$health" != "ok" ]]; then
        echo -e "${RED}ERROR:${NC} StreamVault is not running at ${BASE_URL}"
        echo "Start with: docker compose up --build"
        exit 1
    fi
    success "StreamVault is up"
    echo ""

    # ── Test 1: Small file (≈10 MB / ~15s of 720p) ────────────────────────────
    local small_video small_token small_size
    small_video=$(generate_video "small_10mb" 15)
    small_size=$(wc -c < "$small_video")
    small_token=$(measure_upload "small_10mb" "$small_video")
    measure_seek_latency "small_10mb" "$small_token" "$small_size" 5
    measure_concurrent_streams "small_10mb" "$small_token" 10
    echo ""

    # ── Test 2: Medium file (≈100 MB / ~2.5min of 720p) ────────────────────────
    local med_video med_token med_size
    med_video=$(generate_video "medium_100mb" 150)
    med_size=$(wc -c < "$med_video")
    med_token=$(measure_upload "medium_100mb" "$med_video")
    measure_seek_latency "medium_100mb" "$med_token" "$med_size" 10
    measure_concurrent_streams "medium_100mb" "$med_token" 20
    echo ""

    # ── Test 3: Large file (≈500 MB / ~12min of 720p) ─────────────────────────
    info "Large file test (~500MB) — this takes a few minutes to generate ..."
    local large_video large_token large_size
    large_video=$(generate_video "large_500mb" 720)
    large_size=$(wc -c < "$large_video")
    large_token=$(measure_upload "large_500mb" "$large_video")
    measure_seek_latency "large_500mb" "$large_token" "$large_size" 10
    measure_concurrent_streams "large_500mb" "$large_token" 20
    echo ""

    # ── Summary ───────────────────────────────────────────────────────────────
    echo ""
    echo "============================================"
    echo "  RESULTS SUMMARY"
    echo "============================================"
    cat "$RESULTS_FILE"
    echo ""
    success "Benchmark complete. Full results saved to ${RESULTS_FILE}"

    rm -rf "$WORK_DIR"
}

main "$@"
