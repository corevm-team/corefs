#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COREFS_BIN="${REPO_ROOT}/dist/bin/corefs"
TARGET_BIN="${REPO_ROOT}/target/release/corefs"

IMAGE_PATH="${1:-${SCRIPT_DIR}/corefs-volume.img}"
shift $(( $# > 0 ? 1 : 0 ))

if command -v cargo >/dev/null 2>&1; then
  cd "${REPO_ROOT}"
  exec cargo run --release -- mkfs-image "$IMAGE_PATH" --bootstrap "$@"
elif [[ -x "${COREFS_BIN}" ]]; then
  exec "${COREFS_BIN}" mkfs-image "$IMAGE_PATH" --bootstrap "$@"
elif [[ -x "${TARGET_BIN}" ]]; then
  exec "${TARGET_BIN}" mkfs-image "$IMAGE_PATH" --bootstrap "$@"
else
  echo "No CoreFS binary available. Run ./scripts/build.sh first or install cargo." >&2
  exit 1
fi
