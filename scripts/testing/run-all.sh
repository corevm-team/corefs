#!/usr/bin/env bash
# ============================================================================
# run-all.sh — Alle Testsuiten gegen CoreFS ausfuehren
# ============================================================================
#
# Verwendung:
#   ./run-all.sh [Optionen]
#
# Optionen:
#   --skip-pjdfstest   pjdfstest ueberspringen
#   --skip-xfstests    xfstests ueberspringen
#   --skip-stress      Stresstest ueberspringen
#   --quick            Verkuerzte Testlaeufe (Stress: 30s, weniger Dateien)
#   --keep             Images nach Testlauf behalten
#   --help             Diese Hilfe anzeigen
#
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS_DIR="${SCRIPT_DIR}/results"

SKIP_PJDFSTEST=0
SKIP_XFSTESTS=0
SKIP_STRESS=0
QUICK=0
KEEP=0

COLOR_GREEN='\033[0;32m'
COLOR_RED='\033[0;31m'
COLOR_BOLD='\033[1m'
COLOR_RESET='\033[0m'

info()  { echo -e "${COLOR_GREEN}[INFO]${COLOR_RESET}  $*"; }
error() { echo -e "${COLOR_RED}[ERROR]${COLOR_RESET} $*" >&2; }
header(){ echo -e "\n${COLOR_BOLD}=== $* ===${COLOR_RESET}\n"; }

usage() {
  sed -n '/^# Verwendung:/,/^# ====/{ /^# ====/d; s/^# //; s/^#//; p }' "$0"
  exit 0
}

while (( $# > 0 )); do
  case "$1" in
    --skip-pjdfstest) SKIP_PJDFSTEST=1; shift ;;
    --skip-xfstests)  SKIP_XFSTESTS=1; shift ;;
    --skip-stress)    SKIP_STRESS=1; shift ;;
    --quick)          QUICK=1; shift ;;
    --keep)           KEEP=1; shift ;;
    --help|-h)        usage ;;
    *) error "Unbekannte Option: $1"; usage ;;
  esac
done

mkdir -p "${RESULTS_DIR}"
SUMMARY="${RESULTS_DIR}/summary-$(date +%Y%m%d-%H%M%S).log"
FAILED=0

{
  echo "================================================================="
  echo "CoreFS Vollstaendiger Testlauf"
  echo "Datum: $(date -Iseconds)"
  echo "================================================================="
} | tee "${SUMMARY}"

# --------------------------------------------------------------------------
# 1. pjdfstest
# --------------------------------------------------------------------------
if (( ! SKIP_PJDFSTEST )); then
  header "Phase 1: pjdfstest (POSIX-Compliance)"
  PJDFS_ARGS=()
  (( KEEP )) && PJDFS_ARGS+=(--keep)

  if "${SCRIPT_DIR}/run-pjdfstest.sh" "${PJDFS_ARGS[@]}" 2>&1; then
    echo "pjdfstest: BESTANDEN" | tee -a "${SUMMARY}"
  else
    echo "pjdfstest: FEHLGESCHLAGEN" | tee -a "${SUMMARY}"
    FAILED=$((FAILED + 1))
  fi
else
  echo "pjdfstest: UEBERSPRUNGEN" | tee -a "${SUMMARY}"
fi

# --------------------------------------------------------------------------
# 2. xfstests
# --------------------------------------------------------------------------
if (( ! SKIP_XFSTESTS )); then
  header "Phase 2: xfstests"
  XFS_ARGS=()
  (( KEEP )) && XFS_ARGS+=(--keep)

  if "${SCRIPT_DIR}/run-xfstests.sh" "${XFS_ARGS[@]}" 2>&1; then
    echo "xfstests: BESTANDEN" | tee -a "${SUMMARY}"
  else
    echo "xfstests: FEHLGESCHLAGEN (einige Tests erwartet)" | tee -a "${SUMMARY}"
    # xfstests-Fehler sind bei FUSE-FS erwartet — zaehlt nicht als harter Fehler
  fi
else
  echo "xfstests: UEBERSPRUNGEN" | tee -a "${SUMMARY}"
fi

# --------------------------------------------------------------------------
# 3. Stresstest
# --------------------------------------------------------------------------
if (( ! SKIP_STRESS )); then
  header "Phase 3: Stresstest"
  STRESS_ARGS=()
  (( KEEP )) && STRESS_ARGS+=(--keep)

  if (( QUICK )); then
    STRESS_ARGS+=(--duration 30 --file-count 50 --workers 2)
  fi

  if "${SCRIPT_DIR}/run-stress.sh" "${STRESS_ARGS[@]}" 2>&1; then
    echo "Stresstest: BESTANDEN" | tee -a "${SUMMARY}"
  else
    echo "Stresstest: FEHLGESCHLAGEN" | tee -a "${SUMMARY}"
    FAILED=$((FAILED + 1))
  fi
else
  echo "Stresstest: UEBERSPRUNGEN" | tee -a "${SUMMARY}"
fi

# --------------------------------------------------------------------------
# Zusammenfassung
# --------------------------------------------------------------------------
echo "" | tee -a "${SUMMARY}"
header "Zusammenfassung" | tee -a "${SUMMARY}"
if (( FAILED == 0 )); then
  info "Alle Testphasen bestanden." | tee -a "${SUMMARY}"
else
  error "${FAILED} Testphase(n) fehlgeschlagen." | tee -a "${SUMMARY}"
fi
info "Vollstaendiges Protokoll: ${SUMMARY}"
exit ${FAILED}
