/* Mock heaps for layout work without the emulator (the demo page and the
   heap window in a plain browser): four ESP32-S3-like heaps whose blocks
   come and go each second, from a dozen made-up allocation sites, with the
   allocation log, kept snapshots and free-memory history Rust would keep.
   Three minutes have already passed when it starts, and one site never
   frees, so comparing two moments shows it growing. */
import type { Heap, HeapHistoryPoint, HeapPast, HeapSite, HeapSnapshot, HeapStack } from './api';
import type { HeapEvents } from './heap-log';

const SITES: HeapSite[] = [
  'Display::begin()', 'xTaskCreatePinnedToCore', 'ActivityManager::begin()', 'FontRegistry::discover()',
  'xQueueGenericCreate', 'Reader::open(char const*)',
  'std::_Function_handler<void (std::shared_ptr<epub::Chapter>), Reader::open(char const*)::{lambda(std::shared_ptr<epub::Chapter>)#1}>::_M_invoke(std::_Any_data const&, std::shared_ptr<epub::Chapter>&&)', 'GfxRenderer::insertFont(int)',
  'nvs::PageManager::load()', 'setup()', 'pthread_mutex_init', 'HalGPIO::begin()',
].map((fn, i) => ({ fn, loc: `/Users/me/crosspoint/src/activities/reader/very/deep/path/file${i}.cpp:${40 + i * 7}` }));
/** The site that allocates and never frees. */
const LEAK = 3;
const PAST = 180;    // seconds simulated before the first snapshot
const BOOT = 2000;   // ms over which the first allocations spread

/** A made-up stack for a block from `site`: the allocator, the site, its callers. */
export function mockStack(site: number): HeapStack | null {
  if (site < 0 || site >= SITES.length) return null;
  const frame = (fn: string, loc: string, alloc = false, lib = false) => ({ addr: 0x42000000 + fn.length * 16, fn, loc, inl: [], alloc, lib });
  const idf = '/Users/me/.platformio/packages/framework-espidf/components';
  return {
    frames: [
      frame('multi_heap_malloc', `${idf}/heap/multi_heap_poisoning.c:243`, true, true),
      frame('heap_caps_malloc', `${idf}/heap/heap_caps.c:84`, true, true),
      frame('operator new(unsigned int)', '/builds/idf/crosstool-NG/libstdc++-v3/libsupc++/new_op.cc:50', true, true),
      { ...frame(SITES[site].fn, SITES[site].loc), inl: [{ fn: 'std::make_shared<epub::Chapter>()', loc: '/toolchain/include/c++/bits/shared_ptr.h:1010' }] },
      frame('ActivityManager::loop()', '/Users/me/crosspoint/src/ActivityManager.cpp:120'),
      frame('loop()', '/Users/me/crosspoint/src/main.cpp:512'),
      frame('loopTask(void*)', `${idf}/../framework-arduinoespressif32/cores/esp32/main.cpp:74`, false, true),
    ],
    caller: 3, site: 3,
  };
}

interface Slot { off: number; len: number; used: boolean; site: number; ev: number; ptr: number; req: number }
/** An allocation or free about to be logged. */
interface Change { t: number; slot: Slot; alloc: boolean }

/** One heap: slots of fixed place and size that are allocated or free. */
function mockHeap(name: string, caps: string[], start: number, size: number, avg: number, seed: number, fill = 0.9) {
  let state = seed;
  const random = () => (state = (state * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
  const pool = 0x3a0, slots: Slot[] = [];
  let site = 0, off = pool;
  while (off < size * fill) {
    const len = 16 + Math.floor(random() ** 3 * avg) * 4;
    if (random() < 0.4) site = Math.floor(random() ** 2 * SITES.length);
    // The list holds block + 4; the pointer malloc returns is block + 16.
    slots.push({ off, len: len + 4, used: random() >= 0.25, site, ev: -1, ptr: start + off + 12, req: len - 12 });
    off += len + 4;
  }
  const end = off, rest = size - end - 16;
  let low = Infinity;
  return {
    /** The firmware's first allocations: every slot in use, between `from` and `to`. */
    boot: (from: number, to: number): Change[] =>
      slots.filter((s) => s.used).map((slot) => ({ t: from + random() * (to - from), slot, alloc: true })),
    /** Six frees or allocations between `from` and `to`, as many of each on
        average, so the heap stays about as full; the leaking site never frees. */
    step(from: number, to: number): Change[] {
      const out: Change[] = [];
      const pick = (used: boolean, leak = false) => {
        for (let tries = 0; tries < 40; tries++) {
          const slot = slots[Math.floor(random() * slots.length)];
          if (slot.used === used && (slot.site === LEAK) === leak) return slot;
        }
        return null;
      };
      for (let i = 0; i < 6 && slots.length; i++) {
        const alloc = random() < 0.5, slot = alloc ? pick(false, random() < 0.12) : pick(true);
        if (!slot) continue;
        slot.used = alloc;
        out.push({ t: from + random() * (to - from), slot, alloc });
      }
      return out;
    },
    heap(): Heap {
      const blocks: number[] = [], sites: number[] = [], ev: number[] = [];
      let free = 0, used = 0, largest = 0, nfree = 0;
      for (const s of slots) {
        blocks.push(s.off, s.len, s.used ? 2 : 1, s.used ? s.req : 0);
        sites.push(s.used ? s.site : -1);
        ev.push(s.used ? s.ev : -1);
        if (s.used) used += s.len - 4;
        else { free += s.len - 4; nfree++; largest = Math.max(largest, s.len - 4); }
      }
      blocks.push(end, rest + 4, 1, 0); sites.push(-1); ev.push(-1);
      free += rest; nfree++; largest = Math.max(largest, rest);
      low = Math.min(low, free);
      return { name, caps, start, end: start + size, pool: pool - 4, tail: end + rest + 12, free, used, largest,
        count: slots.length + 1, nfree, infoFree: free, infoMinFree: low, corrupt: 0, blocks, sites, stk: [...sites], ev };
    },
  };
}

export interface MockHeaps {
  /** The heaps now; `traced` false drops sites, stacks and the log, as without an ELF. */
  snapshot(traced: boolean): HeapSnapshot;
  at(time: number): HeapPast | null;
  history(): HeapHistoryPoint[];
  events(gen: number, start: number, stacks: number, sites: number): HeapEvents | null;
  stack(id: number): HeapStack | null;
  sites(from: number): HeapSite[];
}

export function mockHeaps(): MockHeaps {
  const heaps = [
    mockHeap('DRAM', ['32BIT', '8BIT', 'INTERNAL', 'DEFAULT'], 0x3fc9a000, 0x4c000, 220, 7),
    mockHeap('DRAM', ['32BIT', '8BIT', 'DMA', 'INTERNAL', 'DEFAULT'], 0x3fcf0000, 0x8000, 0, 5, 0),
    mockHeap('RTC', ['32BIT', '8BIT', 'INTERNAL', 'DEFAULT', 'RTCRAM'], 0x600fe000, 0x1e00, 24, 3),
    mockHeap('PSRAM', ['32BIT', '8BIT', 'SPIRAM', 'DEFAULT'], 0x3d000000, 0x800000, 9000, 11),
  ];
  // The log as the plugin writes it, in time order; `st` is the site (one stack each).
  const log = { t: [] as number[], k: [] as number[], p: [] as number[], s: [] as number[], st: [] as number[] };
  const append = (changes: Change[]) => {
    for (const { t, slot, alloc } of changes.sort((a, b) => a.t - b.t)) {
      if (alloc) slot.ev = log.t.length;
      log.t.push(t); log.k.push(alloc ? 1 : 2); log.p.push(slot.ptr); log.s.push(alloc ? slot.req : 0); log.st.push(slot.site);
    }
  };
  const kept: HeapSnapshot[] = [];
  const take = (time: number): HeapSnapshot => {
    const snap: HeapSnapshot = {
      time: time / 1000, heaps: heaps.map((h) => h.heap()), missing: 0, took: 4, run: 1,
      traced: true, traceNote: null, elf: 'firmware.elf', elfNote: null, traceGen: 1, traceEvents: log.t.length, traceFormat: true,
    };
    kept.push(snap);
    if (kept.length > 900) kept.shift();
    return snap;
  };
  const begin = Date.now() - PAST * 1000;
  append(heaps.flatMap((h) => h.boot(begin - BOOT, begin)));
  for (let s = PAST - 1; s > 0; s--) {
    const time = Date.now() - s * 1000;
    append(heaps.flatMap((h) => h.step(time - 1000, time)));
    take(time);
  }
  let last = kept[kept.length - 1], traced = true;
  const strip = (snap: HeapSnapshot): HeapSnapshot => traced ? snap : {
    ...snap, traced: false, elf: null, traceGen: 0, traceEvents: 0,
    heaps: snap.heaps.map((h) => ({ ...h, sites: undefined, stk: undefined, ev: undefined })),
  };
  return {
    snapshot(on) {
      traced = on;
      const now = Date.now();
      // As Rust, a snapshot this recent is shared.
      if (now - last.time * 1000 >= 800) {
        append(heaps.flatMap((h) => h.step(last.time * 1000, now)));
        last = take(now);
      }
      return strip(last);
    },
    at(time) {
      if (!kept.length) return null;
      let i = 0;
      kept.forEach((k, j) => { if (Math.abs(k.time - time) < Math.abs(kept[i].time - time)) i = j; });
      return { snapshot: strip(kept[i]), prev: i ? strip(kept[i - 1]) : null };
    },
    history: () => kept.map((s) => ({ t: s.time, h: Object.fromEntries(s.heaps.map((h) => [h.start.toString(16), [h.infoFree, h.largest, h.infoMinFree]])) })),
    events(gen, start, stacks, sites) {
      if (!traced) return null;
      if (gen !== 1) { start = 0; stacks = 0; }
      const end = Math.min(log.t.length, start + 100_000), first = log.t[0] ?? 0;
      return {
        gen: 1, start, total: log.t.length, t0: first,
        // Microseconds after the first event, as heap_events sends them.
        t: log.t.slice(start, end).map((t) => Math.round((t - first) * 1000)),
        k: log.k.slice(start, end), p: log.p.slice(start, end), s: log.s.slice(start, end),
        o: log.k.slice(start, end).map(() => 0), st: log.st.slice(start, end),
        stackStart: stacks, stackSite: SITES.map((_, i) => i).slice(stacks), siteStart: sites, sites: SITES.slice(sites),
      };
    },
    stack: (id) => mockStack(id),
    sites: (from) => SITES.slice(from),
  };
}
