#!/usr/bin/env python3
"""Developer gate: read-only by default; exercise only an explicit disposable VM.

Exercise writes its baseline before mutation and exits with the checkpoint live.
After 45 seconds, --check compares the restored profile and addresses/routes.
Never use this probe as the product's privileged request handler.
"""
import argparse
import json
import os
import pathlib
import platform
import subprocess
import time
import uuid


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def snapshot(adapter):
    return {
        "profile": command("nmcli", "-g", "GENERAL.CON-UUID", "device", "show", adapter),
        "addresses": sorted(a["local"] + "/" + str(a["prefixlen"])
                            for i in json.loads(command("ip", "-j", "address", "show", "dev", adapter))
                            for a in i["addr_info"] if a["family"] == "inet"),
        "routes": sorted([r.get("dst", "default"), r.get("gateway", "")]
                         for r in json.loads(command("ip", "-j", "route", "show", "dev", adapter))),
    }


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--exercise", action="store_true")
    p.add_argument("--check", action="store_true")
    p.add_argument("--disposable-host")
    p.add_argument("--adapter")
    p.add_argument("--record", type=pathlib.Path, default=pathlib.Path("/var/tmp/eink-bridge-probe.json"))
    args = p.parse_args()
    result = {"os": platform.system(), "release": platform.release(), "architecture": platform.machine(),
              "component": command("nmcli", "--version"), "inspect": "passed",
              "install": "untested", "openAsUser": "untested", "restore": "untested"}
    if not args.exercise and not args.check:
        print(json.dumps(result))
        return
    if args.disposable_host != platform.node() or os.geteuid() != 0 or not args.adapter:
        p.error("exercise/check requires root, --adapter and an exact --disposable-host hostname")
    if args.check:
        before = json.loads(args.record.read_text())
        result["restore"] = "passed" if snapshot(args.adapter) == before["baseline"] else "failed"
        result["newProfilesRemoved"] = all(u not in command("nmcli", "-g", "UUID", "connection") for u in before["created"])
        result["checkpointExpired"] = time.time() >= before["deadline"]
        print(json.dumps(result))
        if result["restore"] != "passed" or not result["newProfilesRemoved"]:
            raise SystemExit(1)
        return
    if args.record.exists():
        p.error("record already exists; check/recover that experiment before another exercise")
    import dbus
    bus = dbus.SystemBus()
    service = "org.freedesktop.NetworkManager"
    manager = dbus.Interface(bus.get_object(service, "/org/freedesktop/NetworkManager"), service)
    settings = dbus.Interface(bus.get_object(service, "/org/freedesktop/NetworkManager/Settings"), service + ".Settings")
    device = manager.GetDeviceByIpIface(args.adapter)
    props = dbus.Interface(bus.get_object(service, device), "org.freedesktop.DBus.Properties")
    if int(props.Get(service + ".Device", "DeviceType")) != 1:
        p.error("only Ethernet devices can be exercised")
    active = props.Get(service + ".Device", "ActiveConnection")
    active_props = dbus.Interface(bus.get_object(service, active), "org.freedesktop.DBus.Properties")
    profile = active_props.Get(service + ".Connection.Active", "Connection")
    source = dbus.Interface(bus.get_object(service, profile), service + ".Settings.Connection")
    original = source.GetSettings()
    if original["connection"]["type"] != "802-3-ethernet" or "802-1x" in original:
        p.error("unsupported source profile")
    bridge_uuid, port_uuid = str(uuid.uuid4()), str(uuid.uuid4())
    baseline = snapshot(args.adapter)
    args.record.write_text(json.dumps({"baseline": baseline, "created": [bridge_uuid, port_uuid], "deadline": time.time() + 30}))
    os.chmod(args.record, 0o600)
    checkpoint = manager.CheckpointCreate(dbus.Array([device], signature="o"), dbus.UInt32(30), dbus.UInt32(2))
    try:
        bridge = source.GetSettings()
        bridge.pop("match", None)
        bridge.pop("802-3-ethernet", None)
        bridge["connection"] = {"id": "eink bridge gate", "uuid": bridge_uuid, "type": "bridge", "interface-name": "ebrprobe", "autoconnect": dbus.Boolean(False)}
        bridge["bridge"] = {"stp": dbus.Boolean(False)}
        port = source.GetSettings()
        port.pop("ipv4", None)
        port.pop("ipv6", None)
        port["connection"] = {"id": "eink bridge gate port", "uuid": port_uuid, "type": "802-3-ethernet", "interface-name": args.adapter,
                              "master": bridge_uuid, "slave-type": "bridge", "autoconnect": dbus.Boolean(False)}
        bridge_path = settings.AddConnection(bridge)
        port_path = settings.AddConnection(port)
        manager.ActivateConnection(bridge_path, dbus.ObjectPath("/"), dbus.ObjectPath("/"))
        manager.ActivateConnection(port_path, device, dbus.ObjectPath("/"))
        result["checkpointArmed"] = True
        result["install"] = "applied; awaiting independent timeout rollback"
        print(json.dumps(result))
        # Deliberately exit without Keep/rollback: NM must restore without us.
    except Exception:
        manager.CheckpointRollback(checkpoint)
        raise


if __name__ == "__main__":
    main()
