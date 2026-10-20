# App-managed Bridge Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver bridge setup, verification, repair, and removal entirely through the desktop UI on macOS, Linux, and Windows.

**Architecture:** Add an independent Rust bridge crate containing the typed protocol, platform discovery, transaction coordinator, and a privileged helper binary. Tauri exposes a narrow API to a shared TypeScript wizard and resolves verified network targets for the existing emulator core. Keep privilege, resource ownership, and crash recovery outside the WebView.

**Tech Stack:** Rust/Tokio/Serde, Tauri 2, TypeScript, native OS authorization, launchd/socket_vmnet, NetworkManager D-Bus, Windows TAP-Windows6 and native networking APIs.

**Spec:** [Approved bridge setup design](../specs/2026-10-20-bridge-setup-design.md)

## Global Constraints

- “NAT remains the default.”
- “The browser build keeps its current networking.”
- “failure never silently changes it back to NAT.”
- “The first Linux desktop package is a Debian package for Ubuntu 22.04 and newer, on x64 and ARM64”.
- “Other network managers can use existing bridges; automatic conversion of their configuration is outside this release”.
- “Native Windows ARM64 driver support, automatic conversion of bonds/VLANs/802.1X connections, and automatic Linux Wi-Fi bridging are outside the initial managed-creation scope.”
- “The application never asks for an administrator password in its own form.”
- “QEMU must open the dedicated TAP adapter from the normal user session.”
- “Do not declare the cross-platform feature delivered until all three platform acceptance runs pass.”
- Existing workspace Rust minimums remain: core/workspace 1.89; desktop 1.90. New bridge crate uses workspace values.
- No arbitrary command/path interface for privileged requests. Never elevate the desktop or QEMU as a workaround.
- No production host network changes during ordinary unit tests or CI builds. Native mutation tests run on explicitly selected disposable hosts.

## Review Focus

- Two app instances race to configure the same adapter: serialize at the privileged boundary and revalidate after acquiring the lock (Tasks 2, 4).
- A USB adapter is replaced with one having the same name: reject a stale identity instead of applying its saved plan (Tasks 2, 5–7).
- App crash, host reboot, or cancelled OS authentication: recover the recorded transaction without claiming readiness (Tasks 3–7).
- A user edits the host connection after setup: removal preserves their edits and reports the conflict (Tasks 4, 6, 7).
- A launch inherits empty `X4NET`, or comma/space-containing interface paths: preserve override semantics and argument boundaries (Tasks 8, 9).

---

## Work order and file ownership

This is one integrated feature with three platform backends. Keep one plan
because protocol, ownership, recovery, and UI behavior are shared.

Work in the existing isolated worktree. Implement Tasks 1–4 first. Platform
Tasks 5–7 depend on those contracts and their own Task 1 gate. Task 8 can follow
Task 2 independently of OS provisioning. Tasks 9–10 join the implementations.
Tasks 11–12 establish packaged delivery and acceptance. A failed native gate
blocks that backend's release, while unrelated implementation can continue.

Paths below are relative to the repository root. Commands run from that root
unless a task says otherwise.

| Files | Responsibility |
| --- | --- |
| `eink-qemu/desktop/bridge/Cargo.toml`, `src/lib.rs` | New `eink-bridge` library plus `eink-bridge-helper` binary, no GUI dependencies |
| `bridge/src/model.rs`, `protocol.rs` | Serializable identities, selections, operations, errors, and fixed helper requests |
| `bridge/src/coordinator.rs`, `journal.rs`, `recovery.rs` | Lifecycle, persisted ownership, locking, rollback, reboot reconciliation |
| `bridge/src/transport/{mod,unix,windows}.rs`, `bin/eink-bridge-helper.rs` | Authenticated local IPC and restricted privileged entry point |
| `bridge/src/platform/{mod,macos,linux,windows}.rs` | Discovery and OS-specific plans |
| `bridge/src/platform/{macos,linux,windows}/` | Split installation and mutation code into focused modules as defined in Tasks 5–7 |
| `eink-qemu/desktop/core/src/network.rs` | Typed network target and QEMU argument construction |
| `eink-qemu/desktop/src-tauri/src/bridge.rs` | Tauri commands, event publication, backend-to-core conversion |
| `eink-qemu/desktop/src/bridge-{types,state,dialog}.ts`, `bridge.css` | UI contract, pure reducer, wizard rendering |
| `eink-qemu/desktop/scripts/bridge/` | Dependency manifests, staging, native probes, package lifecycle resources |
| `eink-qemu/desktop/bridge/tests/` | Fake-OS integration tests and opt-in native tests |
| `eink-qemu/tests/test_bridge_*.mjs` | UI state and interaction tests using existing Node test conventions |

## Task 1: Establish the three native implementation gates

**Files:** Create `eink-qemu/desktop/scripts/bridge/probe-macos.sh`,
`probe-linux.py`, `probe-windows.ps1`, and
`docs/superpowers/validation/bridge-platform-gates.md`.

**Interfaces:** Each probe defaults to inspection. Mutation requires the
explicit `--exercise` argument (PowerShell `-Exercise`) and a disposable-host
identifier. Produce JSON containing OS/build/architecture, adapter identity,
tested component versions, `inspect`, `install`, `openAsUser`, and `restore`
results. These are developer probes, not a user setup path.

- [ ] Write the probe expectations before the mutation scripts: macOS ordinary-user socket access with an app-specific launchd service; Linux checkpoint rollback after forced interruption; Windows bridge create/destroy plus TAP open from the original unelevated process.
- [ ] Run inspection on available native hosts. Record missing hosts as untested, never as passed; do not mutate the development machine as a substitute.
- [ ] Implement minimal disposable-host exercises using OS prompts. Pin candidate socket_vmnet and signed TAP package versions and checksums in the evidence record. For Windows, record the actual accepted `netsh bridge` syntax and supported OS builds.
- [ ] Run exercises, including cancellation and restoration. Expected: the unprivileged client opens the backend and host networking returns to its baseline. A failed gate requires a specific design revision before its backend continues.
- [ ] Commit probes and factual evidence with `test: establish native bridge setup gates`. Do not commit private addresses or machine identifiers.

## Task 2: Define shared contracts and discovery

**Files:** Create `bridge/Cargo.toml`, `bridge/src/{lib,model,protocol}.rs`,
`bridge/src/platform/{mod,macos,linux,windows}.rs`,
`bridge/tests/discovery.rs`, and `bridge/tests/protocol.rs`.
Modify `eink-qemu/desktop/{Cargo.toml,Cargo.lock}`.

**Interfaces:** `inspect() -> Result<Inspection, BridgeError>`;
`decode_request(json: &str) -> Result<HelperRequest, BridgeError>`.
All JSON fields use camelCase; tagged enums use camelCase discriminants.
Define these shared types here, with schema/protocol version `1`:

| Type | Required content |
| --- | --- |
| `AdapterId(String)`, `OperationId(String)`, `ResourceId(String)` | Opaque identities; operation/resource IDs randomly generated |
| `Adapter` | `id`, `name`, `kind`, `linkUp`, `eligible`, `reason`, `fingerprint` |
| `Inspection` | `platform`, `adapters`, `capabilities`, `selection`, `readiness`, `activeOperation`, `externalOverride` |
| `BridgeSelection` | `version`, `platform`, `adapterId`, `resourceId: Option<ResourceId>`, `displayName` |
| `PlanRequest` | Tagged `Configure { adapterId }`, `Repair { resourceId }`, `Remove { resourceId }` |
| `BridgePlan` | `id: OperationId`, `request`, `fingerprint`, `changes`, `requiresAuthorization`, `mayDisconnect`, `expiresAt` |
| `Operation` | `id`, `revision: u64`, `state`, `stage`, `confirmationDeadline`, `rebootRequired`, `error` |
| `Readiness` | `needsSetup`, `ready`, `disconnected`, `needsRepair`, `unsupported` |
| `BridgeError` | Stable `code`, user-facing `message`, optional diagnostic `detail` |
| `LaunchTarget` | `LinuxBridge { name, helper }`, `MacSocket { path }`, `WindowsTap { name }` |
| `HelperRequest` | Versioned, tagged `Inspect`, `Plan`, `Apply { planId }`, `Verify { selection }`, `Keep { operationId }`, `Cancel { operationId }`, `Recover` |

- [ ] Add contract tests with exact assertions:
  ```rust
  assert_eq!(decode_request(r#"{"version":99,"kind":"inspect"}"#).unwrap_err().code, "EPROTOCOL");
  assert!(decode_request(r#"{"version":1,"kind":"exec","command":"anything"}"#).is_err());
  ```
  Add discovery fixtures asserting two same-name adapters have different IDs, unplugged saved adapters yield `Disconnected`, and unsupported Linux managers still offer compatible existing bridges.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-bridge`. Expected: missing-contract failures before implementation.
- [ ] Implement the models and read-only platform discovery. Use vmnet-supported interface enumeration on macOS; NetworkManager D-Bus plus kernel bridge inspection on Linux; Windows native adapter/device identities. No system writes in `inspect`.
- [ ] Run the new tests and native inspection probes. Verify serialization fixtures for all three platforms on every host; OS calls remain target-gated.
- [ ] Commit with `feat: define bridge capabilities and typed setup contracts`.

## Task 3: Build the authorization and privileged request boundary

**Files:** Create `bridge/src/transport/{mod,unix,windows}.rs`,
`bridge/src/bin/eink-bridge-helper.rs`, `bridge/src/authorization.rs`,
`bridge/tests/authorization.rs`, and platform installer resources under
`eink-qemu/desktop/scripts/bridge/`.
Modify `bridge/Cargo.toml`, `bridge/src/lib.rs`.

**Interfaces:** `HelperClient::connect() -> Result<HelperClient, BridgeError>`;
`HelperClient::request(HelperRequest) -> Result<HelperReply, BridgeError>`;
`authorize_install() -> Result<(), BridgeError>`.
`HelperReply` carries inspection, plan, operation, or verification results.
Authenticated `Caller` identity comes from OS credentials, not request JSON.

- [ ] Add tests asserting protocol mismatch returns `EPROTOCOL`, another caller's operation returns `EPERM`, oversized/truncated frames are rejected, and cancelled authorization returns `EAUTHCANCELLED` without an install record.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-bridge --test authorization`; expect missing-interface failures.
- [ ] Implement length-prefixed JSON IPC with a 1 MiB message limit. Unix sockets verify peer credentials and protected endpoint ownership; Windows pipes restrict ACLs and verify the client's token/SID. Validate all requests at the helper.
- [ ] Implement platform installation entry points: macOS OS authorization for a fixed bundled installer/helper, Linux package-installed helper plus polkit authorization, Windows UAC for the fixed bundled helper. No caller-supplied executable or shell command. Installer payloads use immutable manifest entries and protected destinations, with symlink/reparse-point checks.
- [ ] Run authorization tests and Task 1 prompt/cancellation exercises. Confirm app and QEMU retain ordinary privileges.
- [ ] Commit with `feat: add authorized bridge helper transport`.

## Task 4: Implement durable transactions and recovery

**Files:** Create `bridge/src/{coordinator,journal,recovery}.rs`,
`bridge/tests/transactions.rs`, and `bridge/tests/support/mod.rs`.
Modify `bridge/src/{lib,protocol}.rs`.

**Interfaces:** `Coordinator::plan(PlanRequest) -> Result<BridgePlan, BridgeError>`;
`apply(OperationId)`, `keep(OperationId)`, `cancel(OperationId)` return
`Result<Operation, BridgeError>`.
`verify(&BridgeSelection) -> Result<Readiness, BridgeError>`;
`resolve(&BridgeSelection) -> Result<LaunchTarget, BridgeError>`;
`recover() -> Result<Vec<Operation>, BridgeError>`.
`PlatformOps` defines `inspect`, `prepare`, `apply_step`, `verify`,
`rollback_step`, and `remove_step` using a platform-tagged `HostSnapshot`
and fixed `Mutation` enum. Each platform defines its snapshot/mutation payloads.

- [ ] Add a fake OS that records mutations, can fail after each completed step, and supplies a controllable clock. Test:
  ```rust
  assert_eq!(second_apply.error_code(), Some("EBUSY"));
  assert_eq!(replaced_adapter.error_code(), Some("ESTALE"));
  assert_eq!(restored_host, original_host);
  assert_eq!(changed_since_install.error_code(), Some("ECONFLICT"));
  ```
  Also test idempotent removal, failed rollback, crash recovery, late Keep after timeout, and resource ownership refusal.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-bridge --test transactions`; expect failures.
- [ ] Implement atomic protected journals, a host-wide mutation lock, per-resource use leases, and monotonically increasing revisions. Write undo intent before each step and completion afterward. Reinspect under the lock; expire unapplied plans after 5 minutes.
- [ ] Implement a 120-second Keep-connection deadline for disruptive conversions. The privileged worker owns recovery independently of UI lifetime. Use NetworkManager's checkpoint as an additional Linux rollback mechanism, and recover incomplete durable records after reboot.
- [ ] Run transaction tests with every mutation boundary fault-injected. Terminal state must distinguish restoration success from failure; no unverified `Ready`.
- [ ] Commit with `feat: make bridge setup recoverable and idempotent`.

## Task 5: Implement macOS provisioning

**Files:** Create `bridge/src/platform/macos/{install,service,verify}.rs`,
`bridge/tests/macos.rs`, and
`eink-qemu/desktop/scripts/bridge/net.serialx.eink-qemu.bridge.plist.in`.
Modify `bridge/src/platform/macos.rs`.

**Interfaces:** macOS implements Task 4 `PlatformOps`; resolves to
`LaunchTarget::MacSocket`. Install under `/Library/Application Support/eink-qemu/bridge`;
use app-owned launchd labels beginning `net.serialx.eink-qemu.bridge`.
Use user-specific, protected socket directories under `/var/run/eink-qemu`.

- [ ] Test launchd argument arrays with space/quote-containing paths, supported-interface validation, missing helper, wrong version, wrong socket owner, and repair/removal with an unrelated Lima/Homebrew service present. Assert the unrelated service is unchanged.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-bridge --test macos`; expect failures.
- [ ] Implement verified pinned payload installation and an app-owned bridged socket_vmnet service. Restrict socket access to the requesting user, record service ownership, and revalidate the physical interface fingerprint before configuration.
- [ ] Implement bounded verification by opening the packet transport as the ordinary user; distinguish service installed, service running, and transport usable. Remove only the recorded service/resources after lease checks.
- [ ] Run tests and the native gate against packaged payloads on Intel and Apple silicon, including reboot, adapter unplug, denied prompt, and repair from an incompatible helper.
- [ ] Commit with `feat: manage macOS bridge services from the app`.

## Task 6: Implement Linux provisioning

**Files:** Create `bridge/src/platform/linux/{network_manager,helper,verify}.rs`,
`bridge/tests/linux.rs`, and Linux polkit/systemd resources under
`eink-qemu/desktop/scripts/bridge/linux/`.
Modify `bridge/src/platform/linux.rs`, `meson.build`,
`qemu-bridge-helper.c`, and `.github/workflows/build.yml`.

**Interfaces:** Linux implements `PlatformOps`; resolves to
`LaunchTarget::LinuxBridge`. Use app-owned NetworkManager connection UUIDs,
not names, for ownership. Store helper configuration under
`/etc/eink-qemu/bridge/` and protected helper binaries under
`/usr/lib/eink-qemu/bridge/`.

- [ ] Add fixtures for DHCP and supported static IPv4/IPv6 profiles; preserve routes, DNS, MTU, and autoconnect settings. Reject bonds, VLAN conversion, 802.1X, Wi-Fi station conversion, and unsupported managers for creation before any mutation.
- [ ] Add tests for checkpoint timeout after UI death, an original profile modified after setup, missing polkit agent, helper access denial, and selecting/removing a reference to an existing non-owned bridge. Assert existing bridge membership stays unchanged.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-bridge --test linux`; expect failures.
- [ ] Implement NetworkManager D-Bus checkpoint creation, supported profile migration, activation, verification, Keep, and rollback. Retain original profiles and combine checkpoint results with the durable journal. Explicitly detect partial checkpoint restoration.
- [ ] Add an `eink-qemu-bridge-helper` Meson target from the existing helper source with `EINK_BRIDGE_ACL_FILE` fixed to `/etc/eink-qemu/bridge/bridge.conf`. That build uses the fixed path without executable-relative relocation; the ordinary `qemu-bridge-helper` retains its original behavior. Stage the new target in the native artifacts. Grant only selected authorized bridge access and verify the installed helper from a normal user process. Existing bridges require access setup without membership edits.
- [ ] Run Linux tests plus native exercises on Ubuntu 22.04 and a newer supported Ubuntu release. Assert host addresses/routes survive and removal restores the original connection, including after reboot.
- [ ] Commit with `feat: provision Linux bridges with timed recovery`.

## Task 7: Implement Windows provisioning

**Files:** Create `bridge/src/platform/windows/{driver,network_bridge,recovery,verify}.rs`,
`bridge/tests/windows.rs`, and Windows package/recovery resources under
`eink-qemu/desktop/scripts/bridge/windows/`.
Modify `bridge/src/platform/windows.rs`.

**Interfaces:** Windows implements `PlatformOps`; resolves to
`LaunchTarget::WindowsTap`. Use device instance IDs and adapter GUIDs for
ownership; persist protected journals under `%ProgramData%\\eink-qemu\\bridge`.

- [ ] Test driver absent/present/shared, invalid signature, architecture mismatch, reboot-required result, unsupported bridge creation, stale adapter ID, non-English display output, and a preexisting VPN TAP adapter.
- [ ] Test removal after user edits and driver reuse by another device. Assert the shared driver and unrelated adapter remain installed. Test recovery after disconnecting the UI and after a reboot boundary.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-bridge --test windows`; expect failures.
- [ ] Implement signed driver verification/install and dedicated adapter creation through fixed native/helper operations. Use the Task 1 verified bridge commands; never assume documentation syntax or parse localized prose as authoritative state.
- [ ] Arm the independent recovery worker before changing host membership. Snapshot supported addressing, confirm native state afterward, and resume pending reboot operations without reporting Ready. Implement Keep/timeout and conflict-aware removal.
- [ ] Verify TAP opens from the original ordinary user token. If this fails, stop this backend and revise the design for a packet broker; do not elevate QEMU or introduce broad device ACL grants.
- [ ] Run tests and native exercises on each supported Windows build from Task 1. Include two user sessions and a shared driver installation.
- [ ] Commit with `feat: manage Windows TAP bridge setup and recovery`.

## Task 8: Add typed network targets to the emulator core

**Files:** Create `eink-qemu/desktop/core/src/network.rs`.
Modify `core/src/{lib,session}.rs`; extend
`eink-qemu/tests/test_wifi_launchers.py` only where launcher behavior changes.

**Interfaces:** `NetworkTarget::{LinuxBridge { name, helper }, MacSocket { path }, WindowsTap { name }}`;
`NetworkOverrides::from_env()`;
`network::args(mode: WifiMode, target: Option<&NetworkTarget>, overrides: &NetworkOverrides) -> Result<Vec<String>, AppError>`.
Add `Session::start_with_network(options, flash, trace, Option<NetworkTarget>)`.
Existing `Session::start` delegates with `None`; do not expose raw managed targets
as WebView-provided `StartOptions`.

- [ ] Add tests for all targets, comma escaping, space/quote-containing paths, `X4NET=""`, `X4NET="-nic none"`, `X4BR`, and unchanged legacy defaults. Both C3 and S3 launch paths must use the target. Wi-Fi off must never attach a network backend.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-core network`; expect failures.
- [ ] Extract the argument construction and implement the new internal start entry point. Preserve precedence: Wi-Fi off disables attachment; otherwise `X4NET` takes precedence, then an applicable `X4BR`, then a managed target, then legacy mode defaults. Never mutate process environment.
- [ ] Run the new core tests and `python3 -m unittest discover -s eink-qemu/tests -p test_wifi_launchers.py`. Expected: old and new launch semantics pass.
- [ ] Commit with `feat: launch managed bridges through typed network targets`.

## Task 9: Connect provisioning to Tauri and settings

**Files:** Create `eink-qemu/desktop/src-tauri/src/bridge.rs`.
Modify `src-tauri/src/{lib,settings}.rs`, `src-tauri/Cargo.toml`,
`src-tauri/capabilities/default.json`, and `src-tauri/build.rs` only if
explicit command permissions require generation changes.

**Interfaces:** Tauri commands `bridge_inspect`, `bridge_plan(request)`,
`bridge_apply(planId)`, `bridge_keep(operationId)`,
`bridge_cancel(operationId)`, `bridge_verify`.
Repair/remove are typed plan requests. Emit `bridge-operation` events carrying
`Operation`; persist `Settings.bridge: Option<BridgeSelection>` only through
validated setup results. Do not add it to the unrestricted settings patch.

- [ ] Add Rust tests for old settings deserialization, failed save, wrong-window access, active emulator rejection, repeated Apply, forged resource IDs, and inherited overrides. Assert managed launch resolution is skipped when an external override controls the launch.
- [ ] Run `cargo test --manifest-path eink-qemu/desktop/Cargo.toml -p eink-qemu-desktop bridge`; expect failures.
- [ ] Implement commands with main-window checks, typed arguments, and native error mapping. Start must reserve the session/bridge lease and recheck readiness before launching, so setup cannot race with Start.
- [ ] Resolve the native target from verified settings in Rust and call Task 8 `start_with_network`; do not accept arbitrary target paths from IPC. Reconcile interrupted helper operations on attach and publish the resulting snapshot.
- [ ] Run Rust tests plus workspace compilation. Verify heap-window/browser paths cannot invoke provisioning.
- [ ] Commit with `feat: expose managed bridge lifecycle to the desktop`.

## Task 10: Implement the shared setup wizard and launch recovery

**Files:** Create `eink-qemu/desktop/src/bridge-{types,state,dialog}.ts`,
`src/bridge.css`, `eink-qemu/tests/test_bridge_state.mjs`,
and `test_bridge_dialog.mjs`.
Modify `src/{backend,backend-tauri,backend-browser,types,setup,app,dialog}.ts`
and `eink-qemu/desktop/index.html`.

**Interfaces:** `Backend.bridge?: BridgeBackend`, absent in the browser backend.
`BridgeBackend` mirrors Task 9 commands and provides
`watch(handler: (operation: Operation) => void): Promise<() => void>`.
`reduceBridge(state: BridgeUiState, action: BridgeAction): BridgeUiState`;
`openBridgeDialog(backend: BridgeBackend, trigger: HTMLElement): Promise<void>`.
UI operation types match Task 2 serialization fixtures.

- [ ] Test late event rejection, double-click Apply, missing selected adapter, authorization cancelled, reboot required, rollback failed, Keep deadline, and a settings-save failure after successful host setup. Assert the Ready view says to join `PICSimLabWifi` and does not display a guest IP or claim a guest connection. Exact assertion examples:
  ```ts
  assert.equal(reduceBridge(newerState, olderEvent), newerState);
  assert.equal(browserBackend.bridge, undefined);
  ```
- [ ] Run `node --test eink-qemu/tests/test_bridge_state.mjs eink-qemu/tests/test_bridge_dialog.mjs`; expect missing-module/behavior failures.
- [ ] Implement the pure state reducer and the dialog using existing styling. Show Choose connection → Review setup → Setting up → Ready, plus Keep connection, reboot, repair, and restoration outcomes. Use `textContent` for OS-provided labels.
- [ ] Integrate Bridged selection and Manage into the firmware card. Preserve focus on close, keyboard navigation, Escape behavior during rollback, and an aria-live progress message. Coordinate with the existing dialog manager so emulator keyboard input is not triggered.
- [ ] Add Start failure actions **Open bridge setup** and **Use NAT**; save NAT only after the user selects it. Show external overrides explicitly and keep Apply disabled during an emulator run.
- [ ] Run Node tests, `pnpm --dir eink-qemu/desktop exec tsc --noEmit`, and the existing setup/keyboard tests. Check the wizard visually in the actual Tauri app at its 1000×640 minimum size and both themes.
- [ ] Commit with `feat: add the complete bridge setup wizard`.

## Task 11: Package dependencies and lifecycle integration

**Files:** Create `eink-qemu/desktop/scripts/bridge/dependencies.json`,
`stage.py`, `verify.py`, `eink-qemu/desktop/scripts/bundle-qemu-linux.sh`,
`eink-qemu/tests/test_bridge_packaging.py`, and uninstall resources under
`scripts/bridge/{macos,linux,windows}/`.
Modify `.github/workflows/build.yml`,
`scripts/bundle-qemu.sh`, `scripts/bundle-qemu-windows.sh`,
`src-tauri/tauri.bundle.conf.json`, and platform bundle configurations.

**Interfaces:** `stage.py --platform <target> --output <dir>` reads a pinned
manifest; `verify.py --platform <target> --root <dir>` returns nonzero for
missing/mismatched payloads. Manifest entries require exact version,
architecture, source URL, SHA-256, license/source notice, and signature policy.

- [ ] Test absent payloads, checksum mismatch, wrong architecture, archive path traversal/symlinks, signature failure, and packaging paths containing spaces. Assert staging cannot wipe a valid existing bundle on a failed prerequisite.
- [ ] Run `python3 -m unittest discover -s eink-qemu/tests -p test_bridge_packaging.py`; expect failures.
- [ ] Stage the Task 1 proven dependency versions and the helper. Add Linux x64/ARM64 desktop package jobs using existing QEMU/CLI artifacts and Ubuntu 22.04 baseline dependencies.
- [ ] Integrate version-aware helper updates, resume-after-reboot metadata, and uninstall cleanup. Linux/Windows package removal invokes conflict-aware cleanup; macOS ships an explicit **Uninstall bridge support** action/helper and documents that dragging the app to Trash does not invoke an uninstall hook.
- [ ] Add bundle checks for helper permissions, notices, ordinary-user startup, and both QEMU architectures. Keep network mutation tests off shared hosted CI runners.
- [ ] Run packaging tests and build packages on their native runners. Inspect each artifact's contents and test install/update/uninstall on disposable hosts.
- [ ] Commit with `build: ship bridge dependencies and Linux desktop packages`.

## Task 12: Verify native delivery and update user documentation

**Files:** Create `docs/superpowers/validation/bridge-acceptance.md`.
Modify `EINK-QEMU.md`, `README.md`, `eink-qemu/desktop/README.md`,
and `eink-qemu/desktop/cli/README.md` where existing/manual configuration
needs distinguishing from the new desktop wizard.

**Interfaces:** Acceptance evidence records package commit/hash, OS/build,
architecture, adapter class, C3/S3 results, normal-user access, reboot/repair/
removal, and host restoration. Record failures and untested combinations.

- [ ] Run the targeted Rust/Node/Python tests from Tasks 2–11 and `cargo test --manifest-path eink-qemu/desktop/Cargo.toml --workspace` on the native build matrix. Run the existing browser build and relevant browser networking tests to establish no regression.
- [ ] From a clean package on all three OS families, complete the wizard without terminal setup. Verify host readiness separately from guest DHCP. Reach a firmware service from a separate LAN device for both C3 and S3 firmware.
- [ ] Exercise denied authorization, dependency installation failure, disconnect during Apply, UI crash, reboot, helper update, removal with later host edits, and shared TAP driver retention. Confirm host restoration and retained-resource explanations.
- [ ] Update documentation to describe actual tested support, the UI flow, explicit NAT fallback, and cleanup. Preserve manual CLI/environment documentation and explain override precedence.
- [ ] Obtain an independent whole-change review focused on privilege boundaries and recovery, then resolve actionable findings and rerun affected checks.
- [ ] Commit with `docs: document verified managed bridge setup`. Report the feature complete only if the native acceptance requirements pass; otherwise report exact remaining failures without claiming cross-platform delivery.

## Plan review and execution

The recommended execution method is subagent-driven, with sequential shared
contracts and separate platform implementation/review tasks. Privileged setup
and host recovery benefit from independent review. Native inline execution is
also possible and uses the same gates and test requirements.

This document is a plan, not execution evidence. No dependency installation,
privileged helper installation, or host network modification has occurred while
writing it.
