#!/bin/sh
# Save a PNG of the app's window without bringing it forward (macOS, needs
# Screen Recording permission for the terminal). Usage: capture-window.sh out.png [pid]
set -eu
out=$1; pid=${2:-}
id=$(swift - "$pid" <<'SWIFT'
import CoreGraphics
import Foundation
let wanted = CommandLine.arguments.count > 1 ? Int(CommandLine.arguments[1]) : nil
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
for window in windows {
    let owner = window[kCGWindowOwnerName as String] as? String ?? ""
    let pid = window[kCGWindowOwnerPID as String] as? Int ?? 0
    let layer = window[kCGWindowLayer as String] as? Int ?? 1
    let bounds = window[kCGWindowBounds as String] as? [String: Double] ?? [:]
    // The main window carries the title; helper windows are untitled.
    let title = window[kCGWindowName as String] as? String ?? ""
    guard layer == 0, title == "eink-qemu", (bounds["Height"] ?? 0) > 300 else { continue }
    if let wanted { if pid != wanted { continue } } else if owner != "eink-qemu-desktop" && owner != "eink-qemu" { continue }
    print(window[kCGWindowNumber as String] as! Int)
    break
}
SWIFT
)
[ -n "$id" ] || { echo 'eink-qemu window not found' >&2; exit 1; }
screencapture -x -o -l "$id" "$out"
