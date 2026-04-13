#!/usr/bin/env bash
# ============================================================================
# run-stress.sh — Intensiver Stresstest fuer CoreFS
# ============================================================================
#
# Fuehrt parallele Dateisystem-Operationen gegen ein gemountetes CoreFS aus,
# um Stabilitaet unter Last zu pruefen. Ergaenzt pjdfstest und xfstests um
# einen praxisnahen Lasttest.
#
# Verwendung:
#   ./run-stress.sh [Optionen]
#
# Optionen:
#   --image <pfad>      CoreFS-Image (Standard: /tmp/corefs-stress.img)
#   --mount <pfad>      Mountpunkt (Standard: /tmp/mnt/corefs-stress)
#   --workers <n>       Parallele Worker (Standard: 4)
#   --duration <sek>    Testdauer in Sekunden (Standard: 120)
#   --file-count <n>    Dateien pro Worker (Standard: 200)
#   --max-size <bytes>  Maximale Dateigroesse (Standard: 1048576 = 1 MiB)
#   --keep              Image nach Testlauf behalten
#   --help              Diese Hilfe anzeigen
#
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
RESULTS_DIR="${SCRIPT_DIR}/results"

# Defaults
IMAGE_PATH="/tmp/corefs-stress.img"
MOUNT_POINT="/tmp/mnt/corefs-stress"
WORKERS=4
DURATION=120
FILE_COUNT=200
MAX_SIZE=1048576
KEEP=0
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
# Argumente
# --------------------------------------------------------------------------
while (( $# > 0 )); do
  case "$1" in
    --image)      IMAGE_PATH="$2"; shift 2 ;;
    --mount)      MOUNT_POINT="$2"; shift 2 ;;
    --workers)    WORKERS="$2"; shift 2 ;;
    --duration)   DURATION="$2"; shift 2 ;;
    --file-count) FILE_COUNT="$2"; shift 2 ;;
    --max-size)   MAX_SIZE="$2"; shift 2 ;;
    --keep)       KEEP=1; shift ;;
    --help|-h)    usage ;;
    *) error "Unbekannte Option: $1"; usage ;;
  esac
done

# --------------------------------------------------------------------------
# CoreFS-Binary
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
    error "Kein CoreFS-Binary gefunden."
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
    if is_mounted; then MOUNTED=1; return 0; fi
    sleep 0.2
    attempts=$((attempts - 1))
  done
  error "Mount nicht bereit: ${MOUNT_POINT}"
  return 1
}

cleanup() {
  set +e
  # Stop-Signal fuer Worker
  touch "${MOUNT_POINT}/.stress_stop" 2>/dev/null || true
  wait 2>/dev/null || true
  if (( MOUNTED )); then umount_corefs; fi
  [[ -n "${MOUNT_PID}" ]] && wait "${MOUNT_PID}" 2>/dev/null
  if (( ! KEEP )); then
    rm -f "${IMAGE_PATH}"
    rmdir "${MOUNT_POINT}" 2>/dev/null || true
  fi
}

# --------------------------------------------------------------------------
# Worker-Funktionen
# --------------------------------------------------------------------------

# Worker: Dateien erzeugen, lesen, ueberschreiben, loeschen
worker_file_ops() {
  local id=$1
  local base="${MOUNT_POINT}/stress/worker-${id}"
  local ops=0
  local errors=0

  mkdir -p "${base}"

  local end_time=$(( $(date +%s) + DURATION ))

  while [[ $(date +%s) -lt ${end_time} ]] && [[ ! -f "${MOUNT_POINT}/.stress_stop" ]]; do
    for (( i=0; i < FILE_COUNT; i++ )); do
      [[ $(date +%s) -ge ${end_time} ]] && break

      local fname="${base}/file-${i}.dat"
      local size=$(( RANDOM % MAX_SIZE + 1 ))

      # Schreiben
      if head -c "${size}" /dev/urandom > "${fname}" 2>/dev/null; then
        ops=$((ops + 1))
      else
        errors=$((errors + 1))
      fi

      # Lesen und verwerfen
      if cat "${fname}" > /dev/null 2>/dev/null; then
        ops=$((ops + 1))
      else
        errors=$((errors + 1))
      fi

      # Ueberschreiben (50% Chance)
      if (( RANDOM % 2 == 0 )); then
        local new_size=$(( RANDOM % MAX_SIZE + 1 ))
        if head -c "${new_size}" /dev/urandom > "${fname}" 2>/dev/null; then
          ops=$((ops + 1))
        else
          errors=$((errors + 1))
        fi
      fi
    done

    # Batch loeschen
    for (( i=0; i < FILE_COUNT; i++ )); do
      [[ $(date +%s) -ge ${end_time} ]] && break
      local fname="${base}/file-${i}.dat"
      if [[ -f "${fname}" ]]; then
        rm -f "${fname}" 2>/dev/null && ops=$((ops + 1)) || errors=$((errors + 1))
      fi
    done
  done

  echo "worker-${id}: ops=${ops} errors=${errors}"
}

# Worker: Verzeichnisse erzeugen und loeschen
worker_dir_ops() {
  local id=$1
  local base="${MOUNT_POINT}/stress/dirs-${id}"
  local ops=0
  local errors=0

  local end_time=$(( $(date +%s) + DURATION ))

  while [[ $(date +%s) -lt ${end_time} ]] && [[ ! -f "${MOUNT_POINT}/.stress_stop" ]]; do
    # Tiefe Verzeichnisstruktur erzeugen
    local depth=$(( RANDOM % 8 + 2 ))
    local path="${base}"
    for (( d=0; d < depth; d++ )); do
      path="${path}/d${d}-${RANDOM}"
    done

    if mkdir -p "${path}" 2>/dev/null; then
      ops=$((ops + 1))
      # Datei im tiefsten Verzeichnis
      echo "test-${RANDOM}" > "${path}/leaf.txt" 2>/dev/null && ops=$((ops + 1))
    else
      errors=$((errors + 1))
    fi

    # Gelegentlich alles loeschen und neu starten
    if (( RANDOM % 5 == 0 )); then
      rm -rf "${base}" 2>/dev/null && ops=$((ops + 1)) || errors=$((errors + 1))
    fi
  done

  echo "dirs-${id}: ops=${ops} errors=${errors}"
}

# Worker: Symlinks und Renames
worker_link_rename() {
  local id=$1
  local base="${MOUNT_POINT}/stress/links-${id}"
  local ops=0
  local errors=0

  mkdir -p "${base}"

  local end_time=$(( $(date +%s) + DURATION ))

  while [[ $(date +%s) -lt ${end_time} ]] && [[ ! -f "${MOUNT_POINT}/.stress_stop" ]]; do
    local src="${base}/src-${RANDOM}.txt"
    local dst="${base}/dst-${RANDOM}.txt"
    local lnk="${base}/lnk-${RANDOM}.txt"

    echo "data-${RANDOM}" > "${src}" 2>/dev/null || { errors=$((errors+1)); continue; }
    ops=$((ops + 1))

    # Rename
    if mv "${src}" "${dst}" 2>/dev/null; then
      ops=$((ops + 1))
    else
      errors=$((errors + 1))
    fi

    # Symlink
    if [[ -f "${dst}" ]]; then
      if ln -s "${dst}" "${lnk}" 2>/dev/null; then
        ops=$((ops + 1))
        # Symlink lesen
        cat "${lnk}" > /dev/null 2>/dev/null && ops=$((ops + 1))
        rm -f "${lnk}" 2>/dev/null
      fi
      rm -f "${dst}" 2>/dev/null
    fi
  done

  echo "links-${id}: ops=${ops} errors=${errors}"
}

# Worker: Concurrent append (mehrere Prozesse schreiben in die gleiche Datei)
worker_concurrent_append() {
  local id=$1
  local target="${MOUNT_POINT}/stress/shared-append.log"
  local ops=0

  local end_time=$(( $(date +%s) + DURATION ))

  while [[ $(date +%s) -lt ${end_time} ]] && [[ ! -f "${MOUNT_POINT}/.stress_stop" ]]; do
    echo "worker-${id}: $(date +%s.%N) line-${RANDOM}" >> "${target}" 2>/dev/null \
      && ops=$((ops + 1))
  done

  echo "append-${id}: ops=${ops}"
}

# --------------------------------------------------------------------------
# Tests ausfuehren
# --------------------------------------------------------------------------
run_stress() {
  local result_file="${RESULTS_DIR}/stress-$(date +%Y%m%d-%H%M%S).log"
  mkdir -p "${RESULTS_DIR}"

  header "CoreFS Stresstest"
  info "Image:     ${IMAGE_PATH}"
  info "Mount:     ${MOUNT_POINT}"
  info "Workers:   ${WORKERS}"
  info "Dauer:     ${DURATION}s"
  info "Dateien:   ${FILE_COUNT} pro Worker"
  info "Max-Size:  ${MAX_SIZE} Bytes"
  info "Ergebnis:  ${result_file}"

  # Image erstellen und mounten
  info "Erstelle CoreFS-Image ..."
  rm -f "${IMAGE_PATH}"
  "${BIN_CMD[@]}" mkfs-image "${IMAGE_PATH}" --demo

  mkdir -p "${MOUNT_POINT}"
  info "Mounte CoreFS (read-write) ..."
  "${BIN_CMD[@]}" mount-image-rw "${IMAGE_PATH}" "${MOUNT_POINT}" &
  MOUNT_PID=$!
  wait_for_mount
  info "CoreFS gemountet."

  mkdir -p "${MOUNT_POINT}/stress"

  {
    echo "================================================================="
    echo "CoreFS Stresstest"
    echo "Datum:      $(date -Iseconds)"
    echo "Workers:    ${WORKERS}"
    echo "Dauer:      ${DURATION}s"
    echo "Dateien:    ${FILE_COUNT} pro Worker"
    echo "Max-Size:   ${MAX_SIZE} Bytes"
    echo "================================================================="
    echo ""
  } | tee "${result_file}"

  local pids=()
  local start_time=$(date +%s)

  # File-Ops-Worker starten
  for (( w=0; w < WORKERS; w++ )); do
    worker_file_ops "$w" >> "${result_file}" 2>&1 &
    pids+=($!)
  done

  # Dir-Ops-Worker (halb so viele)
  local dir_workers=$(( WORKERS / 2 ))
  (( dir_workers < 1 )) && dir_workers=1
  for (( w=0; w < dir_workers; w++ )); do
    worker_dir_ops "$w" >> "${result_file}" 2>&1 &
    pids+=($!)
  done

  # Link/Rename-Worker
  for (( w=0; w < dir_workers; w++ )); do
    worker_link_rename "$w" >> "${result_file}" 2>&1 &
    pids+=($!)
  done

  # Concurrent-Append-Worker
  for (( w=0; w < WORKERS; w++ )); do
    worker_concurrent_append "$w" >> "${result_file}" 2>&1 &
    pids+=($!)
  done

  info "Gestartet: ${#pids[@]} Worker-Prozesse"
  info "Warte auf Abschluss (max ${DURATION}s) ..."

  # Auf alle Worker warten
  local all_ok=1
  for pid in "${pids[@]}"; do
    if ! wait "$pid" 2>/dev/null; then
      all_ok=0
    fi
  done

  local end_time=$(date +%s)
  local elapsed=$(( end_time - start_time ))

  echo "" | tee -a "${result_file}"
  header "Ergebnis" | tee -a "${result_file}"
  echo "  Laufzeit: ${elapsed}s" | tee -a "${result_file}"

  # Integritaetspruefung
  info "Pruefe Dateisystem-Integritaet ..."
  sync

  # Unmounten
  info "Unmounte CoreFS ..."
  umount_corefs
  MOUNTED=0
  wait "${MOUNT_PID}" 2>/dev/null || true
  MOUNT_PID=""

  # fsck
  info "Fuehre fsck aus ..."
  if "${BIN_CMD[@]}" fsck-image "${IMAGE_PATH}" 2>&1 | tee -a "${result_file}"; then
    echo -e "  ${COLOR_GREEN}fsck: BESTANDEN${COLOR_RESET}" | tee -a "${result_file}"
  else
    echo -e "  ${COLOR_RED}fsck: FEHLGESCHLAGEN${COLOR_RESET}" | tee -a "${result_file}"
    all_ok=0
  fi

  echo "" | tee -a "${result_file}"
  if (( all_ok )); then
    info "Stresstest bestanden."
  else
    error "Stresstest mit Fehlern beendet. Details: ${result_file}"
  fi
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
resolve_bin
trap cleanup EXIT
run_stress
