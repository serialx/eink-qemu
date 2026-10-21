#!/bin/sh
# Tauri copies this file to each standard Debian maintainer-script name.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
[ "$(id -u)" = 0 ] || { echo 'Bridge package maintenance requires root.' >&2; exit 1; }

# Never grant setuid, execute a helper, or unpack into an untrusted bridge path.
# Missing paths are allowed before the first unpack. Every existing parent is
# checked separately, so an absent leaf cannot hide a symlinked ancestor.
check_path() {
    [ ! -L "$1" ] || { echo "Refusing symbolic link: $1" >&2; exit 1; }
    if [ -e "$1" ]; then
        [ -d "$1" ] || [ -f "$1" ] || { echo "Refusing special file: $1" >&2; exit 1; }
        owner=$(stat -c %u -- "$1")
        mode=$(stat -c %a -- "$1")
        [ "$owner" = 0 ] && [ "$((0$mode & 0022))" = 0 ] || { echo "Unsafe ownership or permissions: $1" >&2; exit 1; }
        if [ -f "$1" ]; then
            [ "$(stat -c %h -- "$1")" = 1 ] || { echo "Refusing hard-linked file: $1" >&2; exit 1; }
        fi
    fi
}
for path in /usr /usr/lib /usr/lib/eink-qemu /usr/lib/eink-qemu/bridge \
    /usr/lib/eink-qemu/bridge/eink-bridge-helper /usr/lib/eink-qemu/bridge/eink-qemu-bridge-helper \
    /usr/lib/systemd /usr/lib/systemd/system /usr/lib/systemd/system/eink-bridge.service \
    /usr/share /usr/share/polkit-1 /usr/share/polkit-1/actions \
    /usr/share/polkit-1/actions/net.serialx.eink-qemu.bridge.policy \
    /etc /etc/eink-qemu /etc/eink-qemu/bridge /etc/eink-qemu/bridge/bridge.conf \
    /var /var/lib /var/lib/eink-qemu /var/lib/eink-qemu/bridge; do
    check_path "$path"
done

helper=/usr/lib/eink-qemu/bridge/eink-bridge-helper
service=eink-bridge.service
restart_service() {
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload
        # A prior installation may already be running. Replace its process so
        # the installed protocol and journal reader match the new package.
        deb-systemd-invoke restart "$service"
    fi
}
# dpkg invokes /var/lib/dpkg/info/<package>.<phase>; the bare phase is also
# supported for inspecting scripts extracted from a package.
phase=${0##*.}
phase=${phase##*/}
case "$phase" in
    preinst) ;;
    postinst)
        case "${1:-}" in
            configure)
                [ -f "$helper" ] && [ -x "$helper" ]
                [ -f /usr/lib/eink-qemu/bridge/eink-qemu-bridge-helper ]
                # Only the small fixed-ACL packet helper is setuid. The desktop,
                # CLI, QEMU, and provisioning service stay ordinary executables.
                chmod 4755 /usr/lib/eink-qemu/bridge/eink-qemu-bridge-helper
                deb-systemd-helper unmask "$service"
                if deb-systemd-helper --quiet was-enabled "$service"; then
                    deb-systemd-helper enable "$service"
                else
                    deb-systemd-helper update-state "$service"
                fi
                restart_service
                ;;
            abort-upgrade|abort-remove|abort-deconfigure) restart_service ;;
        esac
        ;;
    prerm)
        case "${1:-}" in
            remove|upgrade|deconfigure)
                if [ -d /run/systemd/system ]; then systemctl stop "$service"; fi
                if [ "$1" = remove ]; then
                    if ! "$helper" --uninstall; then
                        echo 'Bridge cleanup did not finish. Support remains installed; stop emulators and review bridge setup before retrying removal.' >&2
                        restart_service || true
                        exit 1
                    fi
                fi
                ;;
        esac
        ;;
    postrm)
        case "${1:-}" in
            remove) deb-systemd-helper mask "$service" ;;
            purge) deb-systemd-helper purge "$service" ;;
        esac
        if [ -d /run/systemd/system ]; then systemctl daemon-reload; fi
        # Retain protected recovery history and any administrator-edited policy.
        # Removing a package must not erase evidence needed to resolve a conflict.
        ;;
    *) echo 'Unknown Debian maintainer-script phase.' >&2; exit 1 ;;
esac
