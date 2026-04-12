#!/usr/bin/env bash
set -euo pipefail

IMAGE_PATH="${1:-./corefs-volume.img}"
MOUNT_POINT="${2:-./mnt/corefs}"
LOG_PATH="${3:-./PERFORMANCE_LOG.md}"

FILES="${COREFS_BENCH_FILES:-1000}"
PAYLOAD_BYTES="${COREFS_BENCH_PAYLOAD:-4096}"
SEQ_MIB="${COREFS_BENCH_SEQ_MIB:-64}"
THREADS="${COREFS_THREADS:-4}"

mkdir -p "$MOUNT_POINT"

cleanup() {
  if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
    if command -v fusermount3 >/dev/null 2>&1; then
      fusermount3 -u "$MOUNT_POINT" || true
    elif command -v fusermount >/dev/null 2>&1; then
      fusermount -u "$MOUNT_POINT" || true
    else
      umount "$MOUNT_POINT" || true
    fi
  fi

  if [[ -n "${MOUNT_PID:-}" ]]; then
    kill "$MOUNT_PID" >/dev/null 2>&1 || true
    wait "$MOUNT_PID" >/dev/null 2>&1 || true
  fi
}

trap cleanup EXIT

if [[ -x "./dist/bin/corefs" ]]; then
  COREFS_BIN="./dist/bin/corefs"
else
  COREFS_BIN="cargo run --release --"
fi

eval "${COREFS_BIN} mkfs-image \"$IMAGE_PATH\" --bootstrap"
eval "${COREFS_BIN} mount-image \"$IMAGE_PATH\" \"$MOUNT_POINT\" --threads \"$THREADS\"" &
MOUNT_PID=$!

for _ in $(seq 1 50); do
  if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
    break
  fi
  sleep 0.2
done

if ! mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
  echo "mount did not become ready at $MOUNT_POINT" >&2
  exit 1
fi

BENCH_DIR="$MOUNT_POINT/.corefs-bench"
rm -rf "$BENCH_DIR"
mkdir -p "$BENCH_DIR"

metadata_start_ns=$(date +%s%N)
for index in $(seq 1 "$FILES"); do
  printf '%*s' "$PAYLOAD_BYTES" '' | tr ' ' 'x' > "$BENCH_DIR/file-$index.bin"
done
metadata_end_ns=$(date +%s%N)

read_start_ns=$(date +%s%N)
for index in $(seq 1 "$FILES"); do
  cat "$BENCH_DIR/file-$index.bin" > /dev/null
done
read_end_ns=$(date +%s%N)

seq_start_ns=$(date +%s%N)
dd if=/dev/zero of="$BENCH_DIR/sequential.bin" bs=1M count="$SEQ_MIB" conv=fsync status=none
sync
seq_end_ns=$(date +%s%N)

metadata_ms=$(( (metadata_end_ns - metadata_start_ns) / 1000000 ))
read_ms=$(( (read_end_ns - read_start_ns) / 1000000 ))
seq_ms=$(( (seq_end_ns - seq_start_ns) / 1000000 ))

timestamp="$(date -u '+%Y-%m-%d %H:%M:%S UTC')"

if [[ ! -f "$LOG_PATH" ]]; then
  {
    echo "# CoreFS Performance Log"
    echo
    echo "| Timestamp | Mode | Files | Payload (B) | Seq Write (MiB) | Create (ms) | Read (ms) | Seq Write (ms) |"
    echo "| --- | --- | --- | --- | --- | --- | --- | --- |"
  } >> "$LOG_PATH"
fi

echo "| $timestamp | fuse-mounted | $FILES | $PAYLOAD_BYTES | $SEQ_MIB | $metadata_ms | $read_ms | $seq_ms |" >> "$LOG_PATH"

if command -v fio >/dev/null 2>&1; then
  fio --name=corefs-randrw \
      --directory="$BENCH_DIR" \
      --filename=fio-randrw.bin \
      --size=32m \
      --rw=randrw \
      --rwmixread=70 \
      --bs=4k \
      --ioengine=sync \
      --direct=0 \
      --numjobs=1 \
      --runtime=10 \
      --time_based \
      --group_reporting \
      > "${LOG_PATH%.md}-fio.txt"
fi
