# Scripts, documentation and CI port

Restored the non-C fork files from `x4prosim-pre-rebase` onto upstream
`v11.1.2`: `eink-qemu/` except `porting/`, `README.md`, `EINK-QEMU.md`,
`test_qemu.sh`, `.github/` and `.gitlab-ci.yml`. The runner/image-tool interfaces,
calibration tools, test data and images are unchanged from the reference tag.
Removed upstream's `.github/workflows/lockdown.yml`, as in the old fork.
No C source, `meson.build` or `pc-bios` file was changed.

## Configure and dependency decisions

All old configure options still exist in this tag; none required a rename.
Checked the four CI configure scripts and both documentation commands against
`../configure --help` in the worktree's scratch `build/`, the configure parser,
`scripts/meson-buildoptions.sh`, and `meson_options.txt`.

| Flags changed | Reason |
| --- | --- |
| Linux native and ARM64: `--extra-cflags=-Werror` → `--enable-werror` | Keep warnings fatal for the build through Meson's supported option, without making configure's compiler feature probes inherit `-Werror`. |
| All CI scripts and both doc commands: add `--enable-png` | `drive.py` requests `screendump -f png`; `--without-default-features` otherwise disables PNG in CI. Install libpng explicitly on each platform. |
| Both doc commands: add `--disable-gnutls` | Match the porting guide's libgcrypt-based build and avoid an unnecessary optional TLS dependency. The old macOS note already suggested this flag. |

Retained CI options (all accepted): `--bindir=bin`, `--datadir=share/qemu`,
`--enable-gcrypt`, `--enable-sdl`, `--enable-pixman`, `--enable-slirp`,
`--enable-stack-protector`, `--prefix=.../install/qemu`, `--target-list=...`,
`--with-pkgversion=...`, `--with-suffix=`, `--without-default-features`;
ARM64 retains `--cross-prefix=aarch64-linux-gnu-`; macOS retains
`--enable-fdt=internal` and `--python=python3`; Windows retains `--static`
and `--disable-werror`. Documentation also retains `--disable-strip`,
`--disable-user`, `--disable-capstone`, `--disable-vnc`, `--disable-gtk`,
`--disable-docs`, and the two-target list.

- Python must be at least 3.9 (`configure`). Linux prerequisites now explicitly
  install Python and venv support; macOS installs Homebrew Python, and Windows
  already installs MSYS2 UCRT Python. Docs use a virtual environment for esptool
  instead of system pip, which fails on externally managed Python installations.
- Meson must be at least 1.5.0 for these builds (`pythondeps.toml`). Removed the
  separate Meson 1.7.0/tomli pins and let configure select an acceptable installed
  Meson or its bundled 1.11.1 wheel in `build/pyvenv`.
- Rust is **disabled by default in this exact tag**, in both `configure` and
  `meson_options.txt`; it is not auto-enabled merely because rustc is installed.
  No Rust flag or toolchain installation is necessary. If explicitly enabled,
  the sources require Rust 1.83 or newer, rustdoc, bindgen and Meson 1.11 or newer.
- GLib must be at least 2.66.0 and libgcrypt at least 1.9.4 (`meson.build`).
  GitHub already uses Debian 12; GitLab now uses Debian 12 and runs the shared
  prerequisite installer instead of relying on the unspecified contents of
  `$CI_DOCKER_REGISTRY/qemu-build:7`. Its three target jobs and deploy jobs keep
  their names and interfaces.
- The unused `prerequisites-old.sh` retains its entry point but delegates to the
  current installer, with a supported-distribution requirement. Its Ubuntu 18.04,
  Python 3.8 and old system libraries cannot satisfy QEMU 11.1.
- Explicit Linux prerequisites include pkg-config, CA certificates and xz for
  dependency discovery, HTTPS fetches and packaging. Runtime installation also
  lists libgcrypt and libpng. First configuration may download missing Python
  packages and subprojects; CI already has network access for these steps.
- Removed the ARM64 `meson.build` substitution forcing libgcrypt's discovery
  method: 11.1 no longer forces `config-tool`. Removed macOS substitutions adding
  Objective-C and user include paths: upstream selects Objective-C on Darwin,
  and the old user include workaround is unnecessary for the system targets.
- Removed the obsolete macOS GLib 2.78.1/distutils patch and global pip installs.

## Build and packaging review

- `./configure` from the source root still redirects into `build/`, and
  `ninja -C build install` remains valid. Corrected failure diagnostics to
  `build/meson-logs/meson-log.txt`; the old path omitted `build/` and could mask
  the actual failure. The diagnostic handler always exits unsuccessfully.
- The configured install layout remains `install/qemu/bin/` and
  `install/qemu/share/qemu/`; no emulator or ROM path rename is required. The
  `esp*.bin` firmware filter remains, and machine-layer commits supply those ROMs.
- Preserved GitHub's ten platform/target combinations, Linux machine smoke tests,
  Windows DLL bundling, archive format and draft release flow. The Bash DLL
  helper uses equivalent here-documents instead of process substitutions so
  the requested macOS `sh -n` check also accepts it; it remains a Bash script.
- Corrected the artifact manifest to `dist/dist-qemu-${TARGET}-${PLATFORM}/...`,
  matching the uploaded artifact name and download directory even when the
  repository/project name is `eink-qemu`.
- Release source archives now clone the triggering tag, rather than the default
  branch. GitLab archives now include the same scripts/docs as GitHub archives.
- README and EINK-QEMU identify upstream QEMU 11.1.2, link the porting-layer guide,
  and update build instructions. All unrelated documentation remains as restored.

## Acceptance

Executed on macOS in this worktree; no CI jobs or firmware boots were run.
The configure-help checks validate option parsing, not platform dependencies,
linking or runtime behavior. Actionlint is unavailable; PyYAML 6.0.3 parsed both
GitHub workflows and GitLab CI. Remaining integration work is the coordinator's
combined-layer build and actual CI execution, including the Windows static link
and ARM64 cross build.

```sh
mkdir -p build
(cd build && ../configure --help > configure-help.txt)
python3 - <<'PY'
from pathlib import Path
import shlex, subprocess, sys, yaml
shell = sorted(Path('.github').rglob('*.sh')) + sorted(Path('eink-qemu').rglob('*.sh')) + [Path('test_qemu.sh')]
for p in shell:
    subprocess.run(['sh', '-n', str(p)], check=True)
    if 'bash' in p.read_text().splitlines()[0]:
        subprocess.run(['bash', '-n', str(p)], check=True)
print(f'sh -n: PASS ({len(shell)} scripts); bash -n: PASS (Bash scripts)')
python = sorted(Path('eink-qemu').rglob('*.py'))
subprocess.run([sys.executable, '-m', 'py_compile', *map(str, python)], check=True)
print(f'python3 -m py_compile: PASS ({len(python)} scripts)')
workflows = sorted(Path('.github').rglob('*.yml')) + [Path('.gitlab-ci.yml')]
for p in workflows:
    assert isinstance(yaml.safe_load(p.read_text()), dict), p
print(f'YAML parse: PASS ({len(workflows)} files, PyYAML {yaml.__version__})')
for p in sorted(Path('.github/workflows/scripts').glob('configure-*.sh')) + [Path('README.md'), Path('EINK-QEMU.md')]:
    lines = p.read_text().splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith(('./configure ', '../configure ')) and line.endswith('\\'))
    command = []
    for line in lines[start:]:
        if line.startswith('||'):
            break
        command.append(line.rstrip().removesuffix('\\'))
        if not line.endswith('\\'):
            break
    command = ' '.join(command).replace('${PWD}', str(Path.cwd())).replace('$PWD', str(Path.cwd())).replace('${TARGET}', 'xtensa-softmmu').replace('${VERSION}', 'validation')
    args = shlex.split(command)[1:]
    subprocess.run(['../configure', '--help', *args], cwd='build', check=True, stdout=subprocess.DEVNULL)
    print(f'{p}: PASS ({len(args)} flags)')
    if p.name == 'configure-native.sh':
        for target in ['riscv32-softmmu', 'riscv32-linux-user']:
            subprocess.run(['../configure', '--help', *[a.replace('xtensa-softmmu', target) for a in args]], cwd='build', check=True, stdout=subprocess.DEVNULL)
            print(f'  GitLab target {target}: PASS')
PY
```

Recorded output before the project rename:

```text
sh -n: PASS (13 scripts); bash -n: PASS (Bash scripts)
python3 -m py_compile: PASS (11 scripts)
YAML parse: PASS (3 files, PyYAML 6.0.3)
.github/workflows/scripts/configure-cross-linux-arm64.sh: PASS (15 flags)
.github/workflows/scripts/configure-macos.sh: PASS (15 flags)
.github/workflows/scripts/configure-native.sh: PASS (14 flags)
  GitLab target riscv32-softmmu: PASS
  GitLab target riscv32-linux-user: PASS
.github/workflows/scripts/configure-win.sh: PASS (15 flags)
README.md: PASS (12 flags)
X4PROSIM.md: PASS (12 flags)
```

The following recorded comparisons retain their historical paths:
`git diff --check x4prosim-pre-rebase -- .github .gitlab-ci.yml README.md
X4PROSIM.md test_qemu.sh x4prosim ':!x4prosim/porting'` passes. The restored raw
device log already contains trailing whitespace on line 43; it is intentionally
preserved byte-for-byte. `git diff x4prosim-pre-rebase -- x4prosim
':!x4prosim/porting' test_qemu.sh` is empty.

The final commit's scope/stat check is recorded below.

```sh
git diff --stat v11.1.2..HEAD -- .github .gitlab-ci.yml README.md EINK-QEMU.md \
  test_qemu.sh eink-qemu/run.sh eink-qemu/drive.py eink-qemu/mkflash.sh \
  eink-qemu/mksd.py eink-qemu/mknvs.py eink-qemu/testdata eink-qemu/sdcal \
  eink-qemu/ghosting eink-qemu/porting/scripts-docs-ci.md
git status --short
```

Scope: 95 files changed, including the restored runner/calibration/data files,
two docs, CI workflows/scripts, lockdown deletion and this report. No changed
C files, `meson.build` files or `pc-bios` paths. Final `git status --short` is
empty. The older `porting/README.md` and `porting/host.md` are pre-existing
commits and are excluded from this layer's scope check.
