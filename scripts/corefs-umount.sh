#!/usr/bin/env bash
set -euo pipefail

MOUNT_POINT="${1:-./mnt/corefs}"

if command -v fusermount3 >/dev/null 2>&1; then
  fusermount3 -u "$MOUNT_POINT"
elif command -v fusermount >/dev/null 2>&1; then
  fusermount -u "$MOUNT_POINT"
else
  umount "$MOUNT_POINT"
fi
