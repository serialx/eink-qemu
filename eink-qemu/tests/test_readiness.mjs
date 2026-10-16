// Run with: node --test eink-qemu/tests/test_readiness.mjs
// The page's readiness from the firmware log (desktop/src/readiness.ts):
// CrossPoint's refresh tags advance the status from Booting to Ready.
import assert from 'node:assert/strict';
import test from 'node:test';
import { Readiness } from '../desktop/src/readiness.ts';

function fresh(now = () => 10000) {
  const seen = [];
  const r = new Readiness((text) => seen.push(text), () => 'X4 Pro', () => 'Accurate timing', now, () => () => {});
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

test('firmware without a memory report settles as Running after Home', () => {
  let settle = null;
  const seen = [];
  const r = new Readiness((text) => seen.push(text), () => 'X4', () => 'Turbo', () => 0,
    (run) => { settle = run; return () => { settle = null; }; });
  r.restart();
  for (let i = 0; i < 3; i++) r.feed(['[326]   Wait complete: refresh (1 ms)']);
  assert.match(seen.at(-1), /finishing startup/);
  settle();
  assert.equal(seen.at(-1), 'X4 · Turbo · Running · Home drawn in 0.00 s');
});

test('a memory report before the settle wins, and stop() cancels it', () => {
  let settle = null;
  const seen = [];
  const r = new Readiness((text) => seen.push(text), () => 'X4', () => 'Turbo', () => 0,
    (run) => { settle = run; return () => { settle = null; }; });
  r.restart();
  for (let i = 0; i < 3; i++) r.feed(['Wait complete: refresh (1 ms)']);
  r.feed(['[MEM] Free: 1 bytes']);
  assert.equal(settle, null);
  assert.match(seen.at(-1), /Ready · Home drawn/);
  r.restart();
  for (let i = 0; i < 3; i++) r.feed(['Wait complete: refresh (1 ms)']);
  r.stop();
  assert.equal(settle, null);
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
