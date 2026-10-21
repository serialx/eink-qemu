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

- macOS: native vmnet interface discovery passed; privileged lifecycle untested.
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

These are successful Linux and x64 Windows mechanism gates, not complete
platform acceptance. Packaged service authorization, reboot recovery, end-to-end
emulator networking and the full UI remain to be validated.

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
