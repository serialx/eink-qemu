# UC8179 refresh animation and BUSY timing

Helper agent's notes for `475a32d` (on `x4prosim` after `b585ac9`): the UC8179
model plays each refresh frame by frame and holds BUSY for the refresh's real
length. Follows the ghosting work in `README.md`. Screenshots are in
`sim-screenshots/refresh-animation/`; the step files are `test/type-delete-pep-steps.txt`
and `test/photo-series-replay-steps.txt`.

## Why

The ghosting model already computed every refresh frame by frame, but only
the end state was ever shown. On the device you see the refresh:
- full refreshes flash: the vendor direct-gray pass swings the page to black
  and back;
- DU and fast updates paint in.

The fixed 300 ms BUSY was also far off the device for full and gray
refreshes.

## What changed (`uc8179.c`)

- **Refresh start (DRF).** `uc8179_refresh()` now only prepares the
  refresh. It:
  - finishes any refresh still playing;
  - applies leak and drift;
  - builds the class image and the frame list;
  - copies N2OCP (NEW -> OLD; the class image is already captured);
  - starts `anim_timer` (QEMU_CLOCK_VIRTUAL, ns).
- **Each tick.** `uc8179_run_frames(s, now)` runs every frame due by `now`,
  at one frame per `frame-us`, sets `redraw`, and re-arms the timer for the
  next frame. After the last frame it sets `t_refresh`/`t_drift`.
  `uc8179_run_frame(s, f)` is the old per-frame body, with `t_drive` stamped
  at the frame's own time.
- **BUSY.** `busy-ms` (now default 0) = derive the time from the frames:
  `max(1, frames * frame-us / 1000)` ms. A nonzero `busy-ms` restores a
  fixed time. BUSY and the animation run on separate timers that end
  together.
- **Overlap and reset.** A new DRF during playback first finishes the old
  refresh instantly. RST low stops the animation where it is (the panel
  stops being driven).
- **New property `animate`** (default true). False applies all frames at the
  DRF, for fast scripted runs; BUSY still lasts the frames' duration.
- **`otp-full-frames` 12 -> 30**, so the full-refresh stand-in (30 away from
  NEW, 30 toward it) takes 60 x 25 ms = 1.5 s, like the device.

## Timing vs the device

Device figures are from comments in freeink-sdk `driver/Uc8179Driver.cpp`
(`gGcFrameUs` etc.):

| Refresh | Before | Now | Device |
|---------|--------|-----|--------|
| Boot full (OTP GC) | 300 ms | 1500 ms | 1493 ms ("boot Full 1493 ms") |
| Direct gray (EPUB page, 50 frames) | 300 ms | 1250 ms | 1189 ms ("50-frame direct gray") |
| DU paint, 24 frames | 300 ms | 600 ms | 662 ms (27.6 ms/frame) |
| OTP fast (menus, 10-frame stand-in) | 300 ms | 250 ms | not logged |

`frame-us` stays 25 ms. The device's DU (27.6 ms) and gray (23.8 ms) frames
straddle it.

## Verification (CrossDink 968e1a67 `x4-pro-debug`, rebased build)

- Boot: UC8179 promoted. The full DRF logs `Wait complete: 8179_DRF (1500 ms)`.
- Home: a Down press moves the selection; no panic/abort in the log.
- EPUB page turn: `8179_DIRECT_GRAY_DRF (1250 ms)`. The page renders clean:
  white 239, black 16, AA 73-75 / 142 (`sim-screenshots/refresh-animation/page-turn-final-clean.png`).
- Animation, captured with frames slowed (`-global uc8179.frame-us=100000`
  or `150000`), because each `drive.py` screendump costs ~0.3 s wall, longer
  than a whole default fast refresh:
  - `sim-screenshots/refresh-animation/fast-refresh-selection-move-frames.png`: Home Down press,
    with the old selection box fading and the new one darkening in steps.
    Difference from the final frame per shot: 129 -> 94 -> 20 -> 0.
  - `sim-screenshots/refresh-animation/page-turn-direct-gray-flash-frames.png`: page turn. AA is
    removed (the B/W base pass), then the page goes gray, then full black,
    then the new page appears.
  - The faint drop/logo shape in the slowed shots is an artifact of the 6x
    frame time: the remnant charge integrates drive x dt. It doesn't appear
    at the default frame time.
- Ghost calibrations rechecked with `otp-full-frames` 30 (a longer full
  refresh leaves more remnant charge):
  - type "pep" and delete (`test/type-delete-pep-steps.txt`, `tools/pepmeasure.py`):
    11.7%, unchanged; device photo ~11%;
  - photo-series replay (`test/photo-series-replay-steps.txt`, `tools/seqmeasure.py`):
    0.072 -> 0.040, ratio 0.56; device 0.086 -> 0.044, ratio 0.51
    (with 12 it was 0.53);
  - menus: uniform (238/239).

## Notes for the project

- **Optional, not included:** ghost.zip's SENS commit made the temperature
  reading a property (`tsens_raw`, default 104 = 25.1 C). Upstream fixed the
  same hang independently in `d571a7afd2` with a fixed 25 C. The old patch
  conflicts with that, so it's dropped here. If a settable chip temperature
  is wanted, it's a 3-line change to upstream's `esp32s3_sens.c`.
- **Host cost:** unchanged in total (same per-frame work), but now spread
  over timer ticks. A 50-frame gray pass no longer blocks the main loop for
  the whole computation at the DRF.
- **Open, from ghost.zip:**
  - whether the ghost fades per refresh or per unit of time (a 2-minute
    untouched photo would tell);
  - B/W page-turn calibration;
  - drift and temperature calibration;
  - a dump of the OTP waveform bodies (`0xA2`) to replace both OTP
    stand-ins.
