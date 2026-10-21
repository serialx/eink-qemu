#!/bin/bash
# Developer lifecycle gate, not an installed authorization or IPC endpoint.
# Place compiled probe-macos and the pinned archive beside this script.
# Inspection never changes networking. Exercise is limited to VirtualBuddy/
# Virtualization.framework guests with an exact explicit hostname guard.
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
here=$(cd "$(dirname "$0")" && pwd)
probe="$here/probe-macos"
if [[ $# == 0 ]]; then exec "$probe"; fi
if [[ $# != 7 || $1 != --exercise || $2 != --disposable-host || $4 != --adapter || $6 != --user ]]; then
    echo 'Usage: probe-macos.sh [--exercise --disposable-host HOST --adapter en0 --user USER]' >&2; exit 2
fi
[[ $(id -u) == 0 && $3 == "$(hostname)" ]] || { echo 'Requires root and exact disposable hostname' >&2; exit 2; }
case $(sysctl -n hw.model) in VirtualMac*) ;; *) echo 'Exercise requires a disposable macOS VM' >&2; exit 2;; esac
adapter=$5
user=$7
[[ $adapter =~ ^en[0-9]+$ && $user =~ ^[a-zA-Z_][a-zA-Z_0-9-]*$ ]] || exit 2
uid=$(id -u "$user")
[[ $uid -ge 501 ]] || { echo 'Requires an ordinary local account' >&2; exit 2; }
group=$(id -gn "$user")
[[ $group =~ ^[a-zA-Z_][a-zA-Z_0-9-]*$ ]] || exit 2
"$probe" eligible "$adapter" || { echo 'vmnet does not offer that interface' >&2; exit 2; }
case $(uname -m) in
    arm64) checksum=c7bf62308fbcfdc29bdfb8373c9b1951f7ac2396446e4390919796a94972e6dc ;;
    x86_64) checksum=2968a82c97e692c2d36f87230152e8018e00589c1b598e8257775adfe83800a1 ;;
    *) exit 2;;
esac
archive="$here/socket_vmnet-1.2.2-$(uname -m).tar.gz"
[[ $(shasum -a 256 "$archive" | cut -d ' ' -f 1) == "$checksum" ]] || { echo 'Payload checksum mismatch' >&2; exit 2; }
base='/Library/Application Support/eink-qemu/bridge'
run=/var/run/eink-qemu
label=net.serialx.eink-qemu.bridge.native-probe
sentinel=net.serialx.eink-qemu.probe-unrelated
plist="/Library/LaunchDaemons/$label.plist"
other="/Library/LaunchDaemons/$sentinel.plist"
for path in "$base" "$run" "$plist" "$other"; do
    [[ ! -e $path && ! -L $path ]] || { echo "Existing resource; refusing to replace $path" >&2; exit 2; }
done
# Refuse an existing loaded service even when its plist is missing.
! launchctl print "system/$label" >/dev/null 2>&1 || exit 2
! launchctl print "system/$sentinel" >/dev/null 2>&1 || exit 2
work=$(mktemp -d /var/tmp/eink-macos-gate.XXXXXX)
# Only paths proven absent above, created by this exercise, are removed.
cleanup() {
    launchctl bootout "system/$label" >/dev/null 2>&1 || true
    launchctl bootout "system/$sentinel" >/dev/null 2>&1 || true
    rm -f "$plist" "$other"
    rm -rf "$base" "$run"
    echo "Evidence: $work" >&2
}
trap cleanup EXIT
trap 'exit 130' INT TERM
network_snapshot() {
    ifconfig "$adapter" | awk '/ether |inet |inet6 |status:/ { print }'
    netstat -rn -f inet | awk 'NR > 4 && $0 !~ /Expire/ { print $1, $2, $3, $4 }' | sort
}
network_snapshot > "$work/before.txt"
"$probe" > "$work/inspect.json"
tar -xzf "$archive" -C "$work" ./opt/socket_vmnet/bin/socket_vmnet
install -d -o root -g wheel -m 755 "$base" "$run"
install -o root -g wheel -m 755 "$work/opt/socket_vmnet/bin/socket_vmnet" "$base/socket_vmnet"
[[ $("$base/socket_vmnet" --version) == v1.2.2 || $("$base/socket_vmnet" --version) == 1.2.2 ]]
install -d -o root -g wheel -m 700 "$run/$uid"
# Root retains directory ownership; only this user can traverse it. Other
# staff members cannot reach the socket, including after a daemon restart.
chmod +a "user:$user allow search" "$run/$uid"
socket="$run/$uid/native-probe.sock"
cat > "$other" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>$sentinel</string><key>ProgramArguments</key><array><string>/bin/sleep</string><string>600</string></array><key>RunAtLoad</key><true/></dict></plist>
EOF
chmod 644 "$other"
launchctl bootstrap system "$other"
shasum -a 256 "$other" > "$work/unrelated.sha256"
for attempt in {1..10}; do
    launchctl print "system/$sentinel" | awk '/pid =/ {print $3}' > "$work/unrelated.pid"
    [[ -s "$work/unrelated.pid" ]] && break
    sleep 1
done
[[ -s "$work/unrelated.pid" ]]
cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array><string>$base/socket_vmnet</string><string>--vmnet-mode=bridged</string><string>--vmnet-interface=$adapter</string><string>--socket-group=$group</string><string>$socket</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>StandardOutPath</key><string>$work/service.stdout</string>
<key>StandardErrorPath</key><string>$work/service.stderr</string>
</dict></plist>
EOF
chmod 644 "$plist"
launchctl bootstrap system "$plist"
for attempt in {1..20}; do [[ -S $socket ]] && break; sleep 1; done
[[ -S $socket ]] || { cat "$work/service.stderr" >&2; exit 1; }
sleep 1
sudo -u "$user" "$probe" open "$socket" > "$work/open-user.json"
# Check the other account can run the probe, so a missing executable cannot
# be mistaken for packet-access denial.
sudo -u nobody -g "$group" "$probe" eligible "$adapter"
# A different account must not gain packet access through shared staff group.
if sudo -u nobody -g "$group" "$probe" open "$socket" > "$work/open-other.json" 2> "$work/open-other.error"; then
    echo 'Unexpected access by another account' >&2; exit 1
fi
launchctl kickstart -k "system/$label"
sleep 3
sudo -u "$user" "$probe" open "$socket" > "$work/open-after-restart.json"
launchctl bootout "system/$label"
rm "$plist" "$socket"
! launchctl print "system/$label" >/dev/null 2>&1
shasum -a 256 -c "$work/unrelated.sha256" >/dev/null
launchctl print "system/$sentinel" | awk '/pid =/ {print $3}' > "$work/unrelated-after.pid"
cmp "$work/unrelated.pid" "$work/unrelated-after.pid"
network_snapshot > "$work/after.txt"
cmp "$work/before.txt" "$work/after.txt"
printf '{"install":"passed","openAsUser":"passed","otherUserDenied":true,"restart":"passed","restore":"passed","unrelatedServicePreserved":true,"socketVmnet":"1.2.2"}\n' | tee "$work/result.json"
