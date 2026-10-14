#!/bin/sh
# Stage a Windows QEMU for the app bundle in src-tauri/qemu, from an install
# tree with bin/qemu-system-{riscv32,xtensa}.exe (and any DLLs beside them):
# the CI's dist-qemu-*-x86_64-w64-mingw32 archives unpacked into one
# directory, or `ninja install` of a MinGW build.
#   qemu/bin/qemu-system-{riscv32,xtensa}.exe and the DLLs they load
#   qemu/share/esp32c3-rom.bin, esp32s3_rev0_rom.bin   (the app passes -L qemu/share)
#   qemu/COPYING                                        QEMU's licence
# Windows finds DLLs beside the .exe, so nothing is rewritten. Runs on any
# host; with llvm-objdump or x86_64-w64-mingw32-objdump it checks that every
# imported DLL is bundled or part of Windows.
set -eu
desktop=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$desktop/../.." && pwd)
src=${1:?usage: bundle-qemu-windows.sh <QEMU install directory>}
out=$desktop/src-tauri/qemu

for name in qemu-system-riscv32 qemu-system-xtensa; do
    [ -f "$src/bin/$name.exe" ] || { echo "bundle-qemu-windows.sh: $src/bin/$name.exe is missing" >&2; exit 1; }
done

rm -rf "$out"
mkdir -p "$out/bin" "$out/share"
cp "$src/bin/qemu-system-riscv32.exe" "$src/bin/qemu-system-xtensa.exe" "$out/bin/"
for dll in "$src"/bin/*.dll; do
    [ -f "$dll" ] && cp "$dll" "$out/bin/"
done
# Debug information is most of a MinGW build's size.
for tool in x86_64-w64-mingw32-strip llvm-strip /opt/homebrew/opt/llvm/bin/llvm-strip; do
    if command -v "$tool" >/dev/null 2>&1; then
        "$tool" --strip-debug "$out"/bin/*.exe
        break
    fi
done
# The X3/X4 (ESP32-C3) and X4 Pro/Picco (ESP32-S3) mask ROMs, from qemu_find_file().
cp "$root/pc-bios/esp32c3-rom.bin" "$root/pc-bios/esp32s3_rev0_rom.bin" "$out/share/"
cp "$root/COPYING" "$out/COPYING"

objdump=
for tool in x86_64-w64-mingw32-objdump llvm-objdump /opt/homebrew/opt/llvm/bin/llvm-objdump; do
    if command -v "$tool" >/dev/null 2>&1; then objdump=$tool; break; fi
done
if [ -z "$objdump" ]; then
    echo "bundle-qemu-windows.sh: no objdump for PE files; DLL imports not checked" >&2
else
    # Windows' own: the C runtime, Win32, Winsock and the API sets.
    system=' ucrtbase.dll msvcrt.dll kernel32.dll user32.dll gdi32.dll advapi32.dll shell32.dll ole32.dll
        oleaut32.dll ws2_32.dll iphlpapi.dll winmm.dll bcrypt.dll crypt32.dll secur32.dll dnsapi.dll
        shlwapi.dll setupapi.dll imm32.dll version.dll comdlg32.dll comctl32.dll dwmapi.dll uxtheme.dll
        ntdll.dll userenv.dll cfgmgr32.dll hid.dll dinput8.dll dxgi.dll d3d11.dll d3d9.dll opengl32.dll
        mswsock.dll rpcrt4.dll wtsapi32.dll netapi32.dll psapi.dll dbghelp.dll winspool.drv '
    bundled=$(ls "$out/bin" | tr 'A-Z' 'a-z')
    missing=$(
        for file in "$out"/bin/*.exe "$out"/bin/*.dll; do
            [ -f "$file" ] && "$objdump" -p "$file" | sed -n 's/^[[:space:]]*DLL Name: //p'
        done | sort -u | while read -r dll; do
            lower=$(printf '%s' "$dll" | tr 'A-Z' 'a-z')
            case "$lower" in (api-ms-win-*) continue ;; esac
            case "$system" in (*[[:space:]]"$lower"[[:space:]]*) continue ;; esac
            printf '%s\n' "$bundled" | grep -qxF "$lower" || echo "$dll"
        done
    )
    if [ -n "$missing" ]; then
        echo "bundle-qemu-windows.sh: DLLs imported but not bundled:" >&2
        echo "$missing" | sed 's/^/  /' >&2
        exit 1
    fi
fi
du -sh "$out" | sed 's/^/qemu bundle: /'
