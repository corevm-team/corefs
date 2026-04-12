#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
IMAGE_PATH="${1:-${SCRIPT_DIR}/corefs-volume.img}"
MOUNT_POINT="${2:-${SCRIPT_DIR}/mnt/corefs}"
LOG_PATH="${3:-${SCRIPT_DIR}/corefs-mount-trace.log}"

shift $(( $# > 2 ? 3 : $# ))

mkdir -p "$(dirname "$LOG_PATH")" "$MOUNT_POINT"

{
  echo "# CoreFS Mount Trace"
  echo "timestamp=$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "image=${IMAGE_PATH}"
  echo "mountpoint=${MOUNT_POINT}"
  echo
  echo "## Diagnose"
  "${REPO_ROOT}/scripts/corefs-doctor.sh" "$IMAGE_PATH" "$MOUNT_POINT" "$@" || true
  echo
  echo "## Mount Attempt"
  if command -v cargo >/dev/null 2>&1; then
    (
      cd "${REPO_ROOT}"
      cargo run --release -- mount-image "$IMAGE_PATH" "$MOUNT_POINT" "$@"
    )
  elif [[ -x "${REPO_ROOT}/dist/bin/corefs" ]]; then
    "${REPO_ROOT}/dist/bin/corefs" mount-image "$IMAGE_PATH" "$MOUNT_POINT" "$@"
  else
    "${REPO_ROOT}/target/release/corefs" mount-image "$IMAGE_PATH" "$MOUNT_POINT" "$@"
  fi
} >"$LOG_PATH" 2>&1 || true

{
  echo
  echo "## Kernel Excerpt"
  dmesg --color=never 2>/dev/null | tail -n 80 || true
  echo
  echo "## Journal Excerpt"
  journalctl -k -n 80 --no-pager 2>/dev/null || true
} >>"$LOG_PATH"

echo "trace written to ${LOG_PATH}"
