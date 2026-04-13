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
# Paketlisten
# --------------------------------------------------------------------------
DEBIAN_PKGS=(
  build-essential git automake autoconf pkg-config libtool
  fuse3 libfuse3-dev libacl1-dev attr libaio-dev uuid-dev
  xfsprogs xfslibs-dev e2fsprogs btrfs-progs dump
  perl
)

FEDORA_PKGS=(
  gcc make git automake autoconf pkgconfig libtool
  fuse3 fuse3-devel libacl-devel attr libaio-devel uuid-devel
  xfsprogs xfsprogs-devel e2fsprogs btrfs-progs dump
  perl-Test-Harness
)

# --------------------------------------------------------------------------
# Distro erkennen
# --------------------------------------------------------------------------
detect_distro() {
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    case "${ID:-}" in
      ubuntu|debian|linuxmint|pop) echo "debian" ;;
      fedora|rhel|centos|rocky|alma) echo "fedora" ;;
      *) echo "unknown" ;;
    esac
  else
    echo "unknown"
  fi
}

# --------------------------------------------------------------------------
# Abhaengigkeiten pruefen und optional installieren
# --------------------------------------------------------------------------
check_system_deps() {
  info "Pruefe System-Abhaengigkeiten ..."

  local missing_cmds=()
  local missing_libs=()

  # Basis-Build-Tools
  for cmd in git make gcc automake autoconf pkg-config; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      missing_cmds+=("$cmd")
    fi
  done

  # libtool liefert unter Debian 'libtoolize', nicht 'libtool'
  if ! command -v libtoolize >/dev/null 2>&1 && \
     ! command -v libtool >/dev/null 2>&1; then
    missing_cmds+=("libtool")
  fi

  # FUSE-Userspace
  if ! command -v fusermount3 >/dev/null 2>&1 && \
     ! command -v fusermount >/dev/null 2>&1; then
    missing_cmds+=("fuse3")
  fi

  # Header-Bibliotheken (xfstests benoetigt xfs/xfs.h, uuid/uuid.h, etc.)
  for header in xfs/xfs.h uuid/uuid.h acl/libacl.h libaio.h; do
    if ! echo "#include <${header}>" | gcc -E -x c - >/dev/null 2>&1; then
      missing_libs+=("${header}")
    fi
  done

  local has_missing=0
  if (( ${#missing_cmds[@]} > 0 )); then
    warn "Fehlende Kommandos: ${missing_cmds[*]}"
    has_missing=1
  fi
  if (( ${#missing_libs[@]} > 0 )); then
    warn "Fehlende Header: ${missing_libs[*]}"
    has_missing=1
  fi

  if (( has_missing )); then
    local distro
    distro="$(detect_distro)"

    echo ""
    if [[ "${distro}" == "debian" ]]; then
      info "Debian/Ubuntu erkannt — installiere fehlende Pakete automatisch ..."
      echo "  sudo apt install -y ${DEBIAN_PKGS[*]}"
      echo ""
      sudo apt install -y "${DEBIAN_PKGS[@]}"
    elif [[ "${distro}" == "fedora" ]]; then
      info "Fedora/RHEL erkannt — installiere fehlende Pakete automatisch ..."
      echo "  sudo dnf install -y ${FEDORA_PKGS[*]}"
      echo ""
      sudo dnf install -y "${FEDORA_PKGS[@]}"
    else
      error "Unbekannte Distribution — bitte manuell installieren:"
      echo ""
      echo "Unter Debian/Ubuntu:"
      echo "  sudo apt install ${DEBIAN_PKGS[*]}"
      echo ""
      echo "Unter Fedora/RHEL:"
      echo "  sudo dnf install ${FEDORA_PKGS[*]}"
      exit 1
    fi

    # Nochmal pruefen ob alles da ist
    for cmd in git make gcc automake autoconf pkg-config; do
      if ! command -v "$cmd" >/dev/null 2>&1; then
        error "Kommando '${cmd}' nach Installation immer noch nicht gefunden."
        exit 1
      fi
    done
    if ! command -v libtoolize >/dev/null 2>&1 && \
       ! command -v libtool >/dev/null 2>&1; then
      error "Weder 'libtool' noch 'libtoolize' nach Installation gefunden."
      exit 1
    fi
    for header in xfs/xfs.h uuid/uuid.h; do
      if ! echo "#include <${header}>" | gcc -E -x c - >/dev/null 2>&1; then
        error "Header '${header}' nach Installation immer noch nicht gefunden."
        exit 1
      fi
    done

    info "Alle Abhaengigkeiten erfolgreich installiert."
  else
    info "Alle Abhaengigkeiten vorhanden."
  fi
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
