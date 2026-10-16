// Run with: node --test eink-qemu/tests/test_heap_log.mjs
// The heap window's allocation log (desktop/src/heap-log.ts): taking pieces
// of heap_events, replaying them, and rebuilding block lists from them.
import assert from 'node:assert/strict';
import test from 'node:test';
import { EventLog, blockLen } from '../desktop/src/heap-log.ts';

const S = 0x3fc90000, ALLOC = 1, FREED = 2, REALLOC = 3;
const ptr = (off) => S + off + 12;   // the list holds block + 4; malloc returns block + 16

// e0 allocates at 0x20 (site 5); e1 at 0x60 (site 6); e2 frees e0; e3 moves e1 to 0x100.
const events = (from = 0, to = 4, gen = 1) => {
  const all = {
    t: [0, 1000, 2000, 3000], k: [ALLOC, ALLOC, FREED, REALLOC],
    p: [ptr(0x20), ptr(0x60), ptr(0x20), ptr(0x100)], s: [20, 40, 0, 100], o: [0, 0, 0, ptr(0x60)], st: [0, 1, 1, 1],
  };
  const slice = Object.fromEntries(Object.entries(all).map(([k, v]) => [k, v.slice(from, to)]));
  return { gen, start: from, total: 4, t0: 1_000_000, ...slice, stackStart: from ? 2 : 0, stackSite: from ? [] : [5, 6],
    siteStart: from ? 7 : 0, sites: from ? [] : [...Array(7)].map((_, i) => ({ fn: `f${i}`, loc: '' })) };
};
// The heap as the last walk found it: e3's block (TLSF gave it 0x80) and a
// block the log never saw allocated, at 0x400.
const walked = {
  name: 'DRAM', caps: [], start: S, end: S + 0x1000, pool: 0x1c, tail: 0xff0,
  free: 0, used: 0, largest: 0, count: 5, nfree: 3, infoFree: 0, infoMinFree: 0, corrupt: 0,
  blocks: [0x20, 0xe0, 1, 0, 0x100, 0x80, 2, 100, 0x180, 0x280, 1, 0, 0x400, 0x40, 2, 52, 0x440, 0xbb0, 1, 0],
  sites: [-1, 6, -1, -1, -1], stk: [-1, 1, -1, -1, -1], ev: [-1, 3, -1, -1, -1],
};

test('takes heap_events in pieces and replays the live allocations', () => {
  const log = new EventLog();
  assert.equal(log.ingest(events(0, 2)), true);
  assert.equal(log.ingest(events(3, 4)), false, 'a piece that skips events is refused');
  assert.equal(log.ingest(events(2, 4)), true);
  assert.equal(log.n, 4);
  assert.deepEqual([...log.t.slice(0, 4)], [0, 1, 2, 3]);
  assert.equal(log.t0, 1_000_000);
  assert.deepEqual([...log.stateAt(2)], [[ptr(0x20), [20, 0, 0]], [ptr(0x60), [40, 1, 1]]]);
  assert.deepEqual([...log.stateAt(4)], [[ptr(0x100), [100, 1, 3]]]);
  // A free and a realloc remember the block they released.
  assert.deepEqual([log.fs[2], log.fst[2], log.fs[3], log.fst[3]], [20, 0, 40, 1]);
  assert.deepEqual([...log.nalloc.slice(0, 5)], [0, 1, 2, 2, 3]);
  assert.equal(log.countAt(1.5), 2);
  assert.equal(log.firstOf.get(6), 1);
  assert.equal(log.sites.length, 7);
});

test('a new generation starts the log over', () => {
  const log = new EventLog();
  log.ingest(events());
  log.ingest(events(0, 1, 2));
  assert.equal(log.gen, 2);
  assert.equal(log.n, 1);
  assert.deepEqual([...log.stateAt(1)], [[ptr(0x20), [20, 0, 0]]]);
});

test('rebuilds block lists at an earlier moment', () => {
  const log = new EventLog();
  log.ingest(events());
  const [h] = log.heapsAt(2, [walked]);
  // e0 and e1 at TLSF's size for their requests, the unseen block kept, gaps free.
  assert.deepEqual(h.blocks, [
    0x20, blockLen(20), 2, 20,
    0x20 + blockLen(20), 0x60 - 0x20 - blockLen(20), 1, 0,
    0x60, blockLen(40), 2, 40,
    0x60 + blockLen(40), 0x400 - 0x60 - blockLen(40), 1, 0,
    0x400, 0x40, 2, 52,
    0x440, 0xbb0, 1, 0,
  ]);
  assert.deepEqual(h.sites, [5, -1, 6, -1, -1, -1]);
  assert.deepEqual(h.ev, [0, -1, 1, -1, -1, -1]);
  assert.equal(h.used, blockLen(20) - 4 + blockLen(40) - 4 + 0x40 - 4);
  // At the end, the walked sizes come back.
  const [now] = log.heapsAt(4, [walked]);
  assert.deepEqual(now.blocks, walked.blocks);
  assert.deepEqual(now.ev, [-1, 3, -1, -1, -1]);
});

test('a gap too small for a free block is its neighbour slack', () => {
  const log = new EventLog();
  log.ingest({ ...events(0, 1), p: [ptr(0x20)], s: [20] });
  log.ingest({ ...events(1, 2), p: [ptr(0x20 + blockLen(20) + 8)], s: [40], stackStart: 2, siteStart: 7 });
  const [h] = log.heapsAt(2, [{ ...walked, blocks: [0x20, 0xfd0, 1, 0], sites: [-1], stk: [-1], ev: [-1] }]);
  assert.deepEqual(h.blocks.slice(0, 8), [0x20, blockLen(20) + 8, 2, 20, 0x20 + blockLen(20) + 8, blockLen(40), 2, 40]);
});

test('counts churn and follows a site over time', () => {
  const log = new EventLog();
  log.ingest(events());
  const churn = log.churn(3, 10_000);
  assert.deepEqual(churn.get(5), { allocs: 1, bytes: 20, frees: 1 });
  assert.deepEqual(churn.get(6), { allocs: 2, bytes: 140, frees: 1 });
  assert.deepEqual([...log.siteSeries(6, 0, 3, 3)], [blockLen(40), blockLen(40), blockLen(100)]);
  assert.deepEqual([...log.siteSeries(5, 0, 3, 3)], [blockLen(20), 0, 0]);
});
