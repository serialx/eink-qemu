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
| Ubuntu 22.04 ARM64, UTM shared network | Packaged wizard configuration and confirmed removal; package update, leased-removal refusal, cleanup, and reinstall with reversible network fixtures | C3 and S3 DHCP, HTTP, and mDNS address resolution on the shared virtual network |
| Windows 11 Pro x64 build 26300, UTM shared network | Actual UAC decline and approval; wizard setup and Keep; native configure, repair, cancelled/confirmed removal, lease checks; package updates preserve a retained bridge | C3 and S3 DHCP, HTTP, and mDNS address resolution on the shared virtual network |

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

The installed Windows build also completed the **Manage bridge → Bridge ready**
check after its update and launched the saved X4 Pro firmware. The reader reached
Home in 183.06 seconds after emulator startup. Native bridge inspection before
startup took several minutes in the emulated x64 VM. The S3 scan found three
networks; joining PICSimLabWifi assigned `192.168.64.20`. The host received HTTP
200, and a freshly reloaded browser displayed the connected reader and version
1.6.5-ko.0. mDNS resolved `crosspoint.local` to the same address on interface 41.
These results establish shared-network connectivity, not physical-LAN access.

The latest Windows package also removed the retained bridge successfully. The
uninstaller removed its service, desktop, and helper, cleared the resource
journal, and preserved the user's SD image. Reinstall passed with matching
desktop, helper, and QEMU payload hashes and an automatic running service.

The latest Ubuntu wizard completed connection selection, review, Apply, Keep,
and **Bridge ready** using keyboard navigation. A separate attempt against the
original generated Netplan connection was correctly refused before mutation.
The successful run used the reversible persistent-profile fixture described
above; it does not expand the supported profile types.
The S3 reader reached Home in 1.69 seconds, found three networks, and received
`192.168.64.20` after joining PICSimLabWifi. The host received HTTP 200 and opened
its connected reader page; mDNS resolved the same address. The emulator was
closed and the fixture cleanup completed, restoring the original profiles and
removing the temporary authorization.

The current macOS QA app includes support removal and is development signed
for macOS 15 or newer; it is not the release artifact. Its native helper update
installed the exact bundled payload and preserved the retained bridge service.
Removal refused a held resource lease and restarted the helper without
interrupting that bridge. It also refused an altered helper plist without
stopping the helper; the reversible fixture restored the original plist.
Cancelling the real macOS removal prompt returned **Authorization cancelled**
and preserved support. Approved removal then completed through the real UI:
the helper, packet service, sockets, and temporary maintenance job were absent;
the durable result was successful; the resource journal was empty with no
pending rollback or cleanup; and the saved selection was cleared. The firmware
and SD image remained present with their previous sizes and modification times.
Reopening setup offered installation again. Approved reinstallation installed
the exact bundled helper, started its service and IPC socket, and reached
**Choose a connection** without recreating the removed bridge. The old resource
plist and socket remained absent.

A separate native fault test made the helper plist temporarily immutable.
Removal deleted the packet-service payload, then failed with `ECLEANUP` on the
plist. Recovery restored the original payload bytes and restarted the helper;
all three support files matched their pre-test hashes. The test removed the
immutable flag, and the actual app reopened **Choose a connection** successfully.
The VM is idle with bridge support installed and no configured bridge.

The update exposed a dialog stall: an existing completed bridge operation
entered **Saving setup** without starting verification. Both support-update
paths now invoke the same verification step used by inspection. Two regression
tests reproduced the missing call before the fix and pass afterward; the
updated QA app reaches **Bridge ready** when reopening the retained setup.

Current-source regression results: 254 macOS workspace tests passed, with four
existing tests ignored; 77 Ubuntu ARM64 bridge tests and 35 Windows x64
transaction/reboot-pause/cleanup tests passed. All 31 bridge UI tests,
TypeScript checking, the frontend build, and 18 packaging/probe tests passed. These suites use temporary
fixtures and do not replace native network lifecycle tests of the new packages.

## Remaining acceptance work

- Build and verify the advertised macOS 12.3 baseline with release-compatible
  dependencies, Developer ID signing, and notarization. Test Intel macOS.
- Complete Linux x64 native coverage and Linux/Windows C3/S3 physical-LAN
  acceptance. Their shared-network results do not satisfy that requirement.
- Retest the latest recovery changes through installed services and the actual
  wizard, including interrupted setup and reboot. Windows package removal with
  a retained bridge and subsequent reinstall now pass.
- Finish the visual and keyboard matrix, dependency-failure, disconnected-adapter,
  and version-mismatch flows. Windows testing remains at the user's requested
  800 × 600 resolution.
- Investigate the Ubuntu WebKit process SIGSEGV and the Windows C3 firmware
  interrupt-watchdog reset. A successful retry does not explain either failure.

Automated mouse clicks can land at a stale guest pointer position in UTM,
including Windows Explorer. Short drags at the intended position followed by
an observed pointer update and a separate click allowed the S3 touchscreen test
to proceed. The initial drag press still lands at the old position, so it must
be checked for unintended navigation. This is a test-control problem, not
evidence of an emulator defect.

An app-created Linux bridge currently supports one running emulator. Its
temporary QEMU TAP cannot yet be reliably distinguished from an unexpected
member, so verification, repair, removal, and a second launch require it to be
idle. Strict membership checks resume after the emulator releases its lease.
