#!/usr/bin/env sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
EVIDENCE_DIR="${COREFS_CERT_EVIDENCE_DIR:-$SCRIPT_DIR/evidence}"
export COREFS_CERT_EVIDENCE_DIR="$EVIDENCE_DIR"

if [ "${1:-}" = "--release" ]; then
  cargo test -p corefs-certification --release -- --nocapture
else
  cargo test -p corefs-certification -- --nocapture
fi
