# Managed bridge acceptance

Status on 2026-10-23: managed bridge setup ships as **experimental** in 0.9.0
with the maintainer's approval. Native acceptance is still in progress; this
release designation does not close the remaining acceptance work below.
Linux and Windows packages include managed support. macOS release packages
do not include the signed managed helper. This is the current checklist; the
[platform gate record](bridge-platform-gates.md) contains the detailed evidence
and earlier failed attempts.

The live Windows route/profile tests and the macOS real-authorization-reference
test are opt-in native acceptance checks. They require a prepared network adapter
or an installed private bridge authorization policy, which generic CI runners
do not provide. Standard CI still runs the fixture-based route/profile tests and
forged-authorization rejection checks. A green CI run does not complete native
acceptance.

## Verified scope

Tests run in disposable VMs. The development Mac's own networking was not
reconfigured. Host readiness and firmware connectivity are separate results:
“Bridge ready” verifies the host transport, not the reader's DHCP or web service.

| Test environment | Wizard and lifecycle evidence | Firmware evidence |
| --- | --- | --- |
| macOS 26.5.2 ARM64, VirtualBuddy, bridged Ethernet | Ordinary-user wizard setup and system authorization; native removal, reboot recovery, and lease protection | C3 and S3 acquired LAN addresses; a separate peer opened their HTTP services and resolved mDNS |
| Ubuntu 22.04 ARM64, UTM shared network | Packaged wizard configuration and confirmed removal; package update, leased-removal refusal, cleanup, and reinstall with reversible network fixtures | C3 and S3 DHCP, HTTP, and mDNS address resolution on the shared virtual network |
| Windows 11 Pro x64 build 26300, UTM shared network and bridged Ethernet | Actual UAC decline and approval; wizard setup and Keep; native configure, repair, cancelled/confirmed removal, lease checks; package updates preserve a retained bridge; V40 LAN removal and native restoration comparison passed after a fresh approval retry | C3 and S3 DHCP, HTTP, and mDNS passed on both the shared virtual network and physical LAN |
| Ubuntu 24.04.5 x64, UTM shared network and bridged Ethernet | 90 ordinary-user bridge regressions; package update, native creation, verification, lease-protected removal refusal and confirmed removal; V39 normal desktop password-prompt cancellation, setup and removal passed; V30 actual wizard retry and cold-boot recovery passed; V33 wizard setup and Keep passed on Ethernet | C3 and S3 DHCP, HTTP, and mDNS passed on the physical LAN |

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
| Windows x64 NSIS installer V54 | `809297d381783143056848634b137afd21b26f38ec1670d56ed0af9c910141fb` | Update and installed-payload checks passed against files extracted from the installer; journal and archived failure evidence were preserved, and the helper is automatic and running. V46 preceding-package interrupted-setup cold-boot recovery and independent exact restoration passed |
| Ubuntu x64 Debian package V58 | `e2f17d88aeef34ab0b9d13c6e37c655e377d20cbd202e611fa525b9d5c77d191` | Archive checks, installation, unchanged recovery record, running helper, and delayed-timer regression against installed QEMU passed. V47 preceding package passed installed-service setup, lease protection, removal, and unconfirmed recovery; V57 passed adapter removal during Apply and restoration. Earlier packages passed cold-boot recovery and C3/S3 physical-LAN access |

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
That maintenance run ended with bridge support installed and no configured bridge.
The later V42 helper update installed the exact bundled payload; its fresh
setup test is covered by the subsequent successful lifecycle checks below.

The update exposed a dialog stall: an existing completed bridge operation
entered **Saving setup** without starting verification. Both support-update
paths now invoke the same verification step used by inspection. Two regression
tests reproduced the missing call before the fix and pass afterward; the
updated QA app reaches **Bridge ready** when reopening the retained setup.

Regression results before the V44 Windows membership-read change: 273 macOS workspace tests passed, with four
existing tests ignored; 90 Ubuntu x64 ordinary-user bridge tests and six native Windows idle-recovery regressions passed; 80 Ubuntu ARM64 bridge tests and 35 Windows x64
transaction/reboot-pause/cleanup tests passed. All 35 bridge UI tests,
TypeScript checking, the frontend build, and 18 packaging/probe tests passed. These suites use temporary
fixtures and do not replace native network lifecycle tests of the new packages.

## V43 Windows startup timeout investigation

The V42 Windows package reproduced the cold-boot recovery failure after a power
cut during **Preserve connection settings**, with eight mutations completed.
Operation `b318bffc7e407e3d8fe239ef86f14544` ended failed at revision 21, retaining
`undo_required=true`. The diagnostic now identifies the failing read:
`ETIMEDOUT` in **Read Windows bridge membership**. The read-only PowerShell CIM
query exceeded its 120-second bound. After startup, the same native topology
inspection completed successfully in 15.412 seconds. Saved settings, bridge
settings, and bridge address reads also completed; reading the physical IP
interface correctly failed while that adapter remained attached to the bridge.
No recovery pass is claimed for this run.

The test's own injection marker was not flushed before the forced power cut
and read back as zero bytes. The host had already captured its complete contents
and the successful stop/start result. The post-boot verifier uses those captured
values; the product's protected journal remained readable. Future power-cut
markers must be flushed before signalling readiness.

Evidence: `/tmp/eink-windows-ui-boot-v43-host.json`,
`/tmp/eink-windows-journal-afterboot-v43.json`, and
`/tmp/eink-windows-recovery-inspect-v43.log`. The latter is a read-only diagnostic,
not proof of restored networking. The V44 candidate retries only a timed-out
membership query once, taking fresh device snapshots and retaining all identity,
output, and ownership checks. Two regressions failed against the pre-fix read. All nine native Windows topology
tests pass with the candidate, including the real read-only topology query.
The V44 package update passed after comparing the installed desktop against
the actual installer payload (packaging changes its metadata). The failed
recovery journal was preserved byte-for-byte. Actual UI **Retry restoration**
completed at revision 23 with no error, resources, rollback, or cleanup remaining.
The independent native comparison passed: original physical bindings and saved
configuration matched, owned bridge devices were absent, and both configuration
and runtime changes were empty. Evidence: `/tmp/eink-windows-restore-v44.log`
and `/tmp/eink-windows-restore-v44.exit`. The subsequent cold-boot failure and its
IPv6 runtime-setting diagnosis are recorded below. Regression evidence: `/tmp/eink-topology-v44-red.log`,
`/tmp/eink-topology-v44.log`, and `/tmp/eink-package-verified-v44.exit`.

## V43 Linux UI crash observation

A bounded 30-minute debugger attachment to the Ubuntu 22.04 ARM64 app's WebKit
process captured no SIGSEGV, SIGBUS, or SIGABRT. During that window, the actual UI
loaded C3 firmware, joined PICSimLabWifi with NAT address `10.0.2.15`, displayed
live heap data, loaded its matching ELF, and reset the firmware. The debugger
detached on its scheduled timeout (`124`). This does not establish the cause or
resolution of the earlier WebKit crash. Evidence:
`/tmp/eink-webkit-gdb-v43.log` and `/tmp/eink-webkit-v43.exit`.

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

## Idle recovery lock contention fix (V41–V42)

The periodic recovery worker took the exclusive operation lock even when it
only needed to read an idle, confirmation-waiting, or reboot-waiting record.
This could reject Apply or Keep with **Another bridge operation is running**.
Recovery now reads atomic status without that lock when no action is needed.
Restoration, cancellation, and cleanup still acquire the lock and reread the
record before changing anything. Authorization grants remain single-use.

Deterministic regressions pause recovery's snapshot read while the foreground
client reviews, applies, or confirms a plan. The original code rejected review
and Keep; the initial fix also failed a separate reboot-wait lock test. All six
final regressions pass, including cancellation and an expired observation that
must not undo a concurrently confirmed operation. They also pass inside Windows
x64 (six passed in 4.72 seconds). The workspace has 273 passing tests and four
existing ignored platform fixtures; Linux has 90 passing ordinary-user bridge
tests. Evidence: `/tmp/eink-idle-recovery-red-v41.log`,
`/tmp/eink-idle-recovery-reboot-red-v42.log`,
`/tmp/eink-idle-recovery-windows-v42.log`, `/tmp/eink-workspace-v42.log`, and
`/tmp/eink-linux-x64-v22/netplan-unprivileged-tests-v42.log`.

The installed V41 Windows wizard completed administrator approval, Apply, Keep,
and **Bridge ready** on its first attempt. Operation
`5919598667625a245c77785d0f3980c6` ended at revision 32 without an error or pending
rollback or cleanup. This shared-network baseline included an IPv6 ULA and a
default IPv6 route. Evidence: `/tmp/eink-windows-ready-v41.jpg` and
`/tmp/eink-windows-operation-v41.json`.

The installed V41 Ubuntu service passed creation, confirmation, transport
verification, lease-protected removal refusal, removal, and automatic restoration
when the client exited without Keep. The watchdog restored the connection before
any further client request. Cleanup restored the original IPv6 and Netplan
profiles and removed the temporary authorization. Both lifecycle and cleanup
exited zero. Evidence: `/tmp/eink-noble-native-lifecycle-v41.log`,
`/tmp/eink-noble-native-lifecycle-v41.exit`, and
`/tmp/eink-noble-cleanup-v41.exit`.

V42 adds the reboot-wait case to the same fix. Its installed Windows update
preserved the ready bridge journal byte-for-byte, restored the automatic
service, and matched all packaged payload hashes. The Ubuntu package update
also preserved its clean journal and passed installed-file and service checks.
Evidence: `/tmp/eink-package-update-v42.exit`,
`/tmp/eink-package-update-v42.result`, and `/tmp/eink-noble-package-v42.log`.

The V42 Windows wizard also completed removal on the first administrator
approval and Keep, without a busy refusal. Operation
`3dd4a1f45df66b9158568f29d94175ac` reached revision 14 with no error, resources,
rollback, or pending cleanup. The independent native comparison matched the
original physical bindings, saved configuration, IPv4 and IPv6 runtime settings;
all owned bridge devices were absent. The archived earlier failure was unchanged.
Evidence: `/tmp/eink-windows-remove-kept-v42.json`,
`/tmp/eink-windows-restore-v42.log`, and `/tmp/eink-windows-restore-v42.exit`.

The V43 Ubuntu retest used the installed V42 package and the normal desktop
password prompt, with no temporary authorization rule. The unchanged saved test
credential was accepted using slower, grouped keyboard input. Earlier input
attempts also dropped visible path characters, so those rejected attempts do not
establish a password or polkit defect.

After actual UI Apply reached verification, the VM shut down without Keep and
cold-booted. The installed service restored the original connection automatically:
operation `9fba8c8d29d8f5aad0ce65a8e71d69f3` changed from verifying revision 18 to
cancelled revision 20, with no error, resources, rollback, or cleanup remaining.
The independent backup recovery script had not run. Cleanup then restored the
original IPv6 and Netplan profiles, removed the test timer, and verified the
normal desktop and helper service. Both recovery and cleanup checks passed.
Evidence: `/tmp/eink-noble-ui-boot-v43-observe.log`,
`/tmp/eink-noble-ui-cleanup-v43.log`, and `/tmp/eink-noble-ui-cleanup-v43.exit`.

The V44 Windows cold-boot test passed the membership query but still failed
restoration with `ECONFLICT`. Read-only inspection left the journal unchanged
and identified the active IPv6 MTU as the sole option mismatch: both migration
and original snapshots recorded 1280, while the rebooted bridge used its earlier
recorded baseline of 1500. All persistent MTU values were unset. Recovery stopped
before deleting the owned bridge. Operation `ee401f5c94b2b7fc619dff9302039b78`
stopped at failed revision 21. Evidence:
`/tmp/eink-windows-ui-boot-v44-host.json`,
`/tmp/eink-windows-recovery-progress-v44.json`, and
`/tmp/eink-windows-recovery-inspect-v44.log`. A regression reproduced the same
conflict on Windows. The correction accepts only this recorded runtime reset
when validating removal of an owned bridge. All five Windows option tests pass,
including the regression that failed before the correction. The full native
suite passed 58 tests, with two failures in
checks that assume IP transports are on the physical adapter; the retained
failed bridge still owned them. Both checks passed after bridge removal
(91.85 seconds), with the journal unchanged. Twelve fixture-specific tests
remained ignored. The V45 installer
updated the VM successfully, preserving the failed journal byte-for-byte and
verifying all installed payloads and the automatic service. Actual UI retry removed the owned bridge and restored physical bindings and
saved settings, but ended with `EHOSTNETWORK` at revision 23. The physical
adapter had IPv4 and IPv6 link-local connectivity; its earlier router-advertised
IPv6 prefix, default route, and MTU 1280 were absent. The host's Shared Network
interface also lacked that IPv6 prefix. This is a readiness failure, not a
complete recovery pass.

The disposable VM's failed record was preserved in
`qa-ipv6-network-unavailable-v45.json` with SHA-256
`54b395191aa3fdd6f7514f1d64b08163ee5a7eee206fb65c12f2498dda312059`.
A guarded QA reset required the exact failed operation, absent owned devices,
restored original bindings/saved configuration, and a closed desktop before
moving the record. It made no network changes and explicitly recorded
`originalRecoveryPassed=false`. The original V39 archive remains retained.
A new cold-boot test used the current baseline, with Ubuntu remaining on Shared
Network to keep the virtual network present.
Evidence: `/tmp/eink-windows-retry-v45.json`,
`/tmp/eink-windows-recovery-inspect-v45.log`,
`/tmp/eink-native-physical-v45.log`, and
`/tmp/eink-windows-fresh-prepare-v45.json`.

That V46 test passed with the installed V45 package. The actual wizard accepted
its first administrator approval without a busy refusal. After eight recorded
setup steps, the test cut power during settings migration and restarted Windows.
Without reopening the app or clicking Retry, the automatic service restored the
connection. Operation `a653e4586b0bb9d88a58e0fdccf84b48` ended at cancelled
revision 21, with no error, owned resources, rollback, or cleanup pending.
Ethernet was up and the unrelated TAP remained present. A separate native
comparison then matched the original bindings, saved configuration, addresses,
routes, and DNS, and confirmed that the owned devices were absent. The original
failed archive remained unchanged. Both checks exited zero.

The fresh baseline had IPv4 and IPv6 link-local connectivity; it did not have
the earlier router-advertised IPv6 prefix. Recovery was observed complete about
604 seconds after the power cut in this emulated x64 VM. This establishes
automatic recovery for this baseline, not a short disruption bound or recovery
of an unavailable external network. Evidence:
`/tmp/eink-windows-ui-boot-v46-host.json`,
`/tmp/eink-windows-ui-boot-v46-result.json`, and
`/tmp/eink-windows-ui-boot-v46-restore.log`.

The updated macOS helper completed actual-wizard setup after a fresh password
approval. Operation `6a6a00c2e88e0c0e811f5b6f52a21394` reached ready revision 7;
public inspection confirmed the selected Ethernet adapter and ready state.
The subsequent V56 removal passed after the helper updates described below.
Evidence: `/tmp/eink-macos-bridge-ready-v44.jpg` and
`/tmp/eink-macos-inspection-v44.json`. An expired review also exposed misleading
connection-change wording; the source now reports that the review expired. The
existing expiry/no-mutation regression passes.

A bounded host-native C3 diagnostic used the matching firmware ELF, NAT, and a
panic breakpoint. Ten scan/normal-restart cycles completed without a panic;
all ten screenshots show the network list. The diagnostic emulator was stopped.
This does not reproduce Windows execution timing or resolve its earlier
interrupt-watchdog failure. Evidence: `/tmp/eink-c3-watchdog-cycles-v44.log`,
`/tmp/eink-c3-watchdog-gdb-v44.log`, and
`/tmp/eink-c3-watchdog-diagnostic-v44b/shots/`.
Symbolication of the earlier Windows crash dump found that its reported stack
window starts at `systimer_hal.1` and continues through static timer objects.
Its two code addresses are timer conversion functions stored in those objects,
so they do not establish a call stack or fault location. Evidence:
`/tmp/eink-windows-watchdog-symbols-v45.log` and
`/tmp/eink-windows-panic-capture-disasm-v45.log`.
The earlier firmware and WebKit crashes remain unexplained. Release signing
remains blocked.

### Atomic journal read recovery regression (V47)

The post-merge host suite exposed a Unix recovery race. A read-only journal
handle can be opened immediately before atomic publication replaces its file.
The old inode then has zero directory links, even though the open handle still
holds a valid private snapshot. Requiring exactly one link rejected this read
with `EJOURNAL` and stopped the recovery worker. Repetition reproduced the
failure on run 23; the worker's diagnostic identified the ownership/permissions
validation, with recovery still pending in the journal.

Read-only handles now accept the unlinked snapshot. Writable lock handles still
require exactly one link; all handles still reject extra hard links, unsafe
ownership, permissions, and applicable ACLs. A deterministic atomic-replacement
test failed before the correction and passed afterward, including rejection of
an unlinked writable handle and a hard-linked record. All 100 repeated recovery
suite runs passed after the correction (300 tests). The full macOS workspace
suite passed 274 tests with five native checks ignored, and the Linux container
passed 91 bridge tests as an ordinary user. Windows compilation of all bridge
targets also passed. The installed Linux 0.9.0 package passed ordinary-user setup, lease protection,
removal, and unattended rollback after the client exited without confirming.
The independent watchdog reached cancelled state with no error, owned resources,
rollback, or cleanup pending before another client request. Fixture cleanup
restored the original network profile and removed the temporary action-specific
polkit rule. Both lifecycle and cleanup checks exited zero. The macOS 0.9.0
helper update preserved its existing ready bridge; V56 subsequently passed
removal after delayed system authorization, as recorded below.

Evidence: `/tmp/eink-recovery-race-v46.log`,
`/tmp/eink-journal-unlinked-red-v47.log`,
`/tmp/eink-journal-unlinked-green-v47.log`,
`/tmp/eink-recovery-stress-v47.log`, `/tmp/eink-workspace-v47.log`,
`/tmp/eink-linux-x64-v22/netplan-unprivileged-tests-v47.log`, and
`/tmp/eink-windows-check-v47.log`.
Installed-package evidence: `/tmp/eink-noble-package-v47.log`,
`/tmp/eink-noble-native-lifecycle-v47.log`,
`/tmp/eink-noble-native-lifecycle-v47.exit`, and
`/tmp/eink-noble-cleanup-verified-v47.txt`.

### Review expiry and Windows firmware investigation (V49)

An expired review disabled Apply/Remove without explaining how to continue.
The dialog now tells the user to choose Back and review the changes again.
Setup and removal regressions verify that expiry blocks application, displays
this explanation, and permits a fresh review. All 37 bridge UI tests pass;
the Windows 0.9.0 QA installer builds with this change.

The Windows firmware diagnostic captured an interrupt-watchdog panic while
booting the matching C3 firmware under a panic breakpoint. Its saved exception
PC is `0x40380264` in `_interrupt_handler`, immediately after enabling nested
interrupts; the interrupted handler's cause is `0x80000005`. This establishes
the captured context, not the cause of the original failure. A comparison
without debugger pauses reached Home; the subsequent idle/scan results are
recorded below. Evidence: `/tmp/eink-panic-v49.json`,
`/tmp/eink-windows-watchdog-v49-console.log`, and
`/tmp/eink-progress-v50.json`.

The earlier QA installer contained a CLI built with a dynamically linked Visual
C++ runtime that was absent in the clean VM. The default CLI packaging and CI
already request static linking. The replacement QA package follows that path;
the standalone corrected CLI launches on the clean Windows VM. Earlier
controller failures (missing runtime and PowerShell exit-code handling) were
test-harness failures, not completed firmware checks.

### Delayed timer wakeups and authorization (V51–V54)

The Windows C3 comparison without a debugger also reset at about 90 seconds
of firmware uptime. A native-host reproduction then isolated the cause:
pausing QEMU for one second during fixed-rate instruction-counted sleep made
its virtual clock skip the pending wakeup timer and expire the firmware's
interrupt watchdog. Idle clock advancement now stops at the earliest pending
guest timer. Adaptive instruction counting and sleep-off behavior are unchanged.

The new regression fails against the old binary and passes with the fix.
All 23 C3/S3 sleep and delayed-wakeup tests pass. The real C3 firmware also
survived ten one-second host pauses with no panic, restart, or change to its
Home image. Windows and macOS QA packages containing the corrected QEMU build
have been built. The Windows V52 diagnostic stayed stable through 148 seconds
of firmware time, but its controller's ten-minute wall-clock deadline stopped
the run before the requested 300 firmware seconds. Its debugger incorrectly
classified the deliberate shutdown stop as a panic; the stop PC was outside
the panic handler and no firmware panic was logged. V54 corrects that
classification and gives the slower x64 VM more time. V54 completed 300
seconds of emulated idle time without a panic or watchdog restart and exited
zero. Its six-cycle scan script only exercised three actual scans: the firmware
intentionally restarts on leaving Wi-Fi, and the next inputs sometimes arrived
before that restart finished. The corrected V56 script waits through each
restart and requires exactly one additional restart per completed scan. V56
passed all six scans using QEMU from the installed V54 package, with exactly
six intentional restarts and no panic breakpoint. Every return to Home had
the same pixel hash; the final scan screenshot shows the expected three
networks. The installed executable hashes match the extracted installer, and
the update preserved the recovery journal and running helper service.

Evidence: `/tmp/eink-c3-host-delay-result-v51.json`,
`/tmp/eink-c3-host-delay-fixed-result-v51.json`,
`/tmp/eink-icount-wakeup-red-v51.log`,
`/tmp/eink-icount-wakeup-green-v51.log`,
`/tmp/eink-sleep-regressions-v51.log`, `/tmp/eink-progress-v52.json`,
`/tmp/eink-progress-v54-final.json`, `/tmp/eink-firmware-wifi-exit-v54.txt`,
`/tmp/eink-progress-v56-final.json`, `/tmp/eink-windows-scan-6-v56.png`, and
`/tmp/eink-payload-check-v56.json`.

Repeated macOS removal attempts exposed another usability failure: the
five-minute review could expire while the system password prompt remained
open. After fresh native authorization, the helper now reconstructs the
protected plan while holding its mutation and resource locks. It proceeds only
if the complete snapshot, mutations, and displayed plan are identical, apart
from the expiry. Changed connections, cancelled or replaced operations, other
owners, and active leases remain protected. Ordinary Apply still rejects an
expired review. All 33 transaction tests and the full bridge suite pass. In the real V56
wizard test, the password prompt remained open for about nine minutes before
approval. Removal then completed successfully. The UI showed **Bridge setup
removed**, the helper returned no selected resource, and the per-user native
service, launch definition, and packet socket were absent. The helper remained
installed and ready for another setup.

Evidence: `/tmp/eink-auth-refresh-v54.log` and
`/tmp/eink-bridge-regression-v54-native.log`,
`/tmp/eink-macos-delayed-removal-v56.png`,
`/tmp/eink-macos-removal-v56.json`, and
`/tmp/eink-macos-removal-native-v56.txt`.

### Windows expired review and keyboard check (V57/V58)

The installed V54 app passed review expiry at 800 × 600. It displayed the
instruction to choose **Back** and review again, disabled Apply, and retained
the selected Ethernet connection. A fresh review completed and enabled Apply.
Escape closed setup and visibly focused the firmware control; Space reopened
its card. No network changes were requested. Evidence:
`/tmp/eink-windows-expired-review-v57.png`,
`/tmp/eink-windows-fresh-review-v58.png`,
`/tmp/eink-windows-return-focus-v58.png`,
`/tmp/eink-windows-reopen-card-v58.png`, and `/tmp/eink-review-v58.json`.

### Windows S3 idle check (V58)

The installed V54 Xtensa executable passed a full minute of S3 firmware idle
time using the matching X4 Pro image, NAT, and instruction-counted sleep. The
ordinary-user run finished with zero restarts and an unchanged Home pixel hash
(`1d68d868607742c4d6c445d1eb03491731ea966dd33523787944cb6d57f654e0`).
The smoke sequence took 430 seconds of wall time. The matching native macOS QA
binary also passed the same minute with zero restarts and the same image hash.

The preceding V57 Windows attempt hit the CLI's 605-second wall-clock limit
for a single 60-second guest wait after reaching about 32 seconds of firmware
uptime. It showed continuing firmware progress, not a restart. V58 divided the
wait into 600 bounded 0.1-second guest waits; no product timeout was relaxed.
This closes that smoke test, not the separate physical-LAN tests recorded above.
Evidence: `/tmp/eink-s3-final-v58.json`, `/tmp/eink-windows-s3-idle-v58.png`,
`/tmp/eink-s3-native-idle-v58.log`, and `/tmp/eink-s3-final-v57d.json`.

### Final Ubuntu x64 package (V58)

The Ubuntu 22.04 build includes both final fixes (`bc98329f4d` and
`a2cfe4befc`). The Debian verifier passed payload architecture, libc baseline,
permissions, and package lifecycle checks. All 32 Linux transaction tests passed
as an ordinary user with the fixture's private-directory permissions. Initial
runs without that restrictive umask were refused by the journal's ownership
and permission checks, before exercising transactions.

Installation into Ubuntu 24.04 x64 passed package-file verification, helper
permissions, service startup, and byte-for-byte preservation of the clean
recovery record. The installed QEMU passed the delayed-wakeup regression as the
ordinary `ubuntu` user: a one-second host pause did not skip the guest timer.
The regression used the same assembly fixture, compiled on the host and copied
into the VM. The updated desktop also launched as the ordinary user and rendered
in both light and dark themes; the helper was active and the temporary polkit
rule was absent. Evidence: `/tmp/eink-linux-x64-v22/final-package-v58.log`,
`/tmp/eink-linux-x64-v22/transactions-private-v58.log`, and
`/tmp/eink-noble-package-v58.log` (exit 0), `/tmp/eink-desktop-v58.txt`, and
`/tmp/eink-ui-health-v58.txt`, and `/tmp/eink-linux-desktop-v58.png`.

### Ubuntu x64 UI checks (V53)

The installed 0.9.0 app passed keyboard connection selection and review in
light theme. Its generated Netplan profile was refused before authorization or
network mutation, with the supported persistent-profile guidance. Temporarily
stopping the helper displayed package-repair instructions; restoring it and
choosing **Check again** returned to the connection chooser. Stopping
NetworkManager disabled automatic setup with an explanation; restoring it and
refreshing restored the connected adapter. Escape closed the chooser and
Return reopened the firmware controls. Both services and the original
connection profile were restored, and the backup timers were stopped.

Evidence: `/tmp/eink-linux-keyboard-profile-refusal-v53.png`,
`/tmp/eink-linux-missing-helper-v53.png`,
`/tmp/eink-linux-helper-restored-v53.png`,
`/tmp/eink-linux-missing-networkmanager-v53.png`,
`/tmp/eink-linux-networkmanager-restored-v53.png`, and
`/tmp/eink-restored-network-v53.txt`.

V55 also disconnected the selected Ethernet adapter while the real polkit
password prompt was open. After successful authentication, setup refused with
**Connect this Ethernet adapter before bridge setup.** The journal remained
awaiting authorization with no resources, rollback, or cleanup pending: no
bridge mutation had started. Restoring the connection and choosing **Inspect
again** returned to the connected chooser. Fixture cleanup exited zero and
restored the original profile and IPv6 settings. This covers disconnection
during authorization, not a cable loss after native mutations begin.
Evidence: `/tmp/eink-linux-disconnected-approval-v55.png` and
`/tmp/eink-noble-ui-cleanup-v55.log`.

### Ubuntu x64 adapter removal during Apply (V57)

The installed 0.9.0 service passed a separate interruption test after two native
mutations had completed, while the operation was still **Applying**. The test
briefly paused the helper to capture that exact checkpoint, detached the
verified disposable VM Ethernet adapter from its driver, and resumed Apply
with the adapter absent. This simulates adapter removal, not merely a cable
losing carrier. The helper retained its rollback journal and reported that
restoration needed repair while the adapter was missing. Ten seconds later the
fixture reattached it. An explicit retry by the ordinary owning user restored
the original connection and finished cancelled without an error or pending
resources, rollback, or cleanup. The temporary polkit rule, profile and IPv6
fixtures, and recovery timer were removed. Independent checks confirmed the
original profile UUID, IPv4 address, gateway, and IPv6 mode.

Evidence: `/tmp/eink-linux-at-disconnect-v57.json`,
`/tmp/eink-linux-v57-result-1`, `/tmp/eink-linux-v57-result-2`,
`/tmp/eink-noble-disconnect-v57.log`, and `/tmp/eink-noble-final-v57.log`.

### WebKit crash evidence recovered in V42

The rotated Ubuntu ARM crash-handler log explains the missing core: Apport
dropped it after it exceeded its 2308 MiB limit. The recorded resident memory
was 359384 KiB, with a peak of 472844 KiB; the dump limit alone does not establish
a memory leak. WebKit and JavaScriptCore were installed before this session and
were not upgraded around the crash. An eink-qemu package update did occur while
the earlier application session was open, but no evidence yet connects it to
the crash. The V43 debugger-backed run described above did not reproduce the crash. Evidence:
`/tmp/eink-webkit-history-v42.log` and
`/tmp/eink-linux-webkit-20261022.crash`.

The V55 follow-up attached a debugger to the main WebKit process for 30 minutes
while C3 emulation and the live heap view ran. No SIGSEGV, SIGBUS, or SIGABRT was
captured. The separate heap window also rendered more than 870,000 allocation
events and closed normally; its separate WebKit process was not attached to the
debugger. This run used the previously installed Ubuntu ARM 0.8.1 package,
so it is crash-investigation evidence, not validation of the latest package.
The original crash remains unexplained. Evidence:
`/tmp/eink-webkit-gdb-v55.log`, `/tmp/eink-webkit-v55.exit` (124 is the planned
30-minute debugger timeout), and `/tmp/eink-linux-heap-window-v56.png`.

## Remaining acceptance work

- Build and verify the advertised macOS 12.3 baseline. The current QA QEMU
  executables and bundled libraries require macOS 15.0; changing the Rust
  deployment target alone cannot close this gap. Intel macOS still needs native
  testing. Developer ID signing and notarization remain blocked at the user's
  request because the identity is unavailable; development signing is available
  for disposable ARM64 VM checks.
- Investigate the Ubuntu ARM WebKit SIGSEGV. The original core was discarded by
  Apport, and two 30-minute debugger runs have not reproduced it. The V55 run
  used 0.8.1. These runs do not establish a fix or validate the final ARM64 package.
- Complete the native failure-path coverage beyond the recorded passes. There
  is no recorded native pass for adapter removal during Apply on macOS or
  Windows, or two simultaneous Windows user sessions. Linux's adapter-removal
  test passed after an explicit owning-user recovery retry. Shared coordinator
  and ownership tests do not substitute for those native cases.

The known Windows recovery failures now have passing installed-service
cold-boot and exact-restoration evidence on the fresh network baseline. The
original failure record remains archived. Windows C3 idle, six scans, S3 idle,
review expiry, and keyboard focus checks also pass. Linux setup, normal desktop
authorization, cold-boot recovery, adapter-removal recovery, package update, and
fixture cleanup pass. The macOS helper passed setup and delayed-approval
removal, plus the earlier update, reinstall, lease, and maintenance recovery
checks. Retest these paths when later product changes affect them; completed
checks are not themselves outstanding work.

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
