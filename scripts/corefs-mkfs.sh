#!/usr/bin/env bash
set -euo pipefail

IMAGE_PATH="${1:-./corefs-volume.img}"
shift $(( $# > 0 ? 1 : 0 ))

if [[ -x "./dist/bin/corefs" ]]; then
  ./dist/bin/corefs mkfs-image "$IMAGE_PATH" --bootstrap "$@"
else
  cargo run --release -- mkfs-image "$IMAGE_PATH" --bootstrap "$@"
fi
