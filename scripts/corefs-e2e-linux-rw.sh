#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COREFS_BIN="${REPO_ROOT}/dist/bin/corefs"
TARGET_BIN="${REPO_ROOT}/target/release/corefs"

IMAGE_PATH="${1:-${SCRIPT_DIR}/corefs-e2e.img}"
MOUNT_POINT="${2:-${SCRIPT_DIR}/mnt/corefs-e2e}"
ZIP_PATH="${3:-}"

LOG_PATH="${SCRIPT_DIR}/corefs-e2e.log"
SOURCE_PAYLOAD="${SCRIPT_DIR}/corefs-e2e-source.bin"
TEXT_PAYLOAD="corefs e2e smoke payload"
MOUNT_PID=""
MOUNTED=0

require_linux() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "This script only supports Linux." >&2
    exit 1
  fi
}

resolve_bin() {
  if command -v cargo >/dev/null 2>&1; then
    cd "${REPO_ROOT}"
    BIN_CMD=(cargo run --release --)
    return
  fi
  if [[ -x "${COREFS_BIN}" ]]; then
    BIN_CMD=("${COREFS_BIN}")
    return
  fi
  if [[ -x "${TARGET_BIN}" ]]; then
    BIN_CMD=("${TARGET_BIN}")
    return
  fi
  echo "No CoreFS binary available. Run ./scripts/build.sh first or install cargo." >&2
  exit 1
}

is_mounted() {
  if command -v mountpoint >/dev/null 2>&1; then
    mountpoint -q "${MOUNT_POINT}"
  else
    grep -qs " ${MOUNT_POINT} " /proc/mounts
  fi
}

umount_corefs() {
  if ! is_mounted; then
    return 0
  fi
  if command -v fusermount3 >/dev/null 2>&1; then
    fusermount3 -u "${MOUNT_POINT}"
  elif command -v fusermount >/dev/null 2>&1; then
    fusermount -u "${MOUNT_POINT}"
  else
    umount "${MOUNT_POINT}"
  fi
}

cleanup() {
  set +e
  if [[ ${MOUNTED} -eq 1 ]]; then
    umount_corefs
  fi
  if [[ -n "${MOUNT_PID}" ]]; then
    wait "${MOUNT_PID}" >/dev/null 2>&1
  fi
}

wait_for_mount() {
  local attempts=50
  while (( attempts > 0 )); do
    if is_mounted; then
      MOUNTED=1
      return 0
    fi
    sleep 0.2
    attempts=$((attempts - 1))
  done
  echo "CoreFS mount did not become ready: ${MOUNT_POINT}" >&2
  return 1
}

assert_file_contains() {
  local path="$1"
  local needle="$2"
  if ! grep -q --fixed-strings "${needle}" "${path}"; then
    echo "Expected ${path} to contain: ${needle}" >&2
    exit 1
  fi
}

require_linux
resolve_bin
trap cleanup EXIT

mkdir -p "$(dirname "${MOUNT_POINT}")"
rm -f "${LOG_PATH}" "${SOURCE_PAYLOAD}"
rm -f "${IMAGE_PATH}"
mkdir -p "${MOUNT_POINT}"

echo "Creating demo image: ${IMAGE_PATH}"
"${BIN_CMD[@]}" mkfs-image "${IMAGE_PATH}" --demo

echo "Preparing source payload"
#head -c 26214400 /dev/urandom > "${SOURCE_PAYLOAD}"
head -c 262144000 /dev/urandom > "${SOURCE_PAYLOAD}"

echo "Starting RW mount: ${MOUNT_POINT}"
"${BIN_CMD[@]}" mount-image-rw "${IMAGE_PATH}" "${MOUNT_POINT}" >"${LOG_PATH}" 2>&1 &
MOUNT_PID=$!

wait_for_mount

echo "Running read/write checks"
assert_file_contains "${MOUNT_POINT}/etc/corefs.conf" "volume=corefs"
mkdir -p "${MOUNT_POINT}/data"
printf '%s\n' "${TEXT_PAYLOAD}" > "${MOUNT_POINT}/data/message.txt"
cp "${SOURCE_PAYLOAD}" "${MOUNT_POINT}/data/payload.bin"
cmp "${SOURCE_PAYLOAD}" "${MOUNT_POINT}/data/payload.bin"

if [[ -n "${ZIP_PATH}" ]]; then
  echo "Running unzip workload: ${ZIP_PATH}"
  mkdir -p "${MOUNT_POINT}/archive"
  unzip -oq "${ZIP_PATH}" -d "${MOUNT_POINT}/archive"
fi

sync

echo "Unmounting CoreFS"
umount_corefs
MOUNTED=0
wait "${MOUNT_PID}"
MOUNT_PID=""

echo "Verifying persisted image"
"${BIN_CMD[@]}" fsck-image "${IMAGE_PATH}"

echo "Remounting read-only for persistence verification"
"${BIN_CMD[@]}" mount-image "${IMAGE_PATH}" "${MOUNT_POINT}" >>"${LOG_PATH}" 2>&1 &
MOUNT_PID=$!
wait_for_mount
assert_file_contains "${MOUNT_POINT}/etc/corefs.conf" "volume=corefs"
assert_file_contains "${MOUNT_POINT}/data/message.txt" "${TEXT_PAYLOAD}"
cmp "${SOURCE_PAYLOAD}" "${MOUNT_POINT}/data/payload.bin"
umount_corefs
MOUNTED=0
wait "${MOUNT_PID}"
MOUNT_PID=""

echo "CoreFS Linux RW end-to-end test passed."
