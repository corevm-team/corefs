#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COREFS_BIN="${REPO_ROOT}/dist/bin/corefs"
TARGET_BIN="${REPO_ROOT}/target/release/corefs"

IMAGE_PATH="${1:-${SCRIPT_DIR}/corefs-volume.img}"
MOUNT_POINT="${2:-${SCRIPT_DIR}/mnt/corefs}"
SKIP_DOCTOR=0

mkdir -p "$MOUNT_POINT"
shift $(( $# > 1 ? 2 : $# ))

EXTRA_ARGS=()
for arg in "$@"; do
  if [[ "$arg" == "--skip-doctor" ]]; then
    SKIP_DOCTOR=1
  else
    EXTRA_ARGS+=("$arg")
  fi
done

if [[ -x "${COREFS_BIN}" ]]; then
  BIN_CMD=( "${COREFS_BIN}" )
elif [[ -x "${TARGET_BIN}" ]]; then
  BIN_CMD=( "${TARGET_BIN}" )
else
  cd "${REPO_ROOT}"
  BIN_CMD=( cargo run --release -- )
fi

if [[ ${SKIP_DOCTOR} -ne 1 ]]; then
  "${BIN_CMD[@]}" diagnose-mount "$IMAGE_PATH" "$MOUNT_POINT" "${EXTRA_ARGS[@]}"
fi

exec "${BIN_CMD[@]}" mount-image "$IMAGE_PATH" "$MOUNT_POINT" "${EXTRA_ARGS[@]}"
