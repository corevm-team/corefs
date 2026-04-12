#!/usr/bin/env bash
set -euo pipefail

IMAGE_PATH="${1:-./corefs-volume.img}"
MOUNT_POINT="${2:-./mnt/corefs}"

mkdir -p "$MOUNT_POINT"
shift $(( $# > 1 ? 2 : $# ))

if [[ -x "./dist/bin/corefs" ]]; then
  ./dist/bin/corefs mount-image "$IMAGE_PATH" "$MOUNT_POINT" "$@"
else
  cargo run --release -- mount-image "$IMAGE_PATH" "$MOUNT_POINT" "$@"
fi
