/* What the Heap tab (heap.ts) and the heap window (heap-window.ts) share:
   formatting, C++ names, the map colours, the painter that wraps a heap's
   blocks into rows of a chosen size, hit-testing, tooltips and the call
   stack markup. Nothing here knows which page it is on. */
import type { Heap, HeapFrame, HeapStack } from './types';

/** A block's flags (core/src/heap.rs). */
export const FREE = 1, POISONED = 2, CORRUPT = 4;
export const ROW = 10;           // css px per map row, the gap included
export const GAP = 2;            // css px between rows
export const STRIP = 10;         // css px: a folded heap's bar
export const GUTTER = 44;        // css px: the row offsets left of a map
const TICK = 4;                  // rows between offset labels
const FIT_ROWS = 24;             // rows a heap fills at the "Fit" scale
const MAX_ROWS = 128;            // rows at most; a bigger heap gets a coarser scale
export const SCALES = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024].map((k) => k * 1024);
export const FADE = 4000;        // ms a new allocation or a free stays highlighted

export const fmt = (n: number) => n.toLocaleString('en-US');
export const kib = (n: number) => n >= 1048576 ? `${(n / 1048576).toFixed(2)} MiB` : `${(n / 1024).toFixed(1)} KiB`;
/** A signed size: "+12.0 KiB", "−3.5 KiB", "0". */
export const delta = (n: number) => Math.abs(n) < 51.2 ? '0' : `${n > 0 ? '+' : '−'}${kib(Math.abs(n))}`;
export const hex = (n: number) => '0x' + n.toString(16).padStart(8, '0');
export const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);
export const per = (bytes: number) => bytes >= 1048576 ? `${bytes / 1048576} MiB` : `${bytes / 1024} KiB`;
export const ago = (ms: number) => { const s = Math.max(0, Math.round(ms / 1000)); return s < 120 ? `${s} s` : `${Math.round(s / 60)} min`; };
export const load = <T>(key: string, fallback: T): T => {
  try { const s = localStorage.getItem(key); return s === null ? fallback : JSON.parse(s) as T; } catch { return fallback; }
};
export const save = (key: string, value: unknown) => { try { localStorage.setItem(key, JSON.stringify(value)); } catch {} };
export const errorOf = (error: unknown) => {
  const { code = 'EIO', message = String(error) } = (typeof error === 'object' && error) ? error as { code?: string; message?: string } : {};
  return { code, message };
};

/* Names: C++ ones run to hundreds of characters, so short places get a
   condensed form and the details the whole name, wrapped. */

/** "a<b<c>>::f" -> "a<…>::f"; operator names are left alone. */
export function untemplate(fn: string) {
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
export const unargs = (fn: string) => {
  let depth = 0;
  for (let i = 0; i < fn.length; i++) {
    const ch = fn[i];
    if (ch === '<') depth++;
    else if (ch === '>') depth--;
    else if (ch === '(' && depth <= 0 && i > 0 && !fn.slice(0, i).endsWith('operator')) return fn.slice(0, i);
  }
  return fn;
};
/** "ns::Class::method(int)" -> "Class::method" for lists. */
export const shortName = (fn: string) => untemplate(unargs(fn.replace(/\(anonymous namespace\)::/g, ''))).split('::').slice(-2).join('::');
/** "ns::Class<T>::method(int, char*)" -> "ns::Class<…>::method()" for the tooltip. */
export const callName = (fn: string) => `${untemplate(unargs(fn))}()`;
/** A location as file:line, and the directory it is in. */
export const splitLoc = (loc: string) => {
  const clean = loc.replace(/ \(discriminator \d+\)$/, ''), cut = clean.lastIndexOf('/');
  return { file: clean.slice(cut + 1), dir: cut > 0 ? clean.slice(0, cut + 1) : '' };
};

/** What a heap is for, from where ESP-IDF puts it on the ESP32-C3 and -S3. */
export function role(h: Heap, mainStart: number) {
  if (h.name !== 'DRAM') return '';
  if (h.start === 0x3fcdc710 || h.start === 0x3fce9710) return 'from boot ROM';
  if (h.start === 0x3fcc0000 && h.caps.includes('RETENTION')) return 'retention';
  if (h.start === 0x3fcf0000) return 'data only';
  return h.start === mainStart ? 'main' : '';
}
/** Internal memory first, then PSRAM; by address within each. */
const rank = (h: Heap) => ['DRAM', 'IRAM', 'RTC', 'PSRAM'].indexOf(h.name);
export const byKind = (a: Heap, b: Heap) => rank(a) - rank(b) || a.start - b.start;
export const isInternal = (h: Heap) => h.name !== 'PSRAM';
/** Of a heap's free memory, the share the largest block does not cover. */
export const fragmented = (h: Heap) => h.free ? Math.round(100 * (1 - h.largest / h.free)) : 0;

/** A heap's bytes per row at `scale`, and whether its size forced a coarser one. */
export function rowBytes(size: number, scale: number | 'fit'): { bpr: number; capped: boolean } {
  if (scale === 'fit') {
    let bpr = 256;
    while (Math.ceil(size / bpr) > FIT_ROWS) bpr *= 2;
    return { bpr, capped: false };
  }
  let bpr = scale;
  while (Math.ceil(size / bpr) > MAX_ROWS) bpr *= 2;
  return { bpr, capped: bpr !== scale };
}

/* Colours come from the stylesheet's tokens, read again when the theme changes. */

type RGB = [number, number, number];
export const SITE_COLORS = 9;
export interface Colors {
  used: string; usedAlt: string; usedFill: string; free: string; meta: string;
  fresh: string; freed: string; corrupt: string; bg: string; text: string; faint: string; line: string;
  /** --heap-site-0…8, and each one's neighbour shade. */
  sites: string[]; sitesAlt: string[];
  /** Sites without a colour of their own (the heap window's smaller ones). */
  other: string; otherAlt: string;
}
export function readColors(el: Element): Colors {
  const style = getComputedStyle(el);
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
  const used = rgb('--heap-used'), other = rgb('--heap-other');
  return {
    used: css(used), usedAlt: alt(used), usedFill: css(used, 0.12), free: css(rgb('--heap-free')), meta: css(rgb('--heap-meta')),
    fresh: css(rgb('--heap-fresh')), freed: css(rgb('--heap-freed')), corrupt: css(rgb('--heap-corrupt')),
    bg: css(bg), text: css(rgb('--text')), faint: css(rgb('--faint')), line: css(rgb('--border')),
    sites: site.map((c) => css(c)), sitesAlt: site.map(alt), other: css(other), otherAlt: alt(other),
  };
}
/** Run `changed` when the page's theme or the system's changes. */
export function onTheme(changed: () => void) {
  new MutationObserver(changed).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', changed);
}

/* The map: a heap's bytes in address order, wrapped into rows of `bpr` bytes
   (a folded heap is one bar), every block drawn to scale. */

export interface MapLayout { bpr: number; width: number; folded: boolean }
export interface MapStyle {
  /** A used block's colour by its index, and the shade a same-coloured neighbour takes. */
  color(n: number): string;
  alt(n: number): string;
  /** 0..1: how strongly the new-allocation highlight lies over block n. */
  glow?(n: number): number;
  /** Below 1 to fade block n toward the free ground. */
  alpha?(n: number): number;
  /** Freed blocks, highlighted by weight; drawn under the blocks when `under`. */
  freed?: { off: number; len: number; w: number }[];
  under?: boolean;
  /** The selected block, outlined in the text colour. */
  outline?: { off: number; len: number } | null;
}

/** Size the canvas, write the gutter's row offsets and paint `h`. */
export function paintMap(canvas: HTMLCanvasElement, gutter: HTMLElement | null, h: Heap, layout: MapLayout, C: Colors, style: MapStyle) {
  const size = h.end - h.start, { bpr, width, folded } = layout, rows = Math.ceil(size / bpr);
  const height = folded ? STRIP : rows * ROW - GAP;
  const dpr = devicePixelRatio || 1, pw = Math.round(width * dpr), ph = Math.round(height * dpr);
  if (canvas.width !== pw || canvas.height !== ph) {
    canvas.width = pw;
    canvas.height = ph;
  }
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;
  if (gutter && !folded) {
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
  const bar = folded ? ph : Math.round((ROW - GAP) * dpr), pitch = ROW * dpr, sx = pw / bpr;
  /** Each row's piece of [off, off+len), at least `min` device px wide. */
  const pieces = (off: number, len: number, min: number, each: (x: number, y: number, w: number) => void) => {
    for (let a = off, e = Math.min(off + len, size); a < e;) {
      const r = Math.floor(a / bpr), s = a - r * bpr, take = Math.min(e - a, bpr - s);
      each(s * sx, r * pitch, Math.max(take * sx, min));
      a += take;
    }
  };
  const fill = (off: number, len: number, color: string, min = 0) => {
    ctx.fillStyle = color;
    pieces(off, len, min, (x, y, w) => ctx.fillRect(x, y, w, bar));
  };
  const freed = () => {
    for (const f of style.freed ?? []) {
      if (f.w <= 0 || f.w > 1) continue;
      ctx.globalAlpha = 0.85 * f.w;
      fill(f.off, f.len, C.freed, 1);
      ctx.globalAlpha = 1;
    }
  };
  fill(0, size, C.free);
  const b = h.blocks;
  fill(0, b.length ? b[0] : size, C.meta);
  if (b.length) fill(h.tail, size - h.tail, C.meta);
  if (style.under) freed();
  let prevColor = '', alt = false;
  for (let i = 0, n = 0; i < b.length; i += 4, n++) {
    const off = b[i], len = b[i + 1], fl = b[i + 2];
    if (fl & FREE) { prevColor = ''; continue; }
    const base = style.color(n);
    // Neighbours of one colour alternate shade so each allocation stands apart.
    alt = base === prevColor ? !alt : false;
    prevColor = base;
    const a = style.alpha?.(n) ?? 1;
    if (a < 1) ctx.globalAlpha = a;
    fill(off, len, fl & CORRUPT ? C.corrupt : alt ? style.alt(n) : base, 1);
    if (len * sx >= 4) fill(off, Math.min(len, 1 / sx), C.bg);
    ctx.globalAlpha = 1;
    const w = style.glow?.(n) ?? 0;
    if (w > 0 && w <= 1) { ctx.globalAlpha = 0.85 * w * a; fill(off, len, C.fresh, 1); ctx.globalAlpha = 1; }
  }
  if (!style.under) freed();
  if (style.outline) {
    const lw = Math.max(1, Math.round(1.5 * dpr));
    ctx.strokeStyle = C.text;
    ctx.lineWidth = lw;
    pieces(style.outline.off, style.outline.len, 3 * dpr, (x, y, w) => ctx.strokeRect(x + lw / 2, y + lw / 2, w - lw, bar - lw));
  }
}

/** The map row a block starts on, in css px from the canvas top. */
export const rowTop = (off: number, layout: MapLayout) => layout.folded ? 0 : Math.floor(off / layout.bpr) * ROW;

export type MapHit =
  | { kind: 'meta'; meta: string; addr: number }
  | { kind: 'block'; n: number; off: number; len: number; fl: number; req: number; used: boolean; nUsed: number; nFree: number };

/** What is under the pointer: heap overhead, or the biggest block there. */
export function hitMap(canvas: HTMLCanvasElement, h: Heap, layout: MapLayout, e: MouseEvent): MapHit | null {
  const rect = canvas.getBoundingClientRect(), size = h.end - h.start;
  const x = e.clientX - rect.left, y = e.clientY - rect.top;
  const row = layout.folded ? 0 : Math.floor(y / ROW);
  // The bytes one css pixel covers, so a block thinner than a pixel can be found.
  const span = Math.max(1, layout.bpr / layout.width);
  const lo = Math.floor(row * layout.bpr + x / layout.width * layout.bpr), hi = lo + span;
  if (x < 0 || x >= layout.width || lo < 0 || lo >= size) return null;
  const b = h.blocks, n = b.length / 4;
  let a = 0, z = n; // first block ending after lo
  while (a < z) { const m = (a + z) >> 1; if (b[m * 4] + b[m * 4 + 1] <= lo) a = m + 1; else z = m; }
  if (!n || hi <= b[0] || a >= n || lo >= h.tail) {
    return { kind: 'meta', meta: lo < (b[0] ?? size) ? 'heap and TLSF control' : 'pool end', addr: h.start + lo };
  }
  let nUsed = 0, nFree = 0, best = a;
  for (let i = a; i < n && b[i * 4] < hi; i++) {
    if (b[i * 4 + 2] & FREE) { nFree++; continue; }
    nUsed++;
    if ((b[best * 4 + 2] & FREE) || b[i * 4 + 1] > b[best * 4 + 1]) best = i;
  }
  const fl = b[best * 4 + 2];
  return { kind: 'block', n: best, off: b[best * 4], len: b[best * 4 + 1], fl, req: b[best * 4 + 3], used: !(fl & FREE), nUsed, nFree };
}
/** A block's addresses: its TLSF header (prev_phys_block) and the pointer malloc returned. */
export const blockAddr = (start: number, off: number) => start + off - 4;
export const pointerOf = (start: number, off: number, fl: number) => blockAddr(start, off) + (fl & POISONED ? 16 : 8);

/* Tooltips: rows of [label, value], or [label, value, true] for a long value
   on its own line; an empty label makes a hint. */

export type TipRow = [string, string] | [string, string, true];
export function showTip(tip: HTMLElement, e: MouseEvent, rows: TipRow[]) {
  tip.innerHTML = rows.map(([k, val, wide]) => k
    ? `<div${wide ? ' class="wide"' : ''}><span>${esc(k)}</span><b class="num">${esc(val)}</b></div>`
    : `<div class="hint">${esc(val)}</div>`).join('');
  tip.hidden = false;
  const tw = tip.offsetWidth, th = tip.offsetHeight;
  tip.style.left = `${Math.max(8, Math.min(e.clientX + 14, innerWidth - tw - 8))}px`;
  tip.style.top = `${e.clientY + 14 + th > innerHeight ? Math.max(8, e.clientY - th - 10) : e.clientY + 14}px`;
}

/* Call stacks: fetched once by id, then drawn as the tab's block details do. */

export class Stacks {
  private known = new Map<number, HeapStack | null | Promise<HeapStack | null>>();
  private fetch: (id: number) => Promise<HeapStack | null>;
  constructor(fetch: (id: number) => Promise<HeapStack | null>) { this.fetch = fetch; }
  /** The stack, 'pending' while it is fetched (`ready` runs when it arrives), or null. */
  get(id: number, ready?: () => void): HeapStack | null | 'pending' {
    if (id < 0) return null;
    let stack = this.known.get(id);
    if (stack === undefined) {
      const pending = this.fetch(id).catch(() => null).then((found) => { this.known.set(id, found); return found; });
      this.known.set(id, stack = pending);
      if (ready) void pending.then(ready);
    } else if (stack instanceof Promise && ready) {
      void stack.then(ready);
    }
    return stack instanceof Promise ? 'pending' : stack;
  }
  clear() { this.known.clear(); }
}

export const locHtml = (loc: string) => {
  if (!loc) return '';
  const { file, dir } = splitLoc(loc);
  return `<span class="hd-loc" title="${esc(loc)}"><b>${esc(file)}</b>${dir ? ` <span>${esc(dir)}</span>` : ''}</span>`;
};
const frameHtml = (f: HeapFrame, cls: string) =>
  `<li class="${cls}"><code>${esc(f.fn)}</code>${locHtml(f.loc)}` +
  f.inl.map((x) => `<div class="hd-inl"><span>inlined</span> <code>${esc(x.fn)}</code>${locHtml(x.loc)}</div>`).join('') + '</li>';

/** "allocated by" and the call stack, the allocator's own frames folded. */
export function stackHtml(stack: HeapStack, allocOpen: boolean, label = 'call stack') {
  const fr = stack.frames, by = fr[stack.site];
  return (by ? `<div class="hd-by"><span class="hd-label">allocated by</span><code>${esc(by.fn)}</code>${locHtml(by.loc)}</div>` : '') +
    `<div class="hd-stack"><div class="hd-stack-head"><span class="hd-label">${esc(label)}</span>` +
    `<button class="hd-copy" type="button">Copy</button></div>` +
    (stack.caller ? `<details${allocOpen ? ' open' : ''}><summary>${stack.caller} allocator frame${stack.caller > 1 ? 's' : ''}</summary>` +
      `<ol>${fr.slice(0, stack.caller).map((f) => frameHtml(f, 'alloc')).join('')}</ol></details>` : '') +
    // A wrapper the allocator is reached through library code from (a
    // make_unique whose constructor copies a string) is the allocator's too.
    `<ol start="${stack.caller + 1}">${fr.slice(stack.caller).map((f, k) => frameHtml(f, stack.caller + k === stack.site ? 'site' : f.alloc ? 'alloc' : f.lib ? 'lib' : '')).join('')}</ol></div>`;
}

/** Copy a stack as text, one frame a line, and say so on the button. */
export async function copyStack(button: HTMLButtonElement, stack: HeapStack) {
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
