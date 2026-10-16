# Firmware profiles

eink-qemu emulates the hardware and assumes nothing about the firmware
beyond ESP-IDF: the ROM and bootloader messages, crash reports, the
`I (1234) tag:` log format and the heap allocator. Everything it knows
about one particular firmware lives here, as data that every front end
reads (the Rust core behind the CLI and desktop app, and the web page):

- `crosspoint.json`: CrossPoint Reader and its forks. The log lines that
  mark a finished boot (three completed refreshes, then a memory report),
  its measurements (page render times, memory reports), and the NVS entry
  its X3 build expects. The NVS entry is written only into images that
  contain its strings (`detect.image_strings`).
- `xteink-stock.json`: the stock firmware's boot gate, so a run that
  waits on it can say why.

Firmware without a profile still boots, runs scripts, takes screenshots
and heap snapshots: a boot counts as ready once the screen has been quiet
for a few seconds, and the emulator's own counters (screen updates, panel
refreshes) stand in for the firmware's measurements.

Patterns are regular expressions in the common subset of Rust's `regex`
and JavaScript (no look-around, no backreferences).
