#!/usr/bin/env bash
#
# Bundle the DLLs the Windows build needs beside its executables.
#
# eink-qemu: QEMU is built dynamically (plugins need it), so the executables
# and the plugins in ../plugins load glib, pixman, SDL and the rest from the
# MSYS2 UCRT64 tree. ldd lists every DLL a binary loads, transitively; those
# that resolve into /ucrt64/bin are copied next to the executables, where
# Windows looks first (for the plugins too, as QEMU's process loads them).
#

set -euo pipefail

INSTALL_BIN_DIR="${1:-install/qemu/bin}"
PLUGIN_DIR="${INSTALL_BIN_DIR}/../plugins"
MINGW_BIN_DIR="/ucrt64/bin"

binaries=("${INSTALL_BIN_DIR}"/*.exe)
if compgen -G "${PLUGIN_DIR}/*.dll" > /dev/null; then
    binaries+=("${PLUGIN_DIR}"/*.dll)
fi

declare -A dlls=()
for binary in "${binaries[@]}"; do
    echo "Analyzing: ${binary}"
    while IFS= read -r line; do
        # "libfoo.dll => /ucrt64/bin/libfoo.dll (0x...)"
        if [[ "${line}" =~ ^[[:space:]]*([^[:space:]]+\.dll)[[:space:]]*'=>'[[:space:]]*(/ucrt64/bin/[^[:space:]]+) ]]; then
            dlls["${BASH_REMATCH[1]}"]="${BASH_REMATCH[2]}"
        elif [[ "${line}" == *"not found"* ]]; then
            echo "ERROR: ${binary}: ${line}"
            exit 1
        fi
    done < <(ldd "${binary}")
done

for dll in $(printf '%s\n' "${!dlls[@]}" | sort); do
    cp -v "${MINGW_BIN_DIR}/${dll}" "${INSTALL_BIN_DIR}/"
done
echo "Bundled ${#dlls[@]} DLLs."
