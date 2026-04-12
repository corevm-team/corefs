#!/usr/bin/env bash
set -euo pipefail

IMAGE_PATH="${1:-./corefs-volume.img}"
shift $(( $# > 0 ? 1 : 0 ))

cargo run --release -- mkfs-image "$IMAGE_PATH" --bootstrap "$@"
