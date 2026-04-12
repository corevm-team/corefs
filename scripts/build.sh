#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

PROFILE="release"
RUN_FMT=1
RUN_CHECK=1
RUN_TESTS=1
RUN_BUILD=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug)
      PROFILE="debug"
      shift
      ;;
    --skip-fmt)
      RUN_FMT=0
      shift
      ;;
    --skip-check)
      RUN_CHECK=0
      shift
      ;;
    --skip-test|--skip-tests)
      RUN_TESTS=0
      shift
      ;;
    --version-only)
      RUN_FMT=0
      RUN_CHECK=0
      RUN_TESTS=0
      RUN_BUILD=0
      shift
      ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Usage: ./scripts/build.sh [--debug] [--skip-fmt] [--skip-check] [--skip-test] [--version-only]" >&2
      exit 1
      ;;
  esac
done

DIST_DIR="${REPO_ROOT}/dist"
BIN_DIR="${DIST_DIR}/bin"
META_DIR="${DIST_DIR}/meta"
mkdir -p "${BIN_DIR}" "${META_DIR}"

PACKAGE_NAME="$(awk -F ' = ' '$1 == "name" {gsub(/"/, "", $2); print $2; exit}' Cargo.toml)"
PACKAGE_VERSION="$(awk -F ' = ' '$1 == "version" {gsub(/"/, "", $2); print $2; exit}' Cargo.toml)"
BUILD_TIMESTAMP="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
BUILD_EPOCH="$(date -u '+%s')"

if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  GIT_COMMIT="$(git rev-parse --short HEAD)"
  GIT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
  if [[ -n "$(git status --short 2>/dev/null)" ]]; then
    GIT_DIRTY=true
    DIRTY_SUFFIX=".dirty"
  else
    GIT_DIRTY=false
    DIRTY_SUFFIX=""
  fi
else
  GIT_COMMIT="unknown"
  GIT_BRANCH="unknown"
  GIT_DIRTY=false
  DIRTY_SUFFIX=""
fi

BUILD_VERSION="${PACKAGE_VERSION}+${GIT_COMMIT}${DIRTY_SUFFIX}"
VERSION_JSON="${META_DIR}/corefs-version.json"
VERSION_TXT="${META_DIR}/corefs-version.txt"
VERSION_ENV="${META_DIR}/corefs-version.env"

cat > "${VERSION_JSON}" <<EOF
{
  "name": "${PACKAGE_NAME}",
  "package_version": "${PACKAGE_VERSION}",
  "build_version": "${BUILD_VERSION}",
  "profile": "${PROFILE}",
  "git_commit": "${GIT_COMMIT}",
  "git_branch": "${GIT_BRANCH}",
  "git_dirty": ${GIT_DIRTY},
  "build_timestamp_utc": "${BUILD_TIMESTAMP}",
  "build_epoch": ${BUILD_EPOCH}
}
EOF

printf '%s\n' "${BUILD_VERSION}" > "${VERSION_TXT}"
cat > "${VERSION_ENV}" <<EOF
COREFS_PACKAGE_NAME=${PACKAGE_NAME}
COREFS_PACKAGE_VERSION=${PACKAGE_VERSION}
COREFS_BUILD_VERSION=${BUILD_VERSION}
COREFS_BUILD_PROFILE=${PROFILE}
COREFS_GIT_COMMIT=${GIT_COMMIT}
COREFS_GIT_BRANCH=${GIT_BRANCH}
COREFS_GIT_DIRTY=${GIT_DIRTY}
COREFS_BUILD_TIMESTAMP_UTC=${BUILD_TIMESTAMP}
COREFS_BUILD_EPOCH=${BUILD_EPOCH}
EOF

echo "CoreFS build metadata written to ${META_DIR}"
echo "Build version: ${BUILD_VERSION}"

if [[ ${RUN_FMT} -eq 1 ]]; then
  cargo fmt --all
fi

if [[ ${RUN_CHECK} -eq 1 ]]; then
  cargo check
fi

if [[ ${RUN_TESTS} -eq 1 ]]; then
  cargo test
fi

if [[ ${RUN_BUILD} -eq 1 ]]; then
  if [[ "${PROFILE}" == "release" ]]; then
    cargo build --release
    cp "${REPO_ROOT}/target/release/corefs" "${BIN_DIR}/corefs"
  else
    cargo build
    cp "${REPO_ROOT}/target/debug/corefs" "${BIN_DIR}/corefs"
  fi
  chmod +x "${BIN_DIR}/corefs"
  echo "Binary staged at ${BIN_DIR}/corefs"
fi
