# Managed bridge acceptance

Status on 2026-10-23: implementation and native acceptance are still in progress.
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
| Windows 11 Pro x64 build 26300, UTM shared network and bridged Ethernet | Actual UAC decline and approval; wizard setup and Keep; native configure, repair, cancelled/confirmed removal, lease checks; package updates preserve a retained bridge; V40 LAN removal and native restoration comparison passed after a fresh approval retry | C3 and S3 DHCP, HTTP, and mDNS passed on both the shared virtual network and physical LAN |
| Ubuntu 24.04.5 x64, UTM shared network and bridged Ethernet | 84 ordinary-user bridge regressions; package update, native creation, verification, lease-protected removal refusal and confirmed removal; V39 normal desktop password-prompt cancellation, setup and removal passed; V30 actual wizard retry and cold-boot recovery passed; V33 wizard setup and Keep passed on Ethernet | C3 and S3 DHCP, HTTP, and mDNS passed on the physical LAN |

Earlier Linux tests used an explicitly authorized temporary polkit rule for the
single bridge action. Each lifecycle fixture removed it afterward. V39 also
passed the normal desktop password-prompt flow on Ubuntu 24.04 x64, without a
temporary authorization rule: cancellation, approved setup, and separately
approved removal.

## Current packages and regression results

The current Windows and Ubuntu packages include the later macOS maintenance
UI changes, package-repair instructions, and the NetworkManager startup fix
described below. Both include the review fixes for durable cancellation during Apply, Linux checkpoint timing,
dynamic connectivity verification, and Linux's explicit **In use** state.

| Package | SHA-256 | Result |
| --- | --- | --- |
| Ubuntu ARM64 Debian package | `0fd615ef3d3abb3feecd90bf1fde91137825655a2db17fbfd78293d06013c0de` | Archive checks and update passed; package-repair UI and retry passed; preceding package passed installed-service cold-boot recovery |
| Windows x64 NSIS installer V36 | `9b3e989f418a32612fd31560cea7a32f682aa604c54e876df58bb6ea4adbe8b5` | Update preserved the failed recovery record byte-for-byte; installed payload hashes and automatic service passed; V39 fresh interrupted-setup cold-boot recovery passed. The original V22 failure remains unresolved and preserved |
| Ubuntu x64 Debian package V36 | `0f26348d17d36c4185e5512ad62e8d014ab8df593a7a6e6c69ccf5aca62b0c25` | Ubuntu 22.04 build and archive checks passed; installed payload and service checks passed; stale-service repair UI, unsupported IPv6-mode refusal, and V39 standard polkit lifecycle passed. Preceding V30 passed UI restoration and fresh cold-boot recovery; V33 passed C3/S3 physical-LAN access |

The preceding Windows package also completed the **Manage bridge → Bridge ready**
check after its update and launched the saved X4 Pro firmware. The reader reached
Home in 183.06 seconds after emulator startup. Native bridge inspection before
startup took several minutes in the emulated x64 VM. The S3 scan found three
networks; joining PICSimLabWifi assigned `192.168.64.20`. The host received HTTP
200, and a freshly reloaded browser displayed the connected reader and version
1.6.5-ko.0. mDNS resolved `crosspoint.local` to the same address on interface 41.
These results establish shared-network connectivity, not physical-LAN access.

The preceding Windows package also removed the retained bridge successfully. The
uninstaller removed its service, desktop, and helper, cleared the resource
journal, and preserved the user's SD image. Reinstall passed with matching
desktop, helper, and QEMU payload hashes and an automatic running service.

The Ubuntu ARM64 wizard completed connection selection, review, Apply, Keep,
and **Bridge ready** using keyboard navigation. A separate attempt against the
original generated Netplan connection was correctly refused before mutation.
The successful run used the reversible persistent-profile fixture described
above; it does not expand the supported profile types.
The S3 reader reached Home in 1.69 seconds, found three networks, and received
`192.168.64.20` after joining PICSimLabWifi. The host received HTTP 200 and opened
its connected reader page; mDNS resolved the same address. The emulator was
closed and the fixture cleanup completed, restoring the original profiles and
removing the temporary authorization.

The installed Ubuntu package also recovered after the desktop process was
forcibly terminated while **Keep connection** was pending. Without further
client requests, the helper restored the original Ethernet connection and
removed the temporary bridge. The observer recorded completion 3.93 seconds
after the confirmation deadline, with no resource, rollback, or cleanup left
pending. The independent fixture-cleanup timer had not fired. Reopening the app
cleared the stale saved selection and offered **Set up bridge**; its chooser
showed the connected Ethernet adapter. The test then restored the original
Netplan and IPv6 fixtures, removed the temporary polkit rule, and stopped its
backup timer.

A subsequent Linux boot test exposed a startup race: systemd considered
NetworkManager active before its profiles and devices were loaded. Recovery
failed with `EROLLBACK`; an explicit ordinary-user Cancel after startup restored
the connection. The helper now waits for NetworkManager's startup to finish
before accessing managed profiles. Three private-D-Bus regression tests failed
without the wait and passed with it. The rebuilt Debian package then passed an
actual wizard Apply followed by shutdown while confirmation was pending and a
cold start. On the new boot, the installed helper restored Ethernet and deleted
the temporary bridge, leaving no resource, rollback, or cleanup pending. The
independent backup timer had not run. The test restored the original Netplan
and IPv6 fixtures and removed its temporary policy and timer. UTM's warm reboot
stalls before Linux starts, so this establishes cold-boot recovery, not an
unattended warm-reboot pass.

The installed Windows service also recovered after the desktop was forcibly
terminated during **Keep connection**. It restored the original Ethernet
adapter, removed the temporary bridge, and preserved the unrelated TAP adapter.
The same service process remained running; the journal ended cancelled without
an error, owned resources, pending rollback, or pending cleanup. The observer
recorded completion 170 seconds after the confirmation deadline in the emulated
x64 VM. This establishes recovery, not a short disruption bound.

Native Linux version-mismatch testing exposed an unusable **Update support**
action: the backend requires package maintenance on Linux and Windows. The UI
now explains how to repair or update the platform package and offers **Check
again**. macOS retains its native install/update action. Four regression tests
failed before the fix and passed afterward; independent review found no
blockers. The rebuilt Debian package displayed the corrected update screen
against a temporary older helper and returned to connection selection after
restoring the installed helper. A separate stopped-service test verified the
missing-support screen and recovery in light theme. Both fixtures were restored,
their backup timers stopped, and no network configuration was changed.

The current Windows package also passed the stopped-service test in light theme
at 800 × 600. It displayed the Windows installer repair instructions with
**Check again**, then returned to **Choose a connection** after the service was
restored. The fixture stopped its backup process and verified the unchanged,
empty resource journal.

The Ubuntu ARM64 wizard also passed an adapter-disconnection check. After the
test took Ethernet offline, **Refresh** showed **Disconnected**, disabled its
selection, and kept **Review setup** unavailable. Keyboard navigation skipped
the disabled adapter. Restoring the original connection and refreshing showed
**Connected** again. The resource journal was unchanged and the restoration
timer was stopped. This covers chooser behavior, not disconnection during Apply.

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
existing tests ignored; 80 Ubuntu ARM64 bridge tests and 35 Windows x64
transaction/reboot-pause/cleanup tests passed. All 35 bridge UI tests,
TypeScript checking, the frontend build, and 18 packaging/probe tests passed. These suites use temporary
fixtures and do not replace native network lifecycle tests of the new packages.

## Latest validation findings

Two Windows reboot-test attempts rolled back during Apply before the planned
power cut. Neither attempt establishes reboot recovery. Both restored the
connection and left no bridge resources or pending cleanup. A native regression
then reproduced journal replacement failing with Windows “Access is denied”
while another thread read status. The helper now retries transient replacement
conflicts for up to one second while preserving the flushed candidate and the
previous durable record. Five native Windows tests pass, including concurrent
reads, a briefly conflicting reader, persistent-conflict preservation,
transaction exclusion, and lease transfer. The precise cause of those earlier
UI rollbacks has not been proven from their logs.

With V22 installed, the actual wizard reached eight completed mutations before
the guarded power cut. On reboot, recovery failed with a Windows command timeout;
the durable record still requires restoration. This is a failed cold-boot test.
Subsequent read-only inspections completed bridge membership, saved settings,
live bridge settings and bridge address checks, taking up to 60 seconds per
inspection on the emulated x64 VM. They do not identify the command that timed
out during startup. Reading the bridged physical adapter's IP profile failed
because Windows had moved its IP interfaces onto the bridge, as expected in
that intermediate state.

Recovery errors were discarding the native cause's code and diagnostic detail.
A regression test reproduced the loss. V23 preserves both, adds context to
Windows membership/settings read errors, and passes all six recovery/cleanup
tests. Its installer preserved the failed operation record exactly. The actual
UI retry removed the owned bridge and adapters and restored the original saved
settings and bindings, while preserving the unrelated TAP. Final readiness
failed with a now-visible `ECONFLICT`: the physical adapter's active IPv6 MTU
was 1500 instead of the recorded 1280. IPv4 was restored; IPv6 had only a
link-local address and no default route. Router discovery remained enabled.
The host's shared VM interface also marked that IPv6 prefix deprecated. This
suggests a shared-network availability problem, but does not establish the
cause of the original startup timeout. The failed record remains intact; no
recovery pass or new cold-boot pass is claimed.

The first Linux x64 package build was rejected by the compatibility check:
its QEMU binaries required newer glibc than the advertised Ubuntu 22.04 baseline.
The replacement build on Ubuntu 22.04 passed the archive checks and was installed
on Ubuntu 24.04. Package verification, helper permissions and ordinary-user
inspection passed; the installed UI launched and rendered.

The Ubuntu 24.04 lifecycle fixture then failed at planning, before any app bridge
mutation or temporary polkit authorization. NetworkManager migrated its native
keyfile to a persistent Netplan YAML file and a generated runtime profile, which
the preceding helper rejected. Netplan also dropped the fixture's explicit IPv6
link-local route metric, so strict fixture cleanup refused to overwrite the
changed profile. A guarded QA cleanup archived the generated files, restored the
original Netplan source, and verified the original connection UUID, IPv6 policy,
IPv4 address and gateway. The original connection was restored before the next test. This storage behavior
is documented by [Netplan's NetworkManager integration guide](https://netplan.readthedocs.io/en/latest/netplan-everywhere/).

V26 recognizes only NetworkManager's canonical UUID-specific Netplan profile
with a protected, root-owned persistent YAML source. Generic generated and
unsaved profiles remain unsupported. Netplan support is limited to automatic
or disabled IP configuration: an inactive native probe proved that its
link-local serialization loses an explicit route metric. The probe was removed
without activating its interface or changing the live connection.

All 81 bridge regressions passed as an ordinary user in the Ubuntu 22.04 build
container. The installed V26 package then passed ordinary-user inspection,
unauthorized Apply rejection, configure/Keep, verification, transferred lease
protection, and confirmed removal on Ubuntu 24.04. The resource journal was
empty afterward. The separate QA fixture initially refused cleanup because
NetworkManager omitted its explicitly recorded default `ipv6.may-fail=true`.
Correcting that fixture comparison restored the complete original profile and
runtime. Its subsequent storage cleanup exceeded the client command's default
ten-second deletion timeout after the deletion had actually completed; a guarded continuation verified the original Netplan source, connection UUID,
IPv6 policy, IPv4 address and gateway, then restarted the helper and stopped
the backup timer. The temporary polkit rule was absent. These fixture failures
are not counted as a fully clean end-to-end run.

A subsequent Ubuntu 24.04 test used the installed wizard to select Ethernet,
review the changes and apply setup. The app reached **Keep connection** before
a guarded shutdown and cold start. Recovery restored the original Ethernet
address, but stopped with `EROLLBACK` and a NetworkManager timeout, leaving
cleanup pending in the durable operation record. The independent fixture backup
had not run. NetworkManager's journal showed the original profile update
completing roughly 20 seconds after recovery began, beyond the helper's
15-second call limit.

The helper now allows up to 60 seconds for a single NetworkManager reply;
it does not retry an uncertain mutation. Update, add and delete errors also
identify the failed operation. A private-bus regression with a 16-second reply
failed under the old limit and passes with the change. All 82 bridge tests
passed as an ordinary user. The updated package passes archive, architecture, libc-baseline and permission
checks. The installed update preserved the failed operation byte-for-byte. The actual
**Retry restoration** action then passed: the app returned to connection
selection, the original Ethernet UUID was active, no temporary bridge remained,
and the journal was cancelled without an error, owned resources or pending
restoration. The independent backup had not run. A fresh installed-wizard test then reached **Keep connection** and shut down
with 87 seconds remaining. On the next boot, the installed service automatically
cancelled that same operation without an error. The original Ethernet connection
was active; both owned saved profiles and the temporary bridge interface were
absent; no resource, rollback or cleanup remained pending. The independent
backup had not run. This establishes Ubuntu 24.04 x64 cold-boot recovery with
the updated timeout. Final fixture cleanup also passed: the original Netplan source, connection UUID,
IPv6 policy, IPv4 address and gateway were restored; temporary authorization and
the backup service/timer were removed; the helper and login manager were active.

## Ubuntu x64 physical-LAN acceptance

The installed V30 package completed the actual wizard on Ubuntu 24.04.5 x64
with UTM bridged to Ethernet `en14`. Selecting `enp0s1`, reviewing, applying,
and keeping the connection reached **Bridge ready**. Operation
`a0865ae405b4fe577f6a1f2e7cb48f4e` committed bridge `eqa0865ae405b4` at revision
20 without an error.

Both CrossPoint 1.6.5-ko.0 targets were loaded through the packaged app's file
chooser. The C3 reader used keyboard navigation; the S3 reader used its
emulated touchscreen. Each joined `PICSimLabWifi` and received `192.168.1.38`
from the physical LAN. They ran sequentially and used the same emulated MAC.
For each target, the development Mac received HTTP 200, opened a fresh reader
web page showing Connected and the matching address, and resolved
`crosspoint.local` to that address. These results establish Linux x64
physical-LAN connectivity separately from the wizard's host-readiness check.

After both tests, the emulator was stopped and the guarded cleanup completed
with exit 0. The original connection UUID, IPv4 address/gateway, IPv6 profile
and runtime were restored. The temporary polkit rule, profile fixtures and
backup timer/service were removed. The normal login manager was restored, the
VM was shut down, and UTM was saved back to **Shared Network**. Evidence is recorded in the V33 entries of
the local execution ledger and `/tmp/eink-noble-lan-cleanup-v33.log`.

## Review fixes and regression checks (V35/V36)

Whole-change review found four issues, now corrected and independently reviewed:

- Linux refuses automatic conversion when IPv6 address generation depends on
  the connection or interface identity, or its effective mode is unknown. It
  admits explicit EUI-64/token mode and disabled IPv6. Refusal occurs before
  network changes; selecting an existing bridge or NAT remains available.
- Windows recovery waits for the original router-derived IPv6 MTU within its
  existing readiness deadline. This applies only when router discovery is
  enabled and the saved profile explicitly has no persistent MTU. It does not
  accept a different MTU, conceal other settings changes, or waive connectivity.
- A refused connection to a stale, installer-owned Unix service socket offers
  bridge-support repair. Endpoint ownership checks and peer authentication are
  unchanged, and the client does not delete the socket.
- The firmware card describes disconnected, repair, setup, unsupported, and
  in-use selections. Closing bridge setup returns keyboard focus to the visible
  firmware control, rather than a button inside the hidden card.

The Windows MTU regression first failed with `ECONFLICT` on the actual x64 VM,
then passed after the fix. All 55 normal Windows library tests passed; 12 tests
requiring separate native fixtures remained ignored. The host workspace passed
255 tests with four ignored, and all 89 JavaScript tests passed. Linux's admission
regressions passed in the Ubuntu 22.04 baseline container (84 bridge tests).
These results do not replace the remaining installed-service lifecycle checks.

The initial full host run exposed a race in the stale-socket test: another
native-command test could briefly retain the listener during process creation.
The fixture now waits, with a five-second bound, for the actual closed endpoint
before asserting its error classification. The complete workspace then passed.

The packaged Windows V35 focus fix also passed at 800 × 600: Escape closed the
recovery dialog, visibly focused the firmware control, and Space reopened its
card. Evidence: `/tmp/eink-windows-dialog-return-focus-v35.jpg` and
`/tmp/eink-windows-dialog-reopen-keyboard-v35.jpg`. Regression logs are
`/tmp/eink-bridge-workspace-v36c.log`, `/tmp/eink-ui-regressions-v36.log`,
`/tmp/eink-windows-native-red-v35c.err`, and `/tmp/eink-windows-all-v36.out`.

The V36 Windows installer (`9b3e989f418a32612fd31560cea7a32f682aa604c54e876df58bb6ea4adbe8b5`)
then updated the disposable VM successfully. The pending recovery journal stayed
byte-for-byte unchanged, the service remained automatic and running, the installed
desktop and helper matched the extracted installer payloads, and QEMU was unchanged.

The Ubuntu x64 V36 package
(`0f26348d17d36c4185e5512ad62e8d014ab8df593a7a6e6c69ccf5aca62b0c25`)
passed archive checks and installation into the Ubuntu 24.04 VM on Shared Network.
Installed files, helper ownership/mode, running service, and the unchanged clean
journal all passed. The development-signed macOS V36 app passed signature/bundle
checks, launched in VirtualBuddy, and opened the connected Ethernet chooser.
Its QA dependencies still require macOS 15.0 or later.

The Windows recovery record remains retained. Its original IPv6 global address
and default route are still unavailable on Shared Network; the host's shared
interface no longer has the previously observed IPv6 prefix. This observation
has not established the cause of the original startup timeout. No host network
settings were changed. This retained operation has not recovered; the later V39
fresh-baseline cold-boot test below passed.

### Installed repair checks, 2026-10-23

The Ubuntu x64 V36 app correctly classified an installer-owned stale Unix socket
as missing bridge support and displayed Debian package repair instructions with
**Check again**. After restoring the service, Check again returned to the
connected Ethernet chooser. The protected journal was byte-for-byte unchanged,
and the backup timer was stopped. Escape followed by Space reopened the visible
firmware card, confirming keyboard focus restoration. Evidence:
`/tmp/eink-noble-stale-support-v36.jpg`, `/tmp/eink-noble-stale-restored-v36.jpg`,
and `/tmp/eink-noble-return-focus-v36.jpg`.

The Windows V36 app retried the retained failed operation through the actual UI.
It entered **Restoring your connection** and then returned to **Connection needs
repair**, retaining the recovery record at revision 25. The native cause is now
`EHOSTNETWORK` after the readiness wait, rather than the previous immediate MTU
conflict. IPv4 remained usable, the unrelated TAP was preserved, and no owned
bridge resources remained. The original IPv6 global address and default route
were still absent, so this is a safe failure-path check, not a recovery pass.
Evidence: `/tmp/eink-windows-network-check-v36.json`,
`/tmp/eink-windows-recovery-journal-v36.json`, and
`/tmp/eink-windows-recovery-retained-v36.jpg`. The app was closed and the idle
Windows VM paused after the check.

The Ubuntu V37 negative-profile check also passed in the installed wizard:
NetworkManager's `default-or-eui64` IPv6 mode was rejected before authorization
or bridge mutation, with guidance to use an existing bridge or NAT. The bridge
journal remained byte-for-byte unchanged. This does not establish the normal
polkit password-prompt flow: the QA desktop has no logind session or desktop
authentication agent, and this attempt stopped at planning.

The QA cleanup initially failed its exact runtime comparison. A read-only
comparison established that the original saved profile and every runtime field
matched except for one link-local address: the fixture snapshot was taken during
address validation and recorded an empty address list. The earlier pre-fixture
baseline and current state both contained `fe80::8cf:91ff:fe4b:d342/64`. A narrowly
guarded cleanup checked that exact difference, retained the original QA record
as an archive, and restored the original Netplan storage. Final cleanup passed:
the original connection and helper were active, both fixture records and test
policy were absent, the exact recovery units were removed, and the normal login
manager was restored. Production recovery checks were not changed. Evidence: `/tmp/eink-noble-ipv6-identity-refused-v37.jpg`,
`/tmp/eink-noble-exact-diagnostic-v37.log`, and
`/tmp/eink-noble-exact-cleanup-v37.log`.

### Windows recovery investigation, V38

Read-only native inspection confirmed that the retained operation's original
saved configuration matches the current physical adapter. The only effective
interface-setting difference is IPv6 MTU (recorded 1280, current 1500), with no
persistent MTU configured. Runtime comparison reports missing active addresses
and routes: IPv4 remains intact, but the original SLAAC prefix and IPv6 default
route are absent. DNS comparison passes after normalizing duplicate entries.
The owned bridge and TAP devices are absent and the unrelated TAP remains.
The diagnostic left the protected journal unchanged.

The Ubuntu x64 VM on the same Shared Network captured three explicit IPv6
router solicitations with valid checksums and hop limit 255, but no router
advertisement during a 45-second capture (zero dropped packets). An earlier
45-second passive capture also saw a solicitation without a reply. This supports
an unavailable shared-network IPv6 router as the current readiness blocker; it
does not identify the cause of the original Windows cold-boot command timeout.
No development-host network settings were changed.

Current native reads completed in 8.7 seconds for topology, 30.9 seconds for
saved settings, and 37.2 seconds for the live profile. They did not reproduce
the startup timeout. The separate native timeout test passed in 61.3 seconds:
the guarded command and its descendant were terminated, and the recovery
journal remained unchanged. Evidence: `/tmp/eink-windows-recovery-inspect-v38.log`,
`/tmp/eink-windows-physical-v38.json`, `/tmp/eink-windows-timeout-v38.json`, and
`/tmp/eink-noble-ra-solicited-v38.log`.

### Normal Linux desktop authorization, V39

With the user's authorization, the disposable Ubuntu account received a
temporary test password and logged into the installed XFCE desktop through
LightDM. The configured session needed correction from an absent Ubuntu desktop
to XFCE. The normal desktop polkit agent then handled the installed wizard's
authorization requests; no test allow rule was installed.

Cancelling the first password prompt displayed **Authorization cancelled**.
The journal retained zero completed mutations, no rollback requirement, and no
owned resources. A fresh plan and approved password prompt completed setup and
**Keep connection**, reaching **Bridge ready**. Ordinary-user IPC verification
and transferred-lease checks passed. The first verification ran during
finalization and reported a missing resource; repeating it after the ready
state passed, so that initial result was a test timing error.

Removal required a fresh password prompt, then completed **Keep connection**
and displayed **Bridge setup removed**. Native checks confirmed no configured
resource. Cleanup restored the original IPv6 profile and runtime, restored
Netplan storage, removed the backup timer, and left the helper running. Cleanup
exited zero. The fixture now waits for the original link-local address to settle
before taking its baseline and explicitly selects supported EUI-64 generation
for the positive test; production recovery checks are unchanged.

Evidence: `/tmp/eink-noble-polkit-prompt-v39.jpg`,
`/tmp/eink-noble-polkit-cancelled-v39.jpg`,
`/tmp/eink-noble-polkit-ready-v39.jpg`,
`/tmp/eink-noble-polkit-remove-prompt-v39.jpg`,
`/tmp/eink-noble-polkit-removed-v39.jpg`,
`/tmp/eink-noble-auth-cancel-v39.json`,
`/tmp/eink-noble-ready-final-v39.json`,
`/tmp/eink-noble-ui-ready-verify-v39.log`, and
`/tmp/eink-noble-ui-cleanup-v39.log`.

### Fresh Windows interrupted-setup recovery, V39

The original failed Windows VM was shut down and preserved. A separate UTM copy
used the current Shared Network baseline. Before resetting only that copy's QA
operation record, native inspection confirmed matching saved configuration and
physical bindings and the absence of all recorded bridge devices. The old record
was archived in the protected journal directory; this preparation was not
counted as recovery of the original failure.

The installed V36 wizard created a fresh plan and requested real UAC approval.
The first attempt returned **Another bridge operation is running** without any
network mutation. A new reviewed plan and approval progressed normally. This
transient busy refusal remains a usability issue; the evidence does not identify
the lock holder.

The test forcibly powered off the copy after eight completed setup mutations,
with the next connection-settings mutation marked in flight. On cold boot, the
installed automatic service restored the connection without a client retry.
The operation ended cancelled with no error, resource, rollback requirement, or
pending cleanup. Ethernet was up, both owned TAP devices and the bridge were
removed, and the unrelated TAP remained. Verification exited zero, 359 seconds
after the recorded boot time. This establishes recovery on the current baseline,
not a short disruption bound. A separate native comparison found matching
original saved configuration, physical bindings, effective settings, addresses,
routes, and DNS, with all recorded bridge devices absent. Reopening the installed
app offered **Set up bridge** and showed Ethernet connected in the chooser.
The app was closed and the restored copy shut down cleanly afterward.

The fresh baseline had IPv4 plus IPv6 link-local connectivity and MTU 1500.
The older failed operation expected an additional SLAAC prefix, IPv6 default
route, and router-advertised MTU 1280. Therefore, this pass supports the current
recovery path but does not explain the original cold-boot timeout or make its
missing IPv6 connectivity a successful restoration.

Evidence: `/tmp/eink-windows-recovery-inspect-v39.log`,
`/tmp/eink-windows-authorization-busy-v39.jpg`,
`/tmp/eink-windows-ui-at-cut-v39.json`,
`/tmp/eink-windows-ui-power-cycle-v39.json`, and
`/tmp/eink-windows-ui-recovery-result-v39.json`,
`/tmp/eink-windows-recovery-inspect-after-v39.log`, and
`/tmp/eink-windows-coldboot-chooser-v39.jpg`.

### Windows physical-LAN acceptance, V40

The fresh Windows x64 test VM was temporarily switched to bridged Ethernet
`en14` with the user's approval. The packaged V36 app completed adapter
selection, review, UAC approval, Apply, and Keep through its actual UI. Operation
`6175bc506e543e69f937e89607ec9d0a` reached Ready at revision 32 with one resource
and no pending rollback or cleanup. The original failed VM remained stopped.

The S3 X4 Pro firmware `1.6.5-ko.0` then ran using the app-created TAP
`{9bb1b0c0-8e56-4a50-921d-8a315fb46382}`. Its File Transfer screen received
`192.168.1.38`; a request from the host returned HTTP 200, and a fresh browser
page showed Connected with the same address and firmware version.
`crosspoint.local` resolved to that address on the physical LAN interface.
Evidence: `/tmp/eink-windows-lan-ready-v40.jpg`,
`/tmp/eink-windows-s3-status-v40.json`,
`/tmp/eink-windows-s3-address-v40.jpg`,
`/tmp/eink-windows-s3-http-v40.headers`, and
`/tmp/eink-windows-s3-browser-v40.png`.

C3 X3 firmware `1.6.5-ko.0` passed the same checks afterward, using the same
managed TAP and emulated MAC. Its File Transfer screen, HTTP 200 response,
fresh browser Connected status, and local name resolution all agreed on
`192.168.1.38`. Evidence is in `/tmp/eink-windows-c3-status-v40.json`,
`/tmp/eink-windows-c3-address-v40.jpg`,
`/tmp/eink-windows-c3-http-v40.headers`, and
`/tmp/eink-windows-c3-browser-v40.png`.

The first removal attempt reproduced the transient busy refusal immediately
after UAC approval. Its journal remained at revision 1, awaiting authorization,
with zero completed mutations, no rollback or cleanup pending, and the retained
bridge resource. The native-command observer showed no helper child commands
between the completed plan and this refusal. This narrows the investigation to
transaction-lock contention but does not identify the lock owner. Evidence:
`/tmp/eink-windows-remove-busy-v40.jpg`,
`/tmp/eink-windows-remove-busy-v40.json`, and
`/tmp/eink-windows-lan-commands-v40.jsonl`.

A fresh review and UAC approval succeeded. Removal operation
`896d2dbf425435eec13513460589a8fb` completed after the UI Keep confirmation.
The app displayed **Bridge setup removed**; revision 14 retained no resources,
rollback requirement, cleanup, or error. The independent native verifier exited
zero: original saved configuration and physical bindings matched, configuration
and runtime comparisons reported no differences, and all recorded bridge devices
were absent. The unrelated TAP remained, and the original failed-operation
archive was unchanged. Evidence: `/tmp/eink-windows-lan-removed-v40.jpg`,
`/tmp/eink-windows-lan-cleanup-operation-v40.json`,
`/tmp/eink-windows-lan-restore-v40.log`, and
`/tmp/eink-windows-lan-restore-v40.exit`.

The app was closed, the test observer stopped, and Windows shut down cleanly.
UTM then saved Shared Network again; its library showed the test copy stopped
with that network mode. Evidence:
`/tmp/eink-windows-lan-restored-shared-v40.jpg`.
These successful LAN runs do not explain the earlier C3 watchdog reset or the
original recovery timeout.
Release signing remains blocked.

## Remaining acceptance work

- Build and verify the advertised macOS 12.3 baseline with release-compatible
  dependencies, Developer ID signing, and notarization. A Developer ID identity
  is currently unavailable; the user confirmed this on 2026-10-22. Development
  signing remains available for VM tests. Intel macOS still needs native testing.
- Complete the remaining Linux x64 native lifecycle/UI coverage. Windows and
  Linux x64 C3/S3 physical-LAN connectivity pass; Windows V40 confirmed removal
  and independent restoration comparison also pass.
- Retest the latest recovery changes through installed services and the actual
  wizard, including interrupted setup and reboot. Ubuntu ARM64 desktop-crash recovery
  during confirmation and cold-boot recovery pass; Ubuntu 24.04 x64 cold-boot
  recovery also passes after the Netplan timeout fix. Windows desktop-crash recovery
  during confirmation and V39 fresh interrupted-setup cold-boot recovery also
  pass. The original Windows timeout remains unexplained. Windows package removal with a retained
  bridge and subsequent reinstall also pass.
- Finish the visual and keyboard matrix, dependency-failure, disconnected-adapter,
  and version-mismatch flows. Windows testing remains at the user's requested
  800 × 600 resolution. Investigate the V39 transient busy refusal after UAC;
  retry succeeded, but the refusal is not yet explained.
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
