# Native bridge implementation gates

These gates test the proposed mechanisms, not delivery of the complete UI.
A successful compiler/unit-test run does not satisfy a native gate.

| Platform | Required evidence before backend delivery |
| --- | --- |
| macOS | App-owned socket_vmnet launchd service; ordinary user opens packet socket; cancellation and removal preserve other services. |
| Linux | NetworkManager checkpoint restores the original Ethernet profile, addresses and routes after the configuring process exits; newly created profiles removed; dedicated QEMU helper works as an ordinary user. |
| Windows | Signed matching-architecture TAP; native bridge create/destroy syntax verified; ordinary-user TAP open; original adapters restored. |

Developer probes default to inspection. Mutation requires `--exercise` (or
`-Exercise`) plus an explicitly named disposable host. Raw evidence belongs in
local test scratch; committed results omit private addresses and host identities.

## 2026-10-20 progress

- macOS (updated 2026-10-21): the disposable VirtualBuddy ARM64 clone runs
  macOS 26.5.2 build 25F84. Native socket_vmnet 1.2.2 lifecycle passed:
  app-specific launchd installation, ordinary-user QEMU-framed packet exchange,
  restart, and removal. Another user in the same socket group received EACCES.
  Removal preserved addresses/routes and an unrelated test service's PID and
  configuration. Cleanup removed all temporary services, sockets, and payloads.
- Linux: created a dedicated Ubuntu 22.04 ARM64 UTM VM, kernel 5.15.0-198,
  NetworkManager 1.36.6. A 30-second checkpoint restored the original active
  Ethernet profile, IPv4 addresses and routes after the configuring process
  exited. Both newly created bridge/port profiles were removed. On 2026-10-21,
  the dedicated `eink-qemu-bridge-helper` built natively from this worktree.
  A process with real/effective UID 1000 received a TAP descriptor, verified
  membership of the isolated test bridge, and wrote an Ethernet frame. An
  unauthorized bridge was refused. Cleanup removed the test bridge, helper
  installation and ACL; the original Ethernet addresses were unchanged. The
  ordinary QEMU helper retains its existing ACL relocation behavior; the new
  helper uses `/etc/eink-qemu/bridge/bridge.conf` without relocation.
- Windows: cloned the existing Windows 11 ARM64 UTM image, build 26300.9711.
  `netsh bridge create` accepts two adapter IDs (GUID, name or interface index).
  TAP 9.27.0 ARM64 catalog signature validated as Microsoft Windows Hardware
  Compatibility Publisher. Native installation and TAP open using a genuine
  non-administrator token succeeded. Native bridge creation and removal each
  caused a kernel bugcheck (`0x7e`, access violation). After cold restart,
  the bridge was absent and the original Ethernet adapter was up. This is a
  failed ARM64 compatibility probe, not successful recovery or x64 acceptance.
- Windows x64 (updated 2026-10-21): the separate emulated UTM VM now
  runs Windows 11 Pro x64 build 26300 with guest tools. The desktop and guest
  agent are operational. A recovery snapshot was saved before bridge tests.
  Official TAP 9.27.0 amd64 archive hash and Microsoft catalog/installer
  signatures validated; installation succeeded without requiring a reboot.
  A genuine temporary non-administrator token opened the TAP successfully
  (`elevated=False`, `opened=True`, error 0); the temporary account was removed.
  Native `create` attached Ethernet but failed to attach TAP despite exit code
  0, both with TAP disconnected and held open with media connected. A separate
  `netsh bridge add <TAP GUID> to <bridge GUID>` then succeeded; native inspection
  reported both adapters bridged. A new genuine non-administrator token reopened
  TAP and set its media connected while both memberships were present.
  The independently scheduled SYSTEM task then removed the complete bridge and
  restored Ethernet's IPv4 addresses/routes, DHCP setting and binding exactly.
  The temporary TAP process/account were closed/removed afterward. Startup
  recovery was armed, but this run did not test a reboot boundary.
  An earlier recovery guard incorrectly treated the bridge's inherited physical
  MAC as a second Ethernet device; restricting it to the recorded hardware
  identity fixed that probe error. This confirms why MAC alone is insufficient.

These are successful Apple-silicon macOS, Linux, and x64 Windows mechanism gates, not complete
platform acceptance. Packaged service authorization, reboot recovery, end-to-end
emulator networking and the full UI remain to be validated.

## macOS implementation validation, 2026-10-21

The original VirtualBuddy VM is preserved; testing used a disposable copy.
`probe-macos.c` provides read-only vmnet discovery and an ordinary-user socket
exercise. Its local tests cover fragmented stream reads, missing sockets, and
refusal of identity changes by an unelevated caller. Native testing exchanged
60-byte QEMU-framed Ethernet packets before and after service restart.

The first cross-user probe stopped because sudoers refused a non-default runas
group before reaching the socket. The corrected probe drops the root process's
real/effective UID and groups directly. A different user with the same primary
and supplementary group as the authorized user then received EACCES from
`connect`. This distinguishes access denial from an unexecuted test.

The Rust service backend also passed installation, ordinary-user socket open,
restart, conflict-aware removal, and repeated removal in the same VM. A
root-owned configuration modified outside the app was preserved and returned
ECONFLICT. Native testing found two implementation details:

- macOS's `/private/var/run` is root:daemon 0775. This exact system directory
  needs an exception to the generic no-group-write check; app directories still
  require root ownership without group/world write access, and connections
  authenticate the root peer through the kernel.
- launchd can leave a terminating service briefly visible after bootout. Restart
  waits for the previous identity to disappear before bootstrapping its replacement.

The backend checks extended ACLs as well as POSIX mode bits. Per-user socket
directories use the native ACL API and UUID identity, with exactly one
traversal-only entry. Recovery journals reject ACL write grants too.

The candidate payload is the official [socket_vmnet 1.2.2 release](https://github.com/lima-vm/socket_vmnet/releases/tag/v1.2.2).
Both downloaded archives match their published SHA-256:

- ARM64 archive: `c7bf62308fbcfdc29bdfb8373c9b1951f7ac2396446e4390919796a94972e6dc`.
- x86_64 archive: `2968a82c97e692c2d36f87230152e8018e00589c1b598e8257775adfe83800a1`.

The backend additionally pins the extracted executable hashes. Intel execution,
reboot recovery, OS authorization, and packaged UI acceptance remain untested.
The production macOS backend is connected to the durable coordinator. A native
acceptance harness passed configuration and repair with a newly constructed
coordinator between plan and apply, plus removal refusal while a transport lease
was held. It also terminated the helper process immediately after native service
creation: startup recovery removed that unconfirmed service using its journal.
The helper executable now serves authenticated local requests and runs its
startup/timed recovery independently of clients. Native tests passed ordinary
user inspection and planning, rejection of missing authorization and valid
authorization references without the required right, cancellation, and isolation
of malformed clients. Denied authorization left no installed bridge resource.

The installer uses matching app/helper code-signing requirements and SMJobBless
to retain macOS 12.3 compatibility. A local development identity signed the test
app and embedded helper successfully. Installation through the system prompt,
reboot acceptance, and distribution signing remain unverified. The release
workflow's existing ad-hoc identity is insufficient for this installation path;
an Apple-issued signing identity must be configured. Apple's
[SMJobBless requirements](https://developer.apple.com/documentation/servicemanagement/smjobbless%28_%3A_%3A_%3A_%3A%29)
describe the paired code requirements; Apple also warns about ad-hoc service
signing in its [Service Management discussion](https://developer.apple.com/forums/thread/799910).

Unix transport leases transfer a read-only lock descriptor to the desktop.
Tests confirm the lock survives sender exit and releases only when the client
closes it; helper restarts therefore cannot make an active emulator's bridge
available for removal. Full desktop session integration remains pending.

Native commands are checked against the guest's own help and resulting adapter
state. Microsoft's [netsh bridge reference](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/netsh-bridge)
describes creation and destruction, but a zero command exit status alone does
not establish complete membership or connectivity.

TAP 9.27.0 archive SHA-256:
`36e2609b7ceefedcb978ce5c48caf9e0e5af83423717c4e2e3c1d7ebca8f62a5`.

Ubuntu image: official Jammy cloud image dated 20261004, SHA-256
`86e13258765945ad792dac75fac7f02f4db9fa73a4a535ffa472266e7041fa19`.

Windows x64 evaluation ISO SHA-256:
`bc3f24086ebadc94489066b5ad78089e2cf5c3491e90e790bb81a2b199c10e38`.

Windows x64 LTSC evaluation ISO SHA-256:
`67cec5865eaa037a72ddc633a717a10a2bed50778862267223ddb9c60ef5da68`.

Windows 11 standard English x64 ISO SHA-256:
`bd4307df32bc8af33b39ccecb1174aeb345386630f89a2b86c7a4e36b55ea650`.

### Desktop integration and Linux profile checks

The wizard now calls the native Tauri commands for inspection, support installation,
planning, applying, confirmation, restoration and saving. The current macOS workspace
suite passes 221 Rust tests (3 ignored), and 14 bridge UI tests pass. TypeScript and
Vite builds pass. Native visual and packaged installer acceptance remain pending;
these results do not establish delivery of the three-platform feature.

An actual QEMU process, launched without a network device, retained the inherited
bridge lease after its launcher closed its descriptor and released it on exit.
Startup reconciliation skips resources with live leases and preserves changed
persistent configuration. The installed macOS helper passed the reboot and restart
checks described below.

On Ubuntu 22.04 ARM64 with NetworkManager 1.36.6, 42 bridge crate tests pass under
Rust 1.90.0 as an ordinary user. The native D-Bus reader round-trips the active
profile, including its variant types, through the protected journal representation.
A separate root exercise creates and removes inactive bridge and port profiles with
autoconnect disabled. It performs no activation or host connection conversion.
NetworkManager adds an equivalent legacy interface-name alias and empty VLAN lists;
comparison ignores only those defaults and still rejects edited values.

NetworkManager profile snapshots use D-Bus wire bytes inside the JSON journal.
Plain JSON serialization of nested variants failed the native round-trip test and
was replaced. Profile access and checkpoint calls follow the official
[NetworkManager D-Bus API](https://www.networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.html)
and [connection settings API](https://www.networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.Settings.Connection.html).
The Linux lifecycle coordinator, authorization service and packaging are still
in progress; the inactive-profile exercise is not a conversion acceptance run.

### Installed macOS authorization and Linux isolation checks

The signed test app installed its helper through SMJobBless in the disposable
VirtualBuddy VM after the user approved the native prompt. Cancelling an earlier
attempt returned `EAUTHCANCELLED` and left no installed helper. This validates the
development-signed test bundle, not distribution signing or the packaged Tauri app.

The first authorized configuration attempt exposed a policy error: Apple's
`authenticate-admin` rule has a zero credential timeout. The guest's authd log
recorded successful authentication, followed by expiration 0.023 seconds later
when the helper checked the transferred reference. The custom rule now requires
administrator authentication with a private, non-shared credential lasting at
most 60 seconds. Native guards destroy its rights after Apply. The helper still
checks the exact right without interaction or extension. Existing administrator
policies are preserved. The test-only old rule was backed up and replaced in the
disposable VM to exercise fresh registration. These settings follow Apple's
[authorization policy documentation](https://developer.apple.com/library/archive/documentation/Security/Conceptual/authorization_concepts/02authconcepts/authconcepts.html).

The corrected native configuration reached `Ready`. An unrelated authorization
reference was denied immediately after approval, and the ordinary guest user
verified the resulting transport. Full workspace regression: 221 passed, 3 ignored.

An actual guest reboot changed the boot-session UUID and recreated the protected
per-user runtime directory and socket. After the first local login unlocked SSH,
the ordinary guest user verified `Ready`. A separate ordinary-client gate acquired
a transferred lease, restarted the installed helper, and confirmed removal planning
still returned `EBUSY`. After releasing the lease, removal planning succeeded;
the gate cancelled that plan and reverified the existing bridge.

The dedicated Linux packet helper now accepts exact `allow UID interface` grants.
Its policy reader rejects writable paths, symlinks, malformed content, and broad
legacy grants; the ordinary QEMU helper retains its original ACL behavior. Both
helper variants built in Ubuntu. An ordinary UID 1000 process opened a TAP and
wrote a frame through the authorized test bridge. A different UID grant, a different
bridge, a writable policy, and a symlinked policy were denied. The gate removed
its helper, policy, and empty bridge, and confirmed the uplink addresses were unchanged.

A native checkpoint regression demonstrated that `DELETE_NEW_CONNECTIONS` removes
unrelated profiles created after the checkpoint. Checkpoints now omit that flag.
The same test passed with the unrelated inactive profile and original active
connection preserved. Lifecycle recovery must delete only recorded app-owned UUIDs
whose settings still match the journal. The Linux lifecycle backend remains unfinished.

The wizard was visually checked at 1000×640 with a fixture backend in light and
dark themes. Connection selection, review, confirmation, Ready, and management
controls fit; Keep was disabled when the deadline elapsed. This is UI validation,
not packaged native acceptance.

### Linux live network preservation

On the same Ubuntu 22.04 ARM64 VM, a native conversion test now checks live
addresses, routes, ordered DNS settings, MAC address, and MTU, then restores the
original profile and verifies its live state. The first run detected a changed
DHCP address and Ethernet route metric 100 becoming bridge metric 425. Pinning the
uplink MAC preserved its DHCP address; recording the effective route and DNS
priorities preserved routing. This follows NetworkManager's documented
[device-dependent route defaults](https://www.networkmanager.dev/docs/api/latest/settings-ipv4.html)
and [bridge MAC setting](https://www.networkmanager.dev/docs/api/latest/settings-bridge.html).

The cloud image's original IPv6 profile uses `ignore`. Its Ethernet interface
disables router advertisements, whereas a new bridge inherits a different kernel
default and acquires additional IPv6 routes. Automatic conversion now rejects
unmanaged IP configuration before mutation. The successful conversion exercise
explicitly prepares a managed link-local IPv6 fixture inside the checkpoint; it
does not claim support for converting `ignore` profiles. The outer checkpoint
restores the original unmanaged profile and exact live host state afterward.

NetworkManager also emits an equivalent string MAC alias and empty wired defaults.
Comparison accepts only matching aliases and equivalent empty/false defaults;
tests retain conflicts for changed MAC, negotiation, blacklist, and s390 options.
The final native conversion and restoration both passed. All 47 Linux bridge tests
passed. Production lifecycle integration and packaged acceptance remain unfinished.

### Durable lifecycle and removal checks

The signed macOS test app completed authorized removal. The owned launchd service
and `socket_vmnet` process were absent afterward, and ordinary-user helper
inspection returned no configured resource and `NeedsSetup`. An unrelated
authorization reference was still denied. An earlier approval arrived after the
five-minute plan deadline and correctly returned `ESTALE` without mutation. The
disposable-host test harness now refreshes the plan after the password prompt and
requires every reviewed field to match; production expiration is unchanged.

Linux native lifecycle checks passed for Cancel, Keep followed by removal, and
recovery after process exit at each of the five mutation steps and immediately
after native commit. Recovery preserved deliberate external profile edits, both
before and after Keep, and disarmed the checkpoint timer instead of allowing it
to overwrite those edits later. A stale MTU plan was rejected before mutation.
Checkpoint identities include the boot, system bus, and NetworkManager owner so
a recycled object path cannot affect another operation. The current native Linux
bridge suite passes all 53 tests.

Boot testing exposed a separate Netplan persistence problem: the generator
recreated the original profile, discarding the temporary IPv6 test settings.
Recovery refused the changed profile. After explicitly restoring the recorded QA
fixture, recovery and final original-profile restoration passed. Automatic setup
now rejects temporary, unsaved, and generated profiles, including Netplan profiles
promoted to `/etc` that can still be regenerated at boot. Native rejection left
the profile and live network unchanged. The check uses NetworkManager's
[profile storage metadata](https://www.networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.Settings.Connection.html).

With a separate persistent NetworkManager fixture, recovery from the durable
journal passed after a cold boot with a different boot ID and no surviving native
checkpoint. UTM warm reboot stalled at the EFI handoff before Linux started; a
cold start was required. This establishes recovery across boot, but does not count
as successful unattended warm reboot or installed-service startup recovery.

Removal now captures a separate undo state for the working bridge. Native Cancel
restored that bridge, and forced process exits after checkpoint creation and each
of four removal steps recovered it from the journal. Recovery also passed after
native removal commit, when the checkpoint was already gone. An external edit
caused recovery to refuse mutation and disarm its timer. After these checks, the
QA harness restored the original Netplan configuration and unmanaged IPv6
profile and removed its temporary storage and lifecycle records.

These checks exercise the native components. Linux service integration, Windows
production implementation, and packaged application acceptance remain unfinished.

### Linux service, repair, and startup checks

The installed Linux helper now serves kernel-authenticated requests, authorizes
Apply through the fixed polkit action, and runs recovery independently of the
client. Its systemd service announces readiness only after startup reconciliation
and socket creation. An ordinary account completed Configure, Keep, Verify,
transport lease acquisition, and Remove through that service. Without an agent
or an authorizing policy, Apply returned `EAUTHDENIED` and made no network change.
Positive authorization tests used an explicitly approved temporary policy for
one test account and one bridge action; the harness removed it after every use.
This tests policy enforcement, not the desktop password prompt.

The service restored an unconfirmed conversion after client exit and after a
helper restart. An ordinary client's transferred lease continued to prevent
removal across a helper restart. Removal succeeded after the client released it.
After simulated power loss, the installed service restored an interrupted
conversion before announcing readiness. A separate confirmed bridge survived a
cold boot with a different boot ID and passed ordinary-user verification and
lease acquisition. The VM still requires a cold start because its warm reboot
stalls before Linux; these results do not establish successful warm reboot.

Native repair recreated missing owned profiles. Cancel and forced exit after
each mutation restored the prior state. Startup activation preserved externally
edited profiles and resumed unchanged inactive profiles. Cancelling repair of
an inactive bridge restored its inactive state. Removal also works when owned
profiles are missing or inactive; cancellation restores that prior state rather
than inventing a working bridge. All removal interruption points and preservation
of external profile edits passed again after this extension.

Committed DHCP/SLAAC verification permits lease renewal while still requiring
active owned profiles and configured IP families. In-flight conversion and
rollback retain exact host address, route, and DNS checks. Static runtime checks
remain exact. An inactive committed bridge returns promptly for repair instead
of exhausting the client's inspection timeout. The native Linux bridge suite
passes 54 tests.

Windows production provisioning, package lifecycle integration, and all three
packaged desktop acceptance runs are still required before delivery.

The final coordinator run caught an activation timing issue after cancelling
removal: the port was active before its bridge finished activating. Rollback now
waits for both native activation results. The full coordinator lifecycle then
passed, and the QA harness restored the original Netplan and IPv6 state. All
network fixture records and the temporary authorization policy were absent.
The installed test helper remains available for package acceptance.

Shared protocol, authorization contracts, journal transactions, and watchdog
tests also passed natively on the Windows x64 VM. Test executables linked with
the static C runtime; the first dynamic-runtime attempt could not start because
the VM lacked its runtime DLL. These shared tests do not establish Windows
provisioning or UAC acceptance.

Windows journal files now validate ownership and ACLs on open handles and reject
reparse points, hard links, and untrusted read/write grants. Native x64 tests
passed for those checks. Resource leases use Windows file-sharing restrictions:
a transferred handle retained by a child process blocks removal after the
parent closes its copy, permits another reader, and releases after the last
holder exits. The native child-process test passed, along with the shared
transaction and recovery suites. This is lease infrastructure; authenticated
Windows service dispatch and provisioning remain pending.

Native Windows named-pipe tests now pass for explicit SYSTEM ownership, restricted
client permissions, kernel caller identity, bounded partial requests, and
read-only lease transfer. A temporary standard account successfully connected
without elevation, could not create another server instance, and rejected a
caller-owned fake endpoint. The account was removed and its absence verified.
The first endpoint check failed because LocalSystem's default object owner was
Administrators; the pipe now explicitly sets SYSTEM as its owner.

The core now passes Windows leases to the child through its otherwise unused
standard input handle. A native child-process test read EOF, retained the lease
after all parent handles closed, blocked an exclusive removal open, and released
it on exit. QEMU's Windows stdio reader now stops at EOF without closing that
borrowed process handle; a C harness exercises the actual reader with EOF,
error, and data-then-EOF inputs. All four core network tests passed on Windows
after restricting Unix path fixtures to Unix. This establishes transport and
launch plumbing, not an actual packaged QEMU firmware run or UAC acceptance.

Windows discovery now pairs interface GUIDs with SetupAPI device instance IDs,
native service/hardware identifiers, and permanent hardware addresses. Fingerprints
change when the underlying device is replaced. The native VM check identified
the Ethernet adapter as eligible and its preexisting TAP as excluded from
automatic setup. The same discovery succeeded under a temporary standard user;
cleanup was verified. Display descriptions no longer determine TAP identity,
and existing TAP adapters are not treated as app-owned resources.

The Rust ordinary-user TAP probe passed against the pinned 9.27 release. It
opens the exact native GUID with QEMU's requested access and sharing mode,
queries the driver version, and closes the handle without enabling media.
Managed Windows verification and acquisition now require this probe in the
desktop process. A failed access check therefore prevents saving readiness or
starting the emulator without elevating either process.

QEMU now accepts an explicit `guid:{...}` TAP target for managed launches;
legacy display-name selection remains available. A C test validates its exact,
bounded syntax. This avoids resolving a changed display name to a different
adapter after setup verification. Both Windows QEMU architectures have now been
built from the patched source and passed the native lease test described below.

### Dedicated Windows TAP installation

The native x64 test now creates a dedicated TAP device using a recorded resource
ID in its device instance path. The helper verifies pinned 9.27 INF, catalog,
and driver hashes, checks the catalog with Windows trust verification, and holds
the protected package open against replacement during installation. Corrupted
payloads and an unsigned catalog were rejected in native tests.

Installation stages the package and calls `DiInstallDevice` for the exact new
device and selected driver. Calling only `DIF_INSTALLDEVICE` initially left the
adapter inaccessible to a standard user; the complete installation sequence
fixed that without custom ACL or registry permission changes. A real temporary
standard user then opened the new device and queried its version. The account
was removed, and its absence was verified.

The lifecycle test also passed rejection of a mismatched recorded GUID, removal,
repeated removal, and preservation of every original device, including an
existing TAP adapter that remained usable. A separate test terminated the
installer process immediately after device registration, before driver
installation or a returned network GUID. Recovery used the persisted resource
ID to remove that incomplete device and restored the original device list.

The shared coordinator now provides an opt-in phase for irreversible cleanup
only after confirmation has been saved. Synthetic transaction tests passed on
macOS and native Windows for cancellation before confirmation, a crash after
deletion, blocked new leases, and cleanup conflicts that wait for an explicit
owner retry. Pending cleanup uses a journal version that older helpers reject;
completed records retain the prior format. The dialog hides cancellation during
finalization and offers a distinct cleanup retry. This phase is infrastructure
for the Windows backend and is not yet a packaged removal test.

These tests cover dedicated-device provisioning, not Windows bridge membership,
UAC, the production service, or packaged UI acceptance. A device deletion must
run only with a durable cleanup record: recreating it does not preserve its
old network GUID. The shared driver package is deliberately retained.

### Native Linux desktop flow

The current Tauri app built and ran under Xfce in the disposable Ubuntu VM.
Keyboard-driven UI testing completed connection selection, review, Apply,
the restoration countdown, Keep, saved readiness, and confirmed removal.
The helper then reported no configured resource, and the fixture restored the
original Netplan and IPv6 state. The temporary action-specific authorization
policy and all network fixture records were absent after cleanup.

This run found and fixed two UI state bugs: polling a pending plan skipped its
review, and reopening a completed removal tried to save a nonexistent bridge.
Regression tests cover both paths; the bridge UI suite now passes 16 tests.
The native app displayed both successful setup and removal results. This was a
source build with the production helper and embedded UI, not packaged installer
or guest firmware networking acceptance. Authorization still used the approved
temporary test policy rather than a desktop password prompt.

### Windows native membership, address preservation, and approval

Native Windows checks now identify member adapters through their `ms_implat`
CIM bindings and the bridge through its separate `ms_bridge` binding. Both
requested members must be present. In this VM, `netsh bridge create` sometimes
returned success after attaching only its first adapter; an explicit add was
needed for the second. Bridge enumeration also lagged behind command completion.
The implementation must verify native identities and membership after each step.

Creating with the dedicated TAP first provides an ownership marker for recovery,
but initially changes the bridge MAC and the host's DHCP address. A native probe
successfully set the bridge's existing `NetworkAddress` property through its
exact CIM identity. The before/during/after comparison then passed for IP
configuration, addresses, routes, DNS, interface metrics, MTU, and MAC, allowing
IPv6 privacy-address rotation. Recovery restored the physical connection and
removed the dedicated TAP. These results cover the VM's DHCP/SLAAC configuration;
static-address migration and the production transaction remain unfinished.

Recovery ownership tests reject a physical-only bridge, unrelated members,
multiple bridges, changed device identities, and a TAP belonging to another
resource. A new bridge GUID alone is insufficient evidence of ownership. Four
native topology tests passed. The bounded native command runner also passed
output-limit and timeout tests, including termination of a child process.

Windows approval now has a bounded, one-use ticket store. Tickets bind the
protected reviewed plan and revision to the original caller's SID, process ID,
and process creation time. Approval requires administrator elevation on both
the pipe token and the pinned process token. Native tests denied a standard
account and a restricted token with the same SID as an administrator process.
Replay, expiry, changed plans, another process, and helper restart all invalidated
approval. The combined native regression batch passed 37 tests across approval,
transport, protocol, transactions, and reboot handling.

The reboot prerequisite state now waits for an explicit owner action after
restart. Startup recovery and the watchdog do not continue host-network changes.
Native coordinator tests passed, and the desktop guard permits that continuation
while still rejecting repeated Apply requests during ordinary setup.

The desktop now launches the fixed installed approval helper through UAC. It
pins protected installation paths and the executable against replacement,
rejects reparse points and hard links, and distinguishes cancelled approval.
Three native tests passed for path validation, pinning, and cancellation/token
validation. The actual UAC prompt and service round trip remain untested.

The production bridge-address operation also passed its native test: stale
state was rejected, the physical MAC was preserved, replay made no extra
change, and conflicting state was rejected. The complete before/during/after
network comparison and adapter cleanup passed.

An additional probe created the bridge from two owned TAPs before attaching
Ethernet. It retained bridge identity through temporary removal and cancellation,
but failed network preservation: IPv6 addresses/routes disappeared during setup,
and Ethernet received an IPv4 link-local address while detached. Final recovery
restored the original network and removed both TAPs. The revised sequence now
preserves the MAC after attaching Ethernet, disables the bridge's host IP
bindings before temporary removal, and restores them after reattachment.
Native production membership operations passed creation, attachment, temporary
removal, cancellation, destruction, and replay. Full network comparisons passed
after setup, temporary removal, and cancellation; cleanup restored the original
connection and removed both owned TAPs.

The Rust IP-binding operation separately passed disable, restore, and replay
for IPv4 and IPv6 on an isolated bridge. The complete binding map returned to
its original state, and Ethernet remained unchanged. This test did not attach
Ethernet to the bridge.

Four native ownership tests passed for the one-, two-, and three-member
intermediate states. Runtime verification now requires usable address states;
tentative or duplicate addresses cannot satisfy readiness. Four native profile
regressions passed, including privacy-address rotation alongside deprecated
addresses. Earlier recorded profile probes did not capture address state.

The Windows dispatcher now connects authenticated peers, protected plans, the
approval store, and coordinator leases. A native pipe-peer test with a backend
that makes no network changes passed: unapproved or stale tickets were refused,
approved tickets allowed one mutation, restart invalidated approval, and a
verified selection returned a lease. The actual UAC prompt and installed
service remain untested.

The native service loop also passed separate-connection authorization, approval
invalidation across a service-loop restart, a client lease surviving that
restart, and restoration of unconfirmed setup when the loop stops. This test
uses the real pipe transport and journal with a backend that does not change
networking. It does not test SCM installation or the complete native backend.

The restoration snapshot now captures persistent address/route lifetimes and
NetBIOS/WINS settings. Old snapshots cannot authorize migration without this
metadata. The snapshot now also distinguishes unset persistent interface options
from active defaults and separates manually configured WINS servers from
runtime values. An initial native test exposed null persistent properties; the
corrected schema passed eight profile tests, including a live dual-stack read.
A native PowerShell parser test also passed with configuration strings containing
quotes and command syntax treated as data. Complete migration remains unfinished.

The native options setter passed stale-state rejection without mutation, changing
and replaying active MTU/manual metrics and IPv4/IPv6 DNS settings, restoring the
original settings, and repeated restoration on a dedicated TAP. Cleanup removed
that adapter. Three additional native regressions passed planning compatibility,
PowerShell parsing/data literals, and rejecting persistent-option conflicts both
at preflight and immediately before a setter. NetBIOS/WINS changes were skipped
because the disconnected fixture did not expose those properties. A subsequent
connected-TAP test passed DHCP changes, persistent MTU/manual metrics, and
NetBIOS/WINS changes, including replay and adapter cleanup. The same options
survived a full Windows restart; the startup verifier removed its TAP and
scheduled task afterward.

A later snapshot correction records each protocol's configured DHCP mode from
its interface registry key. Windows can report a null persistent-interface DHCP
property even when that configured value is present. Nine profile regressions,
four route-source checks, four options checks, and two static-planning/parser
checks passed natively with this correction. A second connected-TAP test
passed mutation, replay, conflict detection, and cleanup with the updated DHCP
setter. The reboot test used the earlier setter. Configured DHCP mode is checked
separately from the active interface mode: Windows can report different values
for an isolated bridge before it has a live Ethernet member.

The static-address setter passed native IPv4/IPv6 address and route creation,
replay, refusal of a conflicting route, and usable-address verification on a
connected dedicated TAP. The test stopped its media holder and removed the
adapter. Typed comparisons confirmed that the physical adapter's configuration
and runtime network remained unchanged in both the prototype and Rust lifecycle
tests. A disconnected TAP silently ignored DHCP disable, so static setup now
checks that DHCP actually stopped before creating addresses.

Route discovery needs additional classification before full migration: Windows
reports protocol 3 for manual, DHCP, and router-advertised routes. The native
route table identifies IPv6 router advertisements, but reports the VM's IPv4
DHCP default route with manual origin. The DHCP gateway registry metadata
identifies that route; protocol or origin alone cannot authorize copying it as
persistent configuration. The source validator now rejects unsupported active-only routes before conversion.
Its live native test accepts the VM's DHCP and router-advertised routes while
regressions reject temporary routes, changed native evidence, and a temporary
manual default-route metric.

A protected volatile registry key provides the Windows boot identity without
depending on the wall clock. Native tests passed stable reopening and rejection
of an untrusted write grant; their temporary registry keys were removed. A full
Windows restart changed both the OS boot time and this identity. The test also
confirmed that Ethernet returned and no temporary TAP devices remained.

Saved configuration is also checked through the persistent IP store and native
DNS API. An isolated bridge test passed reading and verifying those settings
with both host IP bindings disabled, rejecting a changed NetBIOS setting,
restoring the bindings, and removing both test TAPs. This allows removal to
check for outside changes after active IP and DNS-client objects disappear.

The SCM lifecycle adapter passed native service start/stop twice, waiting for
cleanup before reporting stopped, and reporting a deliberate startup failure
with a service-specific error. The temporary LocalSystem service was deleted.
This fixture did not configure networking. A subsequent native coordinator test
passed configuration followed by cancellation and restored the original host
network. The retained-resource lifecycle then passed configuration and Keep,
lease protection, repair after disabling IPv6, cancelled removal, and confirmed
removal. Repair and cancelled removal restored every recorded bridge binding.
Final cleanup restored the physical adapter's configuration and runtime network,
removed the owned TAP devices, and disarmed the test recovery task.

The first removal run exposed a native dependency: disabling both IP bindings
also disables the SMB client and server bindings. Cleanup now requires that
exact expected map while continuing to reject changes to unrelated bindings.
The corrected recovery completed, and the full fresh lifecycle passed. The
installed helper subsequently started through SCM from its fixed Program Files
location, verified its pinned driver, and served inspection to both LocalSystem
and the logged-in guest user. That client was denied Apply without approval
and cancelled its untouched plan. Its token was later found to have administrator
privileges, so that run does not establish standard-user isolation. A separate
native test created a temporary standard account and checked its token. That
account could inspect the installed service but was denied all package
maintenance verbs, direct service cleanup, and Apply without approval. The
account was deleted afterward and the service remained running.

Native package-maintenance tests passed stopping and replacing the helper,
restarting after a forced process crash, preserving a disabled service during
installation, removing a disabled service, repeated removal, and reinstalling.
A separate native test verified that concurrent maintenance is rejected.
Installation with absent recovery directories created private administrator/
SYSTEM storage and started the service; cleanup restored the original journal.

The production NSIS installer then passed installation, an update that replaced
the running service process, removal, and reinstallation in the x64 VM. A
requested custom destination was ignored in favor of the fixed Program Files
location. A subsequent clean-directory run exposed the inherited Windows
Modules Installer permission on Program Files. Installed-code validation now
accepts that exact service SID; journal, IPC, and mutex checks remain unchanged.
Native regression tests reject unrelated service SIDs and user write grants.
The corrected package passed fresh installation, concurrent install/update/
removal refusal, update, removal, reinstallation, and the temporary standard-user
test. Removal deleted packaged executables and drivers, unregistered the
service, and preserved its recovery journal. The archive contained both QEMU
architectures, the CLI, helper, pinned driver, and license notices. This package
was cross-built with cargo-xwin and was unsigned. These lifecycle tests used an
empty resource journal and do not establish package cleanup of a retained
bridge. Real UAC approval, service recovery during native mutations, and packaged
desktop acceptance remain separate gates; these results do not yet establish
a working Windows setup wizard.

### Real Windows QEMU lease validation

Both `qemu-system-riscv32.exe` and `qemu-system-xtensa.exe` passed the same
native test under a temporary standard account. Each opened the exact owned TAP
GUID, emitted its QMP greeting, and retained the sole inherited read-only
bridge lease after receiving EOF on stdin. The lease blocked mutation until
QEMU exited, even while its launcher stayed alive. The same TAP then reopened
successfully. Both test runs removed the temporary account and owned adapter.
The child processes were contained in a job that terminates them if the test
launcher fails.

The first run exposed a TAP-specific error mapping: an occupied TAP returns
Windows error 31 from `CreateFile`, corresponding to the driver's
`STATUS_UNSUCCESSFUL` exclusive-open response. The access check now reports
that opening failure as unavailable/busy; error 31 from an IOCTL remains a
transport failure. The successful retry also verifies reopening after QEMU
exits. See the [driver's exclusive-open implementation](https://github.com/OpenVPN/tap-windows6/blob/master/src/device.c).

These tests used `-M none` with a TAP backend. They validate native access and
lease ownership, not firmware DHCP or reachability from another LAN device.

### Ubuntu 22.04 packaging baseline

Both QEMU targets, the CLI, and the bridge service built natively on Ubuntu
22.04 ARM64. Staging checks ELF architecture, required GLIBC versions, and
library search paths before replacing an existing bundle. The staged executables
require at most GLIBC 2.34; the heaptrace plugin requires 2.17. All executables
started as the ordinary guest user, and both x3 and x4pro boards ran their ROM
startup smoke tests. Six packaging tests cover rejected inputs and preservation
of the previous bundle when publication fails.

Linux QEMU and CLI CI jobs now use native Ubuntu 22.04 runners for both
architectures; both QEMU jobs run the network-backend and board smoke checks.
The ARM64 Debian package also passed archive verification of payload bytes,
architecture, GLIBC requirements, file permissions, and maintainer scripts.
Only the dedicated packet helper receives setuid permission during installation.
The service, desktop, CLI, and QEMU remain ordinary executables.

Native install/update/removal tests passed against the rebuilt package with
SHA-256 `dda90fb8993a18332f4ce50f5b0bd161419551544f5548c89605aa488b1b6577`.
The ordinary client was denied before the temporary test authorization was
installed, then configured and verified a managed bridge. A transferred lease
survived package update and prevented removal. After lease release, package
removal restored the original IPv6 configuration and Netplan storage; reinstall
started with no managed resources. Cleanup removed the temporary authorization
and network fixtures. These tests used the real installed helper and package
hooks, including exclusive service locking and conflict-aware cleanup across
recorded owners. A separate package update test preserved a disabled, stopped
service and replaced the helper process when the service was enabled. Desktop
package acceptance and native x64 validation remain pending.
