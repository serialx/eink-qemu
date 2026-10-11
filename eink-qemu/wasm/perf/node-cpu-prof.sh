#!/bin/sh
# Optional standalone NODE wrapper for run-node.sh; benchmark.py sets flags itself.
set -eu
PERF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
: "${TURBO_PROFILE_DIR:?set TURBO_PROFILE_DIR to an absolute evidence directory}"
mkdir -p "$TURBO_PROFILE_DIR"
exec node --cpu-prof --cpu-prof-dir="$TURBO_PROFILE_DIR" --require "$PERF_DIR/profile-hooks.cjs" "$@"
