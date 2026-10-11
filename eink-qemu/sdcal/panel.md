# X3 panel timing: first UC8253 fit

The X3 machine defaults to the UC8253 variant measured on 2026-10-09. The
register LUTs determine refresh length; no waveform names or byte signatures
are used in the emulator. At the driver's PLL setting `0x09`, the fitted rule is:

```
DRF BUSY (us) = 138000 + 12850 * max(VCOM, WW, BW, WB, BB frames)
PON BUSY (ms) = 127
POF BUSY (ms) = 2  # unmeasured; retained generic default
```

These are virtual-clock deadlines, independent of host execution speed. The
firmware's wait log also includes polling/scheduling delay, so it can exceed the
modeled BUSY pulse by a millisecond or two.

## Device evidence and fit

The initial device data comprised 306 waits during a reading session with the
UC8253 driver. VER gave no answer. The driver's `uc8253X3DefaultConfig()` sends
42 bytes per row from `Uc8253X3Luts.h`: seven groups of six bytes. All five rows
have **identical frame counts within each bank**, including passive VCOM.

| Bank / operation | CDI | Frames (VCOM / WW / BW / WB / BB) | Device wait (ms) | Old emulator (ms) | New BUSY (ms) | Residual vs device midpoint (ms) |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| fast | 0x29 | 19 / 19 / 19 / 19 / 19 | 381–382 | 380 | 382.150 | +0.650 |
| preBwMid (AA-pre-BW(mid)) | 0xA9 | 27 / 27 / 27 / 27 / 27 | 484–486 | 540 | 484.950 | -0.050 |
| gc | 0x29 | 7 / 7 / 7 / 7 / 7 | 227–228 | 140 | 227.950 | +0.450 |
| full | 0x29 | 62 / 62 / 62 / 62 / 62 | 934–935 | 1240 | 934.700 | +0.200 |
| PON | — | — | 127–128 | 2 | 127.000 | -0.500 |

The nonempty group totals are fast `14 + 5`, preBwMid `21 + 6`, gc `7`,
full `52 + 10`, and normal/half `19 + 6 = 25`. Ordinary least squares on the
four refresh midpoints gives `137.5265 + 12.8556 * frames` milliseconds. Rounded
configuration values of 138 ms and 12.85 ms keep each modeled refresh within
0.2% of its device midpoint. A zero-intercept frame-only fit cannot explain
these measurements; the required period would range from 15.07 to 32.50 ms.
The added fixed cost could include analog settling and gate scanning, but this
measurement does not isolate its physical components or prove that it is PON.

## LUT decoding and timing policy

For the six-byte UC8253 format used by this X3, each group contains a level byte,
four unsigned eight-bit frame counts, and a repeat count. Its time is
`(f0 + f1 + f2 + f3) * repeat`. Zero frames consume no time and zero repeats
skip a group. A zero-length group does not prevent later groups from running.
Incomplete trailing groups are ignored. VCOM is decoded for timing without
applying its level byte as a pixel drive.

For UC8279d's seven-byte format, byte 0 is the group repeat, bytes 1–4 contain
level in bits 7:6 and frame count in bits 5:0, and bytes 5/6 repeat the first
and second pair of phases. Its time is
`group_repeat * ((f0 + f1) * state1_repeat + (f2 + f3) * state2_repeat)`.
This corrects the former decoder, which multiplied all four phases only by
byte 6 and could drop a valid later group when its second state was empty.
The same repeats feed the ink model in chronological order.

BUSY uses the longest of all five rows. Equal-row banks cannot establish
whether silicon waits for VCOM specifically, the longest row, or a common
sequencer. The maximum is an explicit conservative modeling policy; a test
with deliberately unequal rows is still needed. This fit does not justify a
claim that VCOM alone determines BUSY.

## PLL and revision limits

`frame-us` means effective frame period at reference PLL `0x09` for UC8253
and `0x0f` for UC8279d. Other values scale that period by
`rate(reference) / rate(current)`, rounded up to a whole microsecond. RST
restores the reference value.

The six-byte LUT family is documented in UltraChip
[UC8179c C0.6, page 24](https://www.orientdisplay.com/wp-content/uploads/2022/09/UC8179.pdf):
PLL codes 0–15 select 5, 10, 15, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110,
130, 150 and 200 Hz. Using that table for the six-byte X3 UC8253 is an inference
from the LUT format and the measured 77.79 Hz effective rate at code `0x09`:
the nominal 80 Hz period is only 2.8% shorter. Writes retain the low four bits.

The public UltraChip [UC8253c A0.61 datasheet, page 26](https://files.waveshare.com/wiki/3.52inch%20e-Paper%20HAT/UC8253c.pdf)
gives nominal rates `5 * (FRS + 1)` Hz for codes 0–23 and
`130 + 10 * (FRS - 24)` Hz for codes 24–31. Its seven-byte LUT layout differs
from the measured X3 despite the similar part name. The model provisionally
uses this table for the seven-byte UC8279d variant, retaining five PLL bits.

These tables supply relative scaling; `frame-us` calibrates the absolute
period at the reference code. A matching-revision datasheet or hardware PLL
sweep is still required to confirm both mappings. No hardware measurement
here validates UC8279d timing. The model does not claim that the generic
UC8253c PDF describes the measured six-byte controller.

## Configuration and trace

`-global uc8279.uc8253=false` selects UC8279d, including its generic timing
defaults (25000 us frames, zero DRF overhead, 2 ms PON/POF). The X3 applies its
calibration only when UC8253 is selected. Explicit global properties take
precedence, including zero values:

```
-global uc8279.frame-us=12850
-global uc8279.refresh-overhead-us=138000
-global uc8279.pon-ms=127
-global uc8279.pof-ms=2
```

A nonzero `busy-ms` overrides the complete DRF duration, including overhead.
Trace `-d trace:uc8279_refresh -D panel.trace` records all five frame totals,
PLL, and BUSY rounded up to milliseconds; deadlines retain microsecond precision.

## Ink and ghosting

X3 uses a copy of the X4 Pro UC8179 ink engine and its default strengths:
per-frame saturation, fast/slow residual charge, edge blooming, drift,
L* grayscale mapping, and animated refreshes. The six- and seven-byte X3
LUT formats are decoded into ordered drive frames, including VCOM, before
entering that engine. The previous net-movement sum and `dead-frames`
property are removed. With PSR REG=0, the engine uses the same approximate
fast/full OTP waveforms as X4 Pro. CDI N2OCP copies NEW to OLD once the
refresh's pixel classes are latched.

The calibrated UC8253 BUSY timing above is retained. Animation and charge
integration use its PLL-scaled frame period. UC8279d's generic `frame-us`
default is now 25000 to match X4 Pro; its former default was 20000.
`-global uc8279.animate=false` runs the same frames immediately.
The existing turbo setting `frame-us=0` also renders immediately, using a
1 us ink step to keep animation arithmetic finite.

Ink parameters use the same names as [the X4 Pro model](../ghosting/README.md),
under `-global uc8279.<property>=<value>`. Current OTP defaults are 10 fast
frames, 30 full frames per direction, and a held-pixel drive of 6 per mille.
These strengths were copied from X4 Pro, not fitted to X3 device photos.

Run `python3 -m unittest eink-qemu.tests.test_panel_ink` to compare the actual
C ink functions with X4 Pro and check LUT ordering, clearing, animation,
turbo mode, and the four calibrated BUSY durations. The harness uses a small
panel and clock/timer stubs, with address and undefined-behavior sanitizers.

Native verification of this change built both `qemu-system-riscv32` and
`qemu-system-xtensa`. CrossPoint 1.6.5 booted and rendered three successive
text-book pages on UC8253, UC8279d, and UC8253 with zero-period turbo
overrides. The UC8253 captures were clean; UC8279d retained faint residue
with the copied settings. This is emulator validation, not a device fit.

## Emulator validation

The supplied CrossPoint develop image with serial control was copied from
`/private/tmp/crosspoint-qemu-serial-cand/qemu-serial-evidence.local/builds/cand/x3-serial.bin`.
A private 16384 MB MBR/FAT32 card was created with `mkfs.vfat -s 16`, using a
copy of `mksd16.py` and the supplied `emu-card/tree` containing Aesop and the
Korean book. Neither source artifact nor physical device was modified.

Boot invocation (paths relative to this worktree):

```sh
X4NET='-nic none' eink-qemu/run.sh \
  panel-timing-evidence.local/x3-serial.bin \
  panel-timing-evidence.local/sd.img -display none \
  -d trace:uc8279_refresh -D panel-timing-evidence.local/refresh.trace \
  > panel-timing-evidence.local/firmware.log
```

The firmware detected the default controller:

```
[498] [XTDET] X3 stock probe VER=FF FF FF BUSY-timeout=0 -> UC8253
```

Serial commands entered Browse Files, selected Aesop, then advanced three pages
(`CMD:PRESS <id> PAGE_FORWARD 80`). The third turn exercised the steady-state
preBwMid + gc pair. Matching ordered trace rows to the firmware waits gives:

| Operation | Firmware `Wait complete` (ms) | Device midpoint (ms) | Worst observed error |
| --- | ---: | ---: | ---: |
| fast | 382–384 | 381.5 | +0.66% |
| preBwMid | 486 | 485 | +0.21% |
| gc | 228–230 | 227.5 | +1.10% |
| full | 935–936 | 934.5 | +0.16% |
| PON | 128 | 127.5 | +0.39% |
| normal/half (25 frames) | 459–461 | unattributed | — |

All five requested timing checks pass the 5% tolerance. The raw
[firmware log](session evidence) and [per-row trace](session evidence) are
checked in. The logs were captured before the provisional PLL mapping was
refined; both versions give identical periods at the exercised `0x09` code.
The screenshot below predates the copied X4 Pro ink model. It is the
emulated panel console captured through
QEMU's monitor after Home settled. Home, Browse Files, and Aesop remain readable;
the former simplified ink approximation retained gray image residue on
later pages. This timing work did not calibrate optical behavior.

![UC8253 Home screen](session evidence)

Both `ninja -C build qemu-system-riscv32 qemu-system-xtensa` targets build.
The linker reports its existing duplicate `-liconv` warning. A standalone C
harness against the actual decoder/timing functions checks group/state repeats,
empty gaps, truncated groups, PLL boundaries and scaling, the four-bank fit,
fixed BUSY override, and extreme-property saturation.

An additional boot with `-global uc8279.uc8253=false` selected UC8279d and
rendered Home. QOM inspection confirmed its 20000/0/2/2 generic timing
properties; its corrected BW_DU decoder now counts 27 frames (540 ms),
including the previously dropped second group, while BW_GC remains 53
frames (1060 ms). A separate UC8253 boot with explicit frame/overhead/PON/POF
values of 10000/0/3/4 retained every override, including zero overhead, and
traced 620 ms for the 62-frame full bank and 190 ms for fast.

## Still unmeasured

The initial 459, 409 and 476 ms cold-open waits and 400–409 ms UI waits were
not attributed to LUT banks. The 25-frame normal/half bank predicts 459.25 ms,
which is a candidate explanation for 459 ms, not a verified attribution.
POF remains at 2 ms until measured. Temperature, voltage, partial gate range,
unequal LUT rows, PLL ratios, and repeated PON while already powered require
separate device experiments; this first fit does not calibrate those effects.
