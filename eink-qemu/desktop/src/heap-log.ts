/* The heaptrace plugin's allocation log as the heap window holds it: every
   allocation and free since QEMU started, in columns (Rust's heap_events
   hands them over in pieces), replayed into the live allocations at any
   moment and into block lists shaped like a snapshot's. Only type imports,
   so node's test runner loads it as it is. */
import type { Heap, HeapSite } from './types';

export const ALLOC = 1, FREED = 2, REALLOC = 3;
const FREE = 1, POISONED = 2;   // a block's flags, as heap-core.ts
const CK = 2048;                // events between replay checkpoints

/** heap_events' answer: events from `start`, times in µs after the first
    event (`t0`, in ms since the epoch), with the stack -> site entries and
    site names after those the caller has. */
export interface HeapEvents {
  gen: number; start: number; total: number; t0: number;
  t: number[]; k: number[]; p: number[]; s: number[]; o: number[]; st: number[];
  stackStart: number; stackSite: number[]; siteStart: number; sites: HeapSite[];
}
/** Live allocations by pointer: requested size, stack id, the event that made it. */
export type Live = Map<number, [number, number, number]>;
/** Allocations, bytes allocated and frees, by the allocating site. */
export type Churn = Map<number, { allocs: number; bytes: number; frees: number }>;

/** The block TLSF makes for a poisoned request of `size` bytes, header included. */
export const blockLen = (size: number) => Math.max(16, ((size + 15) & ~3) + 4);

function grow<T extends Float64Array | Uint32Array | Int32Array | Uint8Array>(a: T, need: number): T {
  if (need <= a.length) return a;
  const b = new (a.constructor as { new(n: number): T })(Math.max(need, a.length * 2));
  b.set(a);
  return b;
}

export class EventLog {
  /** Rust's generation of the log: a new one starts the window over. */
  gen = 0;
  n = 0;
  /** The first event, in ms since the epoch; event times are ms after it. */
  t0 = 0;
  t = new Float64Array(1024);
  k = new Uint8Array(1024);
  p = new Uint32Array(1024);
  s = new Uint32Array(1024);
  o = new Uint32Array(1024);
  st = new Int32Array(1024);
  /** For a free or realloc: the requested size and stack of the block it released. */
  fs = new Uint32Array(1024);
  fst = new Int32Array(1024);
  /** Allocations (and reallocs) among the first i events. */
  nalloc = new Uint32Array(1025);
  stackSite: number[] = [];
  sites: HeapSite[] = [];
  /** Each site's first allocation, by event index. */
  firstOf = new Map<number, number>();
  private cur: Live = new Map();
  private ckpt: Live[] = [new Map()];

  reset(gen: number) {
    this.gen = gen;
    this.n = 0;
    this.t0 = 0;
    this.stackSite = [];
    this.sites = [];
    this.firstOf = new Map();
    this.cur = new Map();
    this.ckpt = [new Map()];
  }

  siteOf(stack: number) { return stack >= 0 ? this.stackSite[stack] ?? -1 : -1; }

  /** Time of the last event, in ms after the first. */
  end() { return this.n ? this.t[this.n - 1] : 0; }

  /** Take a piece of heap_events; false when it did not continue this log. */
  ingest(ev: HeapEvents): boolean {
    if (ev.gen !== this.gen) this.reset(ev.gen);
    if (ev.siteStart === this.sites.length) this.sites.push(...ev.sites);
    if (ev.stackStart === this.stackSite.length) this.stackSite.push(...ev.stackSite);
    if (ev.start !== this.n) return false;
    this.t0 = ev.t0;
    const need = this.n + ev.k.length;
    this.t = grow(this.t, need); this.k = grow(this.k, need); this.p = grow(this.p, need);
    this.s = grow(this.s, need); this.o = grow(this.o, need); this.st = grow(this.st, need);
    this.fs = grow(this.fs, need); this.fst = grow(this.fst, need); this.nalloc = grow(this.nalloc, need + 1);
    for (let j = 0; j < ev.k.length; j++) {
      const i = this.n, k = ev.k[j], p = ev.p[j], s = ev.s[j], st = ev.st[j];
      this.t[i] = Math.max(ev.t[j] / 1000, i ? this.t[i - 1] : 0);
      this.k[i] = k; this.p[i] = p; this.s[i] = s; this.o[i] = ev.o[j]; this.st[i] = st;
      let gone: [number, number, number] | undefined;
      if (k !== ALLOC) {
        const q = k === FREED ? p : ev.o[j];
        gone = this.cur.get(q);
        if (gone) this.cur.delete(q);
      }
      this.fs[i] = gone ? gone[0] : 0;
      this.fst[i] = gone ? gone[1] : -1;
      if (k !== FREED) {
        this.cur.set(p, [s, st, i]);
        const site = this.siteOf(st);
        if (!this.firstOf.has(site)) this.firstOf.set(site, i);
      }
      this.nalloc[i + 1] = this.nalloc[i] + (k !== FREED ? 1 : 0);
      this.n++;
      if (this.n % CK === 0) this.ckpt[this.n / CK] = new Map(this.cur);
    }
    return true;
  }

  /** Live allocations after the first n events. */
  stateAt(n: number): Live {
    n = Math.max(0, Math.min(this.n, n));
    if (n === this.n) return new Map(this.cur);
    const j = Math.floor(n / CK), m = new Map(this.ckpt[j]);
    for (let i = j * CK; i < n; i++) {
      const k = this.k[i];
      if (k === FREED) m.delete(this.p[i]);
      else { if (k === REALLOC) m.delete(this.o[i]); m.set(this.p[i], [this.s[i], this.st[i], i]); }
    }
    return m;
  }

  /** The number of events at or before `time` (ms after the first). */
  countAt(time: number) {
    let a = 0, z = this.n;
    while (a < z) { const m = (a + z) >> 1; if (this.t[m] <= time) a = m + 1; else z = m; }
    return a;
  }

  /** The heaps after the first `n` events, as block lists like a snapshot's.
      Bounds, pool and tail come from `walked`, the latest snapshot's heaps.
      An allocation still alive keeps its walked size; others get the TLSF
      size for their request. Blocks the walk found but the log never saw
      allocated stay where they are, as if there all along. Free blocks are
      the gaps; one under 16 bytes is its neighbour's slack. */
  heapsAt(n: number, walked: Heap[]): Heap[] {
    const alive = [...this.stateAt(n)].sort((a, b) => a[0] - b[0]);
    return walked.map((h) => {
      const size = h.end - h.start, tail = h.tail ?? size, first = h.blocks.length ? h.blocks[0] : h.pool + 4;
      const known = new Map<number, number>();
      const items: { off: number; len: number; fl: number; req: number; site: number; stk: number; ev: number }[] = [];
      for (let i = 0, j = 0; i < h.blocks.length; i += 4, j++) {
        if (h.blocks[i + 2] & FREE) continue;
        const ev = h.ev?.[j] ?? -1;
        if (ev >= 0) known.set(ev, h.blocks[i + 1]);
        else items.push({ off: h.blocks[i], len: h.blocks[i + 1], fl: h.blocks[i + 2], req: h.blocks[i + 3], site: -1, stk: -1, ev: -1 });
      }
      for (const [p, [s, st, i]] of alive) {
        const off = p - 12 - h.start;   // the list holds block + 4; the pointer is block + 16
        if (off < first || off >= tail) continue;
        const len = Math.min(known.get(i) ?? blockLen(s), tail - off);
        items.push({ off, len, fl: POISONED, req: s, site: this.siteOf(st), stk: st, ev: i });
      }
      items.sort((a, b) => a.off - b.off);
      const blocks: number[] = [], sites: number[] = [], stk: number[] = [], ev: number[] = [];
      let pos = first, used = 0, free = 0, largest = 0, nfree = 0, count = 0;
      const gap = (to: number) => {
        if (to - pos < 16 && blocks.length && !(blocks[blocks.length - 2] & FREE)) {
          blocks[blocks.length - 3] += to - pos;
          used += to - pos;
        } else if (to - pos >= 8) {
          blocks.push(pos, to - pos, FREE, 0); sites.push(-1); stk.push(-1); ev.push(-1);
          free += to - pos - 4; largest = Math.max(largest, to - pos - 4); nfree++; count++;
        }
      };
      for (const it of items) {
        if (it.off < pos) continue;   // overlaps one before it: a free the log missed
        gap(it.off);
        blocks.push(it.off, it.len, it.fl, it.req); sites.push(it.site); stk.push(it.stk); ev.push(it.ev);
        used += it.len - 4; count++;
        pos = it.off + it.len;
      }
      gap(tail);
      return { ...h, blocks, sites, stk, ev, used, free, largest, nfree, count, infoFree: free, infoMinFree: NaN, corrupt: 0 };
    });
  }

  /** What was allocated and freed, by allocating site, in the `window` ms up to `time`. */
  churn(time: number, window: number): Churn {
    const out: Churn = new Map();
    const at = (site: number) => { let c = out.get(site); if (!c) out.set(site, c = { allocs: 0, bytes: 0, frees: 0 }); return c; };
    for (let i = this.countAt(time) - 1; i >= 0 && this.t[i] > time - window; i--) {
      const k = this.k[i];
      if (k !== FREED) { const c = at(this.siteOf(this.st[i])); c.allocs++; c.bytes += this.s[i]; }
      if (k !== ALLOC && this.fst[i] >= 0) at(this.siteOf(this.fst[i])).frees++;
    }
    return out;
  }

  /** A site's live bytes, as TLSF would size its blocks, at the end of each
      of `bins` equal steps from `from` to `to` (ms after the first event). */
  siteSeries(site: number, from: number, to: number, bins: number): Float64Array {
    const out = new Float64Array(bins);
    let i = this.countAt(from), bytes = 0;
    for (const [, [s, st]] of this.stateAt(i)) if (this.siteOf(st) === site) bytes += blockLen(s);
    for (let c = 0; c < bins; c++) {
      const until = from + (to - from) * (c + 1) / bins;
      for (; i < this.n && this.t[i] <= until; i++) {
        const k = this.k[i];
        if (k !== ALLOC && this.fst[i] >= 0 && this.siteOf(this.fst[i]) === site) bytes -= blockLen(this.fs[i]);
        if (k !== FREED && this.siteOf(this.st[i]) === site) bytes += blockLen(this.s[i]);
      }
      out[c] = bytes;
    }
    return out;
  }
}
