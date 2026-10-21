# Signed macOS bridge packages

Managed bridge installation requires an Apple-issued app and helper signature
from the same team. Ad-hoc signing cannot satisfy the helper's installation
requirements. Use an Apple Development identity for disposable VM tests;
distribution still requires Developer ID signing and notarization.

From `eink-qemu/desktop`, with QEMU and the CLI already built:

```sh
export APPLE_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)'
export EINK_BRIDGE_MACOS_TEAM_ID='YOURTEAMID'
python3 scripts/bridge/stage_macos.py aarch64-apple-darwin /path/to/socket_vmnet-1.2.2-arm64.tar.gz
sh scripts/bundle-qemu.sh
pnpm tauri build --target aarch64-apple-darwin --bundles app,dmg \
  --config src-tauri/tauri.macos.bridge.conf.json
python3 scripts/bridge/verify_macos_bundle.py \
  target/aarch64-apple-darwin/release/bundle/macos/eink-qemu.app \
  aarch64-apple-darwin "$EINK_BRIDGE_MACOS_TEAM_ID"
```

For Intel, use `x86_64-apple-darwin` and the pinned x86_64 archive. The dependency
manifest provides archive URLs and checksums. Staging builds the helper with the
verified embedded payload, signs it, checks the team and architecture, and only
then replaces previous staged files. QEMU staging signs its libraries and gives
only the QEMU executables the JIT entitlement.

The verifier checks the app/helper relationship, dependency manifest and notice,
binary architecture, signatures, JIT entitlement, and deployment version of
every executable and library. A Rust deployment target does not make newer
Homebrew libraries compatible with an older macOS. Build dependencies for the
intended release baseline before packaging; the verifier rejects a bundle whose
declared minimum is lower than a dependency's minimum.

The existing ad-hoc CI package configuration does not include managed macOS
bridge installation. Signed bundle mechanics and a macOS 15+ development QA
package have been checked, but release signing, the advertised 12.3 dependency
baseline, support uninstallation, and complete UI acceptance remain open. See
the [native validation record](../../../../../docs/superpowers/validation/bridge-platform-gates.md).
