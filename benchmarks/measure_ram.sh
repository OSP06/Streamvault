#!/usr/bin/env bash
#
# RAM Usage During Upload
#
# Proves the O(chunk_size) memory claim: a 500MB upload and a 5MB upload
# should produce the same peak RSS on the backend process.
#
# Runs two uploads simultaneously with memory sampling every 100ms.
# Requires: Docker running, StreamVault up, ffmpeg, jq
#
# Usage:
#   chmod +x benchmarks/measure_ram.sh
#   ./benchmarks/measure_ram.sh

set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost}"
WORK_DIR="$(mktemp -d)"
CONTAINER_NAME="${CONTAINER_NAME:-streamvault-backend-1}"

CYAN='\033[0;36m'; GREEN='\033[0;32m'; NC='\033[0m'
info()   { echo -e "${CYAN}[ram]${NC} $*"; }
result() { echo -e "${GREEN}  ➜${NC} $*"; }

cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

generate_video() {
    local label="$1" duration="$2"
    local out="$WORK_DIR/${label}.mp4"
    ffmpeg -y -loglevel error \
        -f lavfi -i "testsrc=duration=${duration}:size=1280x720:rate=30" \
        -f lavfi -i "sine=frequency=440:duration=${duration}" \
        -c:v libx264 -preset ultrafast -crf 23 -c:a aac -b:a 128k \
        "$out"
    echo "$out"
}

sample_ram_loop() {
    # Samples RSS every 100ms, writes to file. Stopped by signal.
    local outfile="$1"
    echo "0" > "$outfile"
    while true; do
        local mem
        # docker stats --no-stream returns: CONTAINER ID, NAME, CPU %, MEM USAGE/LIMIT ...
        mem=$(docker stats --no-stream --format "{{.MemUsage}}" "$CONTAINER_NAME" 2>/dev/null \
              | awk '{print $1}' | sed 's/MiB//' | sed 's/GiB/*1024/' | bc 2>/dev/null || echo "0")
        if [[ -n "$mem" && "$mem" != "0" ]]; then
            local current
            current=$(cat "$outfile")
            # Track peak
            if (( $(echo "$mem > $current" | bc -l) )); then
                echo "$mem" > "$outfile"
            fi
        fi
        sleep 0.1
    done
}

measure_peak_ram_during_upload() {
    local label="$1" video_file="$2"
    local peak_file="$WORK_DIR/peak_${label}"

    info "Measuring RAM during ${label} upload ..."

    # Get baseline RAM before upload
    local baseline
    baseline=$(docker stats --no-stream --format "{{.MemUsage}}" "$CONTAINER_NAME" \
               | awk '{print $1}' | sed 's/MiB//' | sed 's/GiB/*1024/' | bc 2>/dev/null || echo "0")
    info "  Baseline RSS: ${baseline} MiB"

    # Start RAM sampler in background
    echo "$baseline" > "$peak_file"
    sample_ram_loop "$peak_file" &
    local sampler_pid=$!

    # Run the upload
    curl -s -o /dev/null \
        -X POST "${BASE_URL}/api/upload" \
        -F "video=@${video_file}"

    # Stop sampler
    kill "$sampler_pid" 2>/dev/null || true
    wait "$sampler_pid" 2>/dev/null || true

    local peak
    peak=$(cat "$peak_file")
    local delta
    delta=$(echo "scale=1; $peak - $baseline" | bc)

    result "${label} baseline RAM:  ${baseline} MiB"
    result "${label} peak RAM:      ${peak} MiB"
    result "${label} RAM increase:  +${delta} MiB"
}

main() {
    # Check Docker is available
    if ! docker ps --filter "name=${CONTAINER_NAME}" --format "{{.Names}}" | grep -q "$CONTAINER_NAME"; then
        echo "Container '${CONTAINER_NAME}' not running. Start with: docker compose up --build"
        echo "Or set CONTAINER_NAME env var to match your container name."
        exit 1
    fi

    info "Generating test videos ..."
    local small large
    small=$(generate_video "small_5mb" 7)
    large=$(generate_video "large_500mb" 720)

    echo ""
    echo "============================================"
    echo "  RAM TEST: O(chunk_size) upload memory"
    echo "  Expected: both uploads use ~same peak RAM"
    echo "  (chunk size = 64KB, regardless of file size)"
    echo "============================================"
    echo ""

    measure_peak_ram_during_upload "small_5mb"   "$small"
    echo ""
    measure_peak_ram_during_upload "large_500mb" "$large"

    echo ""
    echo "If both rows show similar RAM increase (~0-5 MiB), the O(chunk) claim holds."
    echo "A large RAM increase on the large file would indicate buffering (it should not)."
}

main "$@"
