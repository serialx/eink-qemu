/* Heap view: the running firmware's heaps (src-tauri/src/heap.rs), polled
   while the Heap tab is open. The map and trend are eink-qemu/heapview.html's,
   fitted to the output panel. */
import type { FileInfo, Heap, HeapSnapshot, HeapStack } from './api';

/** Where snapshots and the ELF come from: Rust, or the demo's mock. */
export interface HeapSource {
  snapshot(): Promise<HeapSnapshot>;
  stack(id: number): Promise<HeapStack | null>;
  pickElf(): Promise<FileInfo | null>;
  forgetElf(): Promise<void>;
  openWindow(): Promise<void>;
}

const INTERVAL = 1000;       // ms between snapshots
const FADE = 4000;           // ms a new allocation or a free stays highlighted
const CELL = 4;              // css px per map cell
const HISTORY = 600;         // trend points kept per heap
const FREE = 1, POISONED = 2, CORRUPT = 4;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const output = document.querySelector<HTMLElement>('.output')!;
const pane = $('heap');

const fmt = (n: number) => n.toLocaleString('en-US');
const kib = (n: number) => n >= 1048576 ? `${(n / 1048576).toFixed(2)} MiB` : `${(n / 1024).toFixed(1)} KiB`;
const hex = (n: number) => '0x' + n.toString(16).padStart(8, '0');
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);

interface View {
  el: HTMLElement;
  heap: Heap;
  /** When each allocation (offset:length) was first seen; 0 if before the view. */
  born: Map<string, number>;
  freed: { off: number; len: number; t: number }[];
  first: boolean;
  cols: number;
  bpc: number;
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
const history = new Map<number, [number, number, number][]>(); // start -> [time, free, largest]

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
    problem = null;
    history.clear();
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

function record(s: HeapSnapshot) {
  for (const h of s.heaps) {
    let points = history.get(h.start);
    if (!points) history.set(h.start, points = []);
    points.push([s.time * 1000, h.infoFree, h.largest]);
    if (points.length > HISTORY) points.splice(0, points.length - HISTORY);
  }
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
  $('heap-live').hidden = !showMaps;
  $('heap-legend').hidden = !showMaps;
  $('heap-note').hidden = !showMaps;
  $('heap-summary').hidden = !showMaps;
  $('heap-list').hidden = !showMaps;
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
  const tiles: [string, string, string, boolean?][] = [
    ['Internal free', kib(sum(internal, (h) => h.infoFree)), `of ${kib(sum(internal, (h) => h.end - h.start))}`],
    ['Lowest free', kib(sum(internal, (h) => h.infoMinFree)), 'internal, since boot'],
    ['Largest block', kib(max(internal, (h) => h.largest)), 'internal, TLSF size'],
  ];
  if (psram.length) tiles.push(['PSRAM free', kib(sum(psram, (h) => h.infoFree)), `largest ${kib(max(psram, (h) => h.largest))}`]);
  if (corrupt) tiles.push(['Corrupt', fmt(corrupt), 'overwritten canaries', true]);
  $('heap-summary').innerHTML = tiles.map(([k, v, sub, bad]) =>
    `<div class="tile${bad ? ' bad' : ''}"><span>${k}</span><b class="num">${v}</b><small>${sub}</small></div>`).join('');

  const list = $('heap-list'), seen = new Set<number>();
  for (const h of heaps) {
    const v = view(h);
    seen.add(h.start);
    track(v, h, Date.now());
    list.appendChild(v.el); // keeps the order above
    const size = h.end - h.start;
    v.el.querySelector('h3')!.textContent = h.name;
    v.el.querySelector('.heap-size')!.textContent = kib(size);
    v.el.querySelector('.heap-addr')!.textContent = hex(h.start);
    v.el.querySelector('.heap-head')!.setAttribute('title', `${hex(h.start)}–${hex(h.end)}\n${h.caps.join(' ')}`);
    const frag = h.free ? Math.round(100 * (1 - h.largest / h.free)) : 0;
    v.el.querySelector('.heap-stats')!.innerHTML = ([
      ['used', kib(h.used)], ['free', kib(h.infoFree)], ['largest', kib(h.largest)],
      ['lowest', kib(h.infoMinFree)], ['blocks', `${fmt(h.count)} · ${fmt(h.nfree)} free`], ['fragmented', `${frag}%`],
    ] as const).map(([k, val]) => `<span><span>${k}</span> <b>${val}</b></span>`).join('')
      + (h.corrupt ? `<span class="bad"><b>${fmt(h.corrupt)} corrupt</b></span>` : '');
    (v.el.querySelector('.heap-usage div') as HTMLElement).style.width = `${(100 * h.used / (h.used + h.free || 1)).toFixed(2)}%`;
    draw(v);
    spark(v);
  }
  for (const [start, v] of views) {
    if (!seen.has(start)) { v.el.remove(); views.delete(start); }
  }
  animate();
}

function view(h: Heap): View {
  let v = views.get(h.start);
  if (v) return v;
  const el = document.createElement('section');
  el.className = 'heap-card';
  el.innerHTML = `
    <div class="heap-head"><h3></h3><span class="heap-size num"></span><span class="heap-addr"></span></div>
    <div class="heap-usage"><div></div></div>
    <div class="heap-stats num"></div>
    <canvas class="heap-map"></canvas>
    <div class="heap-trend"><canvas></canvas><span class="num"></span></div>`;
  v = { el, heap: h, born: new Map(), freed: [], first: true, cols: 1, bpc: 4 };
  const map = el.querySelector<HTMLCanvasElement>('canvas.heap-map')!;
  const self = v;
  map.addEventListener('mousemove', (e) => tip(self, e));
  map.addEventListener('mouseleave', () => { $('heap-tip').hidden = true; });
  views.set(h.start, v);
  return v;
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
let colors: Record<'used' | 'free' | 'meta' | 'fresh' | 'freed' | 'corrupt' | 'muted', RGB> | null = null;
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
  colors = {
    used: rgb('--heap-used'), free: rgb('--heap-free'), meta: rgb('--heap-meta'), fresh: rgb('--heap-fresh'),
    freed: rgb('--heap-freed'), corrupt: rgb('--heap-corrupt'), muted: rgb('--faint'),
  };
}
function themeChanged() {
  colors = null;
  for (const v of views.values()) { draw(v); spark(v); }
}
new MutationObserver(themeChanged).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', themeChanged);

function draw(v: View) {
  if (!colors) readColors();
  const C = colors!, h = v.heap, size = h.end - h.start, now = Date.now();
  const canvas = v.el.querySelector<HTMLCanvasElement>('canvas.heap-map')!;
  const cols = Math.max(16, Math.floor((v.el.querySelector<HTMLElement>('.heap-usage')!.clientWidth || 300) / CELL));
  const maxRows = h.name === 'PSRAM' ? 64 : 40;
  let bpc = 4;
  while (Math.ceil(size / bpc / cols) > maxRows) bpc *= 2;
  const cells = Math.ceil(size / bpc), rows = Math.ceil(cells / cols);
  v.bpc = bpc;
  v.cols = cols;

  // Per-cell colors weighted by bytes, then blended with the highlights.
  const R = new Float32Array(cells), G = new Float32Array(cells), B = new Float32Array(cells), W = new Float32Array(cells);
  const fresh = new Float32Array(cells), freed = new Float32Array(cells);
  const bad = new Uint8Array(cells), edge = new Uint8Array(cells);
  const add = (arr: Float32Array, off: number, len: number, w: number) => {
    for (let a = off, e = Math.min(off + len, size); a < e;) {
      const c = Math.floor(a / bpc), ce = Math.min(e, (c + 1) * bpc);
      arr[c] += (ce - a) * w;
      a = ce;
    }
  };
  const paintRange = (off: number, len: number, c: RGB) => {
    for (let a = off, e = Math.min(off + len, size); a < e;) {
      const i = Math.floor(a / bpc), ie = Math.min(e, (i + 1) * bpc), n = ie - a;
      R[i] += c[0] * n; G[i] += c[1] * n; B[i] += c[2] * n; W[i] += n;
      a = ie;
    }
  };
  const b = h.blocks;
  paintRange(0, b.length ? b[0] : size, C.meta);
  if (b.length) paintRange(h.tail, size - h.tail, C.meta);
  for (let i = 0; i < b.length; i += 4) {
    const off = b[i], len = b[i + 1], fl = b[i + 2];
    if (fl & FREE) { paintRange(off, len, C.free); continue; }
    paintRange(off, len, C.used);
    if (fl & CORRUPT) for (let c = Math.floor(off / bpc); c <= Math.floor((off + len - 1) / bpc); c++) bad[c] = 1;
    if (len >= 2 * bpc) edge[Math.floor(off / bpc)] = 1;
    const t = v.born.get(`${off}:${len}`);
    if (t) {
      const w = 1 - (now - t) / FADE;
      if (w > 0 && w <= 1) add(fresh, off, len, w);
    }
  }
  for (const f of v.freed) {
    const w = 1 - (now - f.t) / FADE;
    if (w > 0 && w <= 1) add(freed, f.off, f.len, w);
  }

  if (canvas.width !== cols || canvas.height !== rows) {
    canvas.width = cols;
    canvas.height = rows;
  }
  canvas.style.width = `${cols * CELL}px`;
  canvas.style.height = `${rows * CELL}px`;
  const ctx = canvas.getContext('2d')!, img = ctx.createImageData(cols, rows), px = img.data;
  for (let c = 0; c < cells; c++) {
    const tot = W[c] || 1;
    let r = R[c] / tot, g = G[c] / tot, bl = B[c] / tot;
    const fw = Math.min(1, fresh[c] / tot), dw = Math.min(1, freed[c] / tot);
    if (fw > 0) { r += (C.fresh[0] - r) * fw; g += (C.fresh[1] - g) * fw; bl += (C.fresh[2] - bl) * fw; }
    if (dw > 0) { r += (C.freed[0] - r) * dw; g += (C.freed[1] - g) * dw; bl += (C.freed[2] - bl) * dw; }
    if (edge[c]) { r *= 0.8; g *= 0.8; bl *= 0.8; }
    if (bad[c]) [r, g, bl] = C.corrupt;
    const p = c * 4;
    px[p] = r; px[p + 1] = g; px[p + 2] = bl; px[p + 3] = 255;
  }
  ctx.putImageData(img, 0, 0);
}

/** Free memory and the largest free block over time. */
function spark(v: View) {
  if (!colors) readColors();
  const canvas = v.el.querySelector<HTMLCanvasElement>('.heap-trend canvas')!, label = v.el.querySelector('.heap-trend span')!;
  const points = history.get(v.heap.start) ?? [];
  const dpr = devicePixelRatio || 1, w = canvas.clientWidth, ht = canvas.clientHeight;
  canvas.width = w * dpr;
  canvas.height = ht * dpr;
  const ctx = canvas.getContext('2d')!;
  ctx.scale(dpr, dpr);
  const last = points[points.length - 1];
  if (points.length < 2) { label.textContent = last ? `free ${kib(last[1])}` : ''; return; }
  let lo = Infinity, hi = -Infinity;
  for (const p of points) { lo = Math.min(lo, p[1], p[2]); hi = Math.max(hi, p[1], p[2]); }
  if (hi - lo < 1024) { lo = Math.max(0, (hi + lo) / 2 - 512); hi = lo + 1024; }
  const t0 = points[0][0], t1 = last[0];
  const x = (t: number) => (t - t0) / (t1 - t0 || 1) * (w - 2) + 1, y = (b: number) => ht - 2 - (b - lo) / (hi - lo) * (ht - 4);
  const css = (c: RGB) => `rgb(${c.join(',')})`;
  ctx.lineWidth = 1.5;
  ctx.lineJoin = 'round';
  for (const [idx, color, dash] of [[2, colors!.fresh, [3, 3]], [1, colors!.used, []]] as const) {
    ctx.strokeStyle = css(color);
    ctx.setLineDash([...dash]);
    ctx.beginPath();
    points.forEach((p, i) => i ? ctx.lineTo(x(p[0]), y(p[idx])) : ctx.moveTo(x(p[0]), y(p[idx])));
    ctx.stroke();
  }
  const secs = Math.round((t1 - t0) / 1000);
  label.innerHTML = `<i class="line used"></i>free ${kib(last[1])} <i class="line fresh"></i>largest ${kib(last[2])}` +
    `<br>${secs >= 120 ? `last ${Math.round(secs / 60)} min` : `last ${secs} s`}`;
}

/* Fade the highlights between snapshots, a few frames a second. */
function animate() {
  cancelAnimationFrame(frame);
  let last = 0;
  const step = (ts: number) => {
    const now = Date.now();
    let active = false;
    for (const v of views.values()) {
      const lit = v.freed.some((f) => now - f.t < FADE) || [...v.born.values()].some((t) => t && now - t < FADE);
      if (lit && ts - last > 80) draw(v);
      active ||= lit;
    }
    if (ts - last > 80) last = ts;
    if (active && visible) frame = requestAnimationFrame(step);
  };
  frame = requestAnimationFrame(step);
}

/* What is under the pointer: heap overhead, or the biggest block in the cell. */
function hit(v: View, e: MouseEvent) {
  const h = v.heap, rect = (e.target as HTMLElement).getBoundingClientRect();
  const col = Math.floor((e.clientX - rect.left) / CELL), row = Math.floor((e.clientY - rect.top) / CELL);
  const lo = (row * v.cols + col) * v.bpc, hi = lo + v.bpc, size = h.end - h.start;
  if (col < 0 || col >= v.cols || lo < 0 || lo >= size) return null;
  const b = h.blocks, n = b.length / 4;
  let a = 0, z = n; // first block ending after lo
  while (a < z) { const m = (a + z) >> 1; if (b[m * 4] + b[m * 4 + 1] <= lo) a = m + 1; else z = m; }
  if (!n || lo < b[0] || a >= n) return { kind: 'meta' as const, meta: lo < (b[0] ?? size) ? 'heap and TLSF control' : 'pool end', addr: h.start + lo };
  let i = a, nUsed = 0, nFree = 0;
  for (; i < n && b[i * 4] < hi; i++) (b[i * 4 + 2] & FREE) ? nFree++ : nUsed++;
  let best = a;
  for (let j = a; j < i; j++) if (b[j * 4 + 1] > b[best * 4 + 1]) best = j;
  const off = b[best * 4], len = b[best * 4 + 1], fl = b[best * 4 + 2], req = b[best * 4 + 3];
  const block = h.start + off - 4; // the TLSF header (prev_phys_block)
  return { kind: 'block' as const, off, len, fl, req, block, used: !(fl & FREE), nUsed, nFree, born: v.born.get(`${off}:${len}`),
    ptr: block + (fl & POISONED ? 16 : 8), stack: h.stk?.[best] ?? -1 };
}

/** The function a stack names as "allocated by", fetched once per stack. */
function allocatedBy(id: number, ready: () => void): string {
  if (id < 0 || !source) return 'no stack recorded';
  let stack = stacks.get(id);
  if (stack === undefined) {
    const pending = source.stack(id).catch(() => null).then((found) => { stacks.set(id, found); return found; });
    stacks.set(id, stack = pending);
    void pending.then(() => { if (!$('heap-tip').hidden) ready(); });
  }
  if (stack instanceof Promise) return 'looking up…';
  const frame = stack?.frames[stack.site];
  if (!frame) return 'unknown';
  // "ns::Class::method(args)" -> "ns::Class::method()"
  return frame.fn.replace(/\((?!\)).*\)( const)?$/, '()');
}

function tip(v: View, e: MouseEvent) {
  const t = $('heap-tip'), x = hit(v, e);
  if (!x) { t.hidden = true; return; }
  let rows: [string, string][];
  if (x.kind === 'meta') {
    rows = [['region', x.meta], ['address', hex(x.addr)]];
  } else {
    rows = [['state', x.fl & CORRUPT ? 'allocated · tail canary overwritten' : x.used ? 'allocated' : 'free'], ['block', hex(x.block)]];
    if (x.used) rows.push(['pointer', hex(x.ptr)]);
    rows.push(['block size', `${fmt(x.len - 4)} B`]);
    if (x.fl & POISONED) rows.push(['requested', `${fmt(x.req)} B`]);
    if (x.used) rows.push(['age', x.born ? `${Math.max(0, Math.round((Date.now() - x.born) / 1000))} s` : 'older than the view']);
    if (x.nUsed + x.nFree > 1) rows.push(['in this cell', `${x.nUsed} allocated, ${x.nFree} free`]);
    if (x.used && snap?.traced) rows.push(['allocated by', allocatedBy(x.stack, () => tip(v, e))]);
  }
  t.innerHTML = rows.map(([k, val]) => `<div><span>${esc(k)}</span><b class="num">${esc(val)}</b></div>`).join('');
  t.hidden = false;
  const tw = t.offsetWidth, th = t.offsetHeight;
  t.style.left = `${Math.max(8, Math.min(e.clientX + 14, innerWidth - tw - 8))}px`;
  t.style.top = `${e.clientY + 14 + th > innerHeight ? e.clientY - th - 10 : e.clientY + 14}px`;
}

new ResizeObserver(() => { if (visible) for (const v of views.values()) { draw(v); spark(v); } }).observe(pane);

let saved: string | null = null;
try { saved = localStorage.getItem('eink-qemu-pane'); } catch {}
showPane(saved === 'heap' ? 'heap' : 'console');
