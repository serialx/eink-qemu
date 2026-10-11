#!/bin/sh
# Compatibility wrapper; mkflash.py accepts apps, build directories and flash dumps.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$here/mkflash.py" "$@"
