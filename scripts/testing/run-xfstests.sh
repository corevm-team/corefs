#!/usr/bin/env bash
# ============================================================================
# run-xfstests.sh — xfstests-Suite gegen CoreFS ausfuehren
# ============================================================================
#
# Verwendung:
#   ./run-xfstests.sh [Optionen]
#
# Optionen:
#   --test-image <pfad>     CoreFS-Image fuer TEST_DEV (Standard: /tmp/corefs-xfs-test.img)
#   --scratch-image <pfad>  CoreFS-Image fuer SCRATCH_DEV (Standard: /tmp/corefs-xfs-scratch.img)
#   --test-mount <pfad>     Mountpunkt TEST_DIR (Standard: /tmp/mnt/corefs-xfs-test)
#   --scratch-mount <pfad>  Mountpunkt SCRATCH_MNT (Standard: /tmp/mnt/corefs-xfs-scratch)
#   --groups <liste>        xfstests-Gruppen, kommasepariert (Standard: generic/quick)
#                            Beispiele: generic/quick, generic/posix, generic/auto,
#                            generic/perms, generic/attr, generic/rw
#   --tests <liste>         Einzelne Tests, kommasepariert (z.B. generic/001,generic/002)
#   --exclude <liste>       Tests ausschliessen, kommasepariert
#   --keep                  Images nach Testlauf behalten
#   --image-size <mb>       Image-Groesse in MiB (Standard: 512)
#   --help                  Diese Hilfe anzeigen
#
# Hinweis:
#   xfstests erwartet FSTYP, TEST_DEV, TEST_DIR, SCRATCH_DEV, SCRATCH_MNT.
#   Da CoreFS ein FUSE-Dateisystem ist, wird FSTYP=fuse.corefs gesetzt.
#   Einige Tests erfordern Root-Rechte (z.B. mount/umount).
#
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SUITES_DIR="${SCRIPT_DIR}/suites"
XFSTESTS_DIR="${SUITES_DIR}/xfstests"
RESULTS_DIR="${SCRIPT_DIR}/results"

# Defaults
TEST_IMAGE="/tmp/corefs-xfs-test.img"
SCRATCH_IMAGE="/tmp/corefs-xfs-scratch.img"
TEST_MOUNT="/tmp/mnt/corefs-xfs-test"
SCRATCH_MOUNT="/tmp/mnt/corefs-xfs-scratch"
IMAGE_SIZE_MB=512
KEEP=0
GROUPS="generic/quick"
TESTS=""
EXCLUDE=""

# Mount-PIDs
TEST_MOUNT_PID=""
SCRATCH_MOUNT_PID=""
TEST_MOUNTED=0
SCRATCH_MOUNTED=0

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
    --test-image)     TEST_IMAGE="$2"; shift 2 ;;
    --scratch-image)  SCRATCH_IMAGE="$2"; shift 2 ;;
    --test-mount)     TEST_MOUNT="$2"; shift 2 ;;
    --scratch-mount)  SCRATCH_MOUNT="$2"; shift 2 ;;
    --groups)         GROUPS="$2"; shift 2 ;;
    --tests)          TESTS="$2"; shift 2 ;;
    --exclude)        EXCLUDE="$2"; shift 2 ;;
    --keep)           KEEP=1; shift ;;
    --image-size)     IMAGE_SIZE_MB="$2"; shift 2 ;;
    --help|-h)        usage ;;
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
    error "Kein CoreFS-Binary gefunden. Zuerst ./scripts/build.sh ausfuehren."
    exit 1
  fi
}

# --------------------------------------------------------------------------
# Mount-Helfer
# --------------------------------------------------------------------------
is_mounted_at() {
  local mp="$1"
  if command -v mountpoint >/dev/null 2>&1; then
    mountpoint -q "${mp}" 2>/dev/null
  else
    grep -qs " ${mp} " /proc/mounts
  fi
}

umount_at() {
  local mp="$1"
  if ! is_mounted_at "$mp"; then return 0; fi
  sync
  if command -v fusermount3 >/dev/null 2>&1; then
    fusermount3 -u "$mp"
  elif command -v fusermount >/dev/null 2>&1; then
    fusermount -u "$mp"
  else
    umount "$mp"
  fi
}

wait_for_mount_at() {
  local mp="$1"
  local attempts=50
  while (( attempts > 0 )); do
    if is_mounted_at "$mp"; then return 0; fi
    sleep 0.2
    attempts=$((attempts - 1))
  done
  error "Mount wurde nicht bereit: ${mp}"
  return 1
}

cleanup() {
  set +e
  if (( TEST_MOUNTED )); then
    umount_at "${TEST_MOUNT}"
  fi
  if (( SCRATCH_MOUNTED )); then
    umount_at "${SCRATCH_MOUNT}"
  fi
  [[ -n "${TEST_MOUNT_PID}" ]] && wait "${TEST_MOUNT_PID}" 2>/dev/null
  [[ -n "${SCRATCH_MOUNT_PID}" ]] && wait "${SCRATCH_MOUNT_PID}" 2>/dev/null
  if (( ! KEEP )); then
    rm -f "${TEST_IMAGE}" "${SCRATCH_IMAGE}"
    rmdir "${TEST_MOUNT}" "${SCRATCH_MOUNT}" 2>/dev/null || true
  fi
}

# --------------------------------------------------------------------------
# Mount- und Unmount-Wrapper fuer xfstests
#
# xfstests erwartet mount/umount-Befehle. Wir schreiben kleine Wrapper,
# die xfstests statt der echten mount/umount nutzt.
# --------------------------------------------------------------------------
create_mount_helpers() {
  local helpers_dir="${SCRIPT_DIR}/.xfstests-helpers"
  mkdir -p "${helpers_dir}"

  # mount.fuse.corefs — wird von xfstests aufgerufen wenn FSTYP=fuse.corefs
  cat > "${helpers_dir}/mount.fuse.corefs" << 'MOUNT_EOF'
#!/usr/bin/env bash
# Wrapper: mount.fuse.corefs <device/image> <mountpoint> [-o options]
set -euo pipefail
IMAGE="$1"
MOUNTPOINT="$2"
shift 2
# Optionen ignorieren wir vorerst
COREFS_BIN="@@COREFS_BIN@@"
exec ${COREFS_BIN} mount-image-rw "${IMAGE}" "${MOUNTPOINT}"
MOUNT_EOF

  # Platzhalter ersetzen
  if [[ "${BIN_CMD[0]}" == "cargo" ]]; then
    local actual_bin="${REPO_ROOT}/target/release/corefs"
    # Sicherstellen, dass Binary existiert
    if [[ ! -x "${actual_bin}" ]]; then
      info "Baue Release-Binary fuer Mount-Helper ..."
      cd "${REPO_ROOT}" && cargo build --release
    fi
    sed -i "s|@@COREFS_BIN@@|${actual_bin}|g" "${helpers_dir}/mount.fuse.corefs"
  else
    sed -i "s|@@COREFS_BIN@@|${BIN_CMD[*]}|g" "${helpers_dir}/mount.fuse.corefs"
  fi

  chmod +x "${helpers_dir}/mount.fuse.corefs"

  HELPERS_DIR="${helpers_dir}"
}

# --------------------------------------------------------------------------
# xfstests local.config erzeugen
# --------------------------------------------------------------------------
create_xfstests_config() {
  local config_file="${XFSTESTS_DIR}/local.config"

  cat > "${config_file}" << EOF
# Automatisch generierte xfstests-Konfiguration fuer CoreFS
# Erstellt: $(date -Iseconds)

export FSTYP=fuse.corefs
export TEST_DEV=${TEST_IMAGE}
export TEST_DIR=${TEST_MOUNT}
export SCRATCH_DEV=${SCRATCH_IMAGE}
export SCRATCH_MNT=${SCRATCH_MOUNT}

# CoreFS-spezifische Mount-Optionen
export MOUNT_OPTIONS=""
export MKFS_OPTIONS=""

# Mount/Unmount Helfer
export FUSE_MOUNT_PROG="${HELPERS_DIR}/mount.fuse.corefs"
EOF

  info "xfstests-Konfiguration: ${config_file}"
}

# --------------------------------------------------------------------------
# CoreFS mkfs-Wrapper fuer xfstests
# --------------------------------------------------------------------------
create_mkfs_helper() {
  local helpers_dir="${HELPERS_DIR}"

  cat > "${helpers_dir}/mkfs.fuse.corefs" << 'MKFS_EOF'
#!/usr/bin/env bash
# Wrapper: mkfs.fuse.corefs <image-path>
set -euo pipefail
IMAGE="${!#}"  # letztes Argument
COREFS_BIN="@@COREFS_BIN@@"
rm -f "${IMAGE}"
exec ${COREFS_BIN} mkfs-image "${IMAGE}"
MKFS_EOF

  if [[ "${BIN_CMD[0]}" == "cargo" ]]; then
    local actual_bin="${REPO_ROOT}/target/release/corefs"
    sed -i "s|@@COREFS_BIN@@|${actual_bin}|g" "${helpers_dir}/mkfs.fuse.corefs"
  else
    sed -i "s|@@COREFS_BIN@@|${BIN_CMD[*]}|g" "${helpers_dir}/mkfs.fuse.corefs"
  fi

  chmod +x "${helpers_dir}/mkfs.fuse.corefs"
}

# --------------------------------------------------------------------------
# Pruefungen
# --------------------------------------------------------------------------
preflight() {
  if [[ ! -x "${XFSTESTS_DIR}/check" ]]; then
    error "xfstests nicht gefunden. Zuerst ausfuehren:"
    echo "  ./scripts/testing/install-test-suites.sh xfstests"
    exit 1
  fi

  if [[ "$(uname -s)" != "Linux" ]]; then
    error "Dieses Skript erfordert Linux."
    exit 1
  fi

  # Viele xfstests erfordern Root
  if [[ "$(id -u)" -ne 0 ]]; then
    warn "xfstests laeuft ohne Root-Rechte — einige Tests werden fehlschlagen."
    warn "Fuer vollstaendige Tests: sudo $0 $*"
  fi
}

# --------------------------------------------------------------------------
# Tests ausfuehren
# --------------------------------------------------------------------------
run_tests() {
  local result_file="${RESULTS_DIR}/xfstests-$(date +%Y%m%d-%H%M%S).log"
  mkdir -p "${RESULTS_DIR}"

  header "xfstests gegen CoreFS"
  info "TEST_DEV:     ${TEST_IMAGE}"
  info "TEST_DIR:     ${TEST_MOUNT}"
  info "SCRATCH_DEV:  ${SCRATCH_IMAGE}"
  info "SCRATCH_MNT:  ${SCRATCH_MOUNT}"
  info "Gruppen:      ${GROUPS}"
  info "Ergebnisse:   ${result_file}"

  # Images erstellen
  info "Erstelle CoreFS-Images ..."
  rm -f "${TEST_IMAGE}" "${SCRATCH_IMAGE}"
  "${BIN_CMD[@]}" mkfs-image "${TEST_IMAGE}"
  "${BIN_CMD[@]}" mkfs-image "${SCRATCH_IMAGE}"

  # Mount-Punkte vorbereiten
  mkdir -p "${TEST_MOUNT}" "${SCRATCH_MOUNT}"

  # Test-Mount starten (xfstests erwartet TEST_DIR bereits gemountet)
  info "Mounte TEST_DIR ..."
  "${BIN_CMD[@]}" mount-image-rw "${TEST_IMAGE}" "${TEST_MOUNT}" &
  TEST_MOUNT_PID=$!
  wait_for_mount_at "${TEST_MOUNT}"
  TEST_MOUNTED=1
  info "TEST_DIR gemountet."

  # Helper-Skripte und Konfiguration erzeugen
  create_mount_helpers
  create_mkfs_helper
  create_xfstests_config

  # xfstests ausfuehren
  cd "${XFSTESTS_DIR}"

  local check_args=()

  # Gruppen
  if [[ -n "${GROUPS}" ]]; then
    IFS=',' read -ra group_arr <<< "${GROUPS}"
    for g in "${group_arr[@]}"; do
      check_args+=(-g "$g")
    done
  fi

  # Einzelne Tests
  if [[ -n "${TESTS}" ]]; then
    IFS=',' read -ra test_arr <<< "${TESTS}"
    check_args+=("${test_arr[@]}")
  fi

  # Ausschluesse
  if [[ -n "${EXCLUDE}" ]]; then
    IFS=',' read -ra excl_arr <<< "${EXCLUDE}"
    for e in "${excl_arr[@]}"; do
      check_args+=(-x "$e")
    done
  fi

  {
    echo "================================================================="
    echo "xfstests Ergebnisse — CoreFS"
    echo "Datum:  $(date -Iseconds)"
    echo "Gruppen: ${GROUPS}"
    echo "================================================================="
    echo ""
  } > "${result_file}"

  info "Starte xfstests: ./check ${check_args[*]:-}"
  echo ""

  # check ausfuehren — Fehler sind erwartet (nicht jeder Test wird bestehen)
  ./check "${check_args[@]}" 2>&1 | tee -a "${result_file}" || true

  echo "" | tee -a "${result_file}"
  info "Vollstaendiges Protokoll: ${result_file}"

  # xfstests-eigene Ergebnisse kopieren
  if [[ -d "${XFSTESTS_DIR}/results" ]]; then
    cp -r "${XFSTESTS_DIR}/results" "${RESULTS_DIR}/xfstests-raw-$(date +%Y%m%d-%H%M%S)" 2>/dev/null || true
    info "xfstests-Rohergebnisse kopiert nach ${RESULTS_DIR}/"
  fi
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
resolve_bin
preflight
trap cleanup EXIT
run_tests
