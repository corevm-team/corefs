#!/usr/bin/env bash
# ============================================================================
# run-pjdfstest.sh — pjdfstest (POSIX-Compliance) gegen CoreFS ausfuehren
# ============================================================================
#
# Verwendung:
#   ./run-pjdfstest.sh [Optionen]
#
# Optionen:
#   --image <pfad>      CoreFS-Image (Standard: /tmp/corefs-pjdfs.img)
#   --mount <pfad>      Mountpunkt (Standard: /tmp/mnt/corefs-pjdfs)
#   --groups <liste>    Kommaseparierte Testgruppen (Standard: alle)
#                        Verfuegbar: chmod, chown, link, mkdir, mkfifo, open,
#                        rename, rmdir, symlink, truncate, unlink, chflags,
#                        ftruncate, granular, misc, posix_fallocate, utimensat
#   --keep              Image und Mount nach Testlauf nicht aufraeumen
#   --image-size <mb>   Image-Groesse in MiB (Standard: 256)
#   --help              Diese Hilfe anzeigen
#
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SUITES_DIR="${SCRIPT_DIR}/suites"
PJDFS_DIR="${SUITES_DIR}/pjdfstest"
RESULTS_DIR="${SCRIPT_DIR}/results"

# Defaults
IMAGE_PATH="/tmp/corefs-pjdfs.img"
MOUNT_POINT="/tmp/mnt/corefs-pjdfs"
IMAGE_SIZE_MB=256
KEEP=0
GROUPS=""
MOUNT_PID=""
MOUNTED=0

COLOR_GREEN='\033[0;32m'
COLOR_RED='\033[0;31m'
COLOR_YELLOW='\033[0;33m'
COLOR_BOLD='\033[1m'
COLOR_RESET='\033[0m'

info()  { echo -e "${COLOR_GREEN}[INFO]${COLOR_RESET}  $*"; }
warn()  { echo -e "${COLOR_YELLOW}[WARN]${COLOR_RESET}  $*"; }
error() { echo -e "${COLOR_RED}[ERROR]${COLOR_RESET} $*" >&2; }
header(){ echo -e "\n${COLOR_BOLD}=== $* ===${COLOR_RESET}\n"; }

usage() {
  sed -n '/^# Verwendung:/,/^# ====/{ /^# ====/d; s/^# //; s/^#//; p }' "$0"
  exit 0
}

# --------------------------------------------------------------------------
# Argumente parsen
# --------------------------------------------------------------------------
while (( $# > 0 )); do
  case "$1" in
    --image)      IMAGE_PATH="$2"; shift 2 ;;
    --mount)      MOUNT_POINT="$2"; shift 2 ;;
    --groups)     GROUPS="$2"; shift 2 ;;
    --keep)       KEEP=1; shift ;;
    --image-size) IMAGE_SIZE_MB="$2"; shift 2 ;;
    --help|-h)    usage ;;
    *) error "Unbekannte Option: $1"; usage ;;
  esac
done

# --------------------------------------------------------------------------
# CoreFS-Binary aufloesen
# --------------------------------------------------------------------------
resolve_bin() {
  local corefs_bin="${REPO_ROOT}/dist/bin/corefs"
  local target_bin="${REPO_ROOT}/target/release/corefs"

  if command -v cargo >/dev/null 2>&1; then
    cd "${REPO_ROOT}"
    BIN_CMD=(cargo run --release --)
  elif [[ -x "${corefs_bin}" ]]; then
    BIN_CMD=("${corefs_bin}")
  elif [[ -x "${target_bin}" ]]; then
    BIN_CMD=("${target_bin}")
  else
    error "Kein CoreFS-Binary gefunden. Zuerst ./scripts/build.sh ausfuehren."
    exit 1
  fi
}

# --------------------------------------------------------------------------
# Mount-Helfer
# --------------------------------------------------------------------------
is_mounted() {
  if command -v mountpoint >/dev/null 2>&1; then
    mountpoint -q "${MOUNT_POINT}" 2>/dev/null
  else
    grep -qs " ${MOUNT_POINT} " /proc/mounts
  fi
}

umount_corefs() {
  if ! is_mounted; then return 0; fi
  sync
  if command -v fusermount3 >/dev/null 2>&1; then
    fusermount3 -u "${MOUNT_POINT}"
  elif command -v fusermount >/dev/null 2>&1; then
    fusermount -u "${MOUNT_POINT}"
  else
    umount "${MOUNT_POINT}"
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
  error "CoreFS-Mount wurde nicht bereit: ${MOUNT_POINT}"
  return 1
}

cleanup() {
  set +e
  if (( MOUNTED )); then
    umount_corefs
  fi
  if [[ -n "${MOUNT_PID}" ]]; then
    wait "${MOUNT_PID}" >/dev/null 2>&1
  fi
  if (( ! KEEP )); then
    rm -f "${IMAGE_PATH}"
    rmdir "${MOUNT_POINT}" 2>/dev/null || true
  fi
}

# --------------------------------------------------------------------------
# Pruefungen
# --------------------------------------------------------------------------
preflight() {
  if [[ ! -x "${PJDFS_DIR}/pjdfstest" ]]; then
    error "pjdfstest nicht gefunden. Zuerst ausfuehren:"
    echo "  ./scripts/testing/install-test-suites.sh pjdfstest"
    exit 1
  fi

  if [[ "$(uname -s)" != "Linux" ]]; then
    error "Dieses Skript erfordert Linux."
    exit 1
  fi

  if ! command -v prove >/dev/null 2>&1; then
    warn "'prove' (Perl TAP-Harness) nicht gefunden — Ergebnisse werden direkt ausgegeben."
    warn "Installieren mit: sudo apt install perl  /  sudo dnf install perl-Test-Harness"
  fi
}

# --------------------------------------------------------------------------
# Tests ausfuehren
# --------------------------------------------------------------------------
run_tests() {
  local result_file="${RESULTS_DIR}/pjdfstest-$(date +%Y%m%d-%H%M%S).log"
  mkdir -p "${RESULTS_DIR}"

  header "pjdfstest gegen CoreFS"
  info "Image:       ${IMAGE_PATH}"
  info "Mount:       ${MOUNT_POINT}"
  info "Ergebnisse:  ${result_file}"
  info "Gruppen:     ${GROUPS:-alle}"

  # Image erstellen
  info "Erstelle CoreFS-Image ..."
  rm -f "${IMAGE_PATH}"
  "${BIN_CMD[@]}" mkfs-image "${IMAGE_PATH}" --demo

  # RW mounten
  mkdir -p "${MOUNT_POINT}"
  info "Mounte CoreFS (read-write) ..."
  "${BIN_CMD[@]}" mount-image-rw "${IMAGE_PATH}" "${MOUNT_POINT}" &
  MOUNT_PID=$!
  wait_for_mount
  info "CoreFS gemountet."

  # In den Mountpunkt wechseln und pjdfstest ausfuehren
  cd "${MOUNT_POINT}"

  # Testverzeichnis anlegen
  mkdir -p "__pjdfstest_work"
  cd "__pjdfstest_work"

  local test_dirs=()
  if [[ -n "${GROUPS}" ]]; then
    IFS=',' read -ra test_dirs <<< "${GROUPS}"
  else
    # Alle Testgruppen finden
    for d in "${PJDFS_DIR}/tests/"*/; do
      [[ -d "$d" ]] && test_dirs+=("$(basename "$d")")
    done
  fi

  local total_pass=0
  local total_fail=0
  local total_skip=0

  {
    echo "================================================================="
    echo "pjdfstest Ergebnisse — CoreFS"
    echo "Datum:  $(date -Iseconds)"
    echo "Image:  ${IMAGE_PATH}"
    echo "Mount:  ${MOUNT_POINT}"
    echo "================================================================="
    echo ""
  } | tee "${result_file}"

  for group in "${test_dirs[@]}"; do
    local test_path="${PJDFS_DIR}/tests/${group}"
    if [[ ! -d "${test_path}" ]]; then
      warn "Testgruppe '${group}' nicht gefunden — ueberspringe."
      continue
    fi

    echo "" | tee -a "${result_file}"
    header "${group}" | tee -a "${result_file}"

    local group_pass=0
    local group_fail=0
    local group_skip=0

    for t in "${test_path}"/*.t; do
      [[ -f "$t" ]] || continue
      local tname
      tname="$(basename "$t")"

      # prove fuer TAP-Ausgabe nutzen, falls vorhanden
      local output
      if command -v prove >/dev/null 2>&1; then
        output=$(prove -e "${PJDFS_DIR}/pjdfstest" "$t" 2>&1) || true
      else
        output=$("${PJDFS_DIR}/pjdfstest" "$t" 2>&1) || true
      fi

      # Zaehlen
      local pass fail skip
      pass=$(echo "$output" | grep -c "^ok" || true)
      fail=$(echo "$output" | grep -c "^not ok" || true)
      skip=$(echo "$output" | grep -ci "skip" || true)

      group_pass=$((group_pass + pass))
      group_fail=$((group_fail + fail))
      group_skip=$((group_skip + skip))

      if (( fail > 0 )); then
        echo -e "  ${COLOR_RED}FAIL${COLOR_RESET}  ${group}/${tname}  (pass=${pass} fail=${fail} skip=${skip})" | tee -a "${result_file}"
        echo "$output" >> "${result_file}"
      else
        echo -e "  ${COLOR_GREEN}OK${COLOR_RESET}    ${group}/${tname}  (pass=${pass} skip=${skip})" | tee -a "${result_file}"
      fi
    done

    total_pass=$((total_pass + group_pass))
    total_fail=$((total_fail + group_fail))
    total_skip=$((total_skip + group_skip))

    echo "  --- ${group}: pass=${group_pass} fail=${group_fail} skip=${group_skip}" | tee -a "${result_file}"
  done

  echo "" | tee -a "${result_file}"
  header "Zusammenfassung" | tee -a "${result_file}"
  echo "  Bestanden:    ${total_pass}" | tee -a "${result_file}"
  echo "  Fehlgeschl.:  ${total_fail}" | tee -a "${result_file}"
  echo "  Uebersprungen: ${total_skip}" | tee -a "${result_file}"
  echo "" | tee -a "${result_file}"

  if (( total_fail > 0 )); then
    error "${total_fail} Tests fehlgeschlagen. Details: ${result_file}"
  else
    info "Alle ausgefuehrten Tests bestanden."
  fi

  info "Vollstaendiges Protokoll: ${result_file}"

  # Aufraumen im Mountpunkt
  cd /
  rm -rf "${MOUNT_POINT}/__pjdfstest_work" 2>/dev/null || true
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
resolve_bin
preflight
trap cleanup EXIT
run_tests
