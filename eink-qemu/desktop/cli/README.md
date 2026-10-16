# eink-qemu command-line tool

`eink-qemu` boots e-ink reader firmware in QEMU and drives it with a script
of steps: key presses and touches, waits on the firmware log or the screen,
screenshots, heap snapshots and allocation traces. It runs unmodified
firmware, the same bytes you flash, with no serial-control patch and no
instrumented build. It replaces the transport of on-device testing; the
method (a test definition, paired baseline and candidate runs, evidence
kept beside the branch) stays the same.

It runs headless by default, in the desktop app's window with `--gui`, as a
long-lived session you drive step by step (`start`, `do`, `stop`), and as a
side-by-side comparison of two builds (`compare`).

## Build and run

Build QEMU first ([EINK-QEMU.md](../../../EINK-QEMU.md#build-and-run)). The
tool finds `build/qemu-system-*` in the repository, or takes `--qemu DIR`
or `EINK_QEMU_BIN`. Then, from the repository root:

```sh
eink-qemu/cli run .pio/build/default ready shot:home
```

`eink-qemu/cli` builds the tool when its sources changed and runs it. The
binary is `eink-qemu/desktop/target/debug/eink-qemu` (Rust 1.89 or newer).

The firmware can be a PlatformIO or ESP-IDF build directory, a release app
`.bin`, a merged `.bin`, or a 16 MiB flash image, as `mkflash.py` takes them.
The board follows the firmware's chip: ESP32-C3 runs as the X3, ESP32-S3 as
the X4 Pro. Name the X4 or the Picco with `--machine`.

## A first run

```sh
eink-qemu/cli run ~/crosspoint-reader/.pio/build/default --put book.epub \
    ready press:confirm wait-screen press:confirm 'wait-log:Page render' \
    shot:page1 heap:page1
```

This makes a blank card (the stock X3 card's 2050 MiB with 32 KiB clusters),
copies the book onto it, boots, opens the file browser, opens the book,
waits for its first page, and keeps a screenshot and the heaps. Each step
prints one JSON record on stdout. The run directory is in the first record
and linked from `eink-qemu-out/latest`:

| Path | What it holds |
| --- | --- |
| `run.jsonl` | Every record: start, one per step, end |
| `log.txt` | The firmware's log as it printed it |
| `console.jsonl` | The same lines with the host time each arrived |
| `frames/` | Every settled screen, named by guest time, and `frames.json` |
| `shots/` | The `shot` steps' screenshots |
| `heaps/` | The `heap` steps' snapshots, block by block |
| `traces/` | The `trace-stop` steps' allocation windows by site |
| `fail/` | When a step fails: the screen, heaps, last 200 log lines and registers |
| `metrics.json` | Render times and memory reports read from the log |
| `report.html` | All of it on one page |

Exit codes: 0 when every step passed, 1 when a step failed, 2 for bad usage
or steps, 3 when the run could not start (firmware, card, QEMU).

## Steps

`eink-qemu steps` prints the list. A step is `kind` or `kind:argument`; the
argument is everything after the first colon, so a regex may contain
colons. Quote steps with spaces for the shell, or put the steps in a file
with `--script FILE`, one per line, where no quoting is needed and `#`
starts a comment.

| Step | What it does |
| --- | --- |
| `ready[:SECONDS]` | Wait for the boot to finish (below) |
| `wait-log:REGEX` | Wait for a log line matching REGEX after the last action |
| `wait-screen[:SECONDS]` | Wait for the screen to change after the last action and settle |
| `wait:SECONDS` | Let the guest run for that much guest time |
| `timeout:SECONDS` | Time limit of the waits after it (default 60, or `--timeout`) |
| `press:KEY[:MS]` | Press and release a key, held 150 ms of guest time by default |
| `touch:X,Y[:MS]` | Tap the touchscreen at panel pixels or percentages (`touch:50%,90%`) |
| `swipe:DIR[:MS]` | Swipe `left`, `right`, `up` or `down` across the middle of the panel |
| `swipe:X1,Y1,X2,Y2[:MS]` | Swipe between two points |
| `reset` | Reset the device; the card and flash stay as they are |
| `shot:NAME` | Save the settled screen as `shots/NAME.png` |
| `heap[:NAME]` | Walk every heap into `heaps/NAME.json`; the record has the totals |
| `trace-start:NAME`, `trace-stop:NAME` | An allocation window (needs `--trace`) |
| `mark:NAME` | A labelled record, for reports and comparisons |
| `expect-log:REGEX`, `expect-no-log:REGEX` | Check the whole log so far |
| `expect-shot:NAME=PNG[@X,Y,W,H...]` | Compare a shot with a PNG, outside masked rectangles |
| `repeat:N` ... `end` | Run the steps between N times |
| `hmp:COMMAND` | A QEMU monitor command; the output is in the record |

Keys: `back confirm left right up down power` on the X3 and X4; `up down
power home` on the X4 Pro; `up down power` on the Picco. Touch and swipe
work on the X4 Pro and the Picco. Steps are checked against the board
before the emulator starts, so a wrong key fails at once with exit code 2.

### Actions, waits and screenshots

`press`, `touch`, `swipe` and `reset` are actions. `wait-log` and
`wait-screen` look only at what happened after the last action (or after
`ready`), so a line from before the press cannot satisfy them. After a
match, the next `wait-log` looks past the matched line.

The screen counts as settled when the display has been quiet for `--settle`
(200 ms of host time) and the panel has not been BUSY for `--panel-idle`
(300 ms of guest time). That spans the gap between a page's black-and-white
pass and its grayscale passes, so a screenshot shows the finished page. A
`shot` right after an action first waits up to 2 s for the screen to start
changing. For CrossPoint, `wait-log:Page render` is the precise end of a
page: its line comes after the last grayscale pass, and its values
(`bw_render_ms`, `total_ms` and the rest) land in the record.

### Ready

`ready` waits for CrossPoint's boot: three completed panel refreshes (Home
drawn), then its first `[MEM]` report, about 10 s of guest time. A reboot
starts the count again and is reported as `restarts`. A card the firmware
cannot mount fails at once. Firmware that logs none of CrossPoint's refresh
lines is ready when its screen has been quiet for 5 s. The stock X4 Pro
firmware waits for Power at boot: put `press:power:2000` before `ready`.

## Time

Records carry `t`, host seconds since the run started, and `guest_ms`, the
guest's virtual clock. Under `-icount` the guest clock is the time the
firmware lives in; heap reads stop it, so they never disturb a measurement.
`wait` and key holds count guest time. Time limits count host time.

Runs are close to deterministic: accurate timing, a fixed real-time clock
(`--rtc`, 2026-01-01 09:00 by default) and a card made the same way give the
same screens and heaps run after run, and firmware timings within a few
milliseconds. When a timing comparison matters, use accurate timing (no
`--turbo`) and the 2050 MiB card.

## Heap profiling

`heap:NAME` stops the guest, walks every ESP-IDF heap (internal DRAM, RTC,
and PSRAM on the X4 Pro) and resumes. The record has each heap's free bytes,
largest free block, lowest free since boot and block count; the file has
every block. With the firmware's ELF (found beside the firmware, or
`--elf`), blocks are attributed to the code that allocated them.

`--trace` records every allocation and free with its call stack (the
heaptrace plugin). Between `trace-start:NAME` and `trace-stop:NAME`, the
record counts allocations, frees and the bytes still held at the end, by
allocation site; `traces/NAME.json` lists every site. The run keeps the
whole allocation log as `trace.bin` (the format of
`contrib/plugins/heaptrace.c`). To watch the heaps and the allocation
timeline live, add `--gui`.

```sh
eink-qemu/cli run .pio/build/default --put book.epub --trace \
    ready press:confirm wait-screen trace-start:open press:confirm \
    'wait-log:Page render' trace-stop:open heap:opened
```

## Comparing two builds

```sh
eink-qemu/cli compare base/.pio/build/default pr/.pio/build/default \
    --put book.epub --script reading.txt --names base,pr
```

Both run at once from identical cards. The comparison directory holds each
run's directory and:

- `compare.md`: a table for a pull request (result, Home time, page drawing
  and total rendering medians, memory reports, every `wait-log` value's
  median, heaps and allocation windows by name, screenshots by name).
- `compare.json`: the same numbers.
- `diffs/`: for each screenshot that differs, its differing pixels in black.

`--require-identical` fails the comparison when a screenshot differs, for
changes that must not change rendering. `--sequential` runs one after the
other.

A reading workload as a script file:

```text
# reading.txt
ready
press:confirm
wait-screen
press:confirm
wait-log:Page render
heap:opened
repeat:12
press:right
wait-log:Page render
end
shot:page13
heap:after-turns
```

## Sessions

```sh
eink-qemu/cli start .pio/build/default --put book.epub ready
eink-qemu/cli do press:confirm wait-screen shot:files
eink-qemu/cli do heap:files
eink-qemu/cli stop
```

`start` boots in the background, runs its steps, prints their records and
returns; the emulator keeps running. `do` runs more steps on it and prints
their records; `stop` ends it and writes the report. `status` says whether
it is alive. They find the session through `eink-qemu-out/latest`, or take
`--session DIR`. A session ends by itself after 30 minutes without a
request (`--hold-timeout`). `run --hold` holds the session in the
foreground instead.

## The desktop window

`--gui` shows the run in the desktop app: the screen, the console, and the
Heap tab with live maps. The steps run as they would headless and their
records still come out here. The window stays open after the steps; close
it to finish. It needs a built app (`pnpm tauri build --no-bundle` in
`eink-qemu/desktop`, or the installed app on macOS), or `EINK_QEMU_DESKTOP`.

## Other commands

- `eink-qemu flash FIRMWARE OUT [--machine M]` writes the 16 MiB flash image.
- `eink-qemu sd new|ls|put|get|mkdir|rm IMG ...` makes and edits card images.
- `eink-qemu report DIR` writes `report.html` again.

## Options worth knowing

| Option | Meaning |
| --- | --- |
| `--sd IMG` | Start from this card; the run uses a copy unless `--sd-in-place` |
| `--put HOST[=/DIR]` | Copy a file or folder onto the card before boot (repeatable) |
| `--machine x3\|x4\|x4pro\|picco` | The board, when the chip does not decide it |
| `--elf auto\|none\|PATH` | The ELF for heap sites and traces |
| `--turbo` | Fast peripherals; quicker, not for timing comparisons |
| `--ghosting`, `--animate` | E-ink ghosting and refresh animation (both off) |
| `--echo-log` | Print the firmware log on stderr as it comes |
| `--gdb PORT` | A GDB server on 127.0.0.1:PORT |
| `--qemu-arg ARG` | Any QEMU argument (repeatable) |

Stdout carries only records; progress lines go to stderr when it is a
terminal (or with `--progress`).

## What stays on the device

The X3 model is calibrated: SD reads within 3 percent of the device, CPU
within about 8 percent, panel BUSY times essentially exact. Use the device
for absolute timing claims, X4 Pro paths that lean on PSRAM (modelled 60 to
90 percent fast), deep-sleep latency, the frontlight, USB, real Wi-Fi, how
ghosting looks, and release sign-off.
