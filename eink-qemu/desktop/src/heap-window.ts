/* The heap window (heap.html): a workbench over the running firmware's
   heaps. Allocation sites on the left, the maps in the middle (painted as
   the Heap tab paints them), an inspector on the right for the selected
   site or block, and a timeline along the bottom. With the allocation log
   (heap-log.ts) the timeline replays the heaps to any event since boot and
   the sites carry their allocation rate and growth; without it, it steps
   through the snapshots Rust keeps. Marking a moment A compares it with the
   cursor, B: the sites by what they gained, the maps by what is new and
   what was freed. */
import '@web/style.css';
import './heap.css';
import './heap-window.css';
import { listen } from '@tauri-apps/api/event';
import { api } from './backend-tauri';
import type { Heap, HeapHistoryPoint, HeapPast, HeapSnapshot, HeapStack, SessionInfo } from './types';
import {
  CORRUPT, FADE, FREE, GUTTER, POISONED, SCALES, Stacks, ago, blockAddr, byKind, callName, copyStack, delta, errorOf,
  esc, fmt, fragmented, hex, hitMap, isInternal, kib, load, locHtml, onTheme, paintMap, per, pointerOf, readColors, role,
  rowBytes, rowTop, save, shortName, showTip, splitLoc, stackHtml, type Colors, type MapLayout, type MapStyle, type TipRow,
} from './heap-core';
import { ALLOC, EventLog, FREED, blockLen, type Churn, type HeapEvents } from './heap-log';

interface Source {
  snapshot(): Promise<HeapSnapshot>;
  at(time: number): Promise<HeapPast | null>;
  history(): Promise<HeapHistoryPoint[]>;
  events(gen: number, start: number, stacks: number, sites: number): Promise<HeapEvents | null>;
  stack(id: number): Promise<HeapStack | null>;
}

const INTERVAL = 1000;       // ms between snapshots
const HISTORY = 900;         // free-memory points kept, as Rust keeps them
const PAST_FADE = 1500;      // ms of replayed time a change stays highlighted
const CHURN = 10000;         // ms of events the allocation rate counts
const GROWTH = 60000;        // ms the change column looks back
const SLOTS = 8;             // sites with a colour of their own
const SPARK_BINS = 80;
const ROWS_SHOWN = 400;      // site rows drawn at most
const BLOCKS_SHOWN = 40;     // a site's blocks listed at most

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const tipBox = $('hw-tip');
const mapsBox = $('hw-maps');
const inspector = $('hw-inspector');
const timeline = $<HTMLCanvasElement>('hw-timeline');
const table = $('hw-table');

let source: Source | null = null;
let snap: HeapSnapshot | null = null;
let problem: { code: string; message: string } | null = null;
let running = true;
let run: number | null = null;
let paused = false, inflight = false, timer = 0;
const log = new EventLog();
let history: HeapHistoryPoint[] = [];
let historyRun: number | null | undefined;
const stacks = new Stacks((id) => source ? source.stack(id) : Promise.resolve(null));

/* Time: the cursor is null while live, else the moment shown (ms since the
   epoch) and, with the log, how many events were replayed to reach it. */
let cursor: { t: number; n: number } | null = null;
/** Without the log, the kept snapshot shown for the cursor, and the moment asked for. */
let pastSnap: HeapPast | null = null;
let wanted: number | null = null, fetching = false;
/** A, compared with the cursor: its moment and its heaps. */
let markA: { t: number; n: number; heaps: Heap[] } | null = null;
let playing: { last: number } | null = null;
let speed = load<number>('eink-qemu-heapwin-speed', 1);
let range = load<number>('eink-qemu-heapwin-range', 180000);
/** The right end of the timeline while looking back (ms since the epoch). */
let tmEnd = 0;

type ColorBy = 'state' | 'site' | 'change';
let colorBy = load<ColorBy>('eink-qemu-heapwin-color', 'site');
/** The mode before marking A, restored when A is cleared. */
let colorBefore: ColorBy | null = null;
let scale = load<number | 'fit'>('eink-qemu-heapwin-scale', 16384);
if (scale !== 'fit' && !SCALES.includes(scale)) scale = 16384;
const folds: Record<string, boolean> = load('eink-qemu-heapwin-folds', {});
type SortKey = 'site' | 'live' | 'blocks' | 'rate' | 'growth' | 'a' | 'b' | 'delta';
let sort = load<{ key: SortKey; desc: boolean }>('eink-qemu-heapwin-sort', { key: 'live', desc: true });
let filter = '';

type Selection =
  | { kind: 'site'; site: number }
  | { kind: 'block'; start: number; off: number; len: number; fl: number; req: number; site: number; stack: number; ev: number;
      /** Live: when it was freed; looking back: not there at the moment shown. */
      freedAt: number; absent: boolean };
let selection: Selection | null = null;
let hoverSite: number | null = null;
let allocOpen = false;

let colors: Colors | null = null;
const C = () => colors ??= readColors(document.documentElement);
onTheme(() => { colors = null; schedule(); });

/* Sources ---------------------------------------------------------------------- */

const demo = !('__TAURI_INTERNALS__' in window) || new URLSearchParams(location.search).has('demo');
async function connect() {
  if (demo) {
    // A plain browser (pnpm dev): mock heaps, with the log unless ?untraced.
    const { mockHeaps } = await import('./heap-mock');
    const mock = mockHeaps(), traced = !new URLSearchParams(location.search).has('untraced');
    source = {
      snapshot: async () => mock.snapshot(traced),
      at: async (time) => mock.at(time),
      history: async () => mock.history(),
      events: async (gen, start, stackCount, siteCount) => mock.events(gen, start, stackCount, siteCount),
      stack: async (id) => mock.stack(id),
    };
  } else {
    source = { snapshot: api.heapSnapshot, at: api.heapSnapshotAt, history: api.heapHistory, events: api.heapEvents, stack: api.heapStack };
    // The window's own errors go to the EINK_QEMU_LOG file, as the main window's do.
    const report = (what: unknown) => void api.note(`Error: heap window: ${(what as Error)?.message ?? what}`);
    addEventListener('error', (event) => report(event.error ?? event.message));
    addEventListener('unhandledrejection', (event) => report(event.reason));
    await listen<SessionInfo>('session', ({ payload }) => {
      running = payload.state === 'running';
      if (running && payload.run !== run) newRun(payload.run);
      if (running) plan(0);
      schedule();
    });
  }
  plan(0);
}

/** Follow the main window's theme toggle. */
addEventListener('storage', (event) => {
  if (event.key !== 'eink-qemu-theme') return;
  if (event.newValue) document.documentElement.dataset.theme = event.newValue;
  else delete document.documentElement.dataset.theme;
});

/* Polling: a snapshot each second while the window shows and is not paused;
   with the log, the events since the last one. */

function plan(delay = INTERVAL) {
  clearTimeout(timer);
  if (paused || document.hidden || !source) return;
  timer = window.setTimeout(poll, delay);
}
document.addEventListener('visibilitychange', () => plan(document.hidden ? INTERVAL : 0));

async function poll() {
  if (!source || inflight) return;
  inflight = true;
  let delay = INTERVAL;
  try {
    const next = await source.snapshot();
    problem = null;
    running = true;
    if (next.run !== undefined && next.run !== run) newRun(next.run);
    if (next.traced) await fetchEvents(next);
    if (historyRun !== run) await fetchHistory();
    snap = next;
    record(next);
    track(next);
    rankSites(next);
    if (selection?.kind === 'block' && !cursor) follow();
  } catch (error) {
    const e = errorOf(error);
    // Busy: an SD operation has the emulator, so keep the last view.
    if (e.code === 'EIDLE') { running = false; delay = 2 * INTERVAL; }
    else if (e.code !== 'EBUSY') { problem = e; delay = 2 * INTERVAL; }
  } finally {
    inflight = false;
    schedule();
    plan(delay);
  }
}

/** A new run: everything about the last one goes. */
function newRun(id: number) {
  run = id;
  snap = null;
  log.reset(0);
  history = [];
  historyRun = undefined;
  cursor = null;
  pastSnap = null;
  wanted = null;
  markA = null;
  stopPlay();
  selection = null;
  stacks.clear();
  slotOf.clear();
  for (const v of views.values()) v.el.remove();
  views.clear();
}

async function fetchEvents(s: HeapSnapshot) {
  for (let guard = 0; guard < 50 && source; guard++) {
    const same = s.traceGen === log.gen;
    if (same && log.n >= (s.traceEvents ?? 0)) return;
    const ev = await source.events(log.gen, same ? log.n : 0, same ? log.stackSite.length : 0, same ? log.sites.length : 0);
    if (!ev) return;
    const before = log.n, gen = log.gen;
    log.ingest(ev);
    if (log.n === before && log.gen === gen) return;
  }
}

async function fetchHistory() {
  if (!source) return;
  historyRun = run;
  try {
    const points = await source.history();
    if (points.length) history = points;
  } catch {
    historyRun = undefined;
  }
}

function record(s: HeapSnapshot) {
  if (history.length && history[history.length - 1].t >= s.time) return;
  history.push({ t: s.time, h: Object.fromEntries(s.heaps.map((h) => [h.start.toString(16), [h.infoFree, h.largest, h.infoMinFree]])) });
  if (history.length > HISTORY) history.splice(0, history.length - HISTORY);
}

/** Whether the allocation log covers the run: sites, stacks, replay. */
const traced = () => !!snap?.traced && log.gen === snap.traceGen && log.n > 0;
/** The live end of the timeline, in ms since the epoch. */
const liveEnd = () => Math.max(snap ? snap.time * 1000 : 0, traced() ? log.t0 + log.end() : 0);
const clock = (t: number) => new Date(t).toLocaleTimeString([], { hour12: false });

/* Live highlights: when each allocation was first seen, and what was just freed. */

const born = new Map<number, { born: Map<string, number>; freed: { off: number; len: number; t: number }[]; first: boolean }>();
function track(s: HeapSnapshot) {
  const now = Date.now();
  for (const h of s.heaps) {
    let v = born.get(h.start);
    if (!v) born.set(h.start, v = { born: new Map(), freed: [], first: true });
    const next = new Map<string, number>(), b = h.blocks;
    for (let i = 0; i < b.length; i += 4) {
      if (b[i + 2] & FREE) continue;
      const key = `${b[i]}:${b[i + 1]}`;
      next.set(key, v.first ? 0 : (v.born.get(key) ?? now));
    }
    if (!v.first) {
      for (const key of v.born.keys()) {
        if (next.has(key)) continue;
        const [off, len] = key.split(':').map(Number);
        v.freed.push({ off, len, t: now });
      }
    }
    v.freed = v.freed.filter((f) => now - f.t < FADE);
    v.born = next;
    v.first = false;
  }
}

/* Site colours: the sites holding the most memory get a colour each, kept
   while they stay among them; the rest share grey. */

const slotOf = new Map<number, number>();
function rankSites(s: HeapSnapshot) {
  if (!s.traced) return;
  const ranked = [...siteStats(s.heaps)].filter(([id]) => id >= 0).sort((a, b) => b[1].bytes - a[1].bytes).map(([id]) => id);
  const top = new Set(ranked.slice(0, SLOTS));
  for (const id of [...slotOf.keys()]) if (!top.has(id)) slotOf.delete(id);
  const taken = new Set(slotOf.values());
  for (const id of top) {
    if (slotOf.has(id)) continue;
    let k = 1;
    while (taken.has(k)) k++;
    slotOf.set(id, k);
    taken.add(k);
  }
}
const siteColor = (site: number) => site < 0 ? C().used : slotOf.has(site) ? C().sites[slotOf.get(site)!] : C().other;
const siteAlt = (site: number) => site < 0 ? C().usedAlt : slotOf.has(site) ? C().sitesAlt[slotOf.get(site)!] : C().otherAlt;
const siteName = (site: number) => site < 0 ? 'No stack recorded' : log.sites[site] ? shortName(log.sites[site].fn) : `Site ${site}`;
const siteFile = (site: number) => site >= 0 && log.sites[site]?.loc ? splitLoc(log.sites[site].loc).file : '';

/* The moment shown ---------------------------------------------------------------- */

interface Moment {
  /** ms since the epoch. */
  t: number;
  /** Events replayed to reach it (with the log). */
  n: number;
  heaps: Heap[];
  /** Rebuilt from the log rather than walked. */
  replayed: boolean;
  /** Without the log, looking back: the kept snapshot before, for highlights. */
  prev: Heap[] | null;
}
let replayCache: { n: number; of: HeapSnapshot; heaps: Heap[] } | null = null;
function replay(n: number): Heap[] {
  if (replayCache?.n !== n || replayCache.of !== snap) replayCache = { n, of: snap!, heaps: log.heapsAt(n, snap!.heaps) };
  return replayCache.heaps;
}
function moment(): Moment | null {
  if (!snap) return null;
  if (!cursor) {
    const t = snap.time * 1000;
    return { t, n: traced() ? log.countAt(t - log.t0) : 0, heaps: snap.heaps, replayed: false, prev: null };
  }
  if (traced()) return { t: cursor.t, n: cursor.n, heaps: replay(cursor.n), replayed: true, prev: null };
  return { t: cursor.t, n: 0, heaps: pastSnap?.snapshot.heaps ?? snap.heaps, replayed: false, prev: pastSnap?.prev?.heaps ?? null };
}

/** Bytes, blocks, the largest block, blocks in PSRAM and bytes by stack, per site. */
interface SiteStat { bytes: number; blocks: number; largest: number; psram: number; stacks: Map<number, number> }
function siteStats(heaps: Heap[]) {
  const out = new Map<number, SiteStat>();
  for (const h of heaps) {
    const b = h.blocks, psram = !isInternal(h);
    for (let i = 0, n = 0; i < b.length; i += 4, n++) {
      if (b[i + 2] & FREE) continue;
      const site = h.sites?.[n] ?? -1, len = b[i + 1];
      let s = out.get(site);
      if (!s) out.set(site, s = { bytes: 0, blocks: 0, largest: 0, psram: 0, stacks: new Map() });
      s.bytes += len; s.blocks++; s.largest = Math.max(s.largest, len - 4);
      if (psram) s.psram++;
      const stack = h.stk?.[n] ?? -1;
      if (stack >= 0) s.stacks.set(stack, (s.stacks.get(stack) ?? 0) + len);
    }
  }
  return out;
}
let statsCache: { heaps: Heap[]; stats: Map<number, SiteStat> } | null = null;
const statsOf = (heaps: Heap[]) => {
  if (statsCache?.heaps !== heaps) statsCache = { heaps, stats: siteStats(heaps) };
  return statsCache.stats;
};

/** Site stats a minute before the moment, from the log; null when it does not reach back that far. */
let growthCache: { t: number; n: number; of: HeapSnapshot; stats: Map<number, SiteStat> | null } | null = null;
function growthBase(m: Moment) {
  if (!traced()) return null;
  if (growthCache?.t === m.t && growthCache.n === log.n && growthCache.of === snap) return growthCache.stats;
  const t = m.t - GROWTH - log.t0;
  const stats = t < 0 ? null : siteStats(log.heapsAt(log.countAt(t), snap!.heaps));
  growthCache = { t: m.t, n: log.n, of: snap!, stats };
  return stats;
}
let churnCache: { t: number; n: number; churn: Churn } | null = null;
function churnAt(m: Moment) {
  if (!traced()) return null;
  if (churnCache?.t !== m.t || churnCache.n !== log.n) churnCache = { t: m.t, n: log.n, churn: log.churn(m.t - log.t0, CHURN) };
  return churnCache.churn;
}

/* Comparing: a block is the same at A and B when the log says it is the same
   allocation, or else when it has the same place and size. */

interface Diff { fresh: Set<number>; freed: { off: number; len: number }[]; freshBytes: number; freedBytes: number }
const blockKeys = (h: Heap) => {
  const keys = new Map<string, number>(), b = h.blocks;
  for (let i = 0, n = 0; i < b.length; i += 4, n++) {
    if (b[i + 2] & FREE) continue;
    const ev = h.ev?.[n] ?? -1;
    keys.set(ev >= 0 ? `e${ev}` : `${b[i]}:${b[i + 1]}`, n);
  }
  return keys;
};
let diffs = new WeakMap<Heap, Diff>();
function diffOf(h: Heap): Diff | null {
  if (!markA) return null;
  let d = diffs.get(h);
  if (d) return d;
  const before = markA.heaps.find((a) => a.start === h.start);
  const was = before ? blockKeys(before) : new Map<string, number>(), is = blockKeys(h);
  d = { fresh: new Set(), freed: [], freshBytes: 0, freedBytes: 0 };
  for (const [key, n] of is) if (!was.has(key)) { d.fresh.add(n); d.freshBytes += h.blocks[n * 4 + 1]; }
  if (before) {
    for (const [key, n] of was) {
      if (is.has(key)) continue;
      d.freed.push({ off: before.blocks[n * 4], len: before.blocks[n * 4 + 1] });
      d.freedBytes += before.blocks[n * 4 + 1];
    }
  }
  diffs.set(h, d);
  return d;
}

/** The colour mode in effect: Site needs the log, Change needs A. */
function mode(): ColorBy {
  if (colorBy === 'change' && markA) return 'change';
  if (colorBy !== 'state' && traced()) return 'site';
  return 'state';
}

/* Moving in time ------------------------------------------------------------------- */

function setCursor(t: number, n?: number) {
  if (!snap) return;
  const end = liveEnd();
  if (traced()) {
    n = Math.max(0, Math.min(log.n, n ?? log.countAt(t - log.t0)));
    const liveN = log.countAt(snap.time * 1000 - log.t0);
    if (t >= end && n >= liveN) { goLive(); return; }
    t = Math.min(t, end);
    cursor = { t, n };
  } else {
    if (!history.length) return;
    // Without the log, the moments are the kept snapshots.
    const i = nearest(t / 1000);
    if (i === history.length - 1) { goLive(); return; }
    t = history[i].t * 1000;
    cursor = { t, n: 0 };
    if (pastSnap?.snapshot.time !== history[i].t) { wanted = history[i].t; void fetchPast(); }
  }
  if (range && (cursor.t < tmEnd - range || cursor.t > tmEnd)) tmEnd = Math.min(end, cursor.t + range / 2);
  follow();
  schedule();
}
const nearest = (time: number) => {
  let best = 0;
  history.forEach((p, i) => { if (Math.abs(p.t - time) < Math.abs(history[best].t - time)) best = i; });
  return best;
};
async function fetchPast() {
  if (fetching || !source) return;
  fetching = true;
  const asked = run;
  try {
    while (wanted !== null && source) {
      const time = wanted;
      const got = await source.at(time).catch(() => null);
      if (asked !== run) break;
      if (wanted === time) wanted = null;
      if (got && cursor) { pastSnap = got; follow(); schedule(); }
    }
  } finally {
    fetching = false;
  }
}

function goLive() {
  stopPlay();
  wanted = null;
  if (!cursor) { schedule(); return; }
  cursor = null;
  pastSnap = null;
  // Highlights start over rather than flash every change made meanwhile.
  for (const v of born.values()) v.freed = [];
  follow();
  schedule();
}

/** One event (or kept snapshot) back or forward; with `second`, one second. */
function step(dir: number, second = false) {
  if (!snap) return;
  stopPlay();
  const m = moment()!;
  if (second) { setCursor(m.t + dir * 1000); return; }
  if (traced()) {
    if (dir > 0 && !cursor) return;
    if (dir > 0 && m.n >= log.n) { goLive(); return; }
    const n = m.n + dir;
    setCursor(n > 0 ? log.t0 + log.t[n - 1] : log.t0, n);
  } else if (history.length) {
    const i = cursor ? nearest(cursor.t / 1000) : history.length - 1;
    const to = Math.max(0, Math.min(history.length - 1, i + dir));
    setCursor(history[to].t * 1000);
  }
}

function play() {
  if (!cursor) return;
  playing = { last: performance.now() };
  const tick = (ts: number) => {
    if (!playing || !cursor) return;
    const t = cursor.t + Math.max(0, ts - playing.last) * speed;
    playing.last = ts;
    if (t >= liveEnd()) { goLive(); return; }
    setCursor(t);
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  schedule();
}
function stopPlay() {
  if (!playing) return;
  playing = null;
  schedule();
}

/** Mark A at `t` (ms since the epoch), or at the cursor. The cursor stays
    B; while it is live, the comparison grows as the firmware runs. */
async function markAt(t?: number) {
  if (!snap) return;
  if (t === undefined || t >= liveEnd()) {
    const m = moment()!;
    setMark({ t: m.t, n: m.n, heaps: m.heaps });
  } else if (traced()) {
    const n = log.countAt(t - log.t0);
    setMark({ t, n, heaps: log.heapsAt(n, snap.heaps) });
  } else if (source && history.length) {
    const asked = run, got = await source.at(history[nearest(t / 1000)].t).catch(() => null);
    if (got && asked === run) setMark({ t: got.snapshot.time * 1000, n: 0, heaps: got.snapshot.heaps });
  }
}
function setMark(mark: NonNullable<typeof markA>) {
  markA = mark;
  diffs = new WeakMap();
  if (colorBy !== 'change') { colorBefore = colorBy; colorBy = 'change'; }
  schedule();
}
function clearMark() {
  markA = null;
  diffs = new WeakMap();
  if (colorBy === 'change') colorBy = colorBefore ?? 'site';
  colorBefore = null;
  schedule();
}

/* Selecting ------------------------------------------------------------------------- */

function select(next: Selection | null) {
  selection = next;
  tipBox.hidden = true;
  if (next?.kind === 'block') reveal(next.start, next.off);
  schedule();
}

/** Keep the selected block's facts current, or note it went; looking back, whether it is there. */
function follow() {
  const s = selection;
  if (s?.kind !== 'block') return;
  const m = moment();
  const h = m?.heaps.find((x) => x.start === s.start);
  if (!m || !h) return;
  const b = h.blocks;
  for (let i = 0, n = 0; i < b.length; i += 4, n++) {
    const ev = h.ev?.[n] ?? -1;
    const same = s.ev >= 0 ? ev === s.ev : b[i] === s.off && b[i + 1] === s.len && !(b[i + 2] & FREE) === !(s.fl & FREE);
    if (!same) continue;
    Object.assign(s, { off: b[i], len: b[i + 1], fl: b[i + 2], req: b[i + 3], site: h.sites?.[n] ?? -1, stack: h.stk?.[n] ?? s.stack, absent: false });
    if (!cursor) s.freedAt = 0;
    return;
  }
  if (cursor) s.absent = true;
  else if (!s.freedAt) s.freedAt = Date.now();
}

/** Scroll the maps so a block's row shows, unfolding its heap. */
function reveal(start: number, off: number) {
  const v = views.get(start);
  if (!v) return;
  if (v.layout.folded) { folds[hex(start)] = false; save('eink-qemu-heapwin-folds', folds); }
  requestAnimationFrame(() => {
    const y = v.canvas.getBoundingClientRect().top - mapsBox.getBoundingClientRect().top + mapsBox.scrollTop + rowTop(off, v.layout);
    if (y < mapsBox.scrollTop + 8 || y > mapsBox.scrollTop + mapsBox.clientHeight - 24) {
      mapsBox.scrollTo({ top: Math.max(0, y - mapsBox.clientHeight / 3), behavior: 'smooth' });
    }
  });
}

/* Rendering: one frame at a time; the side panels at most four times a
   second while replaying. */

let queued = false, panelsAt = 0;
/** A press in the side panels: they are not rebuilt under it, or its click would be lost. */
let pressed = false;
addEventListener('pointerdown', (e) => { pressed = !!(e.target as Element).closest?.('.hw-sites, .hw-inspector'); }, true);
for (const type of ['pointerup', 'pointercancel'] as const) addEventListener(type, () => { if (pressed) { pressed = false; schedule(); } }, true);
function schedule() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => { queued = false; render(); });
}

function render() {
  const m = moment();
  paintHeader(m);
  paintControls(m);
  const ok = paintMessage();
  if (!ok || !m) {
    for (const id of ['hw-summary', 'hw-legend', 'hw-list']) $(id).hidden = true;
    $<HTMLTableSectionElement>('hw-tbody').innerHTML = '';
    $('hw-thead').innerHTML = '';
    $('hw-sites-foot').textContent = '';
    $('hw-sites-count').textContent = '';
    $('hw-sites-empty').hidden = true;
    inspector.innerHTML = '';
    delete inspector.dataset.html;
    drawTimeline(null);
    return;
  }
  for (const id of ['hw-summary', 'hw-legend', 'hw-list']) $(id).hidden = false;
  paintMaps(m);
  const now = performance.now();
  if (!pressed && (!playing || now - panelsAt > 250)) {
    panelsAt = now;
    paintSites(m);
    paintInspector(m);
  }
  drawTimeline(m);
  if (!cursor && !markA) animate();
}

function paintHeader(m: Moment | null) {
  const s = snap;
  $('hw-elf').textContent = s ? (s.elf ? `${s.elf}${traced() ? ` · ${fmt(log.n)} events` : ''}` : 'no ELF') : '';
  const live = $('hw-live');
  live.className = 'hw-live';
  let state: string;
  if (!running) { state = 'Not running'; live.classList.add('off'); }
  else if (problem) { state = 'Cannot read the heaps'; live.classList.add('bad'); }
  else if (!s || !m) state = 'Reading the heaps…';
  else if (markA) {
    live.classList.add('compare');
    state = `Comparing A ${clock(markA.t)} → B ${cursor ? clock(m.t) : 'now'}`;
  } else if (cursor) {
    live.classList.add('past');
    state = `Looking back · ${clock(m.t)} · ${ago(liveEnd() - m.t)} before now`;
  } else {
    if (paused) live.classList.add('off');
    const took = s.took === undefined ? '' : ` · read in ${Math.round(s.took)} ms`;
    const skipped = s.missing ? ` · ${s.missing} heap${s.missing > 1 ? 's' : ''} changing, skipped` : '';
    state = `${paused ? 'Paused' : 'Live'} · ${clock(s.time * 1000)}${paused ? '' : took}${skipped}`;
  }
  $('hw-state').textContent = state;

  const note = $('hw-note');
  const text = !s ? '' : !s.elf
    ? `${s.elfNote ? `${s.elfNote} ` : ''}Without the firmware's ELF, blocks have no allocation sites or call stacks. Choose it in the main window's firmware card.`
    : s.traceFormat === false ? 'The allocation log comes from an older heaptrace plugin: rebuild it and start the emulator again.'
    : !s.traced ? s.traceNote ?? '' : '';
  note.textContent = text;
  note.hidden = !text;
}

function paintControls(m: Moment | null) {
  const effective = mode();
  for (const b of $('hw-colorby').querySelectorAll<HTMLButtonElement>('button')) {
    const c = b.dataset.color as ColorBy;
    b.setAttribute('aria-checked', String(c === effective));
    b.disabled = c === 'site' ? !traced() : c === 'change' ? !markA : false;
  }
  $<HTMLSelectElement>('hw-scale').value = String(scale);
  const pause = $('hw-pause');
  pause.setAttribute('aria-pressed', String(paused));
  pause.textContent = paused ? 'Resume' : 'Pause';

  const ready = !!(m && running && !problem);
  const play = $<HTMLButtonElement>('hw-play');
  play.disabled = !cursor;
  play.querySelector('use')!.setAttribute('href', playing ? '#i-pause' : '#i-play');
  const label = playing ? 'Pause the replay (Space)' : 'Play from the cursor (Space)';
  play.setAttribute('aria-label', label);
  play.title = label;
  for (const b of $('hw-speed').querySelectorAll<HTMLButtonElement>('button')) {
    b.setAttribute('aria-checked', String(Number(b.dataset.speed) === speed));
    b.disabled = !ready;
  }
  for (const b of $('hw-range').querySelectorAll<HTMLButtonElement>('button')) {
    b.setAttribute('aria-checked', String(Number(b.dataset.range) === range));
    b.disabled = !ready;
  }
  $<HTMLButtonElement>('hw-prev').disabled = !ready || (traced() ? m!.n <= 0 : !history.length || (!!cursor && nearest(cursor.t / 1000) === 0));
  $<HTMLButtonElement>('hw-next').disabled = !ready || !cursor;
  $<HTMLButtonElement>('hw-golive').disabled = !cursor;
  $<HTMLButtonElement>('hw-mark').disabled = !ready;
  $('hw-mark').hidden = !!markA;
  $('hw-mark-set').hidden = !markA;
  if (markA) $('hw-mark-time').textContent = clock(markA.t);

  const labelBox = $('hw-time-label');
  if (!ready || !m) { labelBox.innerHTML = ''; return; }
  if (!cursor) {
    labelBox.innerHTML = traced() ? `<b>Live</b> · ${fmt(log.n)} events since QEMU started` : `<b>Live</b>`;
    return;
  }
  let what = `<b>${clock(m.t)}</b> · ${ago(liveEnd() - m.t)} before now`;
  if (traced()) {
    what += ` · event ${fmt(m.n)} of ${fmt(log.n)}`;
    const i = m.n - 1;
    if (i >= 0) {
      const k = log.k[i], size = k === FREED ? log.fs[i] : log.s[i];
      const site = log.siteOf(k === FREED ? log.fst[i] : log.st[i]);
      const op = k === ALLOC ? '+' : k === FREED ? '−' : '↻';
      what += ` · <span class="hw-ev ${k === FREED ? 'freed' : 'fresh'}">${op} ${size ? `${fmt(size)} B` : '?'} ${esc(siteName(site))}</span>`;
    }
  }
  labelBox.innerHTML = what;
}

/** The message in place of the maps; false when there is nothing else to show. */
function paintMessage() {
  const box = $('hw-message');
  let title = '', text = '';
  if (!running) {
    title = 'The emulator is not running';
    text = 'Start the firmware in the main window to see its heaps. While this window is open, the emulator pauses for a moment each second to read them.';
  } else if (problem) {
    title = 'Cannot read the heaps';
    text = problem.message;
  } else if (!snap) {
    title = 'Reading the heaps…';
  } else if (!snap.heaps.length) {
    title = 'Waiting for the firmware';
    text = snap.missing ? 'The heaps were changing in every read; trying again.' : 'No heaps are registered yet; the firmware sets them up early in boot.';
  }
  box.hidden = !title;
  $('hw-message-title').textContent = title;
  $('hw-message-text').textContent = text;
  return !title;
}

/* The maps -------------------------------------------------------------------------- */

interface View { el: HTMLElement; canvas: HTMLCanvasElement; gutter: HTMLElement; heap: Heap; layout: MapLayout }
const views = new Map<number, View>();

function view(h: Heap): View {
  let v = views.get(h.start);
  if (v) return v;
  const el = document.createElement('section');
  el.className = 'heap-card';
  el.innerHTML = `
    <button class="heap-head" type="button">
      <svg class="icon heap-chevron" aria-hidden="true"><use href="#i-chevron"/></svg>
      <span class="heap-name"></span><span class="heap-role"></span>
      <span class="heap-addr"></span><span class="heap-used num"></span>
    </button>
    <div class="heap-stats num"></div>
    <div class="heap-map"><div class="heap-gutter num"></div><canvas></canvas></div>`;
  const canvas = el.querySelector('canvas')!;
  v = { el, canvas, gutter: el.querySelector<HTMLElement>('.heap-gutter')!, heap: h, layout: { bpr: 1, width: 1, folded: false } };
  const self = v;
  el.querySelector('.heap-head')!.addEventListener('click', () => {
    folds[hex(self.heap.start)] = !self.layout.folded;
    save('eink-qemu-heapwin-folds', folds);
    schedule();
  });
  canvas.addEventListener('mousemove', (e) => mapTip(self, e));
  canvas.addEventListener('mouseleave', () => { tipBox.hidden = true; });
  canvas.addEventListener('click', (e) => {
    const x = hitMap(canvas, self.heap, self.layout, e);
    if (!x || x.kind !== 'block' || !x.used) { if (selection?.kind === 'block') select(null); return; }
    const h = self.heap, ev = h.ev?.[x.n] ?? -1;
    const same = selection?.kind === 'block' && selection.start === h.start && (ev >= 0 ? selection.ev === ev : selection.off === x.off && selection.len === x.len);
    select(same ? null : { kind: 'block', start: h.start, off: x.off, len: x.len, fl: x.fl, req: x.req, site: h.sites?.[x.n] ?? -1,
      stack: h.stk?.[x.n] ?? -1, ev, freedAt: 0, absent: false });
  });
  views.set(h.start, v);
  return v;
}

function paintMaps(m: Moment) {
  const heaps = [...m.heaps].sort(byKind);
  const internal = heaps.filter(isInternal), psram = heaps.filter((h) => !isInternal(h));
  const sum = (list: Heap[], f: (h: Heap) => number) => list.reduce((a, h) => a + f(h), 0);
  const max = (list: Heap[], f: (h: Heap) => number) => list.reduce((a, h) => Math.max(a, f(h)), 0);
  const corrupt = sum(heaps, (h) => h.corrupt);
  const parts: string[] = [];
  if (markA) {
    const a = markA.heaps.filter(isInternal), freeA = sum(a, (h) => h.infoFree), freeB = sum(internal, (h) => h.infoFree);
    const d = heaps.map(diffOf).filter((x): x is Diff => !!x);
    parts.push(`Internal free <b>${kib(freeA)}</b> → <b>${kib(freeB)}</b> <span class="${usage(freeA - freeB)}">${delta(freeB - freeA)}</span>`);
    parts.push(`<span class="fresh">new since A</span> <b>${fmt(d.reduce((x, y) => x + y.fresh.size, 0))}</b> blocks · ${kib(d.reduce((x, y) => x + y.freshBytes, 0))}`);
    parts.push(`<span class="freed">freed since A</span> <b>${fmt(d.reduce((x, y) => x + y.freed.length, 0))}</b> blocks · ${kib(d.reduce((x, y) => x + y.freedBytes, 0))}`);
  } else {
    parts.push(`Internal free <b>${kib(sum(internal, (h) => h.infoFree))}</b> of ${kib(sum(internal, (h) => h.end - h.start))}`);
    if (!m.replayed) parts.push(`lowest <b>${kib(sum(internal, (h) => h.infoMinFree))}</b>`);
    parts.push(`largest block <b>${kib(max(internal, (h) => h.largest))}</b>`);
    if (psram.length) parts.push(`PSRAM free <b>${kib(sum(psram, (h) => h.infoFree))}</b>`);
  }
  if (corrupt) parts.push(`<span class="bad"><b>${fmt(corrupt)}</b> corrupt</span>`);
  if (m.replayed) parts.push(`<span class="hw-faint">replayed from the allocation log: block sizes as TLSF would round them</span>`);
  $('hw-summary').innerHTML = parts.map((p) => `<span>${p}</span>`).join('');
  paintLegend(m.heaps);

  const mainStart = heaps.find((h) => h.name === 'DRAM')?.start ?? -1;
  const list = $('hw-list'), seen = new Set<number>();
  // Move a card only when it is out of place: moving one re-anchors the scroll.
  let at = list.firstElementChild;
  for (const h of heaps) {
    const v = view(h);
    seen.add(h.start);
    v.heap = h;
    if (at === v.el) at = at.nextElementSibling;
    else list.insertBefore(v.el, at);
    const size = h.end - h.start, folded = folds[hex(h.start)] ?? h.used === 0;
    v.el.classList.toggle('folded', folded);
    const head = v.el.querySelector('.heap-head')!;
    head.setAttribute('aria-expanded', String(!folded));
    head.setAttribute('title', `${hex(h.start)}–${hex(h.end)}\n${h.caps.join(' ')}\n${folded ? 'Click to show the map' : 'Click to fold to a bar'}`);
    v.el.querySelector('.heap-name')!.textContent = h.name;
    v.el.querySelector('.heap-role')!.textContent = role(h, mainStart);
    v.el.querySelector('.heap-addr')!.textContent = hex(h.start);
    v.el.querySelector('.heap-used')!.innerHTML = h.used ? `<b>${kib(h.used)}</b> / ${kib(size)}` : `empty · ${kib(size)}`;
    const { bpr, capped } = rowBytes(size, scale);
    const d = diffOf(h), before = markA?.heaps.find((a) => a.start === h.start);
    const stats: [string, string, string?][] = [['free', kib(h.infoFree)]];
    if (d && before) stats[0][2] = delta(h.infoFree - before.infoFree);
    stats.push(['largest', kib(h.largest)]);
    if (!m.replayed && !d) stats.push(['lowest', kib(h.infoMinFree)]);
    stats.push(['blocks', `${fmt(h.count)} · ${fmt(h.nfree)} free`]);
    if (d) stats.push(['new', fmt(d.fresh.size)], ['freed', fmt(d.freed.length)]);
    stats.push(['fragmented', `${fragmented(h)}%`]);
    if (capped) stats.push(['scale', `${per(bpr)}/row`]);
    v.el.querySelector('.heap-stats')!.innerHTML = stats.map(([k, val, change]) =>
      `<span class="${k === 'new' ? 'fresh' : k === 'freed' ? 'freed' : ''}"><span>${k}</span> <b>${val}</b>${change ? ` <i>${change}</i>` : ''}</span>`).join('')
      + (h.corrupt ? `<span class="bad"><b>${fmt(h.corrupt)} corrupt</b></span>` : '');
    const map = v.el.querySelector<HTMLElement>('.heap-map')!;
    v.layout = { folded, width: Math.max(60, map.clientWidth - (folded ? 0 : GUTTER)), bpr: folded ? size : bpr };
    draw(v, m);
  }
  for (const [start, v] of views) {
    if (!seen.has(start)) { v.el.remove(); views.delete(start); }
  }
}

function paintLegend(heaps: Heap[]) {
  const sw = (color: string, label: string, extra = '') => `<span><i class="sw" style="background:${color}${extra}"></i>${esc(label)}</span>`;
  const c = C(), mo = mode(), items: string[] = [];
  if (mo === 'change') {
    items.push(sw(c.used, 'there at A and B', ';opacity:.45'), sw(c.fresh, 'new since A'), sw(c.freed, 'freed since A'));
  } else if (mo === 'site') {
    const stats = statsOf(heaps);
    const shown = [...slotOf.keys()].filter((id) => stats.has(id)).sort((a, b) => stats.get(b)!.bytes - stats.get(a)!.bytes);
    for (const id of shown) items.push(sw(siteColor(id), siteName(id)));
    if ([...stats.keys()].some((id) => id >= 0 && !slotOf.has(id))) items.push(sw(c.other, 'other sites'));
    if (stats.has(-1)) items.push(sw(c.used, 'no stack recorded'));
    items.push(sw(c.fresh, 'new'), sw(c.freed, 'just freed'));
  } else {
    items.push(sw(c.used, 'allocated'), sw(c.fresh, 'new'), sw(c.freed, 'just freed'));
  }
  items.push(`<span><i class="sw free"></i>free</span>`, sw(c.meta, 'heap overhead'));
  if (heaps.some((h) => h.corrupt)) items.push(sw(c.corrupt, 'corrupt'));
  $('hw-legend').innerHTML = items.join('');
}

/** The site to light on the maps: the one hovered in the list, else the one selected. */
const focusSite = () => hoverSite ?? (selection?.kind === 'site' ? selection.site : null);

function draw(v: View, m: Moment) {
  const c = C(), h = v.heap, b = h.blocks, mo = mode(), focus = focusSite();
  const site = (n: number) => h.sites?.[n] ?? -1;
  const style: MapStyle = mo === 'site'
    ? { color: (n) => siteColor(site(n)), alt: (n) => siteAlt(site(n)) }
    : { color: () => c.used, alt: () => c.usedAlt };
  if (mo === 'change') {
    const d = diffOf(h)!;
    style.color = (n) => d.fresh.has(n) ? c.fresh : c.used;
    style.alt = (n) => d.fresh.has(n) ? c.fresh : c.usedAlt;
    style.alpha = (n) => d.fresh.has(n) ? 1 : 0.45;
    style.freed = d.freed.map((f) => ({ ...f, w: 1 }));
    style.under = true;
  } else if (!cursor) {
    // Live: what the last snapshots allocated and freed, fading.
    const now = Date.now(), track = born.get(h.start);
    if (track) {
      style.glow = (n) => { const t = track.born.get(`${b[n * 4]}:${b[n * 4 + 1]}`); return t ? 1 - (now - t) / FADE : 0; };
      style.freed = track.freed.map((f) => ({ off: f.off, len: f.len, w: 1 - (now - f.t) / FADE }));
    }
  } else if (m.replayed) {
    // Replaying: what the last moments of the log allocated and freed.
    const rel = m.t - log.t0;
    style.glow = (n) => { const ev = h.ev?.[n] ?? -1; return ev >= 0 ? 1 - (rel - log.t[ev]) / PAST_FADE : 0; };
    const freed: { off: number; len: number; w: number }[] = [];
    for (let i = m.n - 1; i >= 0 && log.t[i] > rel - PAST_FADE; i--) {
      const k = log.k[i];
      if (k === ALLOC || !log.fs[i]) continue;
      const off = (k === FREED ? log.p[i] : log.o[i]) - 12 - h.start;
      if (off >= 0 && off < h.end - h.start) freed.push({ off, len: blockLen(log.fs[i]), w: 1 - (rel - log.t[i]) / PAST_FADE });
    }
    style.freed = freed;
  } else if (m.prev) {
    // A kept snapshot: what changed since the one before stays lit.
    const before = m.prev.find((p) => p.start === h.start);
    if (before) {
      const was = blockKeys(before), is = blockKeys(h);
      const fresh = new Set([...is].filter(([key]) => !was.has(key)).map(([, i]) => i));
      style.glow = (n) => fresh.has(n) ? 0.8 : 0;
      style.freed = [...was].filter(([key]) => !is.has(key)).map(([, i]) => ({ off: before.blocks[i * 4], len: before.blocks[i * 4 + 1], w: 0.8 }));
    }
  }
  if (focus !== null) {
    const base = style.alpha;
    style.alpha = (n) => (site(n) === focus ? 1 : 0.22) * (base ? base(n) : 1);
  }
  const sel = selection;
  style.outline = sel?.kind === 'block' && sel.start === h.start && !(cursor ? sel.absent : sel.freedAt) ? sel : null;
  paintMap(v.canvas, v.layout.folded ? null : v.gutter, h, v.layout, c, style);
}

/* Fade the live highlights between snapshots, a few frames a second. */
let frame = 0;
function animate() {
  cancelAnimationFrame(frame);
  let last = 0;
  const tick = (ts: number) => {
    if (cursor || markA) return;
    const now = Date.now(), m = moment();
    let active = false;
    for (const [start, track] of born) {
      const lit = track.freed.some((f) => now - f.t < FADE) || [...track.born.values()].some((t) => t && now - t < FADE);
      const v = views.get(start);
      if (lit && v && m && ts - last > 80) draw(v, m);
      active ||= lit;
    }
    if (ts - last > 80) last = ts;
    if (active && !document.hidden) frame = requestAnimationFrame(tick);
  };
  frame = requestAnimationFrame(tick);
}

function mapTip(v: View, e: MouseEvent) {
  const x = hitMap(v.canvas, v.heap, v.layout, e);
  if (!x) { tipBox.hidden = true; return; }
  let rows: TipRow[];
  if (x.kind === 'meta') {
    rows = [['region', x.meta], ['address', hex(x.addr)]];
  } else {
    const h = v.heap, start = h.start, site = h.sites?.[x.n] ?? -1, ev = h.ev?.[x.n] ?? -1;
    const d = mode() === 'change' ? diffOf(h) : null;
    rows = [['state', x.fl & CORRUPT ? 'allocated · tail canary overwritten' : x.used ? (d ? (d.fresh.has(x.n) ? 'new since A' : 'there at A and B') : 'allocated') : 'free'],
      ['block', hex(blockAddr(start, x.off))]];
    if (x.used) rows.push(['pointer', hex(pointerOf(start, x.off, x.fl))]);
    rows.push([cursor && traced() ? 'block size ≈' : 'block size', `${fmt(x.len - 4)} B`]);
    if (x.fl & POISONED) rows.push(['requested', `${fmt(x.req)} B`]);
    if (x.used) {
      const age = blockAge(h, x.n, x.off, x.len);
      if (age !== null) rows.push(['age', age]);
    }
    if (x.nUsed + x.nFree > 1) rows.push(['under the pointer', `${x.nUsed} allocated, ${x.nFree} free`]);
    if (x.used && traced()) {
      const fn = log.sites[site]?.fn;
      rows.push(['allocated by', site < 0 ? 'no stack recorded' : fn ? callName(fn) : `site ${site}`, true]);
      if (fn && log.sites[site].loc) rows.push(['at', splitLoc(log.sites[site].loc).file, true]);
      if (ev >= 0) rows.push(['allocated at', clock(log.t0 + log.t[ev])]);
    }
    if (x.used) rows.push(['', 'Click for details and the call stack']);
  }
  showTip(tipBox, e, rows);
}

/** How long a block has lived at the moment shown: from the log, or since this window first saw it. */
function blockAge(h: Heap, n: number, off: number, len: number): string | null {
  const ev = h.ev?.[n] ?? -1, m = moment();
  if (!m) return null;
  if (ev >= 0 && traced()) return ago(m.t - (log.t0 + log.t[ev]));
  if (cursor) return null;
  const t = born.get(h.start)?.born.get(`${off}:${len}`);
  return t ? ago(Date.now() - t) : t === 0 ? 'older than the window' : null;
}

/* The sites ------------------------------------------------------------------------- */

interface Row { site: number; bytes: number; blocks: number; rate: number | null; growth: number | null; a: number; b: number; delta: number }
let rowsShown: Row[] = [];

function siteRows(m: Moment): Row[] {
  const stats = statsOf(m.heaps), churn = churnAt(m), base = markA ? null : growthBase(m);
  const atA = siteStatsA();
  const ids = new Set<number>([...stats.keys(), ...(atA?.keys() ?? [])]);
  const rows: Row[] = [];
  for (const site of ids) {
    const s = stats.get(site), bytes = s?.bytes ?? 0, a = atA?.get(site)?.bytes ?? 0;
    const c = churn?.get(site);
    rows.push({
      site, bytes, blocks: s?.blocks ?? 0, rate: churn ? (c?.allocs ?? 0) / (CHURN / 1000) : null,
      growth: base ? bytes - (base.get(site)?.bytes ?? 0) : null, a, b: bytes, delta: bytes - a,
    });
  }
  return rows;
}
const atAStatsCache = new WeakMap<Heap[], Map<number, SiteStat>>();
function siteStatsA() {
  if (!markA) return null;
  let s = atAStatsCache.get(markA.heaps);
  if (!s) atAStatsCache.set(markA.heaps, s = siteStats(markA.heaps));
  return s;
}

/** The class for a change in memory in use: more reads as a warning, less as relief. */
const usage = (more: number) => more > 51.2 ? 'use-up' : more < -51.2 ? 'use-down' : '';
/** Table cells' compact sizes: "433.7K", "1.05M". */
const short = (n: number) => Math.abs(n) >= 1048576 ? `${(n / 1048576).toFixed(2)}M` : `${(n / 1024).toFixed(1)}K`;
const shortDelta = (n: number) => Math.abs(n) < 51.2 ? '0' : `${n > 0 ? '+' : '−'}${short(Math.abs(n))}`;

function paintSites(m: Moment) {
  const empty = $('hw-sites-empty'), thead = $('hw-thead'), tbody = $<HTMLTableSectionElement>('hw-tbody');
  if (!traced()) {
    thead.innerHTML = '';
    tbody.innerHTML = '';
    table.hidden = true;
    $('hw-filter').parentElement!.hidden = true;
    $('hw-sites-count').textContent = '';
    $('hw-sites-foot').textContent = '';
    empty.hidden = false;
    empty.textContent = !snap?.elf
      ? "Allocation sites need the firmware's ELF, chosen before Start so the emulator records allocations."
      : snap.traceNote || 'No allocations have been recorded yet.';
    return;
  }
  table.hidden = false;
  $('hw-filter').parentElement!.hidden = false;
  empty.hidden = true;
  const compare = !!markA;
  const columns: [SortKey, string, string][] = compare
    ? [['site', 'Site', ''], ['a', 'At A', 'Bytes at A'], ['b', 'At B', 'Bytes at the cursor'], ['delta', 'Change', 'From A to B']]
    : [['site', 'Site', ''], ['live', 'Live', 'Bytes in live blocks'], ['blocks', 'Blocks', 'Live blocks'],
       ['rate', '/s', `Allocations a second over the last ${CHURN / 1000} s`], ['growth', '1 min', 'Change in live bytes over the last minute']];
  if (!columns.some(([k]) => k === sort.key)) sort = { key: compare ? 'delta' : 'live', desc: true };
  const head = `<tr>${columns.map(([key, label, title]) => {
    const on = sort.key === key;
    return `<th class="${key === 'site' ? '' : 'r'}" aria-sort="${on ? (sort.desc ? 'descending' : 'ascending') : 'none'}"${title ? ` title="${esc(title)}"` : ''}>` +
      `<button type="button" data-sort="${key}">${label}${on ? (sort.desc ? ' ▾' : ' ▴') : ''}</button></th>`;
  }).join('')}</tr>`;
  if (thead.innerHTML !== head) thead.innerHTML = head;

  const all = siteRows(m);
  const words = filter.trim().toLowerCase();
  const rows = words ? all.filter((r) => {
    const s = log.sites[r.site];
    return (s ? `${s.fn} ${s.loc}` : siteName(r.site)).toLowerCase().includes(words);
  }) : all;
  const value = (r: Row): number | string => sort.key === 'site' ? siteName(r.site).toLowerCase()
    : sort.key === 'live' ? r.bytes : sort.key === 'blocks' ? r.blocks : sort.key === 'rate' ? r.rate ?? -1
    : sort.key === 'growth' ? r.growth ?? -Infinity : sort.key === 'a' ? r.a : sort.key === 'b' ? r.b : r.delta;
  rows.sort((x, y) => {
    const a = value(x), b = value(y);
    const order = typeof a === 'string' ? a.localeCompare(b as string) : (a as number) - (b as number);
    return (sort.desc ? -order : order) || y.bytes - x.bytes;
  });
  if (!words && compare) rows.splice(0, rows.length, ...rows.filter((r) => r.a || r.b));
  rowsShown = rows.slice(0, ROWS_SHOWN);
  const selected = selection?.kind === 'site' ? selection.site : selection?.kind === 'block' ? selection.site : null;
  const cell = (n: number | null, cls = '') => `<td class="r num ${cls}">${n === null ? '—' : esc(String(n))}</td>`;
  tbody.innerHTML = rowsShown.map((r) => {
    const s = log.sites[r.site], file = siteFile(r.site);
    const name = `<td><span class="hw-site"><i class="sw" style="background:${siteColor(r.site)}"></i>` +
      `<span class="hw-site-name" title="${esc(s?.fn ?? siteName(r.site))}">${esc(siteName(r.site))}</span></span>` +
      (file ? `<span class="hw-site-loc" title="${esc(s!.loc)}"><bdi>${esc(file)}</bdi></span>` : '') + '</td>';
    const cells = compare
      ? `<td class="r num hw-dim">${short(r.a)}</td><td class="r num">${short(r.b)}</td><td class="r num ${usage(r.delta)}">${shortDelta(r.delta)}</td>`
      : `<td class="r num">${short(r.bytes)}</td>${cell(r.blocks)}` +
        `<td class="r num">${r.rate === null ? '—' : r.rate ? r.rate.toFixed(1) : '0'}</td>` +
        `<td class="r num ${r.growth === null ? '' : usage(r.growth)}">${r.growth === null ? '—' : shortDelta(r.growth)}</td>`;
    return `<tr data-site="${r.site}"${r.site === selected ? ' class="on" aria-selected="true"' : ''}>${name}${cells}</tr>`;
  }).join('') + (rows.length > ROWS_SHOWN ? `<tr class="more"><td colspan="${columns.length}">${fmt(rows.length - ROWS_SHOWN)} more sites; filter to find them</td></tr>` : '');

  const total = all.reduce((a, r) => a + r.bytes, 0);
  $('hw-sites-count').textContent = `${fmt(all.filter((r) => r.bytes).length)} · ${kib(total)}`;
  if (compare) {
    const grew = all.filter((r) => r.delta > 0).reduce((a, r) => a + r.delta, 0), shrank = all.filter((r) => r.delta < 0).reduce((a, r) => a + r.delta, 0);
    $('hw-sites-foot').innerHTML = `A → B: <span class="use-up">${delta(grew)}</span> in sites that grew, <span class="use-down">${delta(shrank)}</span> in sites that shrank`;
  } else {
    const rate = all.reduce((a, r) => a + (r.rate ?? 0), 0), grey = all.some((r) => r.site >= 0 && r.bytes && !slotOf.has(r.site));
    $('hw-sites-foot').innerHTML = `${(Math.round(rate * 10) / 10).toFixed(1)} allocations/s` +
      (grey ? ` · <i class="sw" style="background:${C().other}"></i>smaller sites share grey` : '');
  }
}

$('hw-thead').addEventListener('click', (e) => {
  const b = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-sort]');
  if (!b) return;
  const key = b.dataset.sort as SortKey;
  sort = sort.key === key ? { key, desc: !sort.desc } : { key, desc: key !== 'site' };
  save('eink-qemu-heapwin-sort', sort);
  schedule();
});
const rowSite = (e: Event) => {
  const tr = (e.target as HTMLElement).closest<HTMLElement>('tr[data-site]');
  return tr ? Number(tr.dataset.site) : null;
};
$('hw-tbody').addEventListener('click', (e) => {
  const site = rowSite(e);
  if (site === null) return;
  select(selection?.kind === 'site' && selection.site === site ? null : { kind: 'site', site });
});
$('hw-tbody').addEventListener('mouseover', (e) => {
  const site = rowSite(e);
  if (site !== hoverSite) { hoverSite = site; schedule(); }
});
$('hw-tbody').addEventListener('mouseleave', () => { hoverSite = null; schedule(); });
table.addEventListener('keydown', (e) => {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
  e.preventDefault();
  const at = rowsShown.findIndex((r) => selection?.kind === 'site' && r.site === selection.site);
  const next = rowsShown[Math.max(0, Math.min(rowsShown.length - 1, at < 0 ? 0 : at + (e.key === 'ArrowDown' ? 1 : -1)))];
  if (!next) return;
  select({ kind: 'site', site: next.site });
  requestAnimationFrame(() => table.querySelector(`tr[data-site="${next.site}"]`)?.scrollIntoView({ block: 'nearest' }));
});
$<HTMLInputElement>('hw-filter').addEventListener('input', (e) => { filter = (e.target as HTMLInputElement).value; schedule(); });

/* The inspector: the selected site or block, or the run at a glance. */

function paintInspector(m: Moment) {
  const s = selection;
  let html: string;
  if (s?.kind === 'site') html = siteHtml(m, s.site);
  else if (s?.kind === 'block') html = blockHtml(m, s);
  else html = overviewHtml(m);
  // Rewritten only when something changed, so text selection and the open
  // allocator frames survive the snapshots in between.
  if (inspector.dataset.html !== html) {
    inspector.innerHTML = html;
    inspector.dataset.html = html;
  }
  const spark = inspector.querySelector<HTMLCanvasElement>('canvas.hi-spark');
  if (spark && s?.kind === 'site') drawSpark(spark, s.site, m);
}

const fact = (k: string, v: string, cls = '') => `<dt>${esc(k)}</dt><dd class="${cls}">${v}</dd>`;
const closeButton = `<button class="icon-button hi-close" type="button" aria-label="Close (Esc)" title="Close (Esc)"><svg class="icon"><use href="#i-x"/></svg></button>`;

function siteHtml(m: Moment, site: number) {
  const stat = statsOf(m.heaps).get(site), info = log.sites[site];
  const bytes = stat?.bytes ?? 0, blocks = stat?.blocks ?? 0;
  let html = `<div class="hi-head"><i class="sw" style="background:${siteColor(site)}"></i><h2 title="${esc(info?.fn ?? siteName(site))}">${esc(siteName(site))}</h2>${closeButton}</div>`;
  if (info) html += `<p class="hi-fn"><code>${esc(info.fn)}</code></p>${locHtml(info.loc)}`;
  html += `<section class="hi-sec"><div class="hi-row"><span>live bytes over the timeline</span><b class="num">${kib(bytes)}</b></div>` +
    `<canvas class="hi-spark" aria-label="Live bytes of this site over the timeline's span"></canvas><dl class="hi-facts num">`;
  html += fact('blocks', `${fmt(blocks)}${stat?.psram ? ` · ${fmt(stat.psram)} in PSRAM` : ''}`);
  if (stat) html += fact('largest block', `${fmt(stat.largest)} B`);
  const c = churnAt(m)?.get(site);
  html += fact(`allocations · ${CHURN / 1000} s`, c ? `${(c.allocs / (CHURN / 1000)).toFixed(1)}/s · ${kib(c.bytes / (CHURN / 1000))}/s · ${fmt(c.frees)} freed` : 'none');
  if (markA) {
    const a = siteStatsA()?.get(site);
    let fresh = 0, freshBytes = 0, freed = 0, freedBytes = 0;
    for (const h of m.heaps) {
      const d = diffOf(h)!;
      for (const n of d.fresh) if ((h.sites?.[n] ?? -1) === site) { fresh++; freshBytes += h.blocks[n * 4 + 1]; }
      const before = markA.heaps.find((x) => x.start === h.start);
      if (!before) continue;
      const is = blockKeys(h);
      for (const [key, n] of blockKeys(before)) {
        if (!is.has(key) && (before.sites?.[n] ?? -1) === site) { freed++; freedBytes += before.blocks[n * 4 + 1]; }
      }
    }
    html += fact(`at A · ${clock(markA.t)}`, `${kib(a?.bytes ?? 0)} · ${fmt(a?.blocks ?? 0)} blocks`);
    html += fact(`at B · ${cursor ? clock(m.t) : 'now'}`, `${kib(bytes)} · ${fmt(blocks)} blocks`);
    html += fact('change', delta(bytes - (a?.bytes ?? 0)), usage(bytes - (a?.bytes ?? 0)));
    html += fact('new since A', `${fmt(fresh)} blocks · ${kib(freshBytes)}`, 'fresh');
    html += fact('freed since A', `${fmt(freed)} blocks · ${kib(freedBytes)}`, 'freed');
  } else {
    const base = growthBase(m);
    if (base) {
      const g = bytes - (base.get(site)?.bytes ?? 0);
      html += fact('change · 1 min', delta(g), usage(g));
    }
  }
  const first = log.firstOf.get(site);
  if (first !== undefined) html += fact('first allocated', clock(log.t0 + log.t[first]));
  html += '</dl></section>';

  // The stack holding the most of the site's bytes.
  const ranked = stat ? [...stat.stacks].sort((a, b) => b[1] - a[1]) : [];
  if (ranked.length) {
    const stack = stacks.get(ranked[0][0], schedule);
    const label = ranked.length === 1 ? 'call stack' : `call stack · the one with most bytes of ${ranked.length}`;
    html += `<section class="hi-sec heap-detail hi-stack">${stack === 'pending' ? '<p class="hd-note">Looking up the call stack…</p>'
      : stack ? stackHtml(stack, allocOpen, label) : '<p class="hd-note">No call stack was recorded for these blocks.</p>'}</section>`;
  }

  // Its blocks: new since A when comparing, else the newest first.
  const list: { h: Heap; n: number; ev: number }[] = [];
  for (const h of m.heaps) {
    const d = markA ? diffOf(h) : null, b = h.blocks;
    for (let i = 0, n = 0; i < b.length; i += 4, n++) {
      if (b[i + 2] & FREE || (h.sites?.[n] ?? -1) !== site) continue;
      if (d && !d.fresh.has(n)) continue;
      list.push({ h, n, ev: h.ev?.[n] ?? -1 });
    }
  }
  list.sort((a, b) => b.ev - a.ev || a.h.start - b.h.start || a.n - b.n);
  html += `<section class="hi-sec"><div class="hi-row"><span>${markA ? 'blocks new since A' : 'blocks · newest first'}</span><span class="num">${fmt(list.length)}</span></div>`;
  html += list.length ? `<ol class="hi-blocks">${list.slice(0, BLOCKS_SHOWN).map(({ h, n, ev }) => {
    const off = h.blocks[n * 4], len = h.blocks[n * 4 + 1];
    const when = ev >= 0 ? ago(m.t - (log.t0 + log.t[ev])) : '';
    return `<li><button class="hi-block" type="button" data-start="${h.start}" data-n="${n}">` +
      `<span class="num">${hex(pointerOf(h.start, off, h.blocks[n * 4 + 2]))}</span><span class="num">${fmt(len - 4)} B${when ? ` · ${when}` : ''}</span></button></li>`;
  }).join('')}</ol>${list.length > BLOCKS_SHOWN ? `<p class="hi-more">and ${fmt(list.length - BLOCKS_SHOWN)} more</p>` : ''}`
    : `<p class="hi-more">${markA ? 'None: nothing of this site was allocated between A and B.' : 'None at this moment.'}</p>`;
  return html + '</section>';
}

function blockHtml(m: Moment, s: Extract<Selection, { kind: 'block' }>) {
  const used = !(s.fl & FREE), info = used && s.site >= 0 ? log.sites[s.site] : undefined;
  const title = used ? (info ? shortName(info.fn) : 'Allocated block') : 'Free block';
  const state = cursor ? (s.absent ? 'not there at this moment' : s.fl & CORRUPT ? 'corrupt' : 'allocated')
    : s.freedAt ? `freed ${ago(Date.now() - s.freedAt)} ago` : s.fl & CORRUPT ? 'corrupt' : 'allocated';
  const h = m.heaps.find((x) => x.start === s.start);
  let html = `<div class="hi-head"><i class="sw" style="background:${used ? (mode() === 'site' ? siteColor(s.site) : C().used) : C().free}"></i>` +
    `<h2 title="${esc(info?.fn ?? title)}">${esc(title)}</h2><span class="hd-state${(cursor ? s.absent : s.freedAt) || s.fl & CORRUPT ? ' bad' : ''}">${esc(state)}</span>${closeButton}</div>`;
  html += '<dl class="hi-facts num">';
  html += fact('address', `${hex(blockAddr(s.start, s.off))} – ${hex(blockAddr(s.start, s.off) + s.len)}`);
  html += fact('pointer', hex(pointerOf(s.start, s.off, s.fl)));
  html += fact('size', `${fmt(s.len - 4)} B${s.fl & POISONED ? ` · requested ${fmt(s.req)} B` : ''}`);
  if (h) html += fact('heap', `${esc(h.name)} at ${hex(h.start)}`);
  if (s.ev >= 0 && traced()) html += fact('allocated at', `${clock(log.t0 + log.t[s.ev])} · event ${fmt(s.ev + 1)}`);
  const n = h ? blockIndex(h, s) : -1;
  if (h && n >= 0 && !(cursor && s.absent)) {
    const age = blockAge(h, n, s.off, s.len);
    if (age) html += fact('age', age);
  }
  if (markA && h && n >= 0) html += fact('since A', diffOf(h)!.fresh.has(n) ? 'new' : 'there at A too', diffOf(h)!.fresh.has(n) ? 'fresh' : '');
  if (used && traced()) html += fact('site', `<button class="link" type="button" data-site="${s.site}">${esc(siteName(s.site))}</button>`);
  html += '</dl>';
  if (used && traced()) {
    const stack = stacks.get(s.stack, schedule);
    html += `<section class="hi-sec heap-detail hi-stack">${stack === 'pending' ? '<p class="hd-note">Looking up the call stack…</p>'
      : stack ? stackHtml(stack, allocOpen) : '<p class="hd-note">No call stack was recorded for this block.</p>'}</section>`;
  } else if (used) {
    html += `<p class="hd-note">${esc(!snap?.elf ? "Load the firmware's ELF to see where blocks were allocated." : snap.traceNote || 'Allocations are not being recorded, so there is no call stack.')}</p>`;
  }
  return html;
}
const blockIndex = (h: Heap, s: Extract<Selection, { kind: 'block' }>) => {
  const b = h.blocks;
  for (let i = 0, n = 0; i < b.length; i += 4, n++) {
    if (s.ev >= 0 ? (h.ev?.[n] ?? -1) === s.ev : b[i] === s.off && b[i + 1] === s.len) return n;
  }
  return -1;
};

function overviewHtml(m: Moment) {
  const heaps = m.heaps, internal = heaps.filter(isInternal), psram = heaps.filter((h) => !isInternal(h));
  const sum = (list: Heap[], f: (h: Heap) => number) => list.reduce((a, h) => a + f(h), 0);
  let html = `<div class="hi-head"><h2>${cursor ? `At ${clock(m.t)}` : 'Now'}</h2></div><dl class="hi-facts num">`;
  html += fact('internal free', `${kib(sum(internal, (h) => h.infoFree))} of ${kib(sum(internal, (h) => h.end - h.start))}`);
  if (!m.replayed) html += fact('lowest since boot', kib(sum(internal, (h) => h.infoMinFree)));
  html += fact('largest free block', kib(internal.reduce((a, h) => Math.max(a, h.largest), 0)));
  if (psram.length) html += fact('PSRAM free', `${kib(sum(psram, (h) => h.infoFree))} of ${kib(sum(psram, (h) => h.end - h.start))}`);
  html += fact('allocations', `${fmt(sum(heaps, (h) => h.count - h.nfree))} · ${fmt(sum(heaps, (h) => h.nfree))} free fragments`);
  const corrupt = sum(heaps, (h) => h.corrupt);
  if (corrupt) html += fact('corrupt canaries', fmt(corrupt), 'bad');
  if (traced()) {
    const c = churnAt(m);
    const allocs = c ? [...c.values()].reduce((a, x) => a + x.allocs, 0) : 0;
    html += fact(`allocations · ${CHURN / 1000} s`, `${(allocs / (CHURN / 1000)).toFixed(1)}/s`);
  }
  html += '</dl>';
  if (markA) {
    const a = markA.heaps.filter(isInternal), freeA = sum(a, (h) => h.infoFree), freeB = sum(internal, (h) => h.infoFree);
    const d = heaps.map(diffOf).filter((x): x is Diff => !!x);
    html += `<section class="hi-sec"><div class="hi-row"><span>A ${clock(markA.t)} → B ${cursor ? clock(m.t) : 'now'}</span></div><dl class="hi-facts num">`;
    html += fact('internal free', `${kib(freeA)} → ${kib(freeB)}`);
    html += fact('change', delta(freeB - freeA), usage(freeA - freeB));
    html += fact('new since A', `${fmt(d.reduce((x, y) => x + y.fresh.size, 0))} blocks · ${kib(d.reduce((x, y) => x + y.freshBytes, 0))}`, 'fresh');
    html += fact('freed since A', `${fmt(d.reduce((x, y) => x + y.freed.length, 0))} blocks · ${kib(d.reduce((x, y) => x + y.freedBytes, 0))}`, 'freed');
    html += '</dl></section>';
  }
  html += `<p class="hi-hint">${traced() ? 'Select a site in the list, or click a block on a map, to see its call stack and blocks.' : 'Click a block on a map to see its details.'}</p>`;
  html += `<p class="hi-hint">${markA ? 'The maps show what changed between A and B. Move the cursor on the timeline to move B; Shift-click to move A.'
    : 'To compare two moments, mark A (or Shift-click the timeline), then let the firmware run or move the cursor: that is B.'}</p>`;
  return html;
}

inspector.addEventListener('click', (e) => {
  const target = e.target as HTMLElement;
  if (target.closest('.hi-close')) { select(null); return; }
  const copy = target.closest<HTMLButtonElement>('.hd-copy');
  if (copy) {
    const id = selection?.kind === 'block' ? selection.stack : selection?.kind === 'site' ? topStack(selection.site) : -1;
    const stack = id >= 0 ? stacks.get(id) : null;
    if (stack && stack !== 'pending') void copyStack(copy, stack);
    return;
  }
  const site = target.closest<HTMLElement>('button[data-site]');
  if (site) { select({ kind: 'site', site: Number(site.dataset.site) }); return; }
  const block = target.closest<HTMLElement>('.hi-block');
  if (block) {
    const m = moment(), h = m?.heaps.find((x) => x.start === Number(block.dataset.start)), n = Number(block.dataset.n);
    if (!h) return;
    const b = h.blocks;
    select({ kind: 'block', start: h.start, off: b[n * 4], len: b[n * 4 + 1], fl: b[n * 4 + 2], req: b[n * 4 + 3],
      site: h.sites?.[n] ?? -1, stack: h.stk?.[n] ?? -1, ev: h.ev?.[n] ?? -1, freedAt: 0, absent: false });
  }
});
inspector.addEventListener('toggle', (e) => { if ((e.target as HTMLElement).matches('details')) allocOpen = (e.target as HTMLDetailsElement).open; }, true);
const topStack = (site: number) => {
  const m = moment(), stat = m ? statsOf(m.heaps).get(site) : undefined;
  return stat ? [...stat.stacks].sort((a, b) => b[1] - a[1])[0]?.[0] ?? -1 : -1;
};

/** A site's live bytes across the timeline's span, with the cursor and A. */
let sparkCache: { key: string; series: Float64Array } | null = null;
function drawSpark(canvas: HTMLCanvasElement, site: number, m: Moment) {
  const dpr = devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
  if (!w || !h || !traced()) return;
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  const ctx = canvas.getContext('2d')!;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const [a, b] = domain();
  const key = `${site}:${a}:${b}:${log.n}`;
  if (sparkCache?.key !== key) sparkCache = { key, series: log.siteSeries(site, a - log.t0, b - log.t0, SPARK_BINS) };
  const series = sparkCache.series, c = C();
  let lo = Infinity, hi = -Infinity;
  for (const v of series) { lo = Math.min(lo, v); hi = Math.max(hi, v); }
  if (hi - lo < 1024) { const mid = (hi + lo) / 2; lo = Math.max(0, mid - 512); hi = lo + 1024; }
  const x = (i: number) => 1 + i / (series.length - 1) * (w - 2), y = (v: number) => h - 3 - (v - lo) / (hi - lo) * (h - 6);
  const tx = (t: number) => 1 + (t - a) / (b - a || 1) * (w - 2);
  if (markA) {
    ctx.fillStyle = c.usedFill;
    const xa = Math.max(0, tx(markA.t)), xb = Math.min(w, tx(m.t));
    ctx.fillRect(xa, 0, Math.max(0, xb - xa), h);
  }
  ctx.strokeStyle = siteColor(site);
  ctx.lineWidth = 1.5;
  ctx.lineJoin = 'round';
  ctx.beginPath();
  series.forEach((v, i) => i ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v)));
  ctx.stroke();
  if (cursor) {
    ctx.fillStyle = c.text;
    ctx.fillRect(Math.round(tx(m.t)) - 0.5, 0, 1.5, h);
  }
}

/* The timeline ----------------------------------------------------------------------- */

/** The span shown, in ms since the epoch. */
function domain(): [number, number] {
  const end = liveEnd();
  const start = traced() ? log.t0 : history.length ? history[0].t * 1000 : end - 1000;
  if (!cursor || !range) tmEnd = end;
  if (!range) return [start, Math.max(end, start + 1000)];
  const b = Math.min(end, Math.max(tmEnd, start + range));
  return [Math.max(start, b - range), b];
}
let tl: { a: number; b: number; x: (t: number) => number; t: (px: number) => number } | null = null;

function drawTimeline(m: Moment | null) {
  const dpr = devicePixelRatio || 1, w = timeline.clientWidth, h = timeline.clientHeight;
  if (!w || !h) return;
  timeline.width = Math.round(w * dpr);
  timeline.height = Math.round(h * dpr);
  const ctx = timeline.getContext('2d')!;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  tl = null;
  if (!m || !snap) {
    $('hw-from').textContent = '';
    $('hw-to').textContent = '';
    $('hw-hint').textContent = '';
    return;
  }
  const c = C(), [a, b] = domain(), right = w - 4;
  const x = (t: number) => GUTTER + (t - a) / (b - a || 1) * (right - GUTTER);
  tl = { a, b, x, t: (px: number) => a + (px - GUTTER) / (right - GUTTER) * (b - a) };
  const logged = traced(), split = logged ? Math.round(h * 0.55) : h;
  ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, monospace';
  ctx.textAlign = 'right';

  // Internal RAM free, from the snapshots.
  const internal = new Set(snap.heaps.filter(isInternal).map((x) => x.start.toString(16)));
  const points = history.filter((p) => p.t * 1000 >= a - 1000 && p.t * 1000 <= b + 1000).flatMap((p) => {
    let free = 0, any = false;
    for (const [key, [f]] of Object.entries(p.h)) if (internal.has(key)) { free += f; any = true; }
    return any ? [{ t: p.t * 1000, free }] : [];
  });
  if (points.length > 1) {
    let lo = Infinity, hi = -Infinity;
    for (const p of points) { lo = Math.min(lo, p.free); hi = Math.max(hi, p.free); }
    const pad = Math.max((hi - lo) * 0.15, 512);
    lo = Math.max(0, lo - pad); hi += pad;
    const top = 6, bottom = split - 6;
    const y = (v: number) => bottom - (v - lo) / (hi - lo) * (bottom - top);
    const label = (v: number) => v >= 1048576 ? `${(v / 1048576).toFixed(1)}M` : `${Math.round(v / 1024)}K`;
    ctx.fillStyle = c.faint;
    ctx.strokeStyle = c.line;
    ctx.lineWidth = 1;
    for (const v of bottom - top < 36 ? [(hi + lo) / 2] : [hi, lo]) {
      ctx.fillText(label(v), GUTTER - 6, Math.min(bottom, Math.max(top + 7, y(v) + 3.5)));
      ctx.beginPath(); ctx.moveTo(GUTTER, Math.round(y(v)) + 0.5); ctx.lineTo(right, Math.round(y(v)) + 0.5); ctx.stroke();
    }
    ctx.save();
    ctx.beginPath();
    ctx.rect(GUTTER, 0, right - GUTTER, h);
    ctx.clip();
    ctx.beginPath();
    points.forEach((p, i) => i ? ctx.lineTo(x(p.t), y(p.free)) : ctx.moveTo(x(p.t), y(p.free)));
    ctx.lineTo(x(points[points.length - 1].t), bottom); ctx.lineTo(x(points[0].t), bottom); ctx.closePath();
    ctx.fillStyle = c.usedFill;
    ctx.fill();
    ctx.beginPath();
    points.forEach((p, i) => i ? ctx.lineTo(x(p.t), y(p.free)) : ctx.moveTo(x(p.t), y(p.free)));
    ctx.strokeStyle = c.used;
    ctx.lineWidth = 1.75;
    ctx.lineJoin = 'round';
    ctx.stroke();
    ctx.restore();
  } else {
    ctx.textAlign = 'left';
    ctx.fillStyle = c.faint;
    ctx.fillText('Collecting…', GUTTER, split / 2 + 4);
    ctx.textAlign = 'right';
  }

  // Allocations up and frees down, from the log, two pixels a column.
  if (logged) {
    const cols = Math.max(1, Math.floor((right - GUTTER) / 2)), dt = (b - a) / cols;
    const counts: [number, number][] = [];
    let peak = 1;
    for (let k = 0, i0 = log.countAt(a - log.t0); k < cols; k++) {
      const i1 = log.countAt(a + (k + 1) * dt - log.t0);
      const al = log.nalloc[i1] - log.nalloc[i0], fr = i1 - i0 - al;
      counts.push([al, fr]);
      peak = Math.max(peak, al, fr);
      i0 = i1;
    }
    const mid = Math.round(split + (h - split) / 2), room = (h - split) / 2 - 2;
    const bar = (n: number) => n ? Math.max(1.5, Math.sqrt(n / peak) * room) : 0;
    ctx.fillStyle = c.line;
    ctx.fillRect(GUTTER, mid, right - GUTTER, 1);
    counts.forEach(([al, fr], k) => {
      const px = GUTTER + k * 2;
      if (al) { ctx.fillStyle = c.fresh; const hh = bar(al); ctx.fillRect(px, mid - hh, 1.5, hh); }
      if (fr) { ctx.fillStyle = c.freed; ctx.fillRect(px, mid + 1, 1.5, bar(fr)); }
    });
  }

  // Looking back: the time after the cursor dimmed; A and the span to B tinted.
  const tag = (px: number, text: string, color: string) => {
    ctx.fillStyle = color;
    ctx.fillRect(Math.round(px) - 0.75, 0, 1.5, h);
    const tw = 14, tx = Math.min(right - tw, Math.max(GUTTER, px - tw / 2));
    ctx.fillRect(tx, 0, tw, 13);
    ctx.fillStyle = c.bg;
    ctx.textAlign = 'center';
    ctx.font = '600 10px system-ui, sans-serif';
    ctx.fillText(text, tx + tw / 2, 10);
    ctx.textAlign = 'right';
    ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, monospace';
  };
  const cx = x(Math.min(b, m.t));
  if (markA) {
    const xa = Math.max(GUTTER, Math.min(right, x(markA.t)));
    ctx.fillStyle = c.usedFill;
    ctx.fillRect(Math.min(xa, cx), 0, Math.abs(cx - xa), h);
    if (markA.t >= a && markA.t <= b) tag(xa, 'A', c.text);
    else { ctx.fillStyle = c.faint; ctx.textAlign = markA.t < a ? 'left' : 'right'; ctx.fillText(markA.t < a ? '◂ A' : 'A ▸', markA.t < a ? GUTTER + 2 : right - 2, 10); ctx.textAlign = 'right'; }
  }
  if (cursor) {
    ctx.fillStyle = c.bg;
    ctx.globalAlpha = 0.55;
    ctx.fillRect(cx, 0, right - cx, h);
    ctx.globalAlpha = 1;
    if (markA) tag(cx, 'B', c.text);
    else { ctx.fillStyle = c.text; ctx.fillRect(Math.round(cx) - 0.75, 0, 1.5, h); }
  } else if (markA) {
    tag(Math.min(cx, right - 1), 'B', c.used);
  }

  const end = liveEnd();
  $('hw-from').textContent = a <= (logged ? log.t0 : (history[0]?.t ?? 0) * 1000) + 1 ? `${clock(a)} · ${logged ? 'first event' : 'first snapshot kept'}` : clock(a);
  $('hw-to').textContent = b >= end - 1 ? 'now' : clock(b);
  $('hw-hint').textContent = cursor ? 'Drag to move · Shift-click marks A · Esc returns to live' : 'Click or drag to look back · Shift-click marks A';
}

let scrubbing = false;
const timeAt = (e: MouseEvent) => {
  if (!tl) return null;
  const px = e.clientX - timeline.getBoundingClientRect().left;
  // The last few pixels are live.
  if (px >= timeline.clientWidth - 8) return Infinity;
  return tl.t(Math.max(GUTTER, px));
};
timeline.addEventListener('pointerdown', (e) => {
  const t = timeAt(e);
  if (t === null || e.button !== 0) return;
  if (e.shiftKey) { void markAt(t === Infinity ? undefined : t); return; }
  stopPlay();
  scrubbing = true;
  timeline.setPointerCapture(e.pointerId);
  if (t === Infinity) goLive(); else setCursor(t);
});
timeline.addEventListener('pointermove', (e) => {
  if (scrubbing) {
    const t = timeAt(e);
    if (t === Infinity) goLive(); else if (t !== null) setCursor(t);
    return;
  }
  timelineTip(e);
});
for (const type of ['pointerup', 'pointercancel'] as const) timeline.addEventListener(type, () => { scrubbing = false; });
timeline.addEventListener('mouseleave', () => { tipBox.hidden = true; });
timeline.addEventListener('keydown', (e) => {
  const to: Record<string, () => void> = {
    ArrowLeft: () => step(-1, e.shiftKey), ArrowRight: () => step(1, e.shiftKey),
    Home: () => { const [a] = domain(); setCursor(a); }, End: goLive,
  };
  if (!to[e.key]) return;
  e.preventDefault();
  e.stopPropagation();
  to[e.key]();
});

function timelineTip(e: MouseEvent) {
  const t = timeAt(e);
  if (t === null || !snap || !tl) { tipBox.hidden = true; return; }
  const at = t === Infinity ? tl.b : t, rows: TipRow[] = [['time', `${clock(at)} · ${ago(liveEnd() - at)} before now`]];
  const internal = new Set(snap.heaps.filter(isInternal).map((h) => h.start.toString(16)));
  if (history.length) {
    const p = history[nearest(at / 1000)];
    let free = 0, largest = 0;
    for (const [key, [f, l]] of Object.entries(p.h)) if (internal.has(key)) { free += f; largest = Math.max(largest, l); }
    rows.push(['internal free', kib(free)], ['largest block', kib(largest)]);
  }
  if (traced()) {
    const px = 2 * (tl.b - tl.a) / Math.max(1, timeline.clientWidth - GUTTER - 4);
    const i0 = log.countAt(at - px / 2 - log.t0), i1 = log.countAt(at + px / 2 - log.t0);
    const al = log.nalloc[i1] - log.nalloc[i0];
    rows.push(['allocations', fmt(al)], ['frees', fmt(i1 - i0 - al)]);
  }
  rows.push(['', t === Infinity ? 'Click to go back to live' : 'Click or drag to see the heaps then · Shift-click marks A']);
  showTip(tipBox, e, rows);
}

/* Controls -------------------------------------------------------------------------- */

const scaleSelect = $<HTMLSelectElement>('hw-scale');
scaleSelect.innerHTML = `<option value="fit">Fit</option>` + SCALES.map((b) => `<option value="${b}">${per(b)}/row</option>`).join('');
function setScale(next: number | 'fit') {
  scale = next;
  save('eink-qemu-heapwin-scale', next);
  schedule();
}
scaleSelect.addEventListener('change', () => setScale(scaleSelect.value === 'fit' ? 'fit' : Number(scaleSelect.value)));
const scaleStep = (dir: number) => {
  const at = scale === 'fit' ? SCALES.indexOf(16384) : SCALES.indexOf(scale);
  setScale(SCALES[Math.max(0, Math.min(SCALES.length - 1, at + dir))]);
};
$('hw-scale-in').addEventListener('click', () => scaleStep(-1));
$('hw-scale-out').addEventListener('click', () => scaleStep(1));

$('hw-colorby').addEventListener('click', (e) => {
  const b = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-color]');
  if (!b || b.disabled) return;
  colorBy = b.dataset.color as ColorBy;
  if (colorBy !== 'change') save('eink-qemu-heapwin-color', colorBy);
  schedule();
});
$('hw-pause').addEventListener('click', () => {
  paused = !paused;
  if (paused) clearTimeout(timer); else plan(0);
  schedule();
});
$('hw-play').addEventListener('click', () => { if (playing) stopPlay(); else play(); });
$('hw-prev').addEventListener('click', () => step(-1));
$('hw-next').addEventListener('click', () => step(1));
$('hw-golive').addEventListener('click', goLive);
$('hw-mark').addEventListener('click', () => void markAt());
$('hw-mark-clear').addEventListener('click', clearMark);
$('hw-speed').addEventListener('click', (e) => {
  const b = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-speed]');
  if (!b) return;
  speed = Number(b.dataset.speed);
  save('eink-qemu-heapwin-speed', speed);
  schedule();
});
$('hw-range').addEventListener('click', (e) => {
  const b = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-range]');
  if (!b) return;
  range = Number(b.dataset.range);
  save('eink-qemu-heapwin-range', range);
  tmEnd = cursor ? Math.min(liveEnd(), cursor.t + range / 2) : liveEnd();
  schedule();
});

addEventListener('keydown', (e) => {
  const target = e.target as HTMLElement;
  if (target.closest('input, textarea, select')) {
    if (e.key === 'Escape' && target.id === 'hw-filter' && filter) {
      filter = '';
      (target as HTMLInputElement).value = '';
      schedule();
    }
    return;
  }
  if (e.key === 'Escape') {
    if (selection) select(null);
    else if (cursor) goLive();
    else if (markA) clearMark();
    else return;
    e.preventDefault();
  } else if ((e.key === 'ArrowLeft' || e.key === 'ArrowRight') && !target.closest('#hw-table')) {
    e.preventDefault();
    step(e.key === 'ArrowLeft' ? -1 : 1, e.shiftKey);
  } else if (e.key === ' ' && cursor && !target.closest('button')) {
    e.preventDefault();
    if (playing) stopPlay(); else play();
  }
});

new ResizeObserver(() => schedule()).observe(mapsBox);
new ResizeObserver(() => schedule()).observe(timeline);
inspector.addEventListener('mouseleave', () => { tipBox.hidden = true; });

void connect().catch((error) => { problem = errorOf(error); schedule(); });
schedule();
