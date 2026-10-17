# UC8179 ghosting model: notes, data and tests

Later change: refreshes animate and BUSY follows the frames; `otp-full-frames`
is now 30 and `busy-ms` 0 (derive). See `refresh-animation.md`.

Notes from the helper agent that turned the UC8179 model from "show the planes"
into an ink simulation fitted to photos of a real X4 Pro, while running
CrossDink 968e1a67 (`x4-pro-debug`) under the SDL display. Its eight commits are
on `x4prosim` (`6a4a1a4` .. `6545068`; patch 2, the TSENS ready bit, duplicated
`d571a7a` and was dropped). Paths below are relative to this folder; run the
tools from the repo root, e.g. `python3 eink-qemu/ghosting/tools/pepmeasure.py shot.png`.

## TL;DR

- **Extra black SDL window** fixed (`esp_rgb.c`: console created at realize).
- **Battery & Stats hang** fixed (`esp32s3_sens.c`: temperature sensor ready bit).
- **Full-screen ghost after the turbo keyboard** fixed. The model latched a
  "4 gray levels" mode on the first LUT upload. It now follows PSR REG per
  refresh, runs uploaded LUTs frame by frame, and models N2OCP.
- **Ghosting is now simulated physically** (saturating particle motion,
  remnant voltage, blooming, drift, and a hold drive in OTP fast refreshes).
  Defaults are fitted to device photos:
  - type/delete ghost: ~11%, device ~11%;
  - fade over 6 later fast refreshes: x0.53, device x0.51;
  - menus and grayscale book pages: clean, as on the device.
- Every model parameter is a `-global uc8179.<name>=N` property; 0 turns each
  mechanism off.

## Contents

| Path | What |
|------|------|
| `device-photos/` | real X4 Pro photos: the keyboard ghost and a 10-photo series with its steps (the original bug report screenshot is left out: it shows a sync account) |
| `sim-screenshots/` | before/after, photo vs sim, the series replayed in the sim, rejected tunings |
| `test/` | `drive.py` step files (one step per line) for each scenario |
| `tools/` | measurement scripts (need Pillow, except `hist.py`) |

## Commits

| # | Commit | File | What |
|---|--------|------|------|
| 1 | `24db66c7ab` | `hw/display/esp_rgb.c` | RGB console created at realize: no stray black 800x600 SDL window on `x4pro` |
| 2 | `3a30aceea3` | `hw/misc/esp32s3_sens.c` | TSENS_READY + `tsens_raw`: Battery & Stats no longer hangs a core |
| 3 | `4f0d3ae147` | `hw/display/uc8179.c` | PSR REG per refresh, register LUT waveforms, N2OCP |
| 4 | `ae7dab0688` | `eink-qemu/run.sh` | `-display sdl,show-cursor=on` default (user preference; optional) |
| 5 | `97db1b0f6f` | `hw/display/uc8179.c` | flat OTP-fast residue (superseded by 6) |
| 6 | `8154cb52e1` | `hw/display/uc8179.c` | physical ghosting model: remnant, blooming, drift, L* |
| 7 | `05df8eeccd` | `hw/display/uc8179.c` | fitted to photo 1: blooming as edge loss, bloom 35, otp-fast 10, remnant 30/10 |
| 8 | `a082b67020` | `hw/display/uc8179.c` | `otp-hold-drive` fitted to the photo series |

On `x4prosim` they are 1 `6a4a1a4`, 3 `b5a4ba6`, 4 `02d7215`, 5 `f8fb2ef`,
6 `9dfe6bb`, 7 `f598d0e`, 8 `6545068` (2 dropped, see above).

---

## 1. Extra black SDL window (`esp_rgb.c`)

- **Symptom:** with `-display sdl`, an 800x600 black "QEMU" window opened
  beside the panel.
- **Cause:** `esp_rgb_init()` (instance_init) called `graphic_console_init()`
  and blacked the surface. `x4pro` unparents `ss->rgb` without realizing it
  (`hw/xtensa/esp32s3.c`, "keep it console 0"), but the console already
  existed, and SDL opens a window per graphic console.
- **Fix:** create the console in `esp_rgb_realize()`. Machines that realize
  the RGB device are unchanged.
- **Verified:** `xwininfo` shows only the 480x800 panel window mapped. A
  640x384 unmapped window remains; it is SDL's hidden text console for
  parallel0, which is normal. `-parallel none` is not the fix.

## 2. Battery & Stats hang (`esp32s3_sens.c`)

- **Symptom:** Goodies > Battery & Stats: one core at 100% and the page never
  drew.
- **Cause:** the stuck PC resolved to `temperature_sensor_ll_get_raw_value`
  (`hal/esp32s3/include/hal/temperature_sensor_ll.h:100`). It spins on
  `SENS_SAR_TSENS_CTRL_REG` (SENS + 0x50) bit 8 `TSENS_READY` inside a
  critical section, and the plain register file never set that bit.
- **Fix:** reading 0x50 returns `TSENS_READY | 104` (on `x4prosim` this landed
  as `d571a7a` with a fixed raw value; the patch's `tsens_raw` property was not
  taken). IDF converts it as
  0.4386 * raw - 27.88 * offset - 20.52, with offset 0 on the default
  -10..80 C range, so 104 reads 25.1 C.
- **Verified:** the page shows "CPU 25C" (`sim-screenshots/07`).
- Any firmware that reads the chip temperature would hang the same way.

## 3. UC8179 model: how it got here

### 3.1 The original bug (`sim-screenshots/01`)

After the turbo keyboard, every later refresh showed the previous screen at
about half strength.

The old model set `custom_lut = true` on the first write to 0x20..0x24 and
kept it until reset. From then on it showed `(NEW << 1) | OLD` as 4 grays.
That reading is right only for the driver's direct-gray pass. freeink-sdk
`Uc8179Driver.cpp` chooses the waveform per refresh with PSR REG
(`bus.data(kbdLut ? psr0 : psr0 & 0xDF)`): OTP for normal refreshes, register
LUTs for the turbo-keyboard DU, scrubs, paints and direct gray.

### 3.2 Iterations, with the reasons each was kept or rejected

1. **Register LUTs simulated frame by frame, exponential approach per frame.**
   Rejected. Every LUT the driver uploads is DC-balanced (net drive 0), and
   the exponential approach never saturates. Direct-gray pages got a gray
   background (~0.92) and a ghost of the previous page.
2. **Clamped integrator, 1/swing-frames per frame (swing 6).** Kept as the
   base. Balanced rows land on a level. With swing 6, the vendor gray table
   (light = black + 4 white frames, dark = black + 2) lands exactly on 2/3 and
   1/3, and its black/white rows land exactly. The OTP path was idealized.
3. **Flat OTP-fast residue, 8%.** The user: "WAY too much". Every pixel that
   ever changed sat 8% short until a full refresh, so the whole history (boot
   logo, Home) showed through.
4. **Same at 2% with 16-bit ink.** Accepted, then replaced by the physical
   model.
5. **Physical model, first defaults** (remnant 250/100, bloom 40).
   - The remnant kicked the whole held background to 234 instead of 240, and
     old content showed as *lighter* traces
     (`rejected/remnant-250-...png`).
   - Blooming applied to undriven pixels left gray halos around every drawn
     text. Once the text was erased, the halo pixels were held, so outlines
     of old screens piled up (`rejected/bloom-on-undriven-pixels-halos.png`).
6. **Fit to `device-photos/keyboard-type-delete-pep.jpg`.**
   - Blooming became an edge loss on driven pixels only.
   - Bloom swept: 30 gives 9.1%, 45 gives 14.1%; 35 chosen.
   - `otp-fast-frames` 7 -> 10, because the device shows no traces of older
     screens.
   - Remnant 30/10, so traces of older screens are at most 1 sRGB level.
7. **Fit to the 10-photo series.** The device's ghost fades over fast
   refreshes that never touch its pixels, so held pixels get
   `otp-hold-drive`. 8 faded too fast (x0.43 over 6 refreshes); 6 gives
   x0.53 against the device's x0.51.

### 3.3 Final model (`hw/display/uc8179.c`; header comment matches)

**Per refresh.** Every pixel gets a class from its OLD/NEW bits:
KK=0, KW=1, WK=2, WW=3 (bit 1 = white, KW mode). A refresh is a list of
frames, each giving a float drive per class (+ toward white).

- **PSR REG=1, register LUTs:** 0x20 VCOM, 0x21 WW, 0x22 KW, 0x23 WK,
  0x24 KK. Each row is 6-byte groups `[levels, TP_A..TP_D, RP]` with up to 10
  groups; the driver writes 7. The format comes from freeink-sdk
  `lut/UltraChipLutBalance.h`. Level codes:

  | Code | Source row | VCOM row |
  |------|------------|----------|
  | 00 | GND | VCOM_DC |
  | 01 | VDH (black) | VDH + VCOM_DC |
  | 10 | VDL (white) | VDL + VCOM_DC |
  | 11 | VDHR, not modeled (`-d unimp` logs it) | floating |

  Drive = source - VCOM. Frames are capped at 8192.
- **PSR REG=0, OTP waveforms.** The waveform bodies were never dumped, so
  these are stand-ins:
  - **Fast** (inside PTIN/PTOUT; the driver uses a whole-panel partial with
    no 0x90 window): KW/WK get `otp-fast-frames` (10) at full drive toward
    NEW. WW/KK get `otp-hold-drive` (6 per mille) toward their own color.
  - **Full** (no PTIN): every pixel gets `otp-full-frames` (30) away from
    NEW, then 30 toward it.
- **N2OCP:** CDI (0x50) byte 0 bit 3 copies NEW to OLD after the refresh.
  The driver's CDI is 0x29 during refresh and 0xA9 idle, both with N2OCP;
  GxEPD2's GDEW075T7 comments the same byte "LUTKW, N2OCP: copy new to old".
- **Reset:** PSR 0x0F (REG=0), CDI 0x31 (N2OCP off), LUTs zero, not in
  partial. The ink state survives a controller reset, as the panel does.

**Per pixel state:** position `p` (0 black .. 1 white), remnant charges
`qf, qs` (drive x s), and `t_drive` (virtual s of the last full drive).
Storage is 16 B/pixel, 6 MB.

**Per frame.** `dt = frame-us`, drive `e` = frame value for the pixel's class:

1. **Blooming**, only if `|e| >= 0.5` and the frame drives classes
   differently: `d = e + bloom * sum over 4-neighbors (e_n - e)`.
2. **Remnant field:** `a = d - (remnant-fast / remnant-fast-ms) * qf -
   (remnant-slow / remnant-slow-ms) * qs`. Per mille / ms is 1/s, so at
   steady state under constant drive the field is -fraction * drive.
3. **Charge update:** `q = q * exp(-dt/tau) + d * dt`, for each of the two
   components.
4. **Particle move:** `p` moves `a / swing-frames` swings toward the rail of
   `a`'s sign. The move is linear until `rail-soft` short of that rail, then
   `x = x * exp(-move / rail-soft)`. Short drives fall short; long ones
   saturate and erase history.
5. `t_drive = now` if `|e| >= 0.5`.

**Between refreshes:**
- The remnant leaks with `exp(-elapsed/tau)`.
- Drift: `dp/dt = (0.5 - p) * drift/1000 / drift-s * exp(-age/drift-s)`,
  where age is the time since `t_drive`. The total pull is bounded by
  `drift` per mille.
- `gfx_update` applies drift at most once per virtual second and redraws.

**Display:** L* is linear in `p` from 4.7 (sRGB 0x10) to 94.5 (0xF0), then
converted to sRGB through a 1025-entry table. This puts the vendor gray LUT's
levels at even L* steps. Direct-gray AA grays now read ~73-75 and ~142,
varying slightly with pixel history (the dwell-time effect).

**Cost:** the full-panel per-frame loop runs on the main loop. A 50-frame
gray pass is the worst case. Scripted runs take about 3 s longer than their
`wait:` total; per-refresh time was not profiled.

### 3.4 Properties (`-global uc8179.<name>=N`)

| Property | Default | Unit | Meaning | Calibrated? |
|----------|---------|------|---------|-------------|
| `swing-frames` | 6 | frames | full black<->white swing at full drive | vendor gray table (exact 1/3, 2/3) |
| `rail-soft` | 100 | per mille | exponential zone near each rail | photo 1 (with bloom) |
| `otp-fast-frames` | 10 | frames | OTP fast stand-in, changed pixels | device shows no menu ghosts |
| `otp-full-frames` | 30 | frames | OTP full stand-in, each direction | no (just saturates) |
| `otp-hold-drive` | 6 | per mille | held pixels' drive in OTP fast | photo series |
| `remnant-fast` / `-ms` | 30 / 1000 | per mille / ms | steady-state remnant fraction / time constant | upper bound only (background uniform) |
| `remnant-slow` / `-ms` | 10 / 30000 | per mille / ms | same, slow | upper bound only |
| `bloom` | 35 | per mille per neighbor | edge drive loss of driven pixels | photo 1 |
| `drift` / `drift-s` | 50 / 1800 | per mille / s | max relax toward mid gray / time constant | **no** |
| `frame-us` | 25000 | us | frame time for remnant/drift | driver logs: DU 27.6 ms, gray 23.8 ms |
| `busy-ms` | 0 | ms | 0 selects frame/profile timing; nonzero fixes BUSY per DRF | see profile |
| `portrait` | true | | console orientation | |

The table describes the legacy UC8179 defaults. UC8279 revision `0x68` uses
measured PLL-dependent frame timing and separate OTP durations; see the
[X4 Pro calibration](../x4cal/README.md). `busy-ms=0` selects those defaults,
not zero BUSY time. Setting a ghosting mechanism to 0 disables it. For
example, `-global uc8179.bloom=0` gives the pure-saturation model.

## 4. Device measurements and calibration

The photos are camera JPEGs of a real X4 Pro running a newer CrossDink (its
Settings > System shows Device items inline; 968e1a67 nests them under
"Device"). Measured in grayscale. "Depth" = (background - dark percentile of
the ghost region) / (background - black reference).

### 4.1 Photo 1, `keyboard-type-delete-pep.jpg` (turbo keyboard, type "pep" then delete)

| Measure | Value |
|---------|-------|
| Background | ~151 |
| Black (header "Device Name") | ~36 |
| Typed text (thin) darkest | 59; p2 78 |
| Ghost of erased "pep" + cursor trail darkest | 141-143 |
| Raw ghost depth | ~8.7% |
| Blur-corrected depth (full-black thin text reaches only ~80% contrast) | ~11% |

The background is clean: no traces of earlier screens. Typed text also looks
lighter than black. The 6-frame DU under-drives W->K as well; the sim
reproduces this via rail-soft and bloom.

Sim, same action (`test/type-delete-pep-steps.txt`, `tools/pepmeasure.py`):
11.7% with the final defaults. Side by side: `sim-screenshots/03` (made at
commit 7's defaults before remnant 60/30 -> 30/10 and the hold drive; 10.9% there).

### 4.2 Photo series (`device-photos/series-*.jpg`, `series-steps.txt`)

Steps:
1. Settings.
2. Device Name, press p x7.
3. Backspace to "CrossDink".
4. Back out.
5-10. Toggle Max Wi-Fi Powersave six times (13:56-13:57, about 10 s apart).

The ghost band is the erased " X4Ppppppp". Photo 1 is the noise floor.
`tools/photomeasure.py device-photos/series-` reproduces these numbers:

| Photo | 1 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |
|-------|---|---|---|---|---|---|---|----|
| p2 depth (raw) | 0.015 | 0.101 | 0.092 | 0.078 | 0.076 | 0.077 | 0.062 | 0.059 |
| minus floor | 0 | 0.086 | 0.077 | 0.063 | 0.061 | 0.062 | 0.047 | 0.044 |
| mean depth | -0.009 | 0.027 | 0.018 | 0.013 | 0.018 | 0.017 | 0.009 | 0.008 |

Findings:
- The ghost **survives the exit refresh** at full strength: 0.086, the same
  as photo 1's ~0.087. Those pixels are white in both OLD and NEW, so they
  are held.
- The keyboard area itself leaves nothing.
- The ghost **roughly halves over 6 fast refreshes** that never change its
  pixels, about 10.5% per refresh.

Sim replay (`test/photo-series-replay-steps.txt`, `tools/seqmeasure.py`,
`sim-screenshots/photo-series-replay/`):

| Step | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 10/4 |
|------|---|---|---|---|---|---|---|----|------|
| Sim (final defaults) | 0.085 | 0.076 | 0.067 | 0.063 | 0.054 | 0.049 | 0.045 | 0.040 | 0.53 |
| Device (minus floor) | n/a | 0.086 | 0.077 | 0.063 | 0.061 | 0.062 | 0.047 | 0.044 | 0.51 |

Notes:
- With `otp-hold-drive=8` the sim gave 0.072 -> 0.031, a ratio of 0.43.
- The sim loses ~10% at the exit refresh; the device appears to lose
  nothing. Small, and within photo noise.
- **Open: per-refresh or per-time?** The series can't separate them,
  because refreshes and time advanced together. It is modeled per refresh.
  - To separate them: photograph right after backing out, then again after
    2+ minutes untouched. The status-bar clock still refreshes once a
    minute, so note how many of those happened.
  - If it fades with time alone, move the mechanism into `uc8179_relax`, as a
    time-based relaxation of under-driven pixels toward their rail.

### 4.3 Other scenarios (final defaults)

| Scenario | Result |
|----------|--------|
| Menus after boot + 2 Down + Goodies | uniform white 239, black 16, no traces (`sim-screenshots/05`) |
| EPUB direct gray, page 2 | white 239, black 16, AA ~73-75 / ~142, no page-1 ghost (`06`) |
| Keyboard Test (Turbo, 6 fr), type, OK | DU ghost then cleared on exit (`02` is the pre-ghost-model reference) |
| Boot to Home, UC8179 promoted, Down moves selection | ok |
| Battery & Stats | renders, "CPU 25C" |

## 5. What the research says about ghosting

Sources are mostly E Ink patents (mechanisms, few numbers). Each mechanism is
listed with how the model uses it.

1. **Remnant voltage.** Ionic polarization leaves a residual field after
   driving. It decays as a sum of exponentials over ~50 ms to more than an
   hour, and adds to or subtracts from the next drive, so the previous image
   "prints" on the next. One patent example: the same white-state drive
   lands ~3 L* apart depending on dwell time.
   *Model:* two-component remnant charge.
2. **Dwell-time dependence.** The impulse a transition needs depends on how
   long the pixel sat in its state. Vendor waveforms add preset/shaking
   pulses and rail-stabilize (drive to black/white first).
   *Model:* falls out of the remnant term.
3. **Fast modes under-drive, and held pixels aren't driven.** DU/A2 don't
   guarantee the target, and a GC flash is the usual cleanup.
   *Model:* `rail-soft` + 6-frame DU; `otp-hold-drive` stands in for whatever
   OTP fast does to held pixels.
4. **Saturating, nonlinear response.** Reflectance rises steeply, then
   flattens near the rails. L* is a cube root of reflectance.
   *Model:* rail-soft exponential zone; display in L*.
5. **Blooming / edge ghosting.** Fringe fields switch part of the neighbor
   (one patent estimates ~1/5 of an adjacent pixel). Dedicated waveform modes
   (GLR16, REGAL) exist to fight edge ghosts.
   *Model:* edge loss on driven pixels. Applying it to undriven neighbors
   produced halos the device doesn't show.
6. **Drift.** Imperfect bistability: extremes relax toward gray over
   minutes to hours, faster when warm. Uneven drift between rewritten and old
   areas reads as ghosting.
   *Model:* bounded drift (uncalibrated).
7. **Temperature.** Waveforms are per temperature range; the wrong range
   over- or under-drives. CrossDink forces TSSET 0x5A (90 C) for fast
   refreshes. *Not modeled.*

Sources:
- Remnant voltage:
  [US9881564B2](https://patents.google.com/patent/US9881564B2/en),
  [WO2005054933A2](https://patents.google.com/patent/WO2005054933A2/en),
  [remnant discharge via backlight](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12339559),
  [11830448](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11830448).
- Dwell time:
  [9886886](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/9886886),
  [7528822](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/7528822),
  [7119772](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/7119772),
  [US20070091417A1](https://patents.google.com/patent/US20070091417).
- Edge ghosting / blooming:
  [US11568827B2](https://patents.google.com/patent/US11568827),
  [9966018](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/9966018),
  [NYCU lateral-field thesis](https://ir.lib.nycu.edu.tw/handle/11536/48983).
- Modes:
  [11380273](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11380273),
  [US10410592B2](https://patents.google.com/patent/US10410592),
  [9613599](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/9613599).
- Drift:
  [20070247406](https://patents.justia.com/patent/20070247406),
  [10002556](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/10002556).
- Software compensation:
  [Ghostbuster (TCAD/EMSOFT 2024)](https://scholars.cityu.edu.hk/en/publications/ghostbuster-a-software-approach-for-reducing-ghosting-effect-on-e/).
- Particle models:
  [PMC7281290](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC7281290/),
  [microcapsule waveform design system](https://www.researchgate.net/publication/315915140_A_design_system_for_driving_waveforms_of_microcapsule_electrophoretic_display).
- Background:
  [Paperino update modes](https://hackaday.io/project/21551-paperino/log/59392-e-paper-basics-1-update-modes),
  [FOSDEM 2026 open waveforms](https://fosdem.org/2026/schedule/event/epaper-driving-waveforms-explained).

## 6. Driver facts this relies on (freeink-sdk `driver/Uc8179Driver.cpp`, CrossDink 968e1a67)

- **PSR:** `psr0` 0x3F; refresh sends `kbdLut ? psr0 : psr0 & 0xDF`, i.e.
  REG set only for register-LUT refreshes. KW mode (bit 4) always.
- **CDI:** 0x29 active, 0xA9 idle, second byte 0x07. Both have N2OCP
  (bit 3) and DDX=01.
- **Fast refresh:**
  - TSSET 0x5A (forced 90 C), PFS 0x20, gate scan 0x02, then PTIN (0x91)
    with no 0x90 window, DRF, and PTOUT (0x92) at finish.
  - DTM1 holds the previous frame (re-streamed in `displayFinish` unless
    `skipResync`).
  - Full refresh fills DTM1 with 0xFF ("absolute from white"), so bit 1 =
    white.
- **Register LUT sets:** all gated DC-balanced by `UltraChipLutBalance.h`.
  - `makeDuLuts(frames)`: KW 0x60, WK 0x90 (f frames away, f to target),
    WW/KK/VCOM hold. Turbo keyboard frames = `kbdFrames` (6 in the sim
    runs). `kPaintFrames` 12.
  - `makeDuRedriveLuts`: KK re-driven n frames.
  - `makeNullLuts`.
  - `kDirectGraySet`: vendor gray table `kUltraChipDirectGray`, rows ordered
    VCOM/white/light/dark/black into R20..R24.
  - `makeDirectGrayHold`: smooth gray.
  - Mirrored sets for dark mode.
- **Frame times measured by the driver:** DU 27.6 ms, direct gray 23.8 ms,
  OTP GC ~1.5 s whole.
- **Driver comments useful for calibration:**
  - 6 frames per phase left overlays "visibly gray";
  - 12 left new black text "a bit gray";
  - both on the reset VCOM (-0.10 V instead of the panel's -2.00 V).

## 7. Reproducing

CLI runs default to ghosting off for pixel QA. Enable `ghosting` to reproduce
the physical model and parameter sweeps below. Use `uc8279` in place of `uc8179`
for X3. The browser defaults to ghosting on; its switch or `?ghosting=0` selects
clean rendering before starting.

```sh
# one step per line (steps contain spaces):
mapfile -t S < eink-qemu/ghosting/test/type-delete-pep-steps.txt   # replace OUT in shot: lines first
QEMU_EXTRA="-global uc8179.ghosting=on" eink-qemu/drive.py flash.bin sd.img log.txt "${S[@]}"
python3 eink-qemu/ghosting/tools/pepmeasure.py OUT.png
QEMU_EXTRA="-global uc8179.ghosting=on -global uc8179.bloom=30" eink-qemu/drive.py ...   # sweep a parameter
```

- **Use a fresh SD image per run.** A used SD changes Home's rows and the
  tap coordinates.
- **Taps:** `qom-set /machine/gt911 tap X,Y` in 480x800 portrait screenshot
  coordinates. Fresh-SD coordinates:

  | Screen | Target | X,Y |
  |--------|--------|-----|
  | Home | Settings | 240,529 |
  | Home | Goodies | 240,591 |
  | Home | Browse Files | 240,343 |
  | Settings | System tab | 416,106 |
  | System | Device | 240,177 |
  | Device | Device Name | 240,218 |
  | Device | Max Wi-Fi Powersave | 420,507 |
  | Goodies | Keyboard Test | 240,369 |
  | Goodies | Battery & Stats | 240,485 |
  | Keyboard Test | Type | 240,137 |
  | Keyboard | `p` | 447,573 |
  | Keyboard | `e` | 125,573 |
  | Keyboard | backspace | 433,693 |
  | Keyboard | OK | 433,757 |
  | Any | back arrow | 26,55 |
- **Books:** `eink-qemu/mksd.py sd.img 2050 <folder>` needs `mcopy` (mtools) and uses stock-X3 32 KiB clusters; see [SD image geometry](../../README.md#sd-image-geometry).
  Test EPUBs are in CrossDink `test/epubs/`.
- `tools/hist.py` prints the top gray levels and the off-white/off-black
  pixel counts of screenshots.

## 8. Open questions / next steps

1. **Per-refresh vs per-time fade:** see 4.2 for the test.
2. **B/W page turns in books** (OTP fast with real content) aren't
   calibrated. A photo after ~10 fast page turns, then one after a full
   refresh, would fit `otp-fast-frames` / `otp-hold-drive` / `bloom` for
   dense text.
3. **Drift** is uncalibrated: photograph a page left for 30-60 min next to a
   freshly drawn area.
4. **Temperature** isn't modeled: TSSET/TSC are ignored and TSC answers
   25 C. On the device, fast refreshes run the 90 C OTP waveform at room
   temperature.
5. **The OTP waveform bodies** were never dumped. A full `0xA2` OTP read from
   the device would replace both OTP stand-ins with the real fast/full
   waveforms per temperature range, run through the same engine.
6. **Not modeled:** LUTBD (0x25 border), VDHR amplitude and VCOM_DC value.
   The 0x90 partial window is honored in the UC8279-0x68 profile only.
7. **BUSY timing:** `busy-ms=0` uses the frame count and frame time for
   legacy UC8179; UC8279-0x68 has measured PLL/OTP timing. A nonzero
   `busy-ms` overrides either profile with a fixed duration.
8. **Performance:** the per-frame loop over 384k pixels runs under the BQL;
   profile it if UI latency matters. Possible speedups: skip all-zero frames
   with closed-form remnant decay; SIMD.

## 9. Other observations

- **Flaky key press:** one `press:down` at `wait:30` didn't register. Three
  reruns passed, and the original model passed once. It looks like
  key-press timing around light sleep; not investigated.
- **Firmware drift:** the device's CrossDink is newer than 968e1a67 (Settings
  layout differs). Tap coordinates above are for 968e1a67.

## 10. Build-environment notes (openSUSE Tumbleweed host, GCC 16, new glibc)

- **Missing dev packages:** pixman, libgcrypt (+libgpg-error), libpng and
  libslirp had no dev packages installed. I built them static into a local
  prefix and set `PKG_CONFIG_PATH`. SDL needs `sdl2-compat-devel`.
- **slirp is effectively required:** `meson.build` wraps
  `dependency('slirp')` in `declare_dependency()`, which always reports
  found, so `--disable-slirp` still compiles `net/slirp.c`.
- **Non-PIE:** static libslirp pulls a non-PIC `libatomic.a`, so build with
  `-Db_pie=false`.
- **libgcrypt `.pc`:** gpg-error is under `Requires.private`, so static links
  fail (`gpg_strerror`) unless it moves to `Requires`. Then run
  `meson configure --clearcache`.
- **GCC 16 / new glibc:** `-Werror=discarded-qualifiers` fails in
  `subprojects/dtc/libfdt/fdt_overlay.c` and `util/log.c`. Use
  `-Ddtc:werror=false -Dc_args=-Wno-error=discarded-qualifiers` (and the
  same for `dtc:c_args`).
- **Hazard:** building CrossDink with the PlatformIO core dir inside the
  eink-qemu worktree let ESP-IDF's cmake run `git submodule update --init
  --recursive` against eink-qemu (it began cloning `roms/edk2`, SLOF,
  QemuMacDrivers). Set `GIT_CEILING_DIRECTORIES` or keep the PlatformIO core
  outside any repo.
- **PlatformIO 6.2.0 + pioarduino 55.03.39:** `SCons.Tool.FortranCommon`,
  because tool-scons swaps 4.8.1 -> 4.11.1 mid-build. Pin
  `platformio==6.1.19`. The first build may still fail once; rerun.
