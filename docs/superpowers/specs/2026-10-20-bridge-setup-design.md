# App-managed bridge setup

Status: approved on 2026-10-20. Implementation and native acceptance are in progress;
see the [current acceptance checklist](../validation/bridge-acceptance.md).

## Outcome and scope

A desktop user can put the emulated reader on their LAN by selecting Bridged,
choosing a host adapter, and approving an operating-system administrator prompt.
They do not install dependencies themselves, edit network configuration, set
environment variables, or launch the app from a terminal.

The user selected app-managed setup and delivery on macOS, Linux, and Windows
together. One wizard provides discovery, installation, verification, repair,
and removal. Native backends perform the different host operations.

The following coverage is proposed, rather than an assumption that every host
configuration can be converted automatically:

| Platform | Initial managed setup | Boundary |
| --- | --- | --- |
| macOS, Intel and Apple silicon | Bundled `socket_vmnet`, an app-owned launchd service, and a vmnet-supported physical interface | Offer only interfaces reported as supported by vmnet |
| Linux | Create an Ethernet bridge through NetworkManager; also select an existing kernel bridge | Other network managers can use existing bridges; automatic conversion of their configuration is outside this release |
| Windows x64 | Install a signed TAP driver if needed, create a dedicated adapter, and bridge it to Ethernet | Detect bridge-creation support at runtime; do not assume every Windows build or Wi-Fi driver supports the operation |

The first Linux desktop package is a Debian package for Ubuntu 22.04 and newer,
on x64 and ARM64, matching the existing native QEMU/CLI build architectures.
Other distributions can build from source. Native Windows ARM64 driver support,
automatic conversion of bonds/VLANs/802.1X connections, and automatic Linux
Wi-Fi bridging are outside the initial managed-creation scope. Existing
compatible bridges remain selectable after validation.

NAT remains the default. Selecting Bridged is explicit, and failure never
silently changes it back to NAT. The browser build keeps its current networking.

## Current behavior and integration points

The firmware card in `eink-qemu/desktop/index.html` provides NAT and Bridged
buttons. `src/setup.ts` paints the selection and links to manual instructions.
`src-tauri/src/settings.rs` persists only the mode.

`core/src/session.rs` constructs platform-specific QEMU arguments. `X4BR`
selects an interface and `X4NET` replaces network arguments. The current macOS
documentation requires users to start a privileged helper and launch the app
with a socket argument in its environment.

The desktop release workflow packages macOS and Windows. Linux desktop
packaging must be added to deliver this feature to Linux users.

## User flow

1. Selecting Bridged opens **Set up bridge** when no usable configuration exists.
   A previously saved configuration is rechecked instead of opening the wizard
   on every launch.
2. **Choose connection** lists friendly adapter names, link status, and a short
   explanation for unavailable choices. The active eligible adapter is
   suggested, but users choose the adapter. Refresh handles USB adapter changes.
   Existing bridges are distinguished from adapters requiring a new bridge.
3. **Review setup** names the selected adapter and lists concrete changes:
   helper/driver installation, service creation, or host bridge creation.
   On Linux and Windows it explains that the selected host connection can
   briefly disconnect. The action is **Install and configure** or **Configure**
   according to the plan.
4. The OS handles administrator authentication. The application never asks
   for an administrator password in its own form.
5. **Setting up** shows named stages, such as installing a component, creating
   a bridge, and checking access. Cancellation before changes is immediate;
   cancellation after changes requests restoration and reports its outcome.
6. **Ready** means the selected backend can be opened by an ordinary app
   session and the host configuration passed its checks. It does not mean the
   reader has connected or received an address.
7. The firmware card shows the chosen connection and **Ready**, **Needs setup**,
   **Adapter disconnected**, or **Needs repair**, with **Manage…**.

Manage provides **Check again**, **Change connection**, **Repair**, and
**Remove setup**. Technical diagnostics are expandable and copyable.
Changing modes or disconnecting an adapter does not automatically delete setup.
Changing a physical adapter always requires a new reviewed setup plan.

On Start, a bridge preflight runs again. If blocked, offer **Open bridge setup**
and an explicit **Use NAT** action. Configuration changes are unavailable while
the emulator is running.

The completion view still tells users to join `PICSimLabWifi` inside their
firmware. Generic guest IP detection is outside this change: the app must not
infer an IP from the helper's address or a host-side readiness result.

## Architecture

Use the existing TypeScript UI and Tauri Rust boundary, with a separate bridge
module instead of adding provisioning logic to `setup.ts` or the large
`src-tauri/src/lib.rs`.

- **UI controller:** wizard state, choices, progress, focus management, and
  recovery actions. Use the application's existing dialog and styling patterns.
- **Unprivileged coordinator:** enumerate capabilities, prepare a typed plan,
  save preferences, supervise operations, and resolve a verified launch target.
- **Platform provisioners:** macOS, Linux, and Windows implementations of
  inspect, plan, apply, verify, repair, and remove.
- **Privileged helper:** perform a fixed set of host changes and maintain
  protected ownership/recovery records. It does not accept arbitrary shell
  commands, executable paths, or unrestricted file writes from the WebView.
- **Core network target:** represent direct bridge, macOS stream socket, and
  Windows TAP selection explicitly. Compose QEMU arguments without changing
  the parent process environment.

The public operations are `inspect`, `plan`, `apply`, `verify`, `cancel`, and
`remove`. Plans contain discovered adapter identities, required components,
expected preconditions, an operation ID, and a readable change summary.
Apply rechecks those preconditions so that a hotplug or intervening host change
cannot apply a stale plan to a different adapter.

Each operation has a monotonic revision and a state:
`inspecting`, `awaitingAuthorization`, `applying`, `verifying`, `ready`,
`rollingBack`, `cancelled`, or `failed`. Progress events and snapshots use the
operation ID/revision to reject late responses. Serialize host changes across
app windows and processes.

Persist a versioned bridge selection with platform, adapter identity, managed
resource ID, and display name. The privileged ownership journal is authoritative
for removal; user-editable preferences cannot authorize deletion of resources.
Readiness is observed, not persisted as an indefinitely trusted boolean.

Keep existing CLI and environment behavior compatible. For desktop launches,
an active `X4NET` or `X4BR` override is visibly identified as an external override;
the UI does not claim the saved managed selection controls that launch.
Do not add a second provisioning wizard to the CLI in this change.

## Platform behavior

### macOS

Bundle a pinned `socket_vmnet` build for each shipped architecture. Install it
in an app-owned, root-controlled location with an app-specific launchd label
and socket path. The app-managed service must not overwrite a Homebrew, Lima,
or user-installed service.

The service owns the vmnet connection and exposes its packet socket only to
the authorized local user. QEMU connects with its stream backend and remains
unprivileged. Service configuration pins the selected physical interface and
uses fixed, validated arguments. Reconfiguration and removal use OS
authorization; ordinary launches use the installed service without repeated
authentication.

The existing ad-hoc signing configuration is not treated as entitlement-based
vmnet access. The shipped helper installation and OS prompt must be tested with
the actual packaged app, including installation paths containing spaces.

### Linux

Use NetworkManager's D-Bus interface for managed creation. Inspect the active
Ethernet profile before proposing changes. Reject automatic conversion if its
settings cannot be preserved, including unsupported authentication or topology.
Preserve supported IPv4/IPv6 addressing, routes, DNS, MTU, and autoconnect
behavior when moving host addressing to the bridge.

Create a checkpoint with a rollback timeout before changing the active
connection. Retain the original profile and record every new resource. Commit
only after backend access, host address/route checks, and the user's
**Keep connection** confirmation. A timeout restores the prior configuration.
The confirmation remains local and does not depend on internet access.

Install the required QEMU bridge helper and an app-specific allowlist through
the privileged installer. Avoid broad grants to arbitrary bridges and avoid
modifying another QEMU installation's configuration. The helper must work from
the installed package, not depend on a writable source checkout.

For existing bridges, validate access and provision only the app's helper
permission. Do not adopt ownership of an existing bridge or alter its members.
On systems without NetworkManager, explain why creation is unavailable and
allow selecting an existing bridge; do not offer a terminal command as the
successful completion of managed setup.

### Windows

Bundle a pinned, properly signed TAP-Windows6 package and its required adapter
installation component. Validate package integrity and the driver signature
before installation. Create a dedicated app-owned adapter and record its
device identity/GUID; friendly names alone are not ownership evidence.

Use native adapter enumeration and detect available `netsh bridge` creation
capabilities. Run fixed bridge operations through the elevated helper. Resolve
adapter IDs immediately before applying changes and verify resulting bridge
membership through OS state rather than relying only on localized output.
Never enable force-compatibility mode automatically.

Snapshot affected adapter addressing and membership, and arm a privileged
recovery operation before conversion. As on Linux, verify host networking and
require **Keep connection** before committing. Recovery must survive the UI
closing. Show pending reboot as a distinct resumable state if the driver
installer requires it.

QEMU must open the dedicated TAP adapter from the normal user session.
Confirm this on supported Windows installations before shipping; running the
entire desktop app as administrator is not an acceptable completion path.
If driver access cannot satisfy this requirement, the Windows backend needs
a separately reviewed packet broker before release.

Do not attach to or remove an unrelated VPN's TAP adapter. Reusing a supported
preexisting configuration does not transfer ownership of its bridge or driver.
Remove an installed driver package only when it is app-owned and no other
device uses it; otherwise retain it and report that fact.

## Authorization, recovery, and lifecycle

All system writes follow the user's reviewed plan and an OS authorization
prompt where required. Helper IPC checks the calling user, operation identity,
allowed operations, resource ownership, and adapter identity. Payloads are
validated again across the privilege boundary. Installations use protected
destinations and reject symlink/reparse-point substitutions.

Write recovery records before changes. Failed or cancelled setup reverses its
own completed steps and reports any restoration failure. Restoring connectivity
takes priority over cancelling a progress indicator. On restart, reconcile
incomplete operations and offer recovery before applying another change.

For Linux and Windows, recovery runs independently of the UI and does not
depend solely on an in-memory snapshot. Preserve records across reboot.
An already changed host configuration must be detected before removal: show
the conflict rather than restoring an old snapshot over subsequent user edits.

Installed configuration persists across app restarts and host reboots.
Closing the emulator or selecting NAT leaves setup installed. Explicit removal
restores app-modified host settings and removes app-owned resources after
checking they are not in use. Repeated setup, repair, and removal are idempotent.

Updates compare the helper protocol/version before use. A mismatched helper
requires a reviewed repair/update; it must not receive incompatible requests.
Desktop uninstallation must remove or explicitly retain managed resources via
an uninstall cleanup path, rather than leaving an undisclosed privileged service.

## Packaging and verification

Package platform dependencies with pinned versions, integrity metadata, license
notices, and source information. Installation must work without Homebrew or
developer tools. Verify packaged payloads before changing the host.

Add Linux desktop build/package jobs and bridge dependency checks to
`.github/workflows/build.yml`. Preserve the macOS Intel/ARM and Windows x64
matrix. CI must validate both RISC-V and Xtensa networking arguments.

Automated coverage must exercise:

- State transitions, stale events, interrupted operations, and cancellation.
- Adapter replacement/renaming, invalid identities, and stale plans.
- Privileged request validation and resource ownership checks.
- QEMU argument construction, environment override precedence, Wi-Fi off,
  and unchanged NAT/browser behavior.
- Failed setup at every mutation boundary, timeout recovery, and repair/remove
  idempotency using fake OS adapters.
- Keyboard navigation, dialog focus, readable errors, and disabled controls.

Run installation tests on disposable native hosts or VMs for each platform,
including denial of authorization, unplugged Ethernet, reboot, update, and
removal. Driver and privilege behavior require native tests; mocked tests do
not establish that a platform is supported.

For the acceptance test, start from a clean packaged installation with no
manual bridge prerequisites. Complete setup entirely in the UI, launch both
ESP32-C3 and ESP32-S3 firmware, obtain a LAN address, and reach a firmware
service from a separate LAN device. Check that the host remains reachable and
that Remove setup restores its prior configuration.

Do not declare the cross-platform feature delivered until all three platform
acceptance runs pass.

## Evidence and implementation gates

These sources establish available mechanisms, not proof that this application's
packaged installer works:

- [socket_vmnet](https://github.com/lima-vm/socket_vmnet) documents bridged
  launchd services, root-controlled installation, and unprivileged QEMU use.
- [NetworkManager checkpoint API](https://networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.html)
  documents timed rollback and explicit checkpoint commit/removal.
- [Microsoft netsh bridge documentation](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/netsh-bridge)
  documents bridge creation/removal. Its syntax and examples differ, so verify
  actual commands and OS availability on the Windows test matrix before
  implementing mutations.
- [TAP-Windows6](https://github.com/OpenVPN/tap-windows6) documents driver
  installation and release signing. Select and pin the distributable signed
  payload during implementation; a source build is not automatically a usable
  production driver.

Early implementation gates are Windows standard-user TAP access and bridge
creation, macOS helper installation under the current signing model, and Linux
checkpoint restoration from the packaged app. Resolve these before polishing
the wizard. A failed gate requires revising the relevant backend design, not
reducing the feature to manual setup or silently omitting a platform.
