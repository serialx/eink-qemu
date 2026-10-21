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

It assumes ESP-IDF and nothing about the firmware itself: any app for these
boards boots, runs scripts, and gets screenshots, heap snapshots and the
emulator's own measurements (display updates, panel refreshes and their
time). What it knows about particular firmware is data in
[`eink-qemu/profiles`](../../profiles/README.md): CrossPoint Reader's boot
milestones, its error lines, render times, memory reports and allocation
wrappers, and the stock Xteink firmware's boot gate.

## Install

### From the desktop app

The macOS app and Windows installer include the CLI, QEMU, ROMs and the
heaptrace plugin. No Cargo installation or environment variables are needed.
On macOS, with the app in `/Applications` and `~/.local/bin` on your PATH:

```sh
mkdir -p ~/.local/bin && ln -s /Applications/eink-qemu.app/Contents/MacOS/eink-qemu ~/.local/bin/eink-qemu
```

The app can also live in `~/Applications`; adjust the symlink accordingly.
On Windows, add the install directory to PATH for the current PowerShell:

```powershell
$env:Path += ";$env:ProgramFiles\eink-qemu"
```

The managed-bridge installer uses this fixed per-machine location. Older
installers may have used a user-selected folder under LocalAppData.
To keep it on PATH, add that directory (containing `eink-qemu.exe` and
`eink-qemu-desktop.exe`) to your **user** Path in Environment Variables,
then open a new terminal.
Neither method needs administrator rights. Keep the CLI inside the app's
layout so it can find QEMU; `--gui` opens the app beside it.

### From a release archive

Download `eink-qemu-cli-<version>-<platform>.tar.xz` and the matching QEMU
archives from the same release. CLI platforms are `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-pc-windows-msvc`,
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. QEMU uses
`x86_64-w64-mingw32` for Windows and drops `unknown-` on Linux.
Take `xtensa-softmmu` for X4 Pro/Picco, `riscv32-softmmu` for X3/X4, or both.
Extract all the archives into the same directory; they share this layout:

```text
qemu/
  bin/eink-qemu[.exe]
  bin/qemu-system-{xtensa,riscv32}[.exe]
  share/qemu/esp*.bin
  plugins/libheaptrace.{dylib,dll,so}
```

Put `qemu/bin` on PATH, or invoke the CLI there directly. It finds QEMU,
ROMs and plugins beside it. The standalone CLI does not include the desktop
app; `--gui` requires a separate desktop installation or
`EINK_QEMU_DESKTOP` pointing to its executable. The QEMU archives retain
their host-library requirements (on macOS, the Homebrew libraries used to
build QEMU); use the desktop bundle for a self-contained macOS installation.

`--qemu DIR` overrides `EINK_QEMU_BIN`, which overrides automatic discovery.
`EINK_QEMU_SHARE` overrides the ROM directory passed as `-L`, and
`EINK_QEMU_PLUGINS` overrides the plugin directory.

### From source

Build QEMU first ([EINK-QEMU.md](../../../EINK-QEMU.md#build-and-run)). The
tool finds `build/qemu-system-*` in the repository, or takes `--qemu DIR`
or `EINK_QEMU_BIN`. Then, from the repository root:

```sh
eink-qemu/cli run .pio/build/default ready shot:home
```

`eink-qemu/cli` builds the tool when its sources changed and runs it. The
binary is `eink-qemu/desktop/target/debug/eink-qemu` (Rust 1.89 or newer);
run it directly when the checkout must not be rebuilt. The examples below
use this wrapper; with an installed CLI, replace `eink-qemu/cli` with
`eink-qemu`.

The firmware can be a PlatformIO or ESP-IDF build directory, a release app
`.bin`, a merged `.bin`, or a 16 MiB flash image, as `mkflash.py` takes them.
A build directory brings its own bootloader and partition table. A release
app gets the bundled bootloader and Arduino-ESP32's 16 MB partition table
(the Picco's stock layout on the Picco); give the firmware's own table with
`--partitions partitions.csv` (or its binary table) when it uses another
layout; it is read and checked as ESP-IDF's `gen_esp32part.py` does. The app
goes to ESP-IDF's boot-default slot: the factory partition, else the lowest
OTA slot. The board defaults to X3 for ESP32-C3 and X4 Pro for ESP32-S3.
Case-insensitive filename tokens of ASCII letters and digits
select X4 with `x4` but no `x3`, X3 with `x3` but no `x4`, or Picco with
`picco` on S3; both `x3` and `x4`, or neither, use the default. Folder names,
`x4pro` as an X4 hint, and hints for another chip are ignored. `--machine`
overrides the filename and default.

## A first run, any firmware

```sh
eink-qemu/cli run build/ ready shot:boot heap:boot
```

`ready` passes once the screen has been quiet for 5 host seconds
(`--ready-quiet`); a screen that never stops changing (a ticking clock)
needs a shorter quiet or a `wait-log:` on a line of your own. `shot` keeps
the screen and `heap` walks the heaps. Look at `shots/boot.png` before you
script input: which key or touch does what is up to the firmware. A press
or touch succeeds once it is delivered, whether or not the firmware reacts;
the `wait-screen` after it is what fails when nothing changes:

```sh
eink-qemu/cli run build/ ready touch:240,400 wait-screen shot:next
```

The firmware log comes from USB Serial/JTAG, where ESP-IDF mirrors its console by default;
firmware that logs only to UART0 needs `--console uart0`. The emulated
boards' ROM prints its boot lines only on USB, so on UART0 the log starts
with the firmware's own lines, and a boot after `reset` counts from the
reset.

The stock Xteink firmware turns itself off again unless Power is still held
when its app starts, a few seconds into the boot: run it with
`--power-hold 3000` (the board holds Power for 1.5 s at a cold boot by
default).

## A first run with CrossPoint (X3)

```sh
eink-qemu/cli run ~/crosspoint-reader/.pio/build/default --put book.epub \
    ready press:confirm wait-screen press:confirm 'wait-log:Page render' \
    shot:page1 heap:page1
```

This makes a blank card (the stock X3 card's 2050 MiB with 32 KiB clusters),
copies the book onto it, boots, and waits for Home. A book in Korean,
Chinese or Japanese also needs CrossPoint's SD fonts on the card (see
[Driving CrossPoint](#driving-crosspoint)); without them its pages show
replacement boxes, and identical screenshots then say nothing about its text. On a blank card Home has
Browse Files selected: the first `confirm` opens the file list, the second
opens its first entry, the book. The run waits for the first page, then
keeps a screenshot and the heaps. Each step prints one JSON record on
stdout. The run directory is in the first record and linked from
`eink-qemu-out/latest`:

| Path | What it holds |
| --- | --- |
| `run.jsonl` | Every record: start, one per step, end |
| `script.txt` | Every step the run (or session) ran, one per line: replay it with `--script` |
| `log.txt` | The firmware's log as it printed it |
| `console.jsonl` | The same lines with the host time each arrived |
| `frames/` | Every settled screen, named by guest time, and `frames.json` |
| `shots/` | The `shot` steps' screenshots |
| `heaps/` | The `heap` steps' snapshots, block by block |
| `traces/` | The `trace-stop` steps' allocation windows by site, and `trace.bin` |
| `fail/` | When a step fails: the screen, heaps, last 200 log lines and registers |
| `metrics.json` | Display updates, panel refreshes and their guest time, error and warning line counts (ESP-IDF's, and CrossPoint's `[ERR]`); CrossPoint's render times and memory reports under `crosspoint` |
| `report.html` | All of it on one page |

Exit codes: 0 when every step passed, 1 when a step failed, 2 for bad usage
or steps (found before the emulator starts), 3 when the run could not start
(firmware, card, QEMU), 130 when interrupted (Ctrl-C or SIGTERM, which still
write the end record and report). A run that could not start leaves
`error.txt`, a `run.jsonl` with the setup error and a `report.html` saying
why, and the same command, `--out` included, can be run again (for
`compare` too, when neither side started).

### The X4 Pro

The X4 Pro has a touchscreen and three keys. Coordinates are pixels of its
screenshots (480 wide, 800 tall, origin top left), or percentages; look at a
shot to find a menu entry, then tap it:

```sh
eink-qemu/cli run ~/crosspoint-reader/.pio/build/x4pro --put book.epub \
    ready shot:home touch:240,348 wait-screen shot:files \
    touch:240,138 'wait-log:Page render' swipe:left 'wait-log:Page render' \
    shot:page2 press:home wait-quiet:3 shot:home-again
```

## Steps

`eink-qemu steps` prints the list. A step is `kind` or `kind:argument`; the
argument is everything after the first colon, so a regex may contain
colons. Quote steps with spaces for the shell, or put the steps in a file
with `--script FILE` (or `--script -` for stdin), one per line, where no
quoting is needed and `#` starts a comment.

| Step | What it does |
| --- | --- |
| `ready[:SECONDS]` | Wait for the boot to finish (below) |
| `wait-log:REGEX` | Wait for a log line matching REGEX after the last action (or `ready`) |
| `wait-screen[:SECONDS]` | Wait for the screen to change after the last action and settle |
| `wait-quiet[:SECONDS]` | Wait until the screen has been unchanged for SECONDS of guest time (default 2) |
| `wait:SECONDS` | Let the guest run for that much guest time |
| `timeout:SECONDS` | Time limit of the waits after it (default 60, or `--timeout`) |
| `press:KEY[:MS]` | Press and release a key, held 150 ms of guest time by default |
| `touch:X,Y[:MS]` | Tap the touchscreen at screenshot pixels or percentages (`touch:50%,90%`) |
| `swipe:DIR[:MS]` | Swipe `left`, `right`, `up` or `down` across the middle of the panel |
| `swipe:X1,Y1,X2,Y2[:MS]` | Swipe between two points |
| `reset` | Reset the device; the card and flash stay as they are |
| `shot:NAME` | Save the settled screen as `shots/NAME.png` |
| `heap[:NAME]` | Walk every heap into `heaps/NAME.json`; the record has the totals |
| `trace-start:NAME`, `trace-stop:NAME` | An allocation window (needs `--trace`) |
| `mark:NAME` | A labelled record; `compare` groups `wait-log` values by the mark before them |
| `heap-on-log:NAME=REGEX` | From here on, walk the heaps whenever a log line matches REGEX (below) |
| `expect-log:REGEX`, `expect-no-log:REGEX` | Check the whole log so far |
| `expect-shot:NAME=PNG[@X,Y,W,H...]` | Compare a shot with a PNG, outside masked rectangles |
| `repeat:N` ... `end` | Run the steps between N times |
| `repeat:until-log:REGEX` ... `end` | Run them until a log line matches REGEX (below); `repeat:N:until-log:REGEX` stops after N passes at most |
| `hmp:COMMAND` | A QEMU monitor command; the output is in the record |

Keys: `back confirm left right up down power` on the X3 and X4; `up down
power home` on the X4 Pro; `up down power` on the Picco. Touch and swipe
work on the X4 Pro and the Picco. Steps are checked against the board
before the emulator starts, so a wrong key fails at once with exit code 2.

Names (shots, heaps, traces, marks) take letters, digits, `.`, `_` and `-`.
In a name, `{i}` is the iteration of the innermost `repeat`, so
`repeat:12 press:right 'wait-log:Page render' shot:page{i} end` keeps
`page1.png` to `page12.png`. A name used again gets `-2`, `-3`, and so on.

### Actions, waits and screenshots

`press`, `touch`, `swipe` and `reset` are actions. `wait-log` and
`wait-screen` look only at what happened after the last action (or after
`ready`): every log line printed before the action is delivered first, so
none of them can satisfy the wait. After a match, the next `wait-log` looks
past the matched line. A line the boot printed before `ready` passed, such
as the first page of a book the firmware boots into, can therefore never
satisfy a `wait-log` after `ready`: the run prints a `warning` record when
the first `wait-log` after `ready` is for such a line, and a `wait-log` that
times out names the latest matching line it did not look at. Check for a
line that may already be in the log with `expect-log`.

A loop can run until something happens rather than a fixed number of
times. `repeat:until-log:REGEX` ... `end` runs its steps until a log line
printed since the loop began matches REGEX. It looks after each pass, and a
`wait-log`, `wait-screen` or `wait-quiet` inside the loop that is waiting
when the line has come, or that passes once it has, ends the loop at once:
the wait is a passed step with `loop_ended` (the line), and the rest of the
pass is skipped. A `press`, `touch` or `swipe` in the loop looks too, after
its wait for the firmware to go quiet (the line that ends a pass often comes
just after the one its last wait matched): when the line has come, the input
is not sent (`sent: false`) and the loop ends. That way a line that comes instead of the one a wait looks
for (an error, say) ends the loop rather than timing the wait out. The line
of an enclosing until-log loop ends the loops inside it too, up to that
loop. Its `repeat` record has `passes`, `ended` (`log`; `outer` when an
enclosing loop's line ended it; `count` when it ran its `N` passes) and the
`line`:

```text
# CrossPoint: turn pages until the reader saves its place in the sixth spine item
repeat:until-log:Progress saved: spine=5
press:right
wait-log:Page render
end
```

Before each input step the tool waits until the firmware's log has been
quiet, the panel idle and the CPU idle (from the emulator's instruction
count) for `--input-quiet` (200 ms of guest time, at most 3 s). Firmware
busy with background work, such as CrossPoint laying out or prewarming the
next pages after a render, often logs nothing while it works, does not poll
its buttons and would miss a short press. The record's `waited_ms` says how
long it waited.

The screen counts as settled when the display has been quiet for `--settle`
(200 ms of host time) and the panel has not been BUSY for `--panel-idle`
(300 ms of guest time). That spans the gap between a page's black-and-white
pass and its grayscale passes, so a screenshot shows the finished page. A
`shot` right after an action first waits up to 2 s for the screen to start
changing.

`wait-screen` returns at the first settled screen after the action, and a
transient screen, such as a loading message, counts. To wait past one, wait
for the line that ends the work (`wait-log:...`), or for the screen to stay
the same (`wait-quiet:3`). For CrossPoint, `wait-log:Page render` is the
precise end of a page: its line comes after the last grayscale pass, and its
values (`bw_render_ms`, `total_ms` and the rest) land in the record.

### Ready

`ready` passes when the screen has shown a drawn (not blank) image,
unchanged for 5 host seconds (`--ready-quiet`), after an update of this boot, and the ROM has not rebooted the firmware
meanwhile (`how` is `screen-quiet`). CrossPoint is read more closely, from
its profile: three completed panel refreshes (Home drawn), then its first
periodic `[MEM]` report, about 10 s of guest time (`how` is `home`). When it
boots straight into a book (it was asleep in one), its reader's first page
counts instead (`how` is `reader`): its `Page render` line, with the report
before or after it; the record's `screen` names the screen it entered
(`Home`, `EpubReader`). The reader shows itself by its `[ACT] Entering
activity` line, or by `[MEM] reader enter` in builds that log it at info
level. A reader that logs no page line (a release build) is ready 12 host
seconds after its last refresh (`how` is `quiet`). A build that logs none of
these is read as a boot to Home, unless a page line comes before the memory
report. A reboot starts
over and is reported as `restarts`; after `reset` (or `hmp:system_reset`),
`ready` waits for the new boot. An image the ROM cannot boot, or a card
CrossPoint cannot mount, fails at once; a card that fails after the boot was
ready fails the next `ready`. When the stock Xteink firmware turns
itself off because Power was not held long enough, the timeout says so: run
with `--power-hold 3000` (or put `press:power:4000` before `ready`).

## Time

Records carry `t`, host seconds since the run started, and `guest_ms`, the
guest's virtual clock. Under `-icount` the guest clock is the time the
firmware lives in; heap reads stop it, so they never disturb a measurement.
`wait`, `wait-quiet`, key holds and the input wait count guest time. Time
limits count host time.

Runs are close to deterministic: accurate timing, a fixed real-time clock
(`--rtc`, 2026-01-01 09:00 by default) and a card made the same way give the
same screens and heaps run after run, and firmware timings within a few
milliseconds. When a timing comparison matters, use accurate timing (no
`--turbo`) and the 2050 MiB card. Clocks at a glance: `wait`, `wait-quiet`,
key holds, `--input-quiet` and `--panel-idle` are guest time; `--ready-quiet`,
`--settle` and every time limit are host time.

## Records

Every record has `type` (`start`, `step`, `end`, and for sessions `done`,
`hold`, `stopped`), and steps have `i` (their order), `step`, `text` (as
written), `ok`, `t`, `ms` (how long the step took), `guest_ms`, `iter` inside
a repeat, and `error` when they fail. Each step adds its own fields:

| Step | Fields |
| --- | --- |
| `ready` | `how` (`screen-quiet`, or for CrossPoint `home`/`reader`/`quiet`), `boot_s` (host seconds from this boot's ROM banner, or without one from the run's start or the last reset, to Home or a book's first page for CrossPoint, or to the last screen update before the quiet; the step's own `ms` also counts the quiet), `home_fw_ms` (CrossPoint: the firmware's ms at Home) or `page_fw_ms` (at the book's first page), `screen`, `restarts` |
| `mark` | `name`, and the counters so far: `screen_updates` (display updates the guest sent), `panel_refreshes`, `panel_refresh_ms` (their guest time) |
| `wait-log` | `line`, `fw_ms` (the line's own timestamp), `values` (`name=123ms` pairs); `loop_ended` instead when its until-log loop's line ended it (also on `wait-screen` and `wait-quiet`) |
| `wait-screen` | `frame`, `repaints` (settled updates before it that redrew the same picture) |
| `wait-quiet` | `frame` |
| `press`, `touch`, `swipe` | `waited_ms` (the input wait), coordinates; `sent: false` and `loop_ended` when its until-log loop ended before the input |
| `shot` | `file`, `pixels_sha256` (of the gray pixels, not the PNG file), `size` |
| `heap` | see below; a `heap-on-log` walk adds `on_log` (the line), `line_fw_ms`, `line_age_ms` and `lines` |
| `repeat` (until-log loops) | `passes`, `ended` (`log`, `outer` or `count`), `line` |
| `trace-stop` | `allocs`, `alloc_bytes`, `freed`, `kept`, `kept_bytes`, `older_freed_bytes`, `top_sites` |
| failed step | `error`, `last_lines`, `fail_dir` |

A `warning` record (`text`, `warning`) says something about a step that did
not fail it, such as a `wait-log` that cannot see a line the boot printed.

The `end` record's `metrics` hold the run's `screen_updates`,
`panel_refreshes`, `panel_refresh_ms`, `log_errors` and `log_warnings`
(ESP-IDF's `E (…)` and `W (…)` lines, and CrossPoint's own `[ERR]` lines),
and for CrossPoint a `crosspoint` object with its page renders, render
medians and lowest `[MEM]` free.

## Heap profiling

`heap:NAME` stops the guest, walks every ESP-IDF heap (internal DRAM, RTC,
and PSRAM on the X4 Pro) and resumes. The record's numbers:

| Field | Meaning |
| --- | --- |
| `free` | Free bytes by every heap's own counter: what `heap_caps_get_free_size` and CrossPoint's `[MEM] Free` report |
| `largest` | The largest free block of any heap; the firmware's `MaxAlloc` is this less allocator overhead |
| `min_free` | The sum of each heap's lowest free since boot; the lows need not have happened at once, so it can differ slightly from `[MEM] Min Free` |
| `dram_free`, `dram_largest` | The same, internal DRAM heaps only |
| `internal_free`, `internal_largest` | Internal memory, DRAM and the RTC heap |
| `heaps` | Each heap: `name`, `free`, `largest`, `min_free`, `used`, `blocks`, `corrupt` |
| `live_sites` | With `--trace`: the allocation sites holding the most requested bytes in live blocks |
| `since_previous` | The change in `free`, `largest`, `min_free` and the DRAM totals since the previous `heap` step |

`heaps/NAME.json` has every block: each heap's `blocks` is a flat list of
four numbers per block (offset from the heap start, length, flags with 1 =
free, 2 = poisoned, 4 = corrupt, and the requested size). With `--trace`,
`sites` gives each block's index into the file's `siteNames` (function and
source line; -1 when the block was not traced).

`heap-on-log:NAME=REGEX` walks the heaps whenever a log line matches, from
the step on, for the rest of the run (in a session, also between requests):
`heap-on-log:err=\[ERR\]` keeps the heaps at each of CrossPoint's errors
as `heaps/err.json`, `err-2.json`, and so on, each with a `heap` record whose
`on_log` is the line. The walk stops the guest as soon as the tool sees the
line, usually within tens of ms; `line_age_ms` (host time) says how long it
was, and a busy host can make it longer. So it shows the heap just after the
firmware logged the line, not at the instant something failed. Lines that
come together get one walk (`lines` counts them); each watch takes at most
100. A walk that cannot be kept is a `warning` record, not a failed step.
The same NAME again watches for its new REGEX; in a repeat, `{i}` in NAME
makes one watch per pass.

`--trace` records every allocation and free with its call stack (the
heaptrace plugin; it needs the firmware's ELF, found beside the firmware or
given with `--elf`). An allocation's site is the first of the firmware's own
functions above the allocator; a function that only wraps an allocation,
such as CrossPoint's `makeUniqueNoThrow`, is listed in the firmware's
profile and counted with the allocator, so its caller is the site. Between `trace-start:NAME` and `trace-stop:NAME`, the
record counts allocations, frees and the bytes still held at the end, by
allocation site; `traces/NAME.json` lists every site. A window sees only
what happened inside it; `live_sites` in a `heap` record covers everything
alive, from boot. The run keeps the whole allocation log as `trace.bin`
(the format of `contrib/plugins/heaptrace.c`). To watch the heaps live, add
`--gui`.

```sh
eink-qemu/cli run .pio/build/default --put book.epub --trace \
    ready press:confirm wait-screen trace-start:open press:confirm \
    'wait-log:Page render' trace-stop:open heap:opened \
    press:back wait-quiet:3 heap:home
```

## Comparing two builds

```sh
eink-qemu/cli compare base/.pio/build/default pr/.pio/build/default \
    --put book.epub --script reading.txt --names base,pr
```

Both run at once from identical cards, and both boards must take the steps
before either starts. Put the workload's prerequisites (the book, its fonts,
the options) in the pull request beside the numbers. The comparison directory holds each run's directory
and:

- `compare.md`: a table for a pull request. It shows the result, the input
  pacing, the boot (to the first drawn Home or book page, from the first
  `ready`; `ready` itself passes later, after the memory report; later
  `ready` passes, such as wakes into a book, as medians by how they
  finished), each `mark`
  window's guest time, the log's error lines, and then only what changed: a
  time that moved by at least the threshold (10 ms or 1% of the base,
  whichever is larger; `--noise-ms`, `--noise-pct`; for host time ten times
  the ms and five times the percentage; for a sum of N values N times the
  ms), a median of a different number of samples, any other number that
  differs, a differing heap, live allocation site, allocation window (its
  allocations, bytes, blocks kept and freed, and older blocks freed) or
  screenshot. A line under it counts the rows it leaves out and gives the
  thresholds. Below it,
  collapsed, every row: each `mark` window's panel refreshes and display
  updates, every `wait-log` value's median and sum, the whole run's
  counters, heaps, live allocation sites (each side's five largest; `-`
  when a site is not among that side's ten largest) and allocation windows
  by name, screenshots by name, and for CrossPoint its page drawing and
  total rendering medians and memory reports. A change under the threshold
  is marked `(below threshold)`, without a percentage: the threshold is a
  display cutoff, not a measured noise level, so check it with a
  comparison of a build against itself.
- `compare.json`: the same numbers.
- `diffs/`: for each screenshot that differs, its differing pixels in black.
- `report.html`: the same tables, every row behind a fold, and the
  screenshot pairs.

A `mark:NAME` starts a window that runs to the next mark (or the last
step): the table gives its guest time and the panel refreshes and display
updates inside it, for any firmware, and groups `wait-log` values by it, so
a script can measure only the page turns, apart from the opening page. The
marked rows come first, and the whole-run rows after them say so.
CrossPoint's periodic `[MEM]` rows are its last 10-second report, not a
measurement at a step: use `heap` steps for that. With `--trace`, each heap
snapshot's live allocation sites are listed side by side. A side that could
not start still gets its outcome in the table. `--require-identical` fails
the comparison when a screenshot differs, for changes that must not change
rendering. `--sequential` runs one after the other; interrupted during the
first, it does not start the second.

Input pacing is part of what a run measures. Each press holds its key 150
ms of guest time (`press:KEY:MS`), and each input first waits for the
firmware to go quiet (`--input-quiet`, 200 ms); a page turn's time counts
from its press, so a harness that presses differently measures slightly
different numbers (a few percent on a page turn). Compare numbers taken with
the same pacing: both sides of a `compare` share it, and its table and each
start record (`pacing`) say what it was. The table's pacing row gives the
quiet period an input waits for (each input's actual wait is its record's
`waited_ms`) and the default key hold; the start records also have
`settle_ms` and `panel_idle_ms`.

A reading workload as a script file:

```text
# reading.txt: open the book, then twelve measured page turns
ready
press:confirm
wait-screen
press:confirm
wait-log:Page render
heap:opened
mark:turns
repeat:12
press:right
wait-log:Page render
shot:page{i}
end
mark:after
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
returns; the emulator keeps running. `do` runs more steps and prints their
records, then a `done` record whose `screen` is the latest settled screen:
look at it to see where you are before the next step. `status` reports
whether the session is alive, whether a request is running, the last step
and the latest screen; it exits 0 when the session is alive, 1 when it has
ended, and 3 when it cannot be reached. `stop` ends the session (cancelling a running `do`),
waits for it to write its end record and report, and prints the outcome:
the session fails if any of its steps failed. They find the session through
`eink-qemu-out/latest`, or take `--session DIR` (pass the run directory a
`start --out DIR` used). A session ends by itself after 30 minutes without
a request (`--hold-timeout`). `run --hold` holds the session in the
foreground instead.

## Driving CrossPoint

Observed on CrossPoint X3 builds; the firmware owns these behaviors, so
check a screenshot when a step does not do what you expect.

- Home lists the book in progress (if any), Browse Files, Library, File
  Transfer and Settings. `up`/`down` move the selection, `confirm` opens.
  Home keeps its selection when you come back, and `back` on Home resumes
  the last book.
- In Browse Files, `confirm` opens a book or folder. In a book, `right`
  turns forward and `left` back (on the X4 Pro, `down` and `up`), and `back`
  returns to Home.
- In Settings, `confirm` on a tab header moves to the next tab; `down` and
  `right` move into the list. `back` from a row returns to its tab header,
  and from the header leaves Settings.
- Coming back from a book, Home selects the book's tile, which sits above
  Browse Files: Settings is then four `down`s away, not three. In a list of
  values (Text Settings, Size), the first `down` selects the first value, not
  the one after the current. Take a `shot` after each move until the path is
  known; `back` on Home resumes the book, so stop pressing `back` at Home.
- To check that a setting survives a reset: change it (for the font size,
  Settings, the Reader tab, Text Settings, Size), press `back` until Home,
  `wait-quiet:3`, then `reset ready`, open the same screen again, and compare
  the shots. Settings are written when you leave the screen.
- Sleep: a long Power press (`press:power:600`) shows the sleep screen and
  the log says "Entering deep sleep"; QEMU then suspends the VM, which waits
  and shots handle. `press:power` wakes it into a new boot: follow it with
  `ready`. A short press does not sleep, and a `press` that succeeded only
  means the key was delivered: check with `shot` or `wait-log:Entering deep
  sleep`.
- On the X4 Pro, tap the menu entries (coordinates are screenshot pixels,
  origin top left, 480 wide by 800 tall in portrait), turn pages with
  `swipe:left` and `swipe:right` or `down`/`up`, and `press:home` returns to
  Home. A tap on the edge of a page does not turn it.
- Books in Korean, Chinese or Japanese need CrossPoint's SD fonts on the
  card, in `/fonts` (see CrossPoint's `docs/sd-card-fonts.md`): `--put
  path/to/fonts` copies a folder named `fonts` there. Without them the pages
  show replacement boxes.

## Driving the stock Xteink firmware

Observed on the stock X4 Pro 7.5.11 dump; check a screenshot when a step
does not do what you expect. It powers itself off unless Power is held when
its app starts (about 2.3 s of guest time): run with `--power-hold 3000`.
It has no CrossPoint log lines, so `ready` is the quiet screen. It is
driven by touch (`down` does nothing on its first dialog). Its heaps walk
without an ELF.

On a blank card, from boot to an opened book (screenshot pixels):

```text
ready
touch:408,155       # close the System Font dialog (its X): an empty Bookshelf
wait-screen
touch:440,84        # the menu button: Read, All Files, USB, Cloud, Settings
wait-screen
touch:330,247       # All Files
wait-screen
touch:200,183       # the first file (an EPUB put on the card with --put)
wait-quiet:5        # it shows "Opening…" first
shot:page1
```

A short `press:power` puts it to sleep: the screen stays, with a small moon
beside the battery, and the log has `hal_sleep ... entered=1`; another
press wakes it (`sleep_result success=1`). That sleep keeps the VM running.
When its boot gate turns it off (no Power hold), the VM is suspended: QEMU's
deep sleep, which `ready` reports instead of waiting out.

## The desktop window

`--gui` shows the run in the desktop app: the screen, the console, and the
Heap tab with live maps. The steps run as they would headless and their
records still come out here, followed by a `steps-done` record when they
finish. The window stays open after the steps; close it (or send SIGTERM to
the app) to finish. The bundled CLI opens its own desktop app. A standalone
CLI finds a sibling app executable or `/Applications/eink-qemu.app` on
macOS; otherwise, set `EINK_QEMU_DESKTOP` to the app executable. From source,
build it with `pnpm tauri build --no-bundle` in `eink-qemu/desktop`.

## In a sandbox

The tool starts QEMU with Unix sockets in `/tmp/eink-qemu-*`, and a held
session listens on `/tmp/eink-qemu-ctl-<pid>.sock`. Opening a card in place
(`--sd-in-place`, `sd`) takes a lock in the desktop app's data directory
(`~/Library/Application Support/net.serialx.eink-qemu/locks` on macOS),
shared so the app and the tool never write one card; set `EINK_QEMU_LOCKS`
to another writable directory when that one is off limits, and start the
desktop app with the same `EINK_QEMU_LOCKS` (it reads it too), or it will
not see the tool's locks. A sandbox that
blocks sockets or those paths needs those permissions granted; `status`
says when it cannot reach a session it is not allowed to connect to, rather
than calling it dead.

## Other commands

- `eink-qemu flash FIRMWARE OUT [--machine M] [--partitions CSV|BIN]` writes the 16 MiB flash image.
- `eink-qemu sd new|ls|put|get|mkdir|rm IMG ...` makes and edits card images.
- `eink-qemu report DIR` writes `report.html` again.

## Options worth knowing

| Option | Meaning |
| --- | --- |
| `--sd IMG` | Start from this card; the run uses a copy unless `--sd-in-place` |
| `--put HOST[=/DIR]` | Copy a file or folder onto the card before boot (repeatable) |
| `--machine x3\|x4\|x4pro\|picco` | The board, when the chip does not decide it |
| `--panel uc8279-68\|uc8179` | X4 Pro display panel (default `uc8279-68`) |
| `--partitions CSV\|BIN` | The firmware's partition table, for an app `.bin` or a build directory |
| `--power-hold MS` | Hold Power this long at a cold boot (default 1500; stock Xteink firmware: 3000) |
| `--console usb\|uart0` | Where the firmware log comes from (USB Serial/JTAG by default) |
| `--ready-quiet SECONDS` | How long the screen must stay unchanged for `ready` (default 5) |
| `--elf auto\|none\|PATH` | The ELF for heap sites and traces |
| `--turbo` | Fast peripherals; quicker, not for timing comparisons |
| `--ghosting`, `--animate` | E-ink ghosting and refresh animation (both off) |
| `--wifi` | Enable Wi-Fi using NAT without host setup (Wi-Fi is off by default) |
| `--wifi-mode nat\|bridge` | Network mode with `--wifi` (default `nat`); bridge mode requires host setup |
| `--input-quiet MS` | The wait for a quiet log, idle panel and idle CPU before input (0 turns it off) |
| `--echo-log` | Print the firmware log on stderr as it comes |
| `--gdb PORT` | A GDB server on 127.0.0.1:PORT (`compare` takes it only with `--sequential`) |
| `--qemu-arg ARG` | Any QEMU argument (repeatable) |

`--wifi --wifi-mode bridge` puts the reader directly on the LAN on Linux,
macOS and Windows. Complete the
[host network setup](../../../EINK-QEMU.md#native-wi-fi) first. `X4BR` selects
another bridge, interface or TAP adapter when bridge mode is selected.
For NAT with port forwarding, set
`X4NET='-nic user,model=esp32_wifi,hostfwd=tcp::8080-:80'` to reach the
device's port 80 at `localhost:8080`. `X4NET` replaces the selected mode's
network arguments when `--wifi` is enabled.

Stdout carries only records; progress lines go to stderr when it is a
terminal (or with `--progress`).

## What stays on the device

The X3 model is calibrated: SD reads within 3 percent of the device, CPU
within about 8 percent, panel BUSY times essentially exact. Use the device
for absolute timing claims, X4 Pro paths that lean on PSRAM (modelled 60 to
90 percent fast), deep-sleep latency, the frontlight, USB, real Wi-Fi, how
ghosting looks, and release sign-off.
