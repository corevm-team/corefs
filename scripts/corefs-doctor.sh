#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COREFS_BIN="${REPO_ROOT}/dist/bin/corefs"
TARGET_BIN="${REPO_ROOT}/target/release/corefs"

IMAGE_PATH="${1:-${SCRIPT_DIR}/corefs-volume.img}"
MOUNT_POINT="${2:-${SCRIPT_DIR}/mnt/corefs}"

shift $(( $# > 1 ? 2 : $# ))

mkdir -p "$(dirname "$MOUNT_POINT")"

if [[ -x "${COREFS_BIN}" ]]; then
  exec "${COREFS_BIN}" diagnose-mount "$IMAGE_PATH" "$MOUNT_POINT" "$@"
elif [[ -x "${TARGET_BIN}" ]]; then
  exec "${TARGET_BIN}" diagnose-mount "$IMAGE_PATH" "$MOUNT_POINT" "$@"
else
  cd "${REPO_ROOT}"
  exec cargo run --release -- diagnose-mount "$IMAGE_PATH" "$MOUNT_POINT" "$@"
fi
