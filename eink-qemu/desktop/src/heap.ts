/* Heap view: the running firmware's heaps (core/src/heap.rs), polled
   while the Heap tab is open. A free-memory trend for internal RAM, then each
   heap as a map wrapped into rows of a chosen size, every block drawn to
   scale and coloured by the function that allocated it; a card folds to a
   single bar, and a clicked block opens its details and call stack. Without
   the ELF the maps are drawn alone. Scrubbing the trend shows the heaps at
   that moment (Rust keeps a snapshot per trend point) until Live. */
import type { FileInfo, Heap, HeapHistoryPoint, HeapPast, HeapSite, HeapSnapshot, HeapStack } from './types';
import {
  CORRUPT, FADE, FREE, GUTTER, POISONED, SCALES, SITE_COLORS, Stacks, ago, blockAddr, byKind, callName, copyStack,
  errorOf, esc, fmt, fragmented, hex, hitMap, isInternal, kib, load, onTheme, paintMap, per, pointerOf, readColors, role,
  rowBytes, save, shortName, showTip, splitLoc, stackHtml, type Colors, type TipRow,
} from './heap-core';

/** Where snapshots and the ELF come from: Rust, or the demo's mock. */
export interface HeapSource {
  snapshot(): Promise<HeapSnapshot>;
  /** The kept snapshot nearest `time` (seconds) and the one before it. */
  at(time: number): Promise<HeapPast | null>;
  history(): Promise<HeapHistoryPoint[]>;
  stack(id: number): Promise<HeapStack | null>;
  sites(from: number): Promise<HeapSite[]>;
  pickElf(): Promise<FileInfo | null>;
  openWindow(): Promise<void>;
  /** The ELF was chosen here, from the Heap tab's message. */
  elfChosen?(info: FileInfo): void;
}

const INTERVAL = 1000;       // ms between snapshots
const HISTORY = 900;         // trend points kept (15 minutes at one a second)

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const output = document.querySelector<HTMLElement>('.output')!;
const pane = $('heap');
const tipBox = $('heap-tip');

interface View {
  el: HTMLElement;
  canvas: HTMLCanvasElement;
  detail: HTMLElement;
  heap: Heap;
  /** When each allocation (offset:length) was first seen; 0 if before the view. */
  born: Map<string, number>;
  freed: { off: number; len: number; t: number }[];
  first: boolean;
  /** Looking back: what changed since the kept snapshot before, drawn steady. */
  mark: { fresh: Set<string>; freed: { off: number; len: number }[] } | null;
  /** The map as last drawn: bytes per row, its width in css px, folded or not. */
  bpr: number;
  width: number;
  folded: boolean;
}

/** The clicked block, kept after it is freed so its details stay readable. */
interface Selected {
  start: number; off: number; len: number; fl: number; req: number;
  site: number; stack: number; born: number; freedAt: number;
  /** Looking back: the block is not there at the moment shown. */
  absent?: boolean;
}

let source: HeapSource | null = null;
let elf: FileInfo | null = null;
let running = false, run = -1;
let visible = false, paused = false, inflight = false;
let timer = 0, frame = 0;
let snap: HeapSnapshot | null = null;
/** Looking back: the kept snapshot shown instead of the live one. */
let past: { snap: HeapSnapshot; prev: HeapSnapshot | null } | null = null;
/** The moment asked for while a kept snapshot is fetched; `pastGen` drops answers after Live. */
let wanted: number | null = null, fetching = false, pastGen = 0, scrubbing = false;
/** The snapshot the maps show: a kept one when looking back, else the live one. */
const shown = () => past?.snap ?? snap;
let problem: { code: string; message: string } | null = null;
const views = new Map<number, View>();
/** Symbolized stacks by id, fetched once. */
const stacks = new Stacks((id) => source ? source.stack(id) : Promise.resolve(null));
/** Allocation site names, indexed by a block's site id. */
let sites: HeapSite[] = [];
let sitesPending = false;
/** Free-memory history; fetched again from Rust when the tab was away. */
let history: HeapHistoryPoint[] = [];
let historyStale = true;
let selected: Selected | null = null;
/** The details' allocator frames, shown or not. */
let allocOpen = false;
/** Bytes per map row, or 'fit' to give every heap about the same height. */
let scale = load<number | 'fit'>('eink-qemu-heap-scale', 8192);
/** Heaps folded or unfolded by hand, by start address; others fold while empty. */
const folds: Record<string, boolean> = load('eink-qemu-heap-folds', {});

/* Tabs ---------------------------------------------------------------- */

const tabs = [...output.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
let available = true;
/** Without native QEMU behind the page there are no heaps: the tab goes away. */
export function setHeapAvailable(on: boolean) {
  available = on;
  $('tab-heap').hidden = !on;
  if (!on && visible) showPane('console');
}
export function showPane(name: 'console' | 'heap') {
  if (name === 'heap' && !available) name = 'console';
  output.dataset.pane = name;
  for (const tab of tabs) {
    const on = tab.dataset.pane === name;
    tab.setAttribute('aria-selected', String(on));
    tab.tabIndex = on ? 0 : -1;
  }
  $('console').hidden = name !== 'console';
  pane.hidden = name !== 'heap';
  try { localStorage.setItem('eink-qemu-pane', name); } catch {}
  if (name === 'heap' && !visible) historyStale = true;
  visible = name === 'heap';
  if (!visible) $('heap-tip').hidden = true;
  if (name === 'console') $('console').scrollTop = $('console').scrollHeight;
  refresh();
}
for (const tab of tabs) tab.addEventListener('click', () => showPane(tab.dataset.pane as 'console' | 'heap'));

/* State from the app -------------------------------------------------------- */

export function attachHeap(from: HeapSource, info: FileInfo | null) {
  source = from;
  setElf(info);
}

/** The session's state: a new run starts the view afresh. */
export function heapSession(state: string, runId: number) {
  if (runId !== run) {
    run = runId;
    snap = null;
    stacks.clear();
    sites = [];
    history = [];
    historyStale = true;
    past = null;
    wanted = null;
    pastGen++;
    selected = null;
    problem = null;
    for (const v of views.values()) v.el.remove();
    views.clear();
  }
  running = state === 'running';
  refresh();
}

/** The ELF changed (the firmware card chose, found or forgot one). */
export function setElf(info: FileInfo | null) {
  if (info && elf && info.path === elf.path && info.size === elf.size) return;
  elf = info;
  problem = null;
  snap = null;
  past = null;
  wanted = null;
  pastGen++;
  sites = [];
  selected = null;
  refresh();
}

/** Use the ELF a dialog or a drop chose; shows the Heap tab. */
export async function useElf(choice: Promise<FileInfo | null>) {
  try {
    const info = await choice;
    if (!info) return;
    setElf(info);
    source?.elfChosen?.(info);
    showPane('heap');
  } catch (error) {
    problem = errorOf(error);
    showPane('heap');
  }
}

$('heap-pause').addEventListener('click', () => {
  paused = !paused;
  const button = $('heap-pause');
  button.setAttribute('aria-pressed', String(paused));
  const label = paused ? 'Resume updates' : 'Pause updates';
  button.setAttribute('aria-label', label);
  button.title = label;
  button.querySelector('use')!.setAttribute('href', paused ? '#i-play' : '#i-pause');
  refresh();
});
$('heap-open').addEventListener('click', () => { void source?.openWindow().catch((error) => { problem = errorOf(error); refresh(); }); });
$('heap-message-action').addEventListener('click', () => { if (source) void useElf(source.pickElf()); });
pane.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (selected) select(null);
  else if (past) goLive();
  else return;
  e.stopPropagation();
});
$('heap-note-action').addEventListener('click', () => { if (source) void useElf(source.pickElf()); });

/* The scale: bytes per map row ------------------------------------------- */

const scaleSelect = $<HTMLSelectElement>('heap-scale');
scaleSelect.innerHTML = `<option value="fit">Fit</option>` +
  SCALES.map((b) => `<option value="${b}">${per(b)}/row</option>`).join('');
function setScale(next: number | 'fit') {
  scale = next;
  scaleSelect.value = String(next);
  save('eink-qemu-heap-scale', next);
  paint();
}
if (scale !== 'fit' && !SCALES.includes(scale)) scale = 8192;
scaleSelect.value = String(scale);
scaleSelect.addEventListener('change', () => setScale(scaleSelect.value === 'fit' ? 'fit' : Number(scaleSelect.value)));
const step = (dir: number) => {
  const at = scale === 'fit' ? SCALES.indexOf(8192) : SCALES.indexOf(scale);
  setScale(SCALES[Math.max(0, Math.min(SCALES.length - 1, at + dir))]);
};
$('heap-scale-in').addEventListener('click', () => step(-1));
$('heap-scale-out').addEventListener('click', () => step(1));

/** A heap's bytes per row at the chosen scale, and whether its size forced a coarser one. */
const bytesPerRow = (size: number) => rowBytes(size, scale);

/* Polling ------------------------------------------------------------------- */

function refresh() {
  clearTimeout(timer);
  paint();
  if (visible && !paused && running && source && !inflight) timer = window.setTimeout(poll, snap ? INTERVAL : 0);
}

async function poll() {
  if (!source || inflight) return;
  inflight = true;
  const asked = run;
  try {
    const next = await source.snapshot();
    if (asked === run) {
      snap = next;
      problem = null;
      void fetchSites(next);
      record(next);
      // Ages and highlights follow the live heaps, also while looking back.
      const now = Date.now();
      for (const h of next.heaps) track(view(h), h, now);
    }
  } catch (error) {
    const e = errorOf(error);
    // Busy: an SD operation has the emulator; idle: the session event follows.
    if (asked === run && e.code !== 'EBUSY' && e.code !== 'EIDLE') problem = e;
  } finally {
    inflight = false;
    refresh();
  }
}

/** Fetch the names of sites the snapshot uses but the view has not seen. */
async function fetchSites(s: HeapSnapshot) {
  if (!source || sitesPending || !s.traced) return;
  let top = -1;
  for (const h of s.heaps) for (const id of h.sites ?? []) if (id > top) top = id;
  if (top < sites.length) return;
  sitesPending = true;
  const asked = run;
  try {
    const more = await source.sites(sites.length);
    if (asked === run) { sites = sites.concat(more); paint(); }
  } catch {
    // The next snapshot asks again.
  } finally {
    sitesPending = false;
  }
}

/** Add a snapshot to the trend; Rust's history fills in while the tab was away. */
function record(s: HeapSnapshot) {
  if (historyStale && source) {
    historyStale = false;
    const asked = run;
    source.history().then((points) => { if (asked === run && points.length) { history = points; paint(); } })
      .catch(() => { historyStale = true; });
  }
  if (history.length && history[history.length - 1].t >= s.time) return;
  history.push({ t: s.time, h: Object.fromEntries(s.heaps.map((h) => [h.start.toString(16), [h.infoFree, h.largest, h.infoMinFree]])) });
  if (history.length > HISTORY) history.splice(0, history.length - HISTORY);
}

/* Painting ------------------------------------------------------------------ */

function message(title: string, text: string, action?: string) {
  $('heap-message-title').textContent = title;
  $('heap-message-text').textContent = text;
  const button = $<HTMLButtonElement>('heap-message-action');
  button.hidden = !action;
  if (action) button.textContent = action;
  $('heap-message').hidden = false;
}

function paint() {
  $('heap-message').hidden = true;
  const showMaps = !!(running && snap?.heaps.length && !problem);
  if (problem?.code === 'EMISMATCH') {
    message('This ELF is not the running firmware', problem.message, 'Choose another ELF…');
  } else if (problem?.code === 'EINVAL' || problem?.code === 'ENOENT') {
    message('Cannot use the ELF', problem.message, 'Choose another ELF…');
  } else if (problem) {
    message('Cannot read the heaps', problem.message);
  } else if (!running) {
    message('The emulator is not running', 'Start the firmware to see its heaps. While it runs, the view pauses ' +
      'the emulator for a moment each second to read them.');
  } else if (!snap) {
    message('Reading the heaps…', '');
  } else if (!snap.heaps.length) {
    message('Waiting for the firmware', snap.missing
      ? 'The heaps were changing in every read; trying again.'
      : 'No heaps are registered yet; the firmware sets them up early in boot.');
  }
  for (const id of ['heap-live', 'heap-legend', 'heap-note', 'heap-bar', 'heap-trend', 'heap-list']) $(id).hidden = !showMaps;
  if (!showMaps || !snap) { $('heap-tip').hidden = true; $('heap-note').hidden = true; return; }
  const s = shown()!;

  $('heap-live').classList.toggle('paused', paused && !past);
  $('heap-live').classList.toggle('past', !!past);
  $('heap-golive').hidden = !past;
  const when = new Date(s.time * 1000).toLocaleTimeString();
  if (past) {
    const back = snap.time - s.time;
    $('heap-state').textContent = `Looking back · ${when} · ${back < 0.5 ? 'now' : `${ago(back * 1000)} before the latest`}`;
  } else {
    const took = s.took === undefined ? '' : ` · read in ${Math.round(s.took)} ms`;
    const skipped = s.missing ? ` · ${s.missing} heap${s.missing > 1 ? 's' : ''} changing, skipped` : '';
    $('heap-state').textContent = `${paused ? 'Paused' : 'Live'} · ${when}${took}${skipped}`;
  }
  // Without the ELF the maps are drawn alone; the note says what it adds.
  const noElf = !s.elf;
  $('heap-note-text').textContent = noElf
    ? `${s.elfNote ? `${s.elfNote} ` : ''}Without the firmware's ELF, blocks are not coloured by allocation site and have no call stack.`
    : s.traced ? '' : s.traceNote ?? '';
  const action = $<HTMLButtonElement>('heap-note-action');
  action.hidden = !noElf;
  action.textContent = s.elfNote ? 'Choose another ELF…' : 'Load ELF…';
  $('heap-note').hidden = !$('heap-note-text').textContent;

  // Internal memory first, then PSRAM; by address within each.
  const heaps = [...s.heaps].sort(byKind);
  const internal = heaps.filter(isInternal), psram = heaps.filter((h) => !isInternal(h));
  const sum = (list: Heap[], f: (h: Heap) => number) => list.reduce((a, h) => a + f(h), 0);
  const max = (list: Heap[], f: (h: Heap) => number) => list.reduce((a, h) => Math.max(a, f(h)), 0);
  const corrupt = sum(heaps, (h) => h.corrupt);
  const parts = [
    `Internal free <b>${kib(sum(internal, (h) => h.infoFree))}</b> of ${kib(sum(internal, (h) => h.end - h.start))}`,
    `lowest <b>${kib(sum(internal, (h) => h.infoMinFree))}</b>`,
    `largest block <b>${kib(max(internal, (h) => h.largest))}</b>`,
  ];
  if (psram.length) parts.push(`PSRAM free <b>${kib(sum(psram, (h) => h.infoFree))}</b>`);
  if (corrupt) parts.push(`<span class="bad"><b>${fmt(corrupt)}</b> corrupt</span>`);
  $('heap-summary').innerHTML = parts.map((p) => `<span>${p}</span>`).join('');
  $('heap-summary').title = 'Lowest: the least free memory since boot. Largest block: TLSF size, the biggest single allocation that can succeed.';
  drawTrend();

  const mainStart = heaps.find((h) => h.name === 'DRAM')?.start ?? -1;
  const list = $('heap-list'), seen = new Set<number>(), now = Date.now();
  // Move a card only when it is out of place: moving one re-anchors the
  // pane's scroll position, which would jump on every snapshot.
  let at = list.firstElementChild;
  for (const h of heaps) {
    const v = view(h);
    seen.add(h.start);
    v.heap = h;
    v.mark = past ? changes(past.prev?.heaps.find((p) => p.start === h.start), h) : null;
    if (at === v.el) at = at.nextElementSibling;
    else list.insertBefore(v.el, at);
    const size = h.end - h.start, folded = folds[hex(h.start)] ?? h.used === 0;
    v.folded = folded;
    v.el.classList.toggle('folded', folded);
    const head = v.el.querySelector('.heap-head')!;
    head.setAttribute('aria-expanded', String(!folded));
    head.setAttribute('title', `${hex(h.start)}–${hex(h.end)}\n${h.caps.join(' ')}\n${folded ? 'Click to show the map' : 'Click to fold to a bar'}`);
    v.el.querySelector('.heap-name')!.textContent = h.name;
    v.el.querySelector('.heap-role')!.textContent = role(h, mainStart);
    v.el.querySelector('.heap-addr')!.textContent = hex(h.start);
    v.el.querySelector('.heap-used')!.innerHTML = h.used ? `<b>${kib(h.used)}</b> / ${kib(size)}` : `empty · ${kib(size)}`;
    const frag = fragmented(h);
    const { bpr, capped } = bytesPerRow(size);
    v.el.querySelector('.heap-stats')!.innerHTML = ([
      ['free', kib(h.infoFree)], ['largest', kib(h.largest)], ['lowest', kib(h.infoMinFree)],
      ['blocks', `${fmt(h.count)} · ${fmt(h.nfree)} free`], ['fragmented', `${frag}%`],
    ] as const).map(([k, val]) => `<span><span>${k}</span> <b>${val}</b></span>`).join('')
      + (capped ? `<span><span>scale</span> <b>${per(bpr)}/row</b></span>` : '')
      + (h.corrupt ? `<span class="bad"><b>${fmt(h.corrupt)} corrupt</b></span>` : '');
    if (selected?.start === h.start) follow(h, now);
    draw(v);
    details(v);
  }
  for (const [start, v] of views) {
    if (!seen.has(start)) { v.el.remove(); views.delete(start); }
  }
  if (selected && !seen.has(selected.start)) selected = null;
  legend(heaps);
  if (!past) animate();
}

/** Allocations new since `before` and those gone, for a kept snapshot. */
function changes(before: Heap | undefined, h: Heap): NonNullable<View['mark']> {
  const used = (heap: Heap) => {
    const keys = new Set<string>(), b = heap.blocks;
    for (let i = 0; i < b.length; i += 4) if (!(b[i + 2] & FREE)) keys.add(`${b[i]}:${b[i + 1]}`);
    return keys;
  };
  if (!before) return { fresh: new Set(), freed: [] };
  const was = used(before), is = used(h);
  return {
    fresh: new Set([...is].filter((k) => !was.has(k))),
    freed: [...was].filter((k) => !is.has(k)).map((k) => { const [off, len] = k.split(':').map(Number); return { off, len }; }),
  };
}

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
    <div class="heap-map"><div class="heap-gutter num"></div><canvas></canvas></div>
    <div class="heap-detail" role="region" aria-label="Block details" hidden></div>`;
  const canvas = el.querySelector('canvas')!, detail = el.querySelector<HTMLElement>('.heap-detail')!;
  v = { el, canvas, detail, heap: h, born: new Map(), freed: [], mark: null, first: true, bpr: 1, width: 1, folded: false };
  const self = v;
  el.querySelector('.heap-head')!.addEventListener('click', () => {
    folds[hex(self.heap.start)] = !self.folded;
    save('eink-qemu-heap-folds', folds);
    paint();
  });
  canvas.addEventListener('mousemove', (e) => tip(self, e));
  canvas.addEventListener('mouseleave', () => { $('heap-tip').hidden = true; });
  canvas.addEventListener('click', (e) => {
    const x = hit(self, e);
    if (!x || x.kind !== 'block') return;
    const same = selected?.start === self.heap.start && selected.off === x.off && !(past ? selected.absent : selected.freedAt);
    select(same ? null : { start: self.heap.start, off: x.off, len: x.len, fl: x.fl, req: x.req, site: x.site, stack: x.stack, born: x.born ?? 0, freedAt: 0 });
  });
  detail.addEventListener('click', (e) => {
    const target = e.target as HTMLElement;
    if (target.closest('.hd-close')) select(null);
    else if (target.closest('.hd-copy')) copySelected(target.closest<HTMLButtonElement>('.hd-copy')!);
  });
  detail.addEventListener('toggle', (e) => { if ((e.target as HTMLElement).matches('details')) allocOpen = (e.target as HTMLDetailsElement).open; }, true);
  views.set(h.start, v);
  return v;
}

function select(next: Selected | null) {
  selected = next;
  $('heap-tip').hidden = true;
  paint();
  if (next) views.get(next.start)?.detail.scrollIntoView({ block: 'nearest' });
}

/** Keep the selected block's facts current, or note when it was freed; when
    looking back, whether it is there at the moment shown. */
function follow(h: Heap, now: number) {
  const s = selected!, b = h.blocks;
  if (past) {
    s.absent = true;
    for (let i = 0, n = 0; i < b.length; i += 4, n++) {
      if (b[i] !== s.off || b[i + 1] !== s.len || !(b[i + 2] & FREE) !== !(s.fl & FREE)) continue;
      Object.assign(s, { absent: false, fl: b[i + 2], req: b[i + 3], site: siteOf(h, n), stack: h.stk?.[n] ?? s.stack });
      break;
    }
    return;
  }
  s.absent = false;
  if (s.freedAt) return;
  for (let i = 0, n = 0; i < b.length; i += 4, n++) {
    if (b[i] !== s.off) continue;
    if (b[i + 2] & FREE ? s.fl & FREE : b[i + 1] === s.len) {
      Object.assign(s, { fl: b[i + 2], req: b[i + 3], site: siteOf(h, n), stack: h.stk?.[n] ?? s.stack });
      return;
    }
    break;
  }
  if (s.fl & FREE) selected = null; // a free block merged into another: nothing to keep
  else s.freedAt = now;
}

/** Note when each allocation first appeared and which blocks were just freed. */
function track(v: View, h: Heap, now: number) {
  const born = new Map<string, number>(), b = h.blocks;
  for (let i = 0; i < b.length; i += 4) {
    if (b[i + 2] & FREE) continue;
    const key = `${b[i]}:${b[i + 1]}`;
    born.set(key, v.first ? 0 : (v.born.get(key) ?? now));
  }
  if (!v.first) {
    for (const key of v.born.keys()) {
      if (born.has(key)) continue;
      const [off, len] = key.split(':').map(Number);
      v.freed.push({ off, len, t: now });
    }
  }
  v.freed = v.freed.filter((f) => now - f.t < FADE);
  v.born = born;
  v.first = false;
}

/* Colors come from the stylesheet's tokens, read again when the theme changes. */
let colors: Colors | null = null;
const readTheme = () => { colors = readColors(pane); };
onTheme(() => {
  colors = null;
  if (snap) paint();
});

/** A block's site id, or -1 without an allocation log. */
const siteOf = (h: Heap, i: number) => shown()?.traced ? h.sites?.[i] ?? -1 : -1;
const siteColor = (site: number) => site >= 0 ? colors!.sites[site % SITE_COLORS] : colors!.used;

function draw(v: View) {
  if (!colors) readTheme();
  const C = colors!, h = v.heap, size = h.end - h.start, now = Date.now(), b = h.blocks;
  const map = v.el.querySelector<HTMLElement>('.heap-map')!, gutter = v.el.querySelector<HTMLElement>('.heap-gutter')!;
  v.width = Math.max(60, map.clientWidth - (v.folded ? 0 : GUTTER));
  v.bpr = v.folded ? size : bytesPerRow(size).bpr;
  // Live, highlights fade; looking back, what changed since the kept
  // snapshot before stays lit.
  const glow = (n: number) => {
    const key = `${b[n * 4]}:${b[n * 4 + 1]}`;
    if (v.mark) return v.mark.fresh.has(key) ? 0.8 : 0;
    const t = v.born.get(key);
    return t ? 1 - (now - t) / FADE : 0;
  };
  const freed = v.mark ? v.mark.freed.map((m) => ({ ...m, w: 0.8 })) : v.freed.map((f) => ({ off: f.off, len: f.len, w: 1 - (now - f.t) / FADE }));
  // The selected block: outlined, in the text colour so it shows on any fill.
  const outline = selected?.start === h.start && !(past ? selected.absent : selected.freedAt) ? selected : null;
  paintMap(v.canvas, gutter, h, v, C, {
    color: (n) => siteColor(siteOf(h, n)),
    alt: (n) => { const site = siteOf(h, n); return site >= 0 ? C.sitesAlt[site % SITE_COLORS] : C.usedAlt; },
    glow, freed, outline,
  });
}

/** The sites holding the most memory, with their colours. */
function legend(heaps: Heap[]) {
  const box = $('heap-sites'), named = !!(shown()?.traced && sites.length);
  $('heap-legend-used').hidden = named;
  if (!named) { box.innerHTML = ''; return; }
  const bytes = new Map<number, number>();
  for (const h of heaps) {
    const b = h.blocks;
    for (let i = 0, n = 0; i < b.length; i += 4, n++) {
      const site = siteOf(h, n);
      if (site >= 0 && !(b[i + 2] & FREE)) bytes.set(site, (bytes.get(site) ?? 0) + b[i + 1]);
    }
  }
  const top = [...bytes].sort((a, b) => b[1] - a[1]).slice(0, 6);
  box.innerHTML = top.map(([site, n]) => {
    const name = sites[site] ? shortName(sites[site].fn) : `site ${site}`;
    return `<span title="${esc(sites[site]?.fn ?? '')}"><i class="sw" style="background:${siteColor(site)}"></i><span class="heap-site-name">${esc(name)}</span> <b class="num">${kib(n)}</b></span>`;
  }).join('');
}

/* The trend: internal RAM's free memory, largest free block and lowest free. */

interface TrendPoint { t: number; free: number; largest: number; low: number }
function trendPoints(): TrendPoint[] {
  const internal = new Set(snap!.heaps.filter((h) => h.name !== 'PSRAM').map((h) => h.start.toString(16)));
  return history.map((p) => {
    let free = 0, largest = 0, low = 0;
    for (const [key, [f, l, m]] of Object.entries(p.h)) {
      if (!internal.has(key)) continue;
      free += f; low += m; largest = Math.max(largest, l);
    }
    return { t: p.t, free, largest, low };
  });
}

let trendGeometry: { points: TrendPoint[]; x: (t: number) => number } | null = null;
function drawTrend() {
  if (!colors) readTheme();
  const C = colors!, canvas = $<HTMLCanvasElement>('heap-trend-canvas'), points = trendPoints();
  const last = points[points.length - 1];
  const span = points.length > 1 ? last.t - points[0].t : 0;
  $('heap-trend-span').textContent = span ? `last ${ago(span * 1000)}` : '';
  const dpr = devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
  if (!w || !h) return;
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  const ctx = canvas.getContext('2d')!;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  trendGeometry = null;
  if (points.length < 2) {
    ctx.fillStyle = C.faint;
    ctx.font = '11px system-ui, sans-serif';
    ctx.fillText('Collecting…', GUTTER, h / 2 + 4);
    return;
  }
  const t0 = points[0].t, t1 = last.t;
  const x = (t: number) => GUTTER + (t - t0) / (t1 - t0 || 1) * (w - GUTTER - 2);
  trendGeometry = { points, x };
  ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, monospace';
  ctx.textAlign = 'right';
  ctx.lineJoin = 'round';
  // Two bands, each scaled to its own values: the largest block is often a
  // fraction of the free total, and one axis would flatten both lines.
  const split = Math.round(h * 0.64);
  band(points, x, w, 3, split - 5, [
    { pick: (p) => p.low, color: C.faint, width: 1, dash: [1, 2] },
    { pick: (p) => p.free, color: C.used, width: 1.75, dash: [], fill: C.usedFill },
  ]);
  band(points, x, w, split + 4, h - 3, [{ pick: (p) => p.largest, color: C.fresh, width: 1.25, dash: [4, 3] }]);
  // Looking back: the moment shown, the time after it dimmed.
  const at = wanted ?? past?.snap.time;
  if (at !== undefined && at !== null) {
    const cx = Math.round(x(Math.min(t1, Math.max(t0, at)))) + 0.5;
    ctx.fillStyle = C.bg;
    ctx.globalAlpha = 0.55;
    ctx.fillRect(cx, 0, w - cx, h);
    ctx.globalAlpha = 1;
    ctx.strokeStyle = C.text;
    ctx.lineWidth = 1.5;
    ctx.beginPath(); ctx.moveTo(cx, 0); ctx.lineTo(cx, h); ctx.stroke();
  }
}

interface Series { pick: (p: TrendPoint) => number; color: string; width: number; dash: number[]; fill?: string }
/** One band of the trend between `top` and `bottom` css px, with its scale in the gutter. */
function band(points: TrendPoint[], x: (t: number) => number, w: number, top: number, bottom: number, series: Series[]) {
  const C = colors!, ctx = $<HTMLCanvasElement>('heap-trend-canvas').getContext('2d')!;
  let lo = Infinity, hi = -Infinity;
  for (const p of points) for (const s of series) { lo = Math.min(lo, s.pick(p)); hi = Math.max(hi, s.pick(p)); }
  const pad = Math.max((hi - lo) * 0.15, 512);
  lo = Math.max(0, lo - pad);
  hi += pad;
  const y = (bytes: number) => bottom - (bytes - lo) / (hi - lo) * (bottom - top);
  const label = (v: number) => v >= 1048576 ? `${(v / 1048576).toFixed(1)}M` : `${Math.round(v / 1024)}K`;
  ctx.fillStyle = C.faint;
  ctx.strokeStyle = C.line;
  ctx.lineWidth = 1;
  ctx.setLineDash([]);
  // A short band has room for one label: its middle.
  const marks = bottom - top < 36 ? [(hi + lo) / 2] : [hi, lo];
  for (const v of marks) ctx.fillText(label(v), GUTTER - 6, Math.min(bottom, Math.max(top + 7, y(v) + 3.5)));
  for (const v of marks) {
    ctx.beginPath(); ctx.moveTo(GUTTER, Math.round(y(v)) + 0.5); ctx.lineTo(w, Math.round(y(v)) + 0.5); ctx.stroke();
  }
  const t0 = points[0].t, t1 = points[points.length - 1].t;
  for (const s of series) {
    ctx.beginPath();
    points.forEach((p, i) => i ? ctx.lineTo(x(p.t), y(s.pick(p))) : ctx.moveTo(x(p.t), y(s.pick(p))));
    if (s.fill) {
      ctx.save();
      ctx.lineTo(x(t1), bottom); ctx.lineTo(x(t0), bottom); ctx.closePath();
      ctx.fillStyle = s.fill;
      ctx.fill();
      ctx.restore();
      ctx.beginPath();
      points.forEach((p, i) => i ? ctx.lineTo(x(p.t), y(s.pick(p))) : ctx.moveTo(x(p.t), y(s.pick(p))));
    }
    ctx.strokeStyle = s.color;
    ctx.lineWidth = s.width;
    ctx.setLineDash(s.dash);
    ctx.stroke();
  }
  ctx.setLineDash([]);
}

$('heap-trend-canvas').addEventListener('mousemove', (e) => {
  const t = $('heap-tip'), g = trendGeometry;
  if (!g) { t.hidden = true; return; }
  const rect = (e.target as HTMLElement).getBoundingClientRect(), px = e.clientX - rect.left;
  let best = g.points[0];
  for (const p of g.points) if (Math.abs(g.x(p.t) - px) < Math.abs(g.x(best.t) - px)) best = p;
  const last = g.points[g.points.length - 1];
  showTip(tipBox, e, [
    ['time', last.t - best.t < 0.5 ? 'now' : `${ago((last.t - best.t) * 1000)} ago`],
    ['free', kib(best.free)], ['largest block', kib(best.largest)], ['lowest since boot', kib(best.low)],
    ['', best === last ? (past ? 'Click to go back to live' : 'Click or drag to look back') : 'Click or drag to see the heaps then'],
  ]);
});
$('heap-trend-canvas').addEventListener('mouseleave', () => { $('heap-tip').hidden = true; });

/* Looking back --------------------------------------------------------------- */

/** Show the kept snapshot of trend point `i`; the latest point is live. */
function lookAt(i: number) {
  const g = trendGeometry;
  if (!g || !source) return;
  i = Math.max(0, Math.min(g.points.length - 1, i));
  if (i === g.points.length - 1) { goLive(); return; }
  const t = g.points[i].t;
  if (t === (wanted ?? past?.snap.time)) return;
  wanted = t;
  drawTrend();
  void fetchPast();
}

async function fetchPast() {
  if (fetching || !source) return;
  fetching = true;
  const gen = pastGen, asked = run;
  try {
    while (wanted !== null && gen === pastGen) {
      const t = wanted;
      const got = await source.at(t).catch(() => null);
      if (gen !== pastGen || asked !== run) break;
      if (wanted === t) wanted = null;
      if (got) past = { snap: got.snapshot, prev: got.prev };
      paint();
    }
  } finally {
    fetching = false;
  }
}

function goLive() {
  pastGen++;
  wanted = null;
  if (!past) { drawTrend(); return; }
  past = null;
  // Highlights start over rather than flash every change made meanwhile.
  for (const v of views.values()) v.freed = [];
  paint();
}

/** The trend point nearest the pointer, or the one shown, by index. */
function pointAt(px: number) {
  const g = trendGeometry!;
  let best = 0;
  g.points.forEach((p, i) => { if (Math.abs(g.x(p.t) - px) < Math.abs(g.x(g.points[best].t) - px)) best = i; });
  return best;
}
function shownIndex() {
  const g = trendGeometry!, t = wanted ?? past?.snap.time;
  if (t === undefined || t === null) return g.points.length - 1;
  let best = 0;
  g.points.forEach((p, i) => { if (Math.abs(p.t - t) < Math.abs(g.points[best].t - t)) best = i; });
  return best;
}

const trendCanvas = $<HTMLCanvasElement>('heap-trend-canvas');
const scrubTo = (e: PointerEvent) => lookAt(pointAt(e.clientX - trendCanvas.getBoundingClientRect().left));
trendCanvas.addEventListener('pointerdown', (e) => {
  if (!trendGeometry || e.button !== 0) return;
  scrubbing = true;
  trendCanvas.setPointerCapture(e.pointerId);
  scrubTo(e);
});
trendCanvas.addEventListener('pointermove', (e) => { if (scrubbing) scrubTo(e); });
for (const type of ['pointerup', 'pointercancel'] as const) trendCanvas.addEventListener(type, () => { scrubbing = false; });
trendCanvas.addEventListener('keydown', (e) => {
  if (!trendGeometry) return;
  const i = shownIndex(), last = trendGeometry.points.length - 1;
  const to = ({ ArrowLeft: i - 1, ArrowRight: i + 1, Home: 0, End: last, Escape: past || wanted !== null ? last : -1 } as Record<string, number>)[e.key];
  if (to === undefined || to < 0) return;
  e.preventDefault();
  e.stopPropagation();
  lookAt(to);
});
$('heap-golive').addEventListener('click', goLive);

/* Fade the highlights between snapshots, a few frames a second. */
function animate() {
  cancelAnimationFrame(frame);
  let last = 0;
  const tick = (ts: number) => {
    const now = Date.now();
    let active = false;
    for (const v of views.values()) {
      const lit = v.freed.some((f) => now - f.t < FADE) || [...v.born.values()].some((t) => t && now - t < FADE);
      if (lit && ts - last > 80) draw(v);
      active ||= lit;
    }
    if (ts - last > 80) last = ts;
    if (active && visible) frame = requestAnimationFrame(tick);
  };
  frame = requestAnimationFrame(tick);
}

/* What is under the pointer: heap overhead, or the biggest block there. */
function hit(v: View, e: MouseEvent) {
  const x = hitMap(v.canvas, v.heap, v, e);
  if (!x || x.kind === 'meta') return x;
  return { ...x, born: v.born.get(`${x.off}:${x.len}`), stack: v.heap.stk?.[x.n] ?? -1, site: siteOf(v.heap, x.n) };
}
const stackOf = (id: number, ready: () => void) => stacks.get(id, ready);

function tip(v: View, e: MouseEvent) {
  const x = hit(v, e);
  if (!x) { $('heap-tip').hidden = true; return; }
  let rows: TipRow[];
  if (x.kind === 'meta') {
    rows = [['region', x.meta], ['address', hex(x.addr)]];
  } else {
    const start = v.heap.start;
    rows = [['state', x.fl & CORRUPT ? 'allocated · tail canary overwritten' : x.used ? 'allocated' : 'free'], ['block', hex(blockAddr(start, x.off))]];
    if (x.used) rows.push(['pointer', hex(pointerOf(start, x.off, x.fl))]);
    rows.push(['block size', `${fmt(x.len - 4)} B`]);
    if (x.fl & POISONED) rows.push(['requested', `${fmt(x.req)} B`]);
    if (x.used && !past) rows.push(['age', x.born ? ago(Date.now() - x.born) : 'older than the view']);
    if (x.nUsed + x.nFree > 1) rows.push(['under the pointer', `${x.nUsed} allocated, ${x.nFree} free`]);
    if (x.used && shown()?.traced) {
      const site = sites[x.site];
      if (site) {
        rows.push(['allocated by', callName(site.fn), true]);
        if (site.loc) rows.push(['at', splitLoc(site.loc).file, true]);
      } else {
        const stack = stackOf(x.stack, () => { if (!$('heap-tip').hidden) tip(v, e); });
        const fn = stack === 'pending' ? 'looking up…' : stack?.frames[stack.site]?.fn;
        rows.push(['allocated by', fn ? (fn === 'looking up…' ? fn : callName(fn)) : 'unknown', true]);
      }
    }
    if (x.used) rows.push(['', shown()?.traced ? 'Click for details and the call stack' : 'Click for details']);
  }
  showTip(tipBox, e, rows);
}

/* Block details, under the heap's map ------------------------------------ */

function details(v: View) {
  const s = selected, box = v.detail;
  if (!s || s.start !== v.heap.start) {
    if (!box.hidden) { box.hidden = true; box.innerHTML = ''; delete box.dataset.html; }
    return;
  }
  const used = !(s.fl & FREE), now = Date.now();
  const site = used && s.site >= 0 ? sites[s.site] : undefined;
  const title = used ? (site ? shortName(site.fn) : 'Allocated block') : 'Free block';
  const state = past ? (s.absent ? 'not there at this moment' : s.fl & CORRUPT ? 'corrupt' : used ? 'allocated' : 'free')
    : s.freedAt ? `freed ${ago(now - s.freedAt)} ago` : s.fl & CORRUPT ? 'corrupt' : used ? 'allocated' : 'free';
  const facts: [string, string][] = [['address', `${hex(blockAddr(s.start, s.off))} – ${hex(blockAddr(s.start, s.off) + s.len)}`]];
  if (used) facts.push(['pointer', hex(pointerOf(s.start, s.off, s.fl))]);
  facts.push(['size', `${fmt(s.len - 4)} B${s.fl & POISONED ? ` · requested ${fmt(s.req)} B` : ''}`]);
  if (used && !past) facts.push(['age', '']);

  let body = '';
  const traced = shown()?.traced;
  if (used && traced) {
    const stack = stackOf(s.stack, () => { if (selected === s) details(v); });
    if (stack === 'pending') {
      body = `<p class="hd-note">Looking up the call stack…</p>`;
    } else if (!stack) {
      body = `<p class="hd-note">No call stack was recorded for this block.</p>`;
    } else {
      body = stackHtml(stack, allocOpen);
    }
  } else if (used) {
    body = `<p class="hd-note">${esc(!shown()?.elf ? "Load the firmware's ELF to see where blocks were allocated."
      : shown()?.traceNote || 'Allocations are not being recorded, so there is no call stack.')}</p>`;
  }
  const html = `<div class="hd-head"><i class="sw" style="background:${used ? siteColor(s.site) : colors!.free}"></i>` +
    `<strong title="${esc(site?.fn ?? title)}">${esc(title)}</strong><span class="hd-state${(past ? s.absent : s.freedAt) || s.fl & CORRUPT ? ' bad' : ''}"></span>` +
    `<button class="icon-button hd-close" type="button" aria-label="Close details" title="Close (Esc)"><svg class="icon"><use href="#i-x"/></svg></button></div>` +
    `<dl class="hd-facts num">${facts.map(([k, val]) => `<dt>${k}</dt><dd${k === 'age' ? ' class="hd-age"' : ''}>${esc(val)}</dd>`).join('')}</dl>` + body;
  // Rewritten only when something changed, so text selection and the open
  // allocator frames survive the snapshots in between; the ticking state
  // and age are set apart.
  if (box.dataset.html !== html) {
    box.innerHTML = html;
    box.dataset.html = html;
  }
  box.querySelector('.hd-state')!.textContent = state;
  const age = box.querySelector('.hd-age');
  if (age) age.textContent = s.born ? ago((s.freedAt || now) - s.born) : 'older than the view';
  box.hidden = false;
}

function copySelected(button: HTMLButtonElement) {
  const stack = selected ? stacks.get(selected.stack) : null;
  if (stack && stack !== 'pending') void copyStack(button, stack);
}

new ResizeObserver(() => { if (visible && snap) { for (const v of views.values()) draw(v); drawTrend(); } }).observe(pane);

let saved: string | null = null;
try { saved = localStorage.getItem('eink-qemu-pane'); } catch {}
showPane(saved === 'heap' ? 'heap' : 'console');
