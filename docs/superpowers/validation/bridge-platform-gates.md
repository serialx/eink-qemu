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
Windows service transport and QEMU inheritance are not yet integrated.
