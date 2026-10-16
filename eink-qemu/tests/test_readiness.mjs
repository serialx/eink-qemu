// Run with: node --test eink-qemu/tests/test_readiness.mjs
// The page's readiness (desktop/src/readiness.ts): CrossPoint's refresh tags
// advance the status from Booting to Ready; other firmware is Ready once its
// screen has been quiet, with the rules of core/src/script/readiness.rs.
import assert from 'node:assert/strict';
import test from 'node:test';
import { Readiness } from '../desktop/src/readiness.ts';

function fresh(now = () => 10000) {
  const seen = [];
  const r = new Readiness((text) => seen.push(text), () => 'X4 Pro', () => 'Accurate timing', now);
  r.startedAt = 0;
  return { r, seen };
}

test('three completed refreshes and the memory report mean Ready', () => {
  for (const tag of ['8179', '8279', 'X3', '8279x4']) {
    const { r, seen } = fresh();
    r.restart();
    // restart() takes the clock's 10 s as the boot; Home then is at 20 s.
    for (let i = 0; i < 3; i++) r.feed([`Wait complete:  ${tag}_DRF (483 ms)`]);
    assert.equal(r.bootSeconds, 0);
    r.feed(['[MEM] Free: 200000 bytes']);
    assert.match(seen.at(-1), /Ready · Home drawn in 0\.00 s/);
  }
});

test('the plain X4 refresh counts, and Home is timed from the restart', () => {
  let clock = 1000;
  const { r, seen } = fresh(() => clock);
  r.restart();
  clock = 3250;
  for (let i = 0; i < 3; i++) r.feed(['Wait complete:  refresh (100 ms)']);
  assert.equal(r.bootSeconds, 2.25);
  assert.match(seen.at(-1), /Home drawn in 2\.25 s · finishing startup/);
});

test('a reboot counts again, and a card the firmware cannot mount is reported', () => {
  const { r, seen } = fresh();
  r.restart();
  r.feed(['Wait complete:  8179_DRF (1 ms)', 'ESP-ROM:esp32s3-20210327']);
  assert.match(seen.at(-1), /Restarting/);
  r.feed(['SD card initialization failed', 'Wait complete:  8179_DRF (1 ms)', 'Wait complete:  8179_DRF (1 ms)',
    'Wait complete:  8179_DRF (1 ms)', '[MEM] Free: 1 bytes']);
  assert.match(seen.at(-1), /could not mount the SD card/);
});

test('firmware that resumes a book is Ready once the screen has been quiet', () => {
  let clock = 0;
  const { r, seen } = fresh(() => clock);
  r.restart();
  r.feed(['Wait complete:  X3_DRF (1 ms)']);
  clock = 5000;
  r.feed(['[MEM] Free: 1 bytes']);
  assert.equal(seen.at(-1), 'X4 Pro · Accurate timing · Ready');
});

// A clock the test moves, and a page that ticks readiness as app.ts does.
function clocked() {
  let clock = 0;
  const { r, seen } = fresh(() => clock);
  r.restart();
  return { r, seen, at(ms) { clock = ms; r.tick(); } };
}

// Stock Xteink X4 Pro firmware 7.5.11 (a 16 MiB dump), booted with Power
// held: ESP-IDF's log, none of CrossPoint's lines.
const stockBoot = [
  'ESP-ROM:esp32s3-20210327', 'Build:Mar 27 2021', 'rst:0x1 (POWERON),boot:0x4 (SPI_FLASH_BOOT)',
  'I (1929) main_task: Started on CPU0', 'I (1929) main_task: Calling app_main()',
  'I (2538) boot-preflight: source=1 validation=2 gate=0 key=1 charge=1 configured=1000ms effective=600ms hold=608ms decision=0 reason=11 auth=0',
];

test('stock firmware is Ready once its screen has been quiet for 5 s', () => {
  const { r, seen, at } = clocked();
  r.feed(stockBoot);
  at(2600); r.screen();
  at(4019); r.feed(['I (4019) main_task: Returned from app_main()']);
  assert.equal(seen.at(-1), 'X4 Pro · Accurate timing · Firmware running');
  at(4100); r.screen();
  at(9099);
  assert.equal(r.bootSeconds, null);
  at(9100);
  assert.equal(seen.at(-1), 'X4 Pro · Accurate timing · Ready · screen settled 4.10 s after boot');
  assert.equal(r.bootSeconds, 4.1);
  // Said once: later updates and ticks leave the status alone.
  r.screen(); at(20000);
  assert.equal(seen.filter((text) => /Ready/.test(text)).length, 1);
});

test('a blank screen after a drawn one is not ready, until something is drawn again', () => {
  const { r, seen, at } = clocked();
  r.feed(['ESP-ROM:esp32s3-20210327']);
  at(1000); r.screen(false);
  at(2000); r.screen(true);
  at(7000);
  assert.ok(!seen.some((text) => /Ready/.test(text)), 'blank at 2 s');
  at(8000); r.screen(false);
  at(13000);
  assert.ok(seen.at(-1).includes('Ready · screen settled 8.00 s after boot'), seen.at(-1));
});

test('the stock power gate asks for Power, and its deep-sleep wakes count as restarts', () => {
  const { r, seen, at } = clocked();
  const gate = (ms) => [`I (${ms}) boot-preflight: source=2 validation=2 gate=0 key=1 charge=0 configured=1000ms effective=600ms hold=119ms decision=2 reason=3 auth=0`,
    `W (${ms}) boot-preflight: reject physical boot before init: reason=3`, `W (${ms}) boot-preflight: enter deep sleep: wake_on=next_press`];
  r.feed(['ESP-ROM:esp32s3-20210327', 'rst:0x1 (POWERON),boot:0x4 (SPI_FLASH_BOOT)', ...gate(2277)]);
  assert.equal(seen.at(-1), 'X4 Pro is off · hold Power (P) for about 3 s to turn it on');
  r.screen();
  for (const ms of [8338, 8629, 9369]) {
    at(ms); r.feed(['ESP-ROM:esp32s3-20210327', 'rst:0x5 (DSLEEP),boot:0x4 (SPI_FLASH_BOOT)']);
    assert.match(seen.at(-1), /Restarting…/);
    r.feed(gate(ms));
  }
  r.feed(['ESP-ROM:esp32s3-20210327']);
  assert.match(seen.at(-1), /Restarting… \(rebooted 4 times: a boot loop\?\)/);
  at(30000);
  assert.doesNotMatch(seen.at(-1), /Ready/);
});

test('a device held at the power gate is off, not Ready, however quiet its screen', () => {
  const { r, seen, at } = clocked();
  r.feed(['ESP-ROM:esp32s3-20210327', 'W (2277) boot-preflight: reject physical boot before init: reason=3']);
  r.screen(); at(30000);
  assert.equal(seen.at(-1), 'X4 Pro is off · hold Power (P) for about 3 s to turn it on');
  // Power held: the wake is a new boot, which can be Ready.
  r.feed(['ESP-ROM:esp32s3-20210327', 'I (2538) boot-preflight: decision=0 reason=11']);
  at(31000); r.screen(); at(36000);
  assert.match(seen.at(-1), /Ready · screen settled/);
});

test('an ESP-IDF app that only draws is Ready after 5 quiet seconds, not while the ROM reboots it', () => {
  const { r, seen, at } = clocked();
  r.feed(['ESP-ROM:esp32c3-api1-20210207']);
  at(1000); r.screen();
  at(2000); r.screen();
  at(6999);
  assert.equal(r.bootSeconds, null);
  at(7000);
  assert.match(seen.at(-1), /Ready · screen settled 2\.00 s after boot/);

  const looping = clocked();
  looping.r.feed(['ESP-ROM:esp32c3-api1-20210207']);
  looping.at(1000); looping.r.screen();
  looping.at(4000); looping.r.feed(['ESP-ROM:esp32c3-api1-20210207']);
  looping.at(5000); looping.r.screen();
  looping.at(8000);
  assert.equal(looping.r.bootSeconds, null, 'a ROM banner 4 s ago is too recent');
  looping.at(10000);
  assert.equal(looping.r.bootSeconds, 1);
});

test('a ROM that cannot boot the image is reported at once, and nothing is Ready', () => {
  const { r, seen, at } = clocked();
  r.feed(['ESP-ROM:esp32s3-20210327', 'E (52) esp_image: invalid segment length 0xffffffff', 'E (55) boot: No bootable app partitions in the partition table']);
  assert.equal(seen.at(-1), 'X4 Pro · The ROM cannot boot this image: invalid segment length');
  r.screen(); at(20000);
  r.feed(['[MEM] Free: 1 bytes']);
  assert.equal(seen.at(-1), 'X4 Pro · The ROM cannot boot this image: invalid segment length');
});

test('after a reset nothing counts until the new boot\'s ROM banner', () => {
  const { r, seen, at } = clocked();
  r.feed(['ESP-ROM:esp32s3-20210327']);
  at(1000); r.screen();
  r.restart(true);
  r.feed(['I (9000) late: a line from before the reset']);
  r.screen(); at(10000);
  assert.equal(r.bootSeconds, null);
  r.feed(['ESP-ROM:esp32s3-20210327']);
  at(11000); r.screen();
  at(16000);
  assert.match(seen.at(-1), /Ready · screen settled 1\.00 s after boot/, 'timed from the new boot\'s ROM banner');
});

test('CrossPoint firmware keeps its own rules: a quiet screen alone is not Ready, across a reset too', () => {
  const { r, seen, at } = clocked();
  r.feed(['ESP-ROM:esp32c3-api1-20210207', '[1000] [DBG] [EPD]   Wait complete:  X3_DRF (1300 ms)']);
  r.screen(); at(20000);
  assert.doesNotMatch(seen.at(-1) ?? '', /Ready/);
  r.restart(true);
  r.feed(['ESP-ROM:esp32c3-api1-20210207']);
  r.screen(); at(40000);
  assert.equal(r.bootSeconds, null);
  r.restart();
  r.feed(['ESP-ROM:esp32c3-api1-20210207']);
  r.screen(); at(60000);
  assert.match(seen.at(-1), /Ready · screen settled/, 'Start with other firmware forgets CrossPoint');
});

test('a reloaded page attached to a running session waits for the next boot', () => {
  let clock = 50000;
  const { r, seen } = fresh(() => clock);
  r.resume();
  r.screen(); clock = 60000; r.tick();
  assert.equal(seen.length, 0);
  r.feed(['ESP-ROM:esp32s3-20210327']);
  assert.match(seen.at(-1), /Restarting…/);
  clock = 61000; r.screen(); clock = 66000; r.tick();
  assert.match(seen.at(-1), /Ready · screen settled/);
});

// From main: CrossPoint forks without a memory report, and a display that
// never answers (firmware for another board).
test('firmware without a memory report settles as Running after Home', () => {
  const { r, seen, at } = clocked();
  for (let i = 0; i < 3; i++) r.feed(['[326]   Wait complete: refresh (1 ms)']);
  assert.match(seen.at(-1), /finishing startup/);
  at(4999);
  assert.match(seen.at(-1), /finishing startup/);
  at(5000);
  assert.equal(seen.at(-1), 'X4 Pro · Accurate timing · Running · Home drawn in 0.00 s');
});

test('a memory report before the settle wins, and stop() silences the run', () => {
  const { r, seen, at } = clocked();
  for (let i = 0; i < 3; i++) r.feed(['Wait complete: refresh (1 ms)']);
  r.feed(['[MEM] Free: 1 bytes']);
  assert.match(seen.at(-1), /Ready · Home drawn/);
  at(9000);
  assert.ok(!seen.some((text) => /Running/.test(text)));
  r.restart();
  for (let i = 0; i < 3; i++) r.feed(['Wait complete: refresh (1 ms)']);
  r.stop();
  const said = seen.length;
  at(20000);
  assert.equal(seen.length, said);
});

test('a panel wait that ran out is not a refresh', () => {
  const { r, seen } = fresh();
  r.restart();
  r.feed(['[30301]   Wait complete:  CMD_SOFT_RESET (30001 ms)']);
  assert.match(seen.at(-1), /display did not answer the firmware \(a 30 s wait ran out\)/);
  for (let i = 0; i < 3; i++) r.feed(['[120317]   Wait complete: refresh (29999 ms)']);
  assert.equal(r.bootSeconds, null);
  assert.equal(seen.length, 1);
});
