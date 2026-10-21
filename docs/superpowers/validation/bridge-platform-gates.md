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
The native service component is not yet connected to the durable coordinator or
the installed helper service.

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
