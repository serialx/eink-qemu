# Managed bridge acceptance

Status on 2026-10-22: implementation and native acceptance are still in progress.
The feature is not ready for release. This is the current checklist; the
[platform gate record](bridge-platform-gates.md) contains the detailed evidence
and earlier failed attempts.

## Verified scope

Tests run in disposable VMs. The development Mac's own networking was not
reconfigured. Host readiness and firmware connectivity are separate results:
“Bridge ready” verifies the host transport, not the reader's DHCP or web service.

| Test environment | Wizard and lifecycle evidence | Firmware evidence |
| --- | --- | --- |
| macOS 26.5.2 ARM64, VirtualBuddy, bridged Ethernet | Ordinary-user wizard setup and system authorization; native removal, reboot recovery, and lease protection | C3 and S3 acquired LAN addresses; a separate peer opened their HTTP services and resolved mDNS |
| Ubuntu 22.04 ARM64, UTM shared network | Packaged wizard configuration and confirmed removal; package update, leased-removal refusal, cleanup, and reinstall with reversible network fixtures | C3 DHCP, HTTP, and mDNS on the shared virtual network; S3 booted, connection pending |
| Windows 11 Pro x64 build 26300, UTM shared network | Actual UAC decline and approval; wizard setup and Keep; native configure, repair, cancelled/confirmed removal, lease checks; package updates preserve a retained bridge | C3 DHCP, HTTP, and mDNS on the shared virtual network; S3 booted, connection pending |

The Linux tests used an explicitly authorized temporary polkit rule for the
single bridge action. Each lifecycle fixture removed it afterward. These runs
do not establish a standard Linux desktop's password-prompt behavior.

## Current packages and regression results

The following packages contain source through `de22c7be03`. They include the
review fixes for durable cancellation during Apply, Linux checkpoint timing,
dynamic connectivity verification, and Linux's explicit **In use** state.

| Package | SHA-256 | Result |
| --- | --- | --- |
| Ubuntu ARM64 Debian package | `b369e88bae1087efa058e7f6b5eaad1050f3c168339e392a510c724b3efed6be` | Archive checks and update passed; installed helper active; app and connection chooser opened |
| Windows x64 NSIS installer | `4793caeb92673c9c7e0957bbb782f95379ef8cccc8bad380e618ff3355a7c61f` | Update passed; retained resource unchanged; automatic helper running; installed desktop, helper, and both QEMU payload hashes matched |

The current macOS QA package predates these review fixes. It is development
signed and explicitly targets macOS 15 or newer; it is not the release artifact.

Current-source regression results: 244 macOS workspace tests passed, with four
existing tests ignored; 77 Ubuntu ARM64 bridge tests and 35 Windows x64
transaction/reboot-pause/cleanup tests passed. All 26 bridge UI tests,
TypeScript checking, and the frontend build passed. These suites use temporary
fixtures and do not replace native network lifecycle tests of the new packages.

## Remaining acceptance work

- Implement and test the explicit macOS **Uninstall bridge support** action,
  including conflict and live-lease refusal. Test native helper upgrade.
- Build and verify the advertised macOS 12.3 baseline with release-compatible
  dependencies, Developer ID signing, and notarization. Test Intel macOS.
- Complete Linux x64 native coverage and Linux/Windows C3/S3 physical-LAN
  acceptance. Their shared-network results do not satisfy that requirement.
- Retest the latest recovery changes through installed services and the actual
  wizard, including interrupted setup and reboot. Complete Windows package
  removal with a retained bridge; earlier package-removal tests used no resources.
- Finish the visual and keyboard matrix, dependency-failure, disconnected-adapter,
  and version-mismatch flows. Windows testing remains at the user's requested
  800 × 600 resolution.
- Investigate the Ubuntu WebKit process SIGSEGV and the Windows C3 firmware
  interrupt-watchdog reset. A successful retry does not explain either failure.

Automated mouse input currently lands at a stale guest pointer position in UTM,
including Windows Explorer. This limits touchscreen firmware testing and is
recorded as a test-control problem, not evidence of an emulator defect.

An app-created Linux bridge currently supports one running emulator. Its
temporary QEMU TAP cannot yet be reliably distinguished from an unexpected
member, so verification, repair, removal, and a second launch require it to be
idle. Strict membership checks resume after the emulator releases its lease.
