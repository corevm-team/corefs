#!/usr/bin/env bash
# CoreFS vs ext4 regression benchmark.
#
# Same workload on:
#   1. CoreFS FUSE mount (image file on host fs)
#   2. Plain ext4 directory on the host fs
#
# Both sit on the same storage medium so the measured difference is the
# CoreFS stack overhead, not disk speed.  Intended for Phase-1 regression
# detection — every meaningful change to FUSE / persist / BlockStore
# should be checked against this.
#
# Env overrides:
#   COREFS_BIN     — path to the `corefs` binary (default: cargo --release build artefact)
#   WORK           — working directory (default: /tmp/corefs-bench)
#   FILES          — small-file count (default 200)
#   PAYLOAD        — bytes per small file (default 4096)
#   SEQ_MIB        — sequential write/read size in MiB (default 128)
#   FSYNC_N        — fsync-heavy file count (default 50)
#   RAND_MIB       — random-IO file size in MiB (default 32)
#   RAND_OPS       — random-IO op count (default 500)
#   APPEND_OPS     — append-log op count (default 2000, each writes 1 KiB + fsync skipped)
#   IMG_SIZE_MIB   — CoreFS image pre-size (default 1024)
#   THREADS        — FUSE worker threads (default 4)
#   STEP_TIMEOUT   — seconds per workload step before skip (default 180)
#
# Writes results to $WORK/results.tsv and a pretty table to stdout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

COREFS_BIN="${COREFS_BIN:-${REPO_ROOT}/target/release/corefs}"
WORK="${WORK:-/tmp/corefs-bench}"
IMG_SIZE_MIB="${IMG_SIZE_MIB:-1024}"
FILES="${FILES:-200}"
PAYLOAD="${PAYLOAD:-4096}"
SEQ_MIB="${SEQ_MIB:-128}"
FSYNC_N="${FSYNC_N:-50}"
RAND_MIB="${RAND_MIB:-32}"
RAND_OPS="${RAND_OPS:-500}"
APPEND_OPS="${APPEND_OPS:-2000}"
THREADS="${THREADS:-4}"
STEP_TIMEOUT="${STEP_TIMEOUT:-180}"

RESULTS="$WORK/results.tsv"

if [[ ! -x "$COREFS_BIN" ]]; then
  echo "corefs binary not found at $COREFS_BIN — run 'cargo build --release' first." >&2
  exit 1
fi

mkdir -p "$WORK"
: > "$RESULTS"
echo -e "fs\tworkload\tms\tops_or_mibs" >> "$RESULTS"

cleanup_mount() {
  local mp="$1"
  if mountpoint -q "$mp" 2>/dev/null; then
    fusermount3 -u "$mp" 2>/dev/null \
      || fusermount -u "$mp" 2>/dev/null \
      || sudo -n umount "$mp" 2>/dev/null \
      || umount "$mp" 2>/dev/null || true
  fi
}

record() {
  local fs="$1" wl="$2" ms="$3" metric="$4"
  echo -e "${fs}\t${wl}\t${ms}\t${metric}" >> "$RESULTS"
  printf "  %-16s %-28s %10d ms   %s\n" "$fs" "$wl" "$ms" "$metric"
}

run_workloads() {
  local fs="$1" mnt="$2"
  local dir="$mnt/bench"
  rm -rf "$dir"; mkdir -p "$dir"

  local t0 t1

  # W1 create
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" python3 -c "
payload = b'x'*$PAYLOAD
for i in range($FILES):
    with open('$dir/f'+str(i), 'wb') as f: f.write(payload)
" || true
  t1=$(date +%s%N)
  local create_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "create_${FILES}x${PAYLOAD}B" "$create_ms" \
    "$(( FILES * 1000 / (create_ms>0?create_ms:1) )) ops/s"

  sync

  # W2 read
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" python3 -c "
import os
for i in range($FILES):
    p = '$dir/f'+str(i)
    if not os.path.exists(p): continue
    with open(p, 'rb') as f: f.read()
" || true
  t1=$(date +%s%N)
  local read_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "read_${FILES}x${PAYLOAD}B" "$read_ms" \
    "$(( FILES * 1000 / (read_ms>0?read_ms:1) )) ops/s"

  # W3 stat (ls -la)
  t0=$(date +%s%N)
  ls -la "$dir" > /dev/null
  t1=$(date +%s%N)
  local stat_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "stat_ls_${FILES}" "$stat_ms" "-"

  # W3b fsync-heavy on a cold (small) volume.  Runs before seq_write so
  # the on-disk image is still compact — the P3 incremental-save path
  # is most visible here because each checkpoint only serializes
  # metadata + the tiny new block, not a stale 128 MiB DATA segment.
  mkdir -p "$dir/fsync-cold"
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" python3 -c "
import os
payload = b'c'*$PAYLOAD
for i in range($FSYNC_N):
    fd = os.open('$dir/fsync-cold/f'+str(i), os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o644)
    os.write(fd, payload)
    os.fsync(fd)
    os.close(fd)
" || true
  t1=$(date +%s%N)
  local fsync_cold_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "fsync_cold_${FSYNC_N}x${PAYLOAD}B" "$fsync_cold_ms" \
    "$(( FSYNC_N * 1000 / (fsync_cold_ms>0?fsync_cold_ms:1) )) ops/s"
  rm -rf "$dir/fsync-cold"

  # W4 sequential write
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" dd if=/dev/zero of="$dir/seq.bin" \
    bs=1M count=$SEQ_MIB conv=fsync status=none || true
  t1=$(date +%s%N)
  local seqw_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "seq_write_${SEQ_MIB}MiB" "$seqw_ms" \
    "$(( SEQ_MIB * 1000 / (seqw_ms>0?seqw_ms:1) )) MiB/s"

  sync

  # W5 sequential read
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" dd if="$dir/seq.bin" of=/dev/null bs=1M status=none || true
  t1=$(date +%s%N)
  local seqr_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "seq_read_${SEQ_MIB}MiB" "$seqr_ms" \
    "$(( SEQ_MIB * 1000 / (seqr_ms>0?seqr_ms:1) )) MiB/s"

  # W6 fsync-heavy
  mkdir -p "$dir/fsync"
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" python3 -c "
import os
payload = b'y'*$PAYLOAD
for i in range($FSYNC_N):
    fd = os.open('$dir/fsync/f'+str(i), os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o644)
    os.write(fd, payload)
    os.fsync(fd)
    os.close(fd)
" || true
  t1=$(date +%s%N)
  local fsync_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "fsync_${FSYNC_N}x${PAYLOAD}B" "$fsync_ms" \
    "$(( FSYNC_N * 1000 / (fsync_ms>0?fsync_ms:1) )) ops/s"

  # W7 random 4K mixed IO
  timeout "$STEP_TIMEOUT" dd if=/dev/zero of="$dir/rand.bin" \
    bs=1M count=$RAND_MIB status=none || true
  sync
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" python3 -c "
import os, random
fd = os.open('$dir/rand.bin', os.O_RDWR)
size = $RAND_MIB*1024*1024
blk = 4096
buf = b'z'*blk
rnd = random.Random(42)
for _ in range($RAND_OPS):
    off = rnd.randrange(0, size - blk) & ~(blk-1)
    if rnd.random() < 0.7:
        os.pread(fd, blk, off)
    else:
        os.pwrite(fd, buf, off)
os.fsync(fd); os.close(fd)
" || true
  t1=$(date +%s%N)
  local rand_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "rand4k_${RAND_OPS}ops_70r30w" "$rand_ms" \
    "$(( RAND_OPS * 1000 / (rand_ms>0?rand_ms:1) )) ops/s"

  # W8 delete
  t0=$(date +%s%N)
  rm -rf "$dir"/f* "$dir/fsync"
  sync
  t1=$(date +%s%N)
  local rm_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "delete_${FILES}_small" "$rm_ms" "-"

  # W9 append-heavy (log-style): many small appends to the same file.
  # Exercises the fast-path in BlockStore::append_to_inode — pre-P2 this
  # was O(existing_bytes) per call (quadratic over the whole loop), so
  # long-running log writers paid hidden cost scaling with the file
  # length rather than the append size.
  t0=$(date +%s%N)
  timeout "$STEP_TIMEOUT" python3 -c "
import os
fd = os.open('$dir/log.bin', os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o644)
rec = b'x'*1024
for _ in range($APPEND_OPS):
    os.write(fd, rec)
os.fsync(fd); os.close(fd)
" || true
  t1=$(date +%s%N)
  local append_ms=$(( (t1-t0)/1000000 ))
  record "$fs" "append_log_${APPEND_OPS}x1KiB" "$append_ms" \
    "$(( APPEND_OPS * 1000 / (append_ms>0?append_ms:1) )) ops/s"
  rm -f "$dir/log.bin"
}

# ── CoreFS FUSE run ────────────────────────────────────────────────────────
CFS_IMG="$WORK/corefs.img"
CFS_MNT="$WORK/corefs-mnt"
mkdir -p "$CFS_MNT"
cleanup_mount "$CFS_MNT"
rm -f "$CFS_IMG"

echo "[*] CoreFS: mkfs image (${IMG_SIZE_MIB} MiB) at $CFS_IMG"
"$COREFS_BIN" mkfs-image "$CFS_IMG" --bootstrap > "$WORK/corefs-mkfs.log" 2>&1

echo "[*] CoreFS: mount (threads=$THREADS)"
"$COREFS_BIN" mount-image-rw "$CFS_IMG" "$CFS_MNT" --threads "$THREADS" \
  > "$WORK/corefs-mount.log" 2>&1 &
CFS_PID=$!
trap 'cleanup_mount "$CFS_MNT"; kill "$CFS_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 50); do
  mountpoint -q "$CFS_MNT" && break
  sleep 0.2
done
mountpoint -q "$CFS_MNT" || {
  echo "CoreFS mount did not become ready" >&2
  cat "$WORK/corefs-mount.log" >&2
  exit 1
}

run_workloads "corefs-fuse" "$CFS_MNT"

cleanup_mount "$CFS_MNT"
wait "$CFS_PID" 2>/dev/null || true
trap - EXIT

# ── ext4 host-fs reference ────────────────────────────────────────────────
EXT_DIR="$WORK/ext4-direct"
rm -rf "$EXT_DIR"; mkdir -p "$EXT_DIR"
echo "[*] ext4-direct: host fs (same medium as the CoreFS image) at $EXT_DIR"
run_workloads "ext4-direct" "$EXT_DIR"

echo
echo "==== RESULTS ===="
if command -v column >/dev/null 2>&1; then
  column -t -s $'\t' "$RESULTS"
else
  cat "$RESULTS"
fi
