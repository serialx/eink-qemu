/* Heap view: the running firmware's heaps (src-tauri/src/heap.rs), polled
   while the Heap tab is open. A free-memory trend for internal RAM, then each
   heap as a map wrapped into rows of a chosen size, every block drawn to
   scale and coloured by the function that allocated it; a card folds to a
   single bar, and a clicked block opens its details and call stack. */
import type { FileInfo, Heap, HeapFrame, HeapHistoryPoint, HeapSite, HeapSnapshot, HeapStack } from './api';

/** Where snapshots and the ELF come from: Rust, or the demo's mock. */
export interface HeapSource {
  snapshot(): Promise<HeapSnapshot>;
  history(): Promise<HeapHistoryPoint[]>;
  stack(id: number): Promise<HeapStack | null>;
  sites(from: number): Promise<HeapSite[]>;
  pickElf(): Promise<FileInfo | null>;
  forgetElf(): Promise<void>;
  openWindow(): Promise<void>;
}

const INTERVAL = 1000;       // ms between snapshots
const FADE = 4000;           // ms a new allocation or a free stays highlighted
const ROW = 10;              // css px per map row, the gap included
const GAP = 2;               // css px between rows
const STRIP = 10;            // css px: a folded heap's bar
const GUTTER = 44;           // css px: the row offsets left of a map, the trend's scale
const TICK = 4;              // rows between offset labels
const FIT_ROWS = 24;         // rows a heap fills at the "Fit" scale
const MAX_ROWS = 128;        // rows at most; a bigger heap gets a coarser scale
const HISTORY = 900;         // trend points kept (15 minutes at one a second)
const SCALES = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024].map((k) => k * 1024);
const FREE = 1, POISONED = 2, CORRUPT = 4;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const output = document.querySelector<HTMLElement>('.output')!;
const pane = $('heap');

const fmt = (n: number) => n.toLocaleString('en-US');
const kib = (n: number) => n >= 1048576 ? `${(n / 1048576).toFixed(2)} MiB` : `${(n / 1024).toFixed(1)} KiB`;
const hex = (n: number) => '0x' + n.toString(16).padStart(8, '0');
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);
const per = (bytes: number) => bytes >= 1048576 ? `${bytes / 1048576} MiB` : `${bytes / 1024} KiB`;
const ago = (ms: number) => { const s = Math.max(0, Math.round(ms / 1000)); return s < 120 ? `${s} s` : `${Math.round(s / 60)} min`; };
const load = <T>(key: string, fallback: T): T => {
  try { const s = localStorage.getItem(key); return s === null ? fallback : JSON.parse(s) as T; } catch { return fallback; }
};
const save = (key: string, value: unknown) => { try { localStorage.setItem(key, JSON.stringify(value)); } catch {} };

interface View {
  el: HTMLElement;
  canvas: HTMLCanvasElement;
  detail: HTMLElement;
  heap: Heap;
  /** When each allocation (offset:length) was first seen; 0 if before the view. */
  born: Map<string, number>;
  freed: { off: number; len: number; t: number }[];
  first: boolean;
  /** The map as last drawn: bytes per row, its width in css px, folded or not. */
  bpr: number;
  width: number;
  folded: boolean;
}

/** The clicked block, kept after it is freed so its details stay readable. */
interface Selected {
  start: number; off: number; len: number; fl: number; req: number;
  site: number; stack: number; born: number; freedAt: number;
}

let source: HeapSource | null = null;
let elf: FileInfo | null = null;
let running = false, run = -1;
let visible = false, paused = false, inflight = false;
let timer = 0, frame = 0;
let snap: HeapSnapshot | null = null;
let problem: { code: string; message: string } | null = null;
const views = new Map<number, View>();
/** Symbolized stacks by id: a value once fetched, else a pending Promise. */
const stacks = new Map<number, HeapStack | null | Promise<HeapStack | null>>();
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
export function showPane(name: 'console' | 'heap') {
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
    selected = null;
    problem = null;
    for (const v of views.values()) v.el.remove();
    views.clear();
  }
  running = state === 'running';
  refresh();
}

function setElf(info: FileInfo | null) {
  elf = info;
  problem = null;
  snap = null;
  sites = [];
  selected = null;
  $('heap-elf-name').textContent = info ? info.name : 'Load ELF…';
  $('heap-elf').title = info ? `${info.path}\nClick to choose another ELF` : "Choose the firmware's ELF";
  $('heap-forget').hidden = !info;
  $('heap-pause').hidden = !info;
  refresh();
}

/** Use the ELF a dialog or a drop chose; shows the Heap tab. */
export async function useElf(choice: Promise<FileInfo | null>) {
  try {
    const info = await choice;
    if (!info) return;
    setElf(info);
    showPane('heap');
  } catch (error) {
    problem = errorOf(error);
    showPane('heap');
  }
}

const errorOf = (error: unknown) => {
  const { code = 'EIO', message = String(error) } = (typeof error === 'object' && error) ? error as { code?: string; message?: string } : {};
  return { code, message };
};

$('heap-elf').addEventListener('click', () => { if (source) void useElf(source.pickElf()); });
$('heap-forget').addEventListener('click', async () => {
  try { await source?.forgetElf(); setElf(null); } catch (error) { problem = errorOf(error); refresh(); }
});
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
pane.addEventListener('keydown', (e) => { if (e.key === 'Escape' && selected) { select(null); e.stopPropagation(); } });

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
function rowBytes(size: number): { bpr: number; capped: boolean } {
  if (scale === 'fit') {
    let bpr = 256;
    while (Math.ceil(size / bpr) > FIT_ROWS) bpr *= 2;
    return { bpr, capped: false };
  }
  let bpr = scale;
  while (Math.ceil(size / bpr) > MAX_ROWS) bpr *= 2;
  return { bpr, capped: bpr !== scale };
}

/* Polling ------------------------------------------------------------------- */

function refresh() {
  clearTimeout(timer);
  paint();
  if (visible && !paused && elf && running && source && !inflight) timer = window.setTimeout(poll, snap ? INTERVAL : 0);
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

/** What a heap is for, from where ESP-IDF puts it on the ESP32-C3 and -S3. */
function role(h: Heap, mainStart: number) {
  if (h.name !== 'DRAM') return '';
  if (h.start === 0x3fcdc710 || h.start === 0x3fce9710) return 'from boot ROM';
  if (h.start === 0x3fcc0000 && h.caps.includes('RETENTION')) return 'retention';
  if (h.start === 0x3fcf0000) return 'data only';
  return h.start === mainStart ? 'main' : '';
}

/* Names: C++ ones run to hundreds of characters, so short places get a
   condensed form and the details the whole name, wrapped. */

/** "a<b<c>>::f" -> "a<…>::f"; operator names are left alone. */
function untemplate(fn: string) {
  if (fn.includes('operator')) return fn;
  let out = '', depth = 0;
  for (const ch of fn) {
    if (ch === '<') { if (depth++ === 0) out += '<…'; continue; }
    if (ch === '>' && depth > 0) { if (--depth === 0) out += '>'; continue; }
    if (!depth) out += ch;
  }
  return out;
}
/** Up to the argument list: "ns::f(int)" -> "ns::f". */
const unargs = (fn: string) => {
  let depth = 0;
  for (let i = 0; i < fn.length; i++) {
    const ch = fn[i];
    if (ch === '<') depth++;
    else if (ch === '>') depth--;
    else if (ch === '(' && depth <= 0 && i > 0 && !fn.slice(0, i).endsWith('operator')) return fn.slice(0, i);
  }
  return fn;
};
/** "ns::Class::method(int)" -> "Class::method" for the legend. */
const shortName = (fn: string) => untemplate(unargs(fn.replace(/\(anonymous namespace\)::/g, ''))).split('::').slice(-2).join('::');
/** "ns::Class<T>::method(int, char*)" -> "ns::Class<…>::method()" for the tooltip. */
const callName = (fn: string) => `${untemplate(unargs(fn))}()`;
/** A location as file:line, and the directory it is in. */
const splitLoc = (loc: string) => {
  const clean = loc.replace(/ \(discriminator \d+\)$/, ''), cut = clean.lastIndexOf('/');
  return { file: clean.slice(cut + 1), dir: cut > 0 ? clean.slice(0, cut + 1) : '' };
};

function paint() {
  $('heap-message').hidden = true;
  const showMaps = !!(elf && running && snap?.heaps.length && !problem);
  if (!elf) {
    message("Watch the firmware's heaps",
      'Load the ELF your firmware was built from (with symbols), or drop it on the window. ' +
      'While the firmware runs, the view pauses the emulator for a moment each second to read its heaps.', 'Load ELF…');
  } else if (problem?.code === 'EMISMATCH') {
    message('This ELF is not the running firmware', problem.message, 'Choose another ELF…');
  } else if (problem?.code === 'EINVAL' || problem?.code === 'ENOENT') {
    message('Cannot use the ELF', problem.message, 'Choose another ELF…');
  } else if (problem) {
    message('Cannot read the heaps', problem.message);
  } else if (!running) {
    message('The emulator is not running', 'Start the firmware to see its heaps.');
  } else if (!snap) {
    message('Reading the heaps…', '');
  } else if (!snap.heaps.length) {
    message('Waiting for the firmware', snap.missing
      ? 'The heaps were changing in every read; trying again.'
      : 'No heaps are registered yet; the firmware sets them up early in boot.');
  }
  for (const id of ['heap-live', 'heap-legend', 'heap-note', 'heap-bar', 'heap-trend', 'heap-list']) $(id).hidden = !showMaps;
  if (!showMaps || !snap) { $('heap-tip').hidden = true; return; }

  $('heap-live').classList.toggle('paused', paused);
  const when = new Date(snap.time * 1000).toLocaleTimeString();
  const took = snap.took === undefined ? '' : ` · read in ${Math.round(snap.took)} ms`;
  const skipped = snap.missing ? ` · ${snap.missing} heap${snap.missing > 1 ? 's' : ''} changing, skipped` : '';
  $('heap-state').textContent = `${paused ? 'Paused' : 'Live'} · ${when}${took}${skipped}`;
  $('heap-note').textContent = snap.traced ? '' : snap.traceNote ?? '';

  // Internal memory first, then PSRAM; by address within each.
  const rank = (h: Heap) => ['DRAM', 'IRAM', 'RTC', 'PSRAM'].indexOf(h.name);
  const heaps = [...snap.heaps].sort((a, b) => rank(a) - rank(b) || a.start - b.start);
  const internal = heaps.filter((h) => h.name !== 'PSRAM'), psram = heaps.filter((h) => h.name === 'PSRAM');
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
    track(v, h, now);
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
    const frag = h.free ? Math.round(100 * (1 - h.largest / h.free)) : 0;
    const { bpr, capped } = rowBytes(size);
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
  animate();
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
  v = { el, canvas, detail, heap: h, born: new Map(), freed: [], first: true, bpr: 1, width: 1, folded: false };
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
    const same = selected?.start === self.heap.start && selected.off === x.off && !selected.freedAt;
    select(same ? null : { start: self.heap.start, off: x.off, len: x.len, fl: x.fl, req: x.req, site: x.site, stack: x.stack, born: x.born ?? 0, freedAt: 0 });
  });
  detail.addEventListener('click', (e) => {
    const target = e.target as HTMLElement;
    if (target.closest('.hd-close')) select(null);
    else if (target.closest('.hd-copy')) void copyStack(target.closest<HTMLButtonElement>('.hd-copy')!);
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

/** Keep the selected block's facts current, or note when it was freed. */
function follow(h: Heap, now: number) {
  const s = selected!, b = h.blocks;
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
  v.heap = h;
}

/* Colors come from the stylesheet's tokens, read again when the theme changes. */
type RGB = [number, number, number];
interface Colors {
  used: string; usedAlt: string; free: string; meta: string; fresh: string; freed: string; corrupt: string;
  bg: string; text: string; faint: string; line: string; usedFill: string;
  sites: string[]; sitesAlt: string[];
}
let colors: Colors | null = null;
const SITE_COLORS = 9;
function readColors() {
  const style = getComputedStyle(pane);
  const probe = document.createElement('canvas').getContext('2d')!;
  const rgb = (name: string): RGB => {
    probe.fillStyle = '#000';
    probe.fillStyle = style.getPropertyValue(name).trim() || '#888';
    probe.fillRect(0, 0, 1, 1);
    const [r, g, b] = probe.getImageData(0, 0, 1, 1).data;
    return [r, g, b];
  };
  const css = (c: RGB, a = 1) => a === 1 ? `rgb(${c.join(',')})` : `rgba(${c.join(',')},${a})`;
  const bg = rgb('--bg');
  // A neighbour from the same function: the colour a third of the way to the card.
  const alt = (c: RGB) => css(c.map((x, i) => Math.round(x + (bg[i] - x) * 0.32)) as RGB);
  const site = [...Array(SITE_COLORS).keys()].map((i) => rgb(`--heap-site-${i}`));
  const used = rgb('--heap-used');
  colors = {
    used: css(used), usedAlt: alt(used), usedFill: css(used, 0.12), free: css(rgb('--heap-free')), meta: css(rgb('--heap-meta')),
    fresh: css(rgb('--heap-fresh')), freed: css(rgb('--heap-freed')), corrupt: css(rgb('--heap-corrupt')),
    bg: css(bg), text: css(rgb('--text')), faint: css(rgb('--faint')), line: css(rgb('--border')),
    sites: site.map((c) => css(c)), sitesAlt: site.map(alt),
  };
}
function themeChanged() {
  colors = null;
  if (snap) paint();
}
new MutationObserver(themeChanged).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', themeChanged);

/** A block's site id, or -1 without an allocation log. */
const siteOf = (h: Heap, i: number) => snap?.traced ? h.sites?.[i] ?? -1 : -1;
const siteColor = (site: number) => site >= 0 ? colors!.sites[site % SITE_COLORS] : colors!.used;

function draw(v: View) {
  if (!colors) readColors();
  const C = colors!, h = v.heap, size = h.end - h.start, now = Date.now();
  const map = v.el.querySelector<HTMLElement>('.heap-map')!, gutter = v.el.querySelector<HTMLElement>('.heap-gutter')!;
  const width = Math.max(60, map.clientWidth - (v.folded ? 0 : GUTTER));
  const bpr = v.folded ? size : rowBytes(size).bpr, rows = Math.ceil(size / bpr);
  const height = v.folded ? STRIP : rows * ROW - GAP;
  v.bpr = bpr;
  v.width = width;

  const dpr = devicePixelRatio || 1, pw = Math.round(width * dpr), ph = Math.round(height * dpr);
  const canvas = v.canvas;
  if (canvas.width !== pw || canvas.height !== ph) {
    canvas.width = pw;
    canvas.height = ph;
  }
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;
  if (!v.folded) {
    let ticks = '';
    for (let r = 0; r < rows; r += TICK) {
      const off = r * bpr;
      ticks += `<span style="top:${r * ROW}px">+${off >= 1048576 ? `${+(off / 1048576).toFixed(2)}M` : `${Math.round(off / 1024)}K`}</span>`;
    }
    gutter.style.height = `${height}px`;
    if (gutter.innerHTML !== ticks) gutter.innerHTML = ticks;
  }

  const ctx = canvas.getContext('2d')!;
  ctx.clearRect(0, 0, pw, ph);
  const bar = v.folded ? ph : Math.round((ROW - GAP) * dpr), pitch = ROW * dpr, sx = pw / bpr;
  /** Each row's piece of [off, off+len), at least `min` device px wide. */
  const pieces = (off: number, len: number, min: number, each: (x: number, y: number, w: number) => void) => {
    for (let a = off, e = Math.min(off + len, size); a < e;) {
      const r = Math.floor(a / bpr), s = a - r * bpr, take = Math.min(e - a, bpr - s);
      each(s * sx, r * pitch, Math.max(take * sx, min));
      a += take;
    }
  };
  const fill = (off: number, len: number, style: string, min = 0) => {
    ctx.fillStyle = style;
    pieces(off, len, min, (x, y, w) => ctx.fillRect(x, y, w, bar));
  };
  fill(0, size, C.free);
  const b = h.blocks;
  fill(0, b.length ? b[0] : size, C.meta);
  if (b.length) fill(h.tail, size - h.tail, C.meta);
  let prevColor = '', alt = false;
  for (let i = 0, n = 0; i < b.length; i += 4, n++) {
    const off = b[i], len = b[i + 1], fl = b[i + 2];
    if (fl & FREE) { prevColor = ''; continue; }
    const site = siteOf(h, n), base = siteColor(site);
    // Neighbours of one colour alternate shade so each allocation stands apart.
    alt = base === prevColor ? !alt : false;
    prevColor = base;
    fill(off, len, fl & CORRUPT ? C.corrupt : alt ? (site >= 0 ? C.sitesAlt[site % SITE_COLORS] : C.usedAlt) : base, 1);
    if (len * sx >= 4) fill(off, Math.min(len, 1 / sx), C.bg);
    const t = v.born.get(`${off}:${len}`);
    if (t) {
      const w = 1 - (now - t) / FADE;
      if (w > 0 && w <= 1) { ctx.globalAlpha = 0.85 * w; fill(off, len, C.fresh, 1); ctx.globalAlpha = 1; }
    }
  }
  for (const f of v.freed) {
    const w = 1 - (now - f.t) / FADE;
    if (w > 0 && w <= 1) { ctx.globalAlpha = 0.85 * w; fill(f.off, f.len, C.freed, 1); ctx.globalAlpha = 1; }
  }
  // The selected block: outlined, in the text colour so it shows on any fill.
  if (selected?.start === h.start && !selected.freedAt) {
    const lw = Math.max(1, Math.round(1.5 * dpr));
    ctx.strokeStyle = C.text;
    ctx.lineWidth = lw;
    pieces(selected.off, selected.len, 3 * dpr, (x, y, w) => ctx.strokeRect(x + lw / 2, y + lw / 2, w - lw, bar - lw));
  }
}

/** The sites holding the most memory, with their colours. */
function legend(heaps: Heap[]) {
  const box = $('heap-sites'), named = !!(snap?.traced && sites.length);
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
  if (!colors) readColors();
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
  showTip(e, [
    ['time', last.t - best.t < 0.5 ? 'now' : `${ago((last.t - best.t) * 1000)} ago`],
    ['free', kib(best.free)], ['largest block', kib(best.largest)], ['lowest since boot', kib(best.low)],
  ]);
});
$('heap-trend-canvas').addEventListener('mouseleave', () => { $('heap-tip').hidden = true; });

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
  const h = v.heap, rect = v.canvas.getBoundingClientRect(), size = h.end - h.start;
  const x = e.clientX - rect.left, y = e.clientY - rect.top;
  const row = v.folded ? 0 : Math.floor(y / ROW);
  // The bytes one css pixel covers, so a block thinner than a pixel can be found.
  const span = Math.max(1, v.bpr / v.width);
  const lo = Math.floor(row * v.bpr + x / v.width * v.bpr), hi = lo + span;
  if (x < 0 || x >= v.width || lo < 0 || lo >= size) return null;
  const b = h.blocks, n = b.length / 4;
  let a = 0, z = n; // first block ending after lo
  while (a < z) { const m = (a + z) >> 1; if (b[m * 4] + b[m * 4 + 1] <= lo) a = m + 1; else z = m; }
  if (!n || hi <= b[0] || a >= n || lo >= h.tail) {
    return { kind: 'meta' as const, meta: lo < (b[0] ?? size) ? 'heap and TLSF control' : 'pool end', addr: h.start + lo };
  }
  let i = a, nUsed = 0, nFree = 0, best = a;
  for (; i < n && b[i * 4] < hi; i++) {
    if (b[i * 4 + 2] & FREE) { nFree++; continue; }
    nUsed++;
    if ((b[best * 4 + 2] & FREE) || b[i * 4 + 1] > b[best * 4 + 1]) best = i;
  }
  const off = b[best * 4], len = b[best * 4 + 1], fl = b[best * 4 + 2], req = b[best * 4 + 3];
  return { kind: 'block' as const, off, len, fl, req, used: !(fl & FREE), nUsed, nFree, born: v.born.get(`${off}:${len}`),
    stack: h.stk?.[best] ?? -1, site: siteOf(h, best) };
}
/** A block's addresses: its TLSF header (prev_phys_block) and the pointer malloc returned. */
const blockAddr = (start: number, off: number) => start + off - 4;
const pointerOf = (start: number, off: number, fl: number) => blockAddr(start, off) + (fl & POISONED ? 16 : 8);

/** A recorded stack, fetched once; `ready` runs when a pending one arrives. */
function stackOf(id: number, ready: () => void): HeapStack | null | 'pending' {
  if (id < 0 || !source) return null;
  let stack = stacks.get(id);
  if (stack === undefined) {
    const pending = source.stack(id).catch(() => null).then((found) => { stacks.set(id, found); return found; });
    stacks.set(id, stack = pending);
    void pending.then(ready);
  }
  return stack instanceof Promise ? 'pending' : stack;
}

/** Tooltip rows: [label, value], or [label, value, true] for a long value on its own line. */
type TipRow = [string, string] | [string, string, true];
function showTip(e: MouseEvent, rows: TipRow[]) {
  const t = $('heap-tip');
  t.innerHTML = rows.map(([k, val, wide]) => k
    ? `<div${wide ? ' class="wide"' : ''}><span>${esc(k)}</span><b class="num">${esc(val)}</b></div>`
    : `<div class="hint">${esc(val)}</div>`).join('');
  t.hidden = false;
  const tw = t.offsetWidth, th = t.offsetHeight;
  t.style.left = `${Math.max(8, Math.min(e.clientX + 14, innerWidth - tw - 8))}px`;
  t.style.top = `${e.clientY + 14 + th > innerHeight ? Math.max(8, e.clientY - th - 10) : e.clientY + 14}px`;
}

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
    if (x.used) rows.push(['age', x.born ? ago(Date.now() - x.born) : 'older than the view']);
    if (x.nUsed + x.nFree > 1) rows.push(['under the pointer', `${x.nUsed} allocated, ${x.nFree} free`]);
    if (x.used && snap?.traced) {
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
    if (x.used) rows.push(['', 'Click for details and the call stack']);
  }
  showTip(e, rows);
}

/* Block details, under the heap's map ------------------------------------ */

const locHtml = (loc: string) => {
  if (!loc) return '';
  const { file, dir } = splitLoc(loc);
  return `<span class="hd-loc" title="${esc(loc)}"><b>${esc(file)}</b>${dir ? ` <span>${esc(dir)}</span>` : ''}</span>`;
};
const frameHtml = (f: HeapFrame, cls: string) =>
  `<li class="${cls}"><code>${esc(f.fn)}</code>${locHtml(f.loc)}` +
  f.inl.map((x) => `<div class="hd-inl"><span>inlined</span> <code>${esc(x.fn)}</code>${locHtml(x.loc)}</div>`).join('') + '</li>';

function details(v: View) {
  const s = selected, box = v.detail;
  if (!s || s.start !== v.heap.start) {
    if (!box.hidden) { box.hidden = true; box.innerHTML = ''; delete box.dataset.html; }
    return;
  }
  const used = !(s.fl & FREE), now = Date.now();
  const site = used && s.site >= 0 ? sites[s.site] : undefined;
  const title = used ? (site ? shortName(site.fn) : 'Allocated block') : 'Free block';
  const state = s.freedAt ? `freed ${ago(now - s.freedAt)} ago` : s.fl & CORRUPT ? 'corrupt' : used ? 'allocated' : 'free';
  const facts: [string, string][] = [['address', `${hex(blockAddr(s.start, s.off))} – ${hex(blockAddr(s.start, s.off) + s.len)}`]];
  if (used) facts.push(['pointer', hex(pointerOf(s.start, s.off, s.fl))]);
  facts.push(['size', `${fmt(s.len - 4)} B${s.fl & POISONED ? ` · requested ${fmt(s.req)} B` : ''}`]);
  if (used) facts.push(['age', '']);

  let body = '';
  if (used && snap?.traced) {
    const stack = stackOf(s.stack, () => { if (selected === s) details(v); });
    if (stack === 'pending') {
      body = `<p class="hd-note">Looking up the call stack…</p>`;
    } else if (!stack) {
      body = `<p class="hd-note">No call stack was recorded for this block.</p>`;
    } else {
      const fr = stack.frames, by = fr[stack.site];
      body = (by ? `<div class="hd-by"><span class="hd-label">allocated by</span><code>${esc(by.fn)}</code>${locHtml(by.loc)}</div>` : '') +
        `<div class="hd-stack"><div class="hd-stack-head"><span class="hd-label">call stack</span>` +
        `<button class="hd-copy" type="button">Copy</button></div>` +
        (stack.caller ? `<details${allocOpen ? ' open' : ''}><summary>${stack.caller} allocator frame${stack.caller > 1 ? 's' : ''}</summary>` +
          `<ol>${fr.slice(0, stack.caller).map((f) => frameHtml(f, 'alloc')).join('')}</ol></details>` : '') +
        `<ol start="${stack.caller + 1}">${fr.slice(stack.caller).map((f, k) => frameHtml(f, stack.caller + k === stack.site ? 'site' : f.lib ? 'lib' : '')).join('')}</ol></div>`;
    }
  } else if (used) {
    body = `<p class="hd-note">${esc(snap?.traceNote || 'Allocations are not being recorded, so there is no call stack.')}</p>`;
  }
  const html = `<div class="hd-head"><i class="sw" style="background:${used ? siteColor(s.site) : colors!.free}"></i>` +
    `<strong title="${esc(site?.fn ?? title)}">${esc(title)}</strong><span class="hd-state${s.freedAt || s.fl & CORRUPT ? ' bad' : ''}"></span>` +
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

async function copyStack(button: HTMLButtonElement) {
  const s = selected;
  const stack = s ? stacks.get(s.stack) : null;
  if (!stack || stack instanceof Promise) return;
  const text = stack.frames.map((f, i) => `#${i} ${f.fn}${f.loc ? `  ${f.loc}` : ''}` +
    f.inl.map((x) => `\n     inlined ${x.fn}${x.loc ? `  ${x.loc}` : ''}`).join('')).join('\n');
  try {
    await navigator.clipboard.writeText(text);
    button.textContent = 'Copied';
  } catch {
    button.textContent = 'Copy failed';
  }
  setTimeout(() => { button.textContent = 'Copy'; }, 1500);
}

new ResizeObserver(() => { if (visible && snap) { for (const v of views.values()) draw(v); drawTrend(); } }).observe(pane);

let saved: string | null = null;
try { saved = localStorage.getItem('eink-qemu-pane'); } catch {}
showPane(saved === 'heap' ? 'heap' : 'console');
