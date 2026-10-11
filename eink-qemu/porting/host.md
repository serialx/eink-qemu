# macOS baseline build

Verified on 2026-10-09 in `serialx/qemu-rebase`, at `b2d0f51e25`: upstream
QEMU v11.1.2 plus the porting guide. The host is Apple Silicon (`aarch64`),
macOS 27.0.1 (26A434). No upstream sources were changed and no fork code was
ported for this build.

## Reproduce the build

Run from this worktree's root, with Homebrew tools on `PATH`:

```sh
mkdir -p build
cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
/usr/bin/time -p ninja
```

This is the exact configure command from the porting guide; no extra flags or
environment overrides were necessary. Plain `ninja` (the full default build,
including tools and test executables) works. The first full build started with
no compiled objects in this worktree and completed all 2,638 steps with exit
status 0 and no compiler warnings. Its measured time, excluding configure and
dependency downloads, was:

```text
real 20.64
user 200.39
sys 84.84
```

The wall-clock time was **20.64 seconds**, using Ninja's default parallelism.
The output binaries, relative to the worktree root, are
`./build/qemu-system-xtensa` and `./build/qemu-system-riscv32`.
The test executables were built; the test suite was not run.

## Toolchain and dependencies

Configure printed these versions:

| Component | Selected version |
| --- | --- |
| Python | 3.14.8, `/opt/homebrew/bin/python3` |
| Meson | 1.11.1, bundled wheel installed in `build/pyvenv` |
| C and C++ | Apple clang 21.0.0 (`clang-2100.3.34.2`), via `cc` and `c++` |
| Objective-C | clang 21.0.0 |
| Linker | Apple ld64 27037.1 |
| Ninja | 1.13.2, `/opt/homebrew/bin/ninja` |
| GLib | 2.90.1 |
| SDL | 2.32.72 (Homebrew `sdl2-compat`) |
| slirp | 4.9.5 (Homebrew `libslirp`) |
| libgcrypt | 1.12.4 |

The Python path in Meson's final summary is
`build/pyvenv/bin/python3.14` (version 3.14). Homebrew pkgconf 3.0.7 found
SDL, slirp, and libgcrypt through `/opt/homebrew/lib/pkgconfig` without a
`PKG_CONFIG_PATH` override. All native dependencies were already installed;
no Homebrew installation or upgrade was needed.

## Host issues and workarounds

- **Python tooling and network access:** the initial sandboxed configure run
  installed the bundled Meson and pycotap wheels, then failed because PyPI
  DNS/network access was blocked while resolving missing Python tooling
  (`setuptools>=44.1.1`). Rerunning the same configure command with network
  access succeeded. Configure populated its own `build/pyvenv`, including
  setuptools 84.0.0, pip 26.2.1, and qemu.qmp 0.0.6; wheel 0.48.0 was already
  available from Homebrew's Python site-packages. Python 3.14.8 and bundled
  Meson 1.11.1 work together here: no Python downgrade, replacement Meson,
  or wheel/source patch was needed. The initial pip cache warning was due
  to sandbox write restrictions and did not require changing permissions.
- **Fetched subprojects:** configure also fetched dtc, keycodemapdb,
  berkeley-softfloat-3, and berkeley-testfloat-3. These generated checkouts
  under `subprojects/` are gitignored. A fresh configure needs access to
  their upstream repositories as well as any missing Python packages.
- **GnuTLS:** retain the guide's `--disable-gnutls`; configure reports
  `GNUTLS support: NO` and uses explicitly enabled libgcrypt. No GnuTLS
  installation is required for this baseline.
- **SDL, slirp, and gcrypt detection:** all three requested features report
  `YES` with the versions above. No custom include/library paths,
  `libgcrypt-config` override, or static slirp workaround was needed.
- **Rust:** this v11.1.2 tree defaults Rust to disabled. Configure prints
  `Compiler for language rust skipped: feature rust disabled` and
  `Rust support: NO`; Rust was not auto-detected or compiled, and no
  additional `--disable-rust` or Meson override was needed.
- **Automatic configure adjustments:** configure printed
  `Disabling PIE due to missing toolchain support` and selected `b_pie=false`.
  Meson also warned that static `iconv` was unavailable for GLib. This is
  a dynamic build (`static build: NO`), and neither message prevented the
  full build or required a manual change.

Local logs are retained in the ignored build directory:
`configure-initial.log`, `configure-network.log`, `build-full.log`,
`build-full.time`, and `acceptance.log`. The commands and results below
are the durable acceptance record.

## Acceptance output

All four commands exited 0. The version suffix refers to the build's source
commit, which contains only the porting guide above upstream v11.1.2.

```text
$ ./build/qemu-system-xtensa --version
QEMU emulator version 11.1.2 (v11.1.2-1-gb2d0f51e25)
Copyright (c) 2003-2026 Fabrice Bellard and the QEMU Project developers

$ ./build/qemu-system-riscv32 --version
QEMU emulator version 11.1.2 (v11.1.2-1-gb2d0f51e25)
Copyright (c) 2003-2026 Fabrice Bellard and the QEMU Project developers

$ ./build/qemu-system-riscv32 -machine help
Supported machines are:
amd-microblaze-v-generic AMD Microblaze-V generic platform
none                 empty machine
opentitan            RISC-V Board compatible with OpenTitan
sifive_e             RISC-V Board compatible with SiFive E SDK
sifive_u             RISC-V Board compatible with SiFive U SDK
spike                RISC-V Spike board
virt                 RISC-V VirtIO board

$ ./build/qemu-system-xtensa -machine help
Supported machines are:
kc705                kc705 EVB (dc232b)
kc705-nommu          kc705 noMMU EVB (de212)
lx200                lx200 EVB (dc232b)
lx200-nommu          lx200 noMMU EVB (de212)
lx60                 lx60 EVB (dc232b)
lx60-nommu           lx60 noMMU EVB (de212)
ml605                ml605 EVB (dc232b)
ml605-nommu          ml605 noMMU EVB (de212)
none                 empty machine
sim                  sim machine (dc232b) (default)
virt                 virt machine (dc232b)
```
