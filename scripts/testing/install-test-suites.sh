#!/usr/bin/env bash
# ============================================================================
# install-test-suites.sh — Installiert pjdfstest und xfstests fuer CoreFS
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUITES_DIR="${SCRIPT_DIR}/suites"

COLOR_GREEN='\033[0;32m'
COLOR_RED='\033[0;31m'
COLOR_YELLOW='\033[0;33m'
COLOR_RESET='\033[0m'

info()  { echo -e "${COLOR_GREEN}[INFO]${COLOR_RESET}  $*"; }
warn()  { echo -e "${COLOR_YELLOW}[WARN]${COLOR_RESET}  $*"; }
error() { echo -e "${COLOR_RED}[ERROR]${COLOR_RESET} $*" >&2; }

# --------------------------------------------------------------------------
# Abhaengigkeiten pruefen
# --------------------------------------------------------------------------
check_system_deps() {
  info "Pruefe System-Abhaengigkeiten ..."

  local missing=()

  # Basis-Build-Tools
  for cmd in git make gcc automake autoconf pkg-config; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      missing+=("$cmd")
    fi
  done

  # FUSE-Userspace
  if ! command -v fusermount3 >/dev/null 2>&1 && \
     ! command -v fusermount >/dev/null 2>&1; then
    missing+=("fuse3 / fuse")
  fi

  if (( ${#missing[@]} > 0 )); then
    error "Fehlende Pakete: ${missing[*]}"
    echo ""
    echo "Unter Debian/Ubuntu:"
    echo "  sudo apt install build-essential git automake autoconf pkg-config \\"
    echo "    fuse3 libfuse3-dev libacl1-dev attr libaio-dev uuid-dev \\"
    echo "    xfsprogs e2fsprogs btrfs-progs dump xfslibs-dev"
    echo ""
    echo "Unter Fedora/RHEL:"
    echo "  sudo dnf install gcc make git automake autoconf pkgconfig \\"
    echo "    fuse3 fuse3-devel libacl-devel attr libaio-devel uuid-devel \\"
    echo "    xfsprogs e2fsprogs btrfs-progs dump xfsprogs-devel"
    exit 1
  fi

  info "Alle Basis-Abhaengigkeiten vorhanden."
}

# --------------------------------------------------------------------------
# pjdfstest installieren
# --------------------------------------------------------------------------
install_pjdfstest() {
  local dest="${SUITES_DIR}/pjdfstest"

  if [[ -x "${dest}/pjdfstest" ]]; then
    info "pjdfstest ist bereits installiert: ${dest}/pjdfstest"
    return 0
  fi

  info "Klone pjdfstest ..."
  rm -rf "${dest}"
  git clone https://github.com/pjd/pjdfstest.git "${dest}"

  info "Baue pjdfstest ..."
  cd "${dest}"
  autoreconf -ifs
  ./configure
  make

  if [[ -x "${dest}/pjdfstest" ]]; then
    info "pjdfstest erfolgreich gebaut: ${dest}/pjdfstest"
  else
    error "pjdfstest Build fehlgeschlagen."
    exit 1
  fi
}

# --------------------------------------------------------------------------
# xfstests installieren
# --------------------------------------------------------------------------
install_xfstests() {
  local dest="${SUITES_DIR}/xfstests"

  if [[ -x "${dest}/check" ]]; then
    info "xfstests ist bereits installiert: ${dest}/check"
    return 0
  fi

  info "Klone xfstests-dev ..."
  rm -rf "${dest}"
  git clone https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git "${dest}"

  info "Baue xfstests ..."
  cd "${dest}"
  make

  if [[ -x "${dest}/check" ]]; then
    info "xfstests erfolgreich gebaut: ${dest}/check"
  else
    error "xfstests Build fehlgeschlagen."
    exit 1
  fi
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
main() {
  local target="${1:-all}"

  check_system_deps
  mkdir -p "${SUITES_DIR}"

  case "${target}" in
    pjdfstest)
      install_pjdfstest
      ;;
    xfstests)
      install_xfstests
      ;;
    all)
      install_pjdfstest
      install_xfstests
      ;;
    *)
      error "Unbekanntes Ziel: ${target}"
      echo "Verwendung: $0 [pjdfstest|xfstests|all]"
      exit 1
      ;;
  esac

  echo ""
  info "Installation abgeschlossen."
  info "Naechster Schritt:"
  echo "  ./scripts/testing/run-pjdfstest.sh    — POSIX-Compliance-Tests"
  echo "  ./scripts/testing/run-xfstests.sh      — xfstests-Suite"
}

main "$@"
