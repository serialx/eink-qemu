/* Live map of an ESP-IDF firmware's heaps: the page heapview.py serves, and
   the desktop app's heap window. mount() writes the markup into `root` and
   polls `api`, which returns what heapview.py's endpoints do:
     snapshot()                         /snapshot
     history()                          /history
     events(gen, start, stacks, sites)  /events
     stack(id)                          /stack */

const MARKUP = `
<div class="wrap">
  <header>
    <h1>Heap view</h1><span id="elf" class="num"></span>
    <div class="status"><span id="dot"></span><span id="state">connecting…</span></div>
    <button id="pause">Pause</button>
  </header>
  <div id="banner"></div>
  <section id="tm" hidden>
    <div class="bar">
      <button id="tm-play" title="Play from the cursor (space)">▶</button>
      <span class="seg" id="tm-speed"><button data-speed="1">1×</button><button data-speed="10">10×</button></span>
      <button id="tm-prev" title="Previous event (←; shift: 1 s)">‹</button>
      <button id="tm-next" title="Next event (→; shift: 1 s)">›</button>
      <span id="tm-label" class="num"></span>
      <span class="right">
        <span class="seg" id="tm-range"><button data-range="30000">30 s</button><button data-range="300000">5 min</button><button data-range="0">all</button></span>
        <button id="tm-live" title="Back to the live heap (Esc)">Live</button>
      </span>
    </div>
    <canvas id="tm-canvas"></canvas>
    <div class="axis num"><span id="tm-from"></span><span class="keys">drag to rewind ·
      <span style="color:var(--fresh)">▮</span> allocations <span style="color:var(--freed)">▮</span> frees ·
      <span style="color:var(--used)">━</span> traced live bytes</span><span id="tm-to"></span></div>
  </section>
  <section class="cards" id="cards"></section>
  <div class="legend">
    <span id="lg-used"><i style="background:var(--used)"></i>allocated</span>
    <span id="lg-untraced" hidden><i style="background:var(--untraced)"></i>no stack recorded</span>
    <span><i style="background:var(--fresh)"></i>just allocated</span>
    <span><i style="background:var(--freed)"></i>just freed</span>
    <span><i style="background:var(--free)"></i>free</span>
    <span><i style="background:var(--meta)"></i>allocator metadata</span>
    <span><i style="background:var(--corrupt)"></i>corrupt canary</span>
    <span class="colorby">Color by <span class="seg" id="colorby"><button data-mode="kind">state</button><button data-mode="site">allocation site</button></span></span>
  </div>
  <section id="sites" hidden></section>
  <div id="empty">No heaps registered yet; the firmware has not initialized its allocator.</div>
  <main id="heaps"></main>
</div>
<div id="tip" hidden></div>`;

export function mount(root, api) {
  root.insertAdjacentHTML("beforeend", MARKUP);
  const FADE = 4000;           // ms a fresh allocation or free stays highlighted when live
  const PAST_FADE = 1500;      // the same in replayed (event log) time
  const CELL = 4;              // css px per map cell
  const FREE = 1, POISONED = 2, CORRUPT = 4;
  const ALLOC = 1, FREED = 2, REALLOC = 3;
  const CHURN_WINDOW = 10000;  // ms of events the churn tab counts
  const CK = 2048;             // events between replay checkpoints

  const $ = (id) => document.getElementById(id);
  const fmt = (n) => n.toLocaleString("en-US");
  const kib = (n) => n >= 1048576 ? (n / 1048576).toFixed(2) + " MiB" : (n / 1024).toFixed(1) + " KiB";
  const hex = (n) => "0x" + n.toString(16).padStart(8, "0");
  const pref = (k, d) => { try { return localStorage.getItem("heapview." + k) || d; } catch (e) { return d; } };
  const save = (k, v) => { try { localStorage.setItem("heapview." + k, v); } catch (e) {} };

  let snap = null, paused = false, timer = 0, colors = null, tipKey = null;
  let mode = pref("mode", "site"), tab = pref("tab", "live");
  let focus = -2, pinnedFocus = -2;   // a site id to highlight, -2 for none
  let markPtr = 0;                     // a block picked from the event list
  const stacks = new Map();    // stack id -> symbolized stack, null, or a pending Promise
  const views = new Map();     // heap start -> per-heap view state
  const history = [];          // [{t, h: {startHex: [free, largest, minFree]}}]
  let siteNames = [];          // site id -> {fn, loc}

  // Time machine state.
  let at = null;               // null: live; else the number of events replayed
  let cursorT = 0;             // ms after the first event, while replaying
  let range = +pref("range", "300000"), tmEnd = 0, playing = null, speed = +pref("speed", "1");
  const pastCache = { n: -1, of: null, heaps: null };

  // ---- Event log, replayed in the page -------------------------------------

  // Columns of every allocation and free since QEMU started. t is ms after
  // the first event; fs/fstk are the size and allocating stack of the block a
  // free or realloc released; live is traced bytes after each event; nalloc is
  // a prefix count of allocations for the timeline histogram; ckpt[j] is the
  // live set after j * CK events.
  let tr = null;
  function resetTrace(gen) {
    tr = { gen, n: 0, t0: 0, t: [], k: [], p: [], s: [], o: [], st: [], fs: [], fstk: [], live: [], nalloc: [0],
           stackSite: [], cur: new Map(), bytes: 0, ckpt: [new Map()] };
    stacks.clear();
    at = null;
    pastCache.n = -1;
  }
  // The desktop bridge uses unsigned generations. Zero also works with
  // heapview.py's initial generation: the empty cursor starts at event zero.
  resetTrace(0);

  function ingest(ev) {
    if (ev.gen !== tr.gen) resetTrace(ev.gen);
    tr.t0 = ev.t0;
    if (ev.siteStart === siteNames.length) siteNames.push(...ev.sites);
    if (ev.stackStart === tr.stackSite.length) tr.stackSite.push(...ev.stackSite);
    if (ev.start !== tr.n) return;
    for (let j = 0; j < ev.k.length; j++) {
      const i = tr.n, k = ev.k[j], p = ev.p[j], s = ev.s[j];
      const t = Math.max(ev.t[j] / 1000, i ? tr.t[i - 1] : 0);
      tr.t.push(t); tr.k.push(k); tr.p.push(p); tr.s.push(s); tr.o.push(ev.o[j]); tr.st.push(ev.st[j]);
      let gone = null;
      if (k !== ALLOC) {
        const q = k === FREED ? p : ev.o[j];
        gone = tr.cur.get(q);
        if (gone) { tr.bytes -= gone[0]; tr.cur.delete(q); }
      }
      tr.fs.push(gone ? gone[0] : 0); tr.fstk.push(gone ? gone[1] : -1);
      if (k !== FREED) {
        const prev = tr.cur.get(p);
        if (prev) tr.bytes -= prev[0];
        tr.cur.set(p, [s, ev.st[j], i]);
        tr.bytes += s;
      }
      tr.live.push(tr.bytes);
      tr.nalloc.push(tr.nalloc[i] + (k !== FREED ? 1 : 0));
      tr.n++;
      if (tr.n % CK === 0) tr.ckpt[tr.n / CK] = new Map(tr.cur);
    }
  }

  // Live allocations after the first n events: ptr -> [size, stack, event].
  function stateAt(n) {
    const j = Math.floor(n / CK), m = new Map(tr.ckpt[j]);
    for (let i = j * CK; i < n; i++) {
      const k = tr.k[i];
      if (k === FREED) m.delete(tr.p[i]);
      else { if (k === REALLOC) m.delete(tr.o[i]); m.set(tr.p[i], [tr.s[i], tr.st[i], i]); }
    }
    return m;
  }

  // The number of events with t <= time.
  function countAt(time) {
    let a = 0, z = tr.n;
    while (a < z) { const m = (a + z) >> 1; if (tr.t[m] <= time) a = m + 1; else z = m; }
    return a;
  }

  async function fetchEvents() {
    if (!snap?.traced) return;
    for (let guard = 0; guard < 50; guard++) {
      const same = snap.traceGen === tr.gen;
      if (same && tr.n >= snap.traceEvents) return;
      const ev = await api.events(tr.gen, same ? tr.n : 0, same ? tr.stackSite.length : 0, siteNames.length);
      if (!ev) return;
      const before = tr.n, gen = tr.gen;
      ingest(ev);
      if (tr.n === before && ev.gen === gen) return;
    }
  }

  // ---- Time machine ----------------------------------------------------------

  const latestT = () => Math.max(tr.n ? tr.t[tr.n - 1] : 0, snap?.time && tr.t0 ? snap.time * 1000 - tr.t0 : 0);

  function setAt(n, t) {
    n = Math.max(0, Math.min(tr.n, n));
    if (n >= tr.n && (t === undefined || t >= latestT() - 1)) return goLive();
    if (at === null) for (const v of views.values()) v.freed = [];
    at = n;
    cursorT = t ?? (n ? tr.t[n - 1] : 0);
    if (range && (cursorT < tmEnd - range || cursorT > tmEnd)) tmEnd = Math.min(latestT(), cursorT + range / 2);
    scheduleRender();
  }

  function goLive() {
    if (at !== null) for (const v of views.values()) { v.first = true; v.freed = []; }
    at = null; markPtr = 0;
    stopPlay();
    scheduleRender();
  }

  let renderQueued = false;
  function scheduleRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => { renderQueued = false; if (snap) render(); });
  }

  // The heaps as the event log says they were after `at` events. Region
  // bounds, pool start and tail come from the live walk. A block still alive
  // keeps its walked size; others get the TLSF minimum for the request, and
  // gaps too small to be a free block (under 16 bytes) count as their slack.
  function pastHeaps() {
    if (pastCache.n === at && pastCache.of === snap) return pastCache.heaps;
    const live = [...stateAt(at)].sort((a, b) => a[0] - b[0]);
    const heaps = snap.heaps.map((h) => {
      const size = h.end - h.start, first = h.blocks.length ? h.blocks[0] : h.pool + 4, tail = h.tail ?? size;
      const blocks = [], sites = [], stk = [], ev = [], known = new Map();
      for (let i = 0; i < h.blocks.length; i += 4) if (h.ev?.[i / 4] >= 0) known.set(h.ev[i / 4], h.blocks[i + 1]);
      let pos = first, used = 0, free = 0, largest = 0, nfree = 0, count = 0;
      const gap = (to) => {
        if (to - pos < 16 && blocks.length && !(blocks[blocks.length - 2] & FREE)) {
          blocks[blocks.length - 3] += to - pos;
          used += to - pos;
        } else if (to - pos >= 8) {
          blocks.push(pos, to - pos, FREE, 0); sites.push(-1); stk.push(-1); ev.push(-1);
          free += to - pos - 4; largest = Math.max(largest, to - pos - 4); nfree++; count++;
        }
      };
      for (const [p, [s, st, i]] of live) {
        const off = p - 12 - h.start;                       // block + 4, as in the live list
        if (off < pos || off >= tail) continue;              // outside, or overlapping a missed free
        const len = Math.min(known.get(i) ?? Math.max(16, ((s + 15) & ~3) + 4), tail - off);
        gap(off);
        blocks.push(off, len, POISONED, s); sites.push(tr.stackSite[st] ?? -1); stk.push(st); ev.push(i);
        used += len - 4; count++;
        pos = off + len;
      }
      gap(tail);
      return { ...h, blocks, sites, stk, ev, used, free, largest, nfree, count, infoFree: free,
               infoMinFree: NaN, corrupt: 0, past: true };
    });
    Object.assign(pastCache, { n: at, of: snap, heaps });
    return heaps;
  }

  // Highlight what the last PAST_FADE ms of replayed events allocated and freed.
  function pastHighlights(v, h) {
    v.clock = cursorT;
    v.born = new Map();
    for (let i = 0; i < h.blocks.length; i += 4) {
      const e = h.ev[i / 4];
      if (e >= 0) v.born.set(h.blocks[i] + ":" + h.blocks[i + 1], tr.t[e]);
    }
    v.freed = [];
    for (let i = at - 1; i >= 0 && tr.t[i] > cursorT - PAST_FADE; i--) {
      if (tr.k[i] === ALLOC || !tr.fs[i]) continue;
      const off = (tr.k[i] === FREED ? tr.p[i] : tr.o[i]) - 12 - h.start;
      if (off >= 0 && off < h.end - h.start) v.freed.push({ off, len: ((tr.fs[i] + 15) & ~3) + 4, t: tr.t[i] });
    }
  }

  function play() {
    if (at === null) return;
    playing = { last: performance.now() };
    $("tm-play").textContent = "❚❚";
    const step = (ts) => {
      if (!playing) return;
      const t = cursorT + Math.max(0, ts - playing.last) * speed;
      playing.last = ts;
      if (t >= latestT()) return goLive();
      setAt(countAt(t), t);
      requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }
  function stopPlay() { playing = null; $("tm-play").textContent = "▶"; }

  function drawTimeline() {
    const box = $("tm");
    box.hidden = !snap?.traced || !tr.n;
    if (box.hidden) return;
    box.classList.toggle("past", at !== null);
    const end = latestT();
    if (at === null || !range) tmEnd = end;
    const b = range ? Math.max(tmEnd, Math.min(end, range)) : end, a = range ? Math.max(0, b - range) : 0;
    const canvas = $("tm-canvas"), dpr = devicePixelRatio || 1, w = canvas.clientWidth, ht = canvas.clientHeight;
    canvas.width = w * dpr; canvas.height = ht * dpr;
    const ctx = canvas.getContext("2d");
    ctx.scale(dpr, dpr);
    const cols = Math.max(1, Math.floor(w / 2)), dt = (b - a) / cols || 1, mid = Math.round(ht * 0.62);
    const counts = [];
    let peak = 1, lo = Infinity, hi = 0;
    for (let c = 0, i0 = countAt(a); c < cols; c++) {
      const i1 = countAt(a + (c + 1) * dt);
      const al = tr.nalloc[i1] - tr.nalloc[i0], fr = i1 - i0 - al, bytes = i1 ? tr.live[i1 - 1] : 0;
      counts.push([al, fr, bytes, i1]);
      peak = Math.max(peak, al, fr);
      if (i1) { lo = Math.min(lo, bytes); hi = Math.max(hi, bytes); }
      i0 = i1;
    }
    const rgb = (c, al = 1) => `rgb(${c.join(" ")} / ${al})`;
    const bar = (n, room) => n ? Math.max(1.5, Math.sqrt(n / peak) * room) : 0;
    counts.forEach(([al, fr], c) => {
      const x = c * w / cols;
      ctx.fillStyle = rgb(colors.fresh, 0.8);
      if (al) { const hh = bar(al, mid - 4); ctx.fillRect(x, mid - hh, 1.6, hh); }
      ctx.fillStyle = rgb(colors.freed, 0.8);
      if (fr) ctx.fillRect(x, mid + 1, 1.6, bar(fr, ht - mid - 2));
    });
    if (hi > 0) {
      if (hi - lo < 1024) lo = Math.max(0, hi - 1024);
      ctx.strokeStyle = rgb(colors.used); ctx.lineWidth = 1.5; ctx.lineJoin = "round"; ctx.beginPath();
      let started = false;
      counts.forEach(([, , bytes, i1], c) => {
        if (!i1) return;
        const x = (c + 1) * w / cols, y = ht - 3 - (bytes - lo) / (hi - lo || 1) * (ht - 8);
        started ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
        started = true;
      });
      ctx.stroke();
    }
    const cx = at === null ? w - 1 : (cursorT - a) / (b - a || 1) * w;
    ctx.fillStyle = rgb(at === null ? colors.ok : colors.past);
    ctx.fillRect(Math.round(cx) - 1, 0, 2, ht);
    ctx.beginPath(); ctx.arc(Math.min(Math.max(cx, 5), w - 5), 5, 5, 0, 2 * Math.PI); ctx.fill();
    box.dataset.a = a; box.dataset.b = b;

    const span = (ms) => ms >= 60000 ? (ms / 60000).toFixed(1) + " min" : (ms / 1000).toFixed(ms < 10000 ? 1 : 0) + " s";
    const clock = (ms) => new Date(tr.t0 + ms).toLocaleTimeString([], { hour12: false }) + "." +
      String(Math.floor((tr.t0 + ms) % 1000)).padStart(3, "0");
    $("tm-from").textContent = a <= 0 ? "first event" : "−" + span(end - a);
    $("tm-to").textContent = b >= end - 1 ? "now" : "−" + span(end - b);
    $("tm-label").innerHTML = at === null
      ? `<b>live</b> · ${fmt(tr.n)} events since QEMU started`
      : `<b>${clock(cursorT)}</b> · ${span(end - cursorT)} ago · event ${fmt(at)} of ${fmt(tr.n)}`;
    $("tm-live").setAttribute("aria-pressed", at === null);
    $("tm-prev").disabled = at === 0;
    $("tm-next").disabled = at === null;
    $("tm-play").disabled = at === null;
    document.querySelectorAll("#tm-range button").forEach((x) => x.setAttribute("aria-pressed", +x.dataset.range === range));
    document.querySelectorAll("#tm-speed button").forEach((x) => x.setAttribute("aria-pressed", +x.dataset.speed === speed));
  }

  function scrubTo(e) {
    const box = $("tm"), r = $("tm-canvas").getBoundingClientRect(), a = +box.dataset.a, b = +box.dataset.b;
    const t = a + Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) * (b - a);
    if (t >= latestT() - (b - a) / r.width * 3) return goLive();   // the last few pixels snap to live
    setAt(countAt(t), t);
  }
  $("tm-canvas").onpointerdown = (e) => {
    stopPlay();
    $("tm-canvas").setPointerCapture(e.pointerId);
    scrubTo(e);
    $("tm-canvas").onpointermove = scrubTo;
  };
  $("tm-canvas").onpointerup = $("tm-canvas").onpointercancel = () => { $("tm-canvas").onpointermove = null; };
  $("tm-live").onclick = goLive;
  $("tm-play").onclick = () => playing ? stopPlay() : play();
  $("tm-prev").onclick = () => { stopPlay(); setAt((at ?? tr.n) - 1); };
  $("tm-next").onclick = () => { stopPlay(); if (at !== null) setAt(at + 1); };
  document.querySelectorAll("#tm-range button").forEach((x) => x.onclick = () => {
    range = +x.dataset.range;
    save("range", range);
    tmEnd = at === null ? latestT() : Math.min(latestT(), cursorT + range / 2);
    scheduleRender();
  });
  document.querySelectorAll("#tm-speed button").forEach((x) => x.onclick = () => {
    speed = +x.dataset.speed;
    save("speed", speed);
    drawTimeline();
  });
  addEventListener("keydown", (e) => {
    if (e.target.closest?.("input, textarea") || !tr.n || $("tm").hidden) return;
    if (e.key === "Escape") goLive();
    else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      stopPlay();
      const dir = e.key === "ArrowLeft" ? -1 : 1;
      if (e.shiftKey) {
        const t = Math.max(0, (at === null ? latestT() : cursorT) + dir * 1000);
        setAt(countAt(t), t);
      } else if (at !== null || dir < 0) setAt((at ?? tr.n) + dir);
    } else if (e.key === " " && at !== null) {
      e.preventDefault();
      playing ? stopPlay() : play();
    }
  });

  // ---- Colors ----------------------------------------------------------------

  function readColors() {
    const cs = getComputedStyle(document.documentElement);
    const rgb = (name) => {
      const c = document.createElement("canvas").getContext("2d");
      c.fillStyle = cs.getPropertyValue(name).trim();
      const v = c.fillStyle;   // normalized #rrggbb
      return [1, 3, 5].map((i) => parseInt(v.slice(i, i + 2), 16));
    };
    colors = Object.fromEntries(["used", "free", "meta", "fresh", "freed", "corrupt", "muted", "line", "untraced",
      "ok", "past", "text"].map((n) => [n, rgb("--" + n)]));
    colors.dark = colors.free[0] < 128;
    siteColors.length = 0;
  }

  // The largest sites get the distinct palette colors. A site keeps its color
  // while it stays among them, so the map does not reshuffle as sizes change;
  // the rest get softer golden-angle hues from their site number.
  const PALETTE = {
    light: ["#2f6fdb", "#2a9d4b", "#8e4fd8", "#c99400", "#d6418f", "#1597b8",
            "#8a5a2b", "#5f8f1f", "#4b3fb0", "#e0672a", "#b8306a", "#3a7f7a"],
    dark:  ["#5f95f5", "#45c26a", "#b07ef5", "#e8b923", "#f06aaf", "#3cc0e0",
            "#c08a55", "#8fbe3c", "#8a80f0", "#f58a4f", "#e05a94", "#5fb3ad"],
  };
  const siteColors = [], slots = new Map();   // site id -> palette index
  function assignSlots(ranked) {
    const top = new Set(ranked.slice(0, PALETTE.light.length));
    for (const id of slots.keys()) if (!top.has(id)) { slots.delete(id); delete siteColors[id]; }
    const used = new Set(slots.values());
    for (const id of top) {
      if (slots.has(id)) continue;
      let k = 0;
      while (used.has(k)) k++;
      slots.set(id, k); used.add(k); delete siteColors[id];
    }
  }
  function siteColor(id) {
    if (id < 0) return colors.untraced;
    if (!siteColors[id]) {
      const slot = slots.get(id);
      if (slot !== undefined) {
        const v = PALETTE[colors.dark ? "dark" : "light"][slot];
        siteColors[id] = [1, 3, 5].map((i) => parseInt(v.slice(i, i + 2), 16));
      } else {
        const h = (id * 137.508) % 360, s = 0.32, l = colors.dark ? [0.5, 0.62][id % 2] : [0.6, 0.72][id % 2];
        const f = (n) => {
          const k = (n + h / 30) % 12, a = s * Math.min(l, 1 - l);
          return Math.round(255 * (l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))));
        };
        siteColors[id] = [f(0), f(8), f(4)];
      }
    }
    return siteColors[id];
  }
  const css = (c) => `rgb(${c.join(",")})`;
  const bySite = () => mode === "site" && snap?.traced;

  // ---- Heaps -------------------------------------------------------------------

  function view(h) {
    let v = views.get(h.start);
    if (!v) {
      const el = document.createElement("section");
      el.className = "heap";
      el.innerHTML = `
        <div class="head"><h2></h2><span class="range num"></span><span class="caps"></span>
          <div class="zoom"><button data-z="-1" title="Less detail">−</button><span class="bpc num"></span>
          <button data-z="1" title="More detail">+</button></div></div>
        <div class="stats num"></div>
        <div class="usage"><div></div></div>
        <div class="mapwrap"><canvas class="map"></canvas></div>
        <div class="detail" hidden></div>
        <div class="trend"><canvas class="spark"></canvas><span class="tl num"></span></div>`;
      v = { el, zoom: 0, born: new Map(), freed: [], first: true, heap: h, bpc: 1, cols: 1, clock: null };
      el.querySelectorAll(".zoom button").forEach((b) => b.onclick = () => {
        v.zoom = Math.max(-4, Math.min(12, v.zoom + +b.dataset.z));
        draw(v);
      });
      const map = el.querySelector("canvas.map");
      map.onmousemove = (e) => tip(v, e);
      map.onmouseleave = () => { $("tip").hidden = true; tipKey = null; };
      map.onclick = (e) => pin(v, hit(v, e));
      views.set(h.start, v);
    }
    return v;
  }

  // Track when each allocation first appeared and which blocks were just freed.
  function track(v, h, now) {
    const born = new Map(), b = h.blocks;
    for (let i = 0; i < b.length; i += 4) {
      if (b[i + 2] & FREE) continue;
      const key = b[i] + ":" + b[i + 1];
      born.set(key, v.first ? 0 : (v.born.get(key) ?? now));
    }
    if (!v.first) {
      for (const [key, t] of v.born) {
        if (!born.has(key)) {
          const [off, len] = key.split(":").map(Number);
          v.freed.push({ off, len, t: now });
        }
      }
    }
    v.freed = v.freed.filter((f) => now - f.t < FADE);
    v.born = born;
    v.first = false;
    v.heap = h;
    v.clock = null;
  }

  function render() {
    const s = snap, now = Date.now(), past = at !== null && tr.n > 0;
    $("elf").textContent = s.elf || "";
    const err = s.error ? s.error : s.traceFormat === false ?
      "The trace file comes from an older heaptrace plugin: rebuild it (ninja -C build contrib/plugins/libheaptrace.so) and restart QEMU." : "";
    $("banner").style.display = err ? "block" : "none";
    $("banner").textContent = err;
    const dot = $("dot");
    dot.className = s.error ? "error" : past ? "past" : paused || !s.running ? "paused" : "live";
    $("state").textContent = s.error ? "error" : past ? "replaying the event log" : paused ? "updates paused" :
      !s.running ? "guest paused" : "live · " + new Date(s.time * 1000).toLocaleTimeString();
    const heaps = (past ? pastHeaps() : [...s.heaps]).sort((a, b) => a.start - b.start);
    $("empty").style.display = !s.error && !heaps.length ? "block" : "none";
    drawTimeline();

    const internal = heaps.filter((h) => h.name !== "PSRAM"), psram = heaps.filter((h) => h.name === "PSRAM");
    const sum = (arr, k) => arr.reduce((a, h) => a + h[k], 0);
    const max = (arr, k) => arr.reduce((a, h) => Math.max(a, h[k]), 0);
    const corrupt = sum(heaps, "corrupt");
    const cards = [["Internal free", kib(sum(internal, "infoFree")), `of ${kib(sum(internal, "end") - sum(internal, "start"))}`]];
    if (!past) cards.push(["Internal min free", kib(sum(internal, "infoMinFree")), "low-water mark since boot"]);
    cards.push(["Largest internal block", kib(max(internal, "largest")), past ? "approximate, from the event log" : "biggest possible malloc"]);
    if (psram.length) cards.push(["PSRAM free", kib(sum(psram, "infoFree")), `largest block ${kib(max(psram, "largest"))}`]);
    cards.push(["Allocations", fmt(heaps.reduce((a, h) => a + h.count - h.nfree, 0)),
      `${fmt(sum(heaps, "nfree"))} free fragments`]);
    if (corrupt) cards.push(["Corrupt canaries", fmt(corrupt), "heap poisoning detected", true]);
    $("cards").innerHTML = heaps.length ? cards.map(([k, v, sub, bad]) =>
      `<div class="card${bad ? " bad" : ""}"><div class="k">${k}</div><div class="v num">${v}</div><div class="s">${sub}</div></div>`).join("") : "";

    renderSites(heaps);

    const main = $("heaps"), seen = new Set();
    for (const h of heaps) {
      const v = view(h);
      seen.add(h.start);
      if (past) { v.heap = h; pastHighlights(v, h); } else track(v, h, now);
      main.appendChild(v.el);   // keeps address order
      const size = h.end - h.start;
      v.el.querySelector("h2").textContent = h.name;
      v.el.querySelector(".range").textContent = `${hex(h.start)}–${hex(h.end)} · ${kib(size)}`;
      v.el.querySelector(".caps").textContent = h.caps.join(" ");
      const frag = h.free ? Math.round(100 * (1 - h.largest / h.free)) : 0;
      v.el.querySelector(".stats").innerHTML = [
        ["used", fmt(h.used)], ["free", fmt(h.infoFree)], ["largest free", fmt(h.largest)],
        ...(past ? [] : [["min free", fmt(h.infoMinFree)]]), ["blocks", `${fmt(h.count)} (${fmt(h.nfree)} free)`],
        ["fragmentation", frag + "%"],
      ].map(([k, val]) => `<span><span class="lbl">${k}</span><b>${val}</b></span>`).join("")
        + (h.corrupt ? `<span style="color:var(--corrupt)"><b>${h.corrupt} corrupt</b></span>` : "")
        + (past ? `<span class="lbl">replayed from the event log; block sizes as TLSF would round them</span>` : "");
      v.el.querySelector(".usage div").style.width = (100 * h.used / (h.used + h.free || 1)).toFixed(2) + "%";
      draw(v);
      spark(v);
    }
    for (const [start, v] of views) {
      if (!seen.has(start)) { v.el.remove(); views.delete(start); }
    }
  }

  // ---- Sites, churn and events -------------------------------------------------

  const siteName = (id) => id < 0 ? "no stack recorded" : siteNames[id] ? shortFn(siteNames[id].fn) : "…";
  const siteLoc = (id) => id >= 0 && siteNames[id]?.loc ? shortLoc(siteNames[id].loc) : "";
  const sw = (id) => `<i class="sw" style="background:${css(siteColor(id))}"></i>`;

  function siteRow(id, amount) {
    const loc = siteLoc(id);
    return `<div class="site${id === pinnedFocus ? " on" : ""}" data-id="${id}" title="${id < 0 ? "" : esc(siteNames[id]?.fn ?? "")}">
      ${sw(id)}<span class="fn">${esc(siteName(id))}${loc ? `<span>${esc(loc)}</span>` : ""}</span>
      <span class="amt num">${amount}</span></div>`;
  }

  function renderSites(heaps) {
    document.querySelectorAll("#colorby button").forEach((b) => b.setAttribute("aria-pressed", b.dataset.mode === mode));
    const traced = snap.traced && tr.n > 0;
    $("lg-used").hidden = mode === "site" && traced;
    $("lg-untraced").hidden = !(mode === "site" && traced);
    const box = $("sites");
    box.hidden = !heaps.length;
    if (box.hidden) return;
    if (!traced) {
      const why = snap.traceNote || "Start QEMU with the heaptrace plugin to see where blocks were allocated, replay the heaps and find allocation churn.";
      box.innerHTML = `<div class="note" style="padding-bottom:4px">${esc(why)}</div>`;
      return;
    }
    // Live bytes per site at the viewed moment, which also decides the palette.
    const tot = new Map();
    for (const h of heaps) {
      const b = h.blocks, st = h.sites || [];
      for (let i = 0; i < b.length; i += 4) {
        if (b[i + 2] & FREE) continue;
        const id = st[i / 4] ?? -1, t = tot.get(id) || [0, 0];
        t[0] += b[i + 1]; t[1]++;
        tot.set(id, t);
      }
    }
    const rows = [...tot].sort((a, b) => b[1][0] - a[1][0]);
    assignSlots(rows.map((r) => r[0]).filter((id) => id >= 0));

    const tabs = [["live", "Live by site"], ["churn", "Churn"], ["events", "Events"]];
    let head, body;
    if (tab === "churn") {
      const end = at === null ? tr.n : at, t1 = at === null ? latestT() : cursorT, t0 = t1 - CHURN_WINDOW;
      const per = new Map();
      let na = 0, nf = 0;
      for (let i = end - 1; i >= 0 && tr.t[i] > t0; i--) {
        if (tr.k[i] !== FREED) {
          const id = tr.stackSite[tr.st[i]] ?? -1, c = per.get(id) || [0, 0, 0];
          c[0]++; c[1] += tr.s[i];
          per.set(id, c); na++;
        }
        if (tr.k[i] !== ALLOC && tr.fstk[i] >= 0) {
          const id = tr.stackSite[tr.fstk[i]] ?? -1, c = per.get(id) || [0, 0, 0];
          c[2]++;
          per.set(id, c); nf++;
        }
      }
      const secs = CHURN_WINDOW / 1000, crows = [...per].sort((a, b) => b[1][0] - a[1][0] || b[1][2] - a[1][2]);
      head = `${secs} s ${at === null ? "up to now" : "before the cursor"} · ${fmt(na)} allocations` +
        ` (${(na / secs).toFixed(1)}/s), ${fmt(nf)} frees · by allocating site`;
      body = crows.length ? `<div class="grid">${crows.slice(0, 24).map(([id, [n, bytes, f]]) =>
        siteRow(id, `<b>${(n / secs).toFixed(1)}/s</b> · ${kib(bytes / secs)}/s${f !== n ? ` · ${fmt(f)} freed` : ""}`)).join("")}</div>`
        : `<div class="note">No allocations in this window.</div>`;
    } else if (tab === "events") {
      const end = at === null ? tr.n : at, from = Math.max(0, end - 60), to = Math.min(tr.n, end + (at === null ? 0 : 6));
      const ref = at === null ? latestT() : cursorT;
      const items = [];
      for (let i = to - 1; i >= from; i--) {
        const k = tr.k[i], aid = tr.stackSite[k === FREED ? tr.fstk[i] : tr.st[i]] ?? -1;
        const fid = tr.stackSite[tr.st[i]] ?? -1;
        const op = k === ALLOC ? ["a", "+"] : k === FREED ? ["f", "−"] : ["r", "↻"];
        const size = k === FREED ? tr.fs[i] : tr.s[i];
        const dt = tr.t[i] - ref, when = (dt > 0.0005 ? "+" : dt < -0.0005 ? "−" : "") + (Math.abs(dt) / 1000).toFixed(3) + " s";
        const extra = k === FREED && fid !== aid ? `<span>freed in ${esc(siteName(fid))}</span>` :
          k === REALLOC ? `<span>moved from ${hex(tr.o[i])}${tr.fs[i] ? `, was ${fmt(tr.fs[i])} B` : ""}</span>` : "";
        items.push(`<div class="ev${i === end - 1 && at !== null ? " cur" : ""}${i >= end ? " later" : ""}" data-ev="${i}"
          title="${hex(tr.p[i])}"><span class="when num">${when}</span><span class="op ${op[0]}">${op[1]}</span>${sw(aid)}
          <span class="sz num">${size ? fmt(size) + " B" : "?"}</span><span class="fn">${esc(siteName(aid))}${extra}</span></div>`);
      }
      head = at === null ? `Newest ${fmt(end - from)} of ${fmt(tr.n)} events · click one to rewind to it`
        : `Events around the cursor (outlined) · click one to jump to it`;
      body = `<div class="evs">${items.join("")}</div>`;
    } else {
      const all = rows.reduce((a, r) => a + r[1][0], 0);
      const SHOW = 24, rest = rows.slice(SHOW);
      const other = rest.reduce((a, r) => [a[0] + r[1][0], a[1] + r[1][1]], [0, 0]);
      head = `${fmt(rows.length)} allocation sites · ${kib(all)} live · hover to highlight, click to keep it highlighted`;
      body = `<div class="grid">${rows.slice(0, SHOW).map(([id, [bytes, n]]) =>
        siteRow(id, `<b>${kib(bytes)}</b> · ${fmt(n)} block${n === 1 ? "" : "s"}`)).join("")}</div>` +
        (rest.length ? `<div class="note">${fmt(rest.length)} smaller sites: ${kib(other[0])} in ${fmt(other[1])} blocks</div>` : "");
    }
    const scroll = box.querySelector(".evs")?.scrollTop ?? 0;
    box.innerHTML = `<div class="tabs"><span class="seg">${tabs.map(([k, label]) =>
      `<button data-tab="${k}" aria-pressed="${k === tab}">${label}</button>`).join("")}</span><span class="sh">${head}</span></div>${body}`;
    const evs = box.querySelector(".evs");
    if (evs) evs.scrollTop = scroll;
  }

  function setFocus(id) {
    if (id === focus) return;
    focus = id;
    for (const v of views.values()) draw(v);
  }
  $("sites").onmouseover = (e) => { const r = e.target.closest(".site"); if (r) setFocus(+r.dataset.id); };
  $("sites").onmouseleave = () => setFocus(pinnedFocus);
  $("sites").onclick = (e) => {
    const t = e.target.closest("[data-tab]");
    if (t) { tab = t.dataset.tab; save("tab", tab); return render(); }
    const ev = e.target.closest(".ev");
    if (ev) {
      // Show the block just after its allocation, or just before its free.
      const i = +ev.dataset.ev, freeing = tr.k[i] === FREED;
      stopPlay();
      markPtr = tr.p[i];
      return setAt(freeing ? i : i + 1, freeing ? tr.t[i] - 0.001 : tr.t[i]);
    }
    const r = e.target.closest(".site");
    if (!r) return;
    pinnedFocus = pinnedFocus === +r.dataset.id ? -2 : +r.dataset.id;
    document.querySelectorAll("#sites .site").forEach((x) => x.classList.toggle("on", +x.dataset.id === pinnedFocus));
    setFocus(pinnedFocus);
  };
  document.querySelectorAll("#colorby button").forEach((b) => b.onclick = () => {
    mode = b.dataset.mode;
    save("mode", mode);
    pinnedFocus = focus = -2;
    if (snap) render();
  });

  // ---- Map ---------------------------------------------------------------------

  // Paint the heap as a grid of cells, each covering bpc bytes in address order.
  function draw(v) {
    const h = v.heap, size = h.end - h.start, now = v.clock ?? Date.now(), fade = v.clock === null ? FADE : PAST_FADE;
    const canvas = v.el.querySelector("canvas.map");
    const width = v.el.querySelector(".mapwrap").clientWidth || 600;
    const cols = Math.max(16, Math.floor(width / CELL));
    const maxRows = h.name === "PSRAM" ? 96 : 40;
    let bpc = 4;
    while (Math.ceil(size / bpc / cols) > maxRows) bpc *= 2;
    bpc = Math.max(4, Math.round(bpc / 2 ** v.zoom));
    const cells = Math.ceil(size / bpc), rows = Math.ceil(cells / cols);
    if (rows > 8192) { v.zoom--; return draw(v); }
    v.bpc = bpc; v.cols = cols;
    v.el.querySelector(".bpc").textContent = `${fmt(bpc)} B/cell`;

    // Per-cell color sums weighted by bytes, then blend.
    const R = new Float32Array(cells), G = new Float32Array(cells), B = new Float32Array(cells);
    const W = new Float32Array(cells), fresh = new Float32Array(cells), freed = new Float32Array(cells);
    const bad = new Uint8Array(cells), edge = new Uint8Array(cells), mark = new Uint8Array(cells);
    const add = (arr, off, len, w = 1) => {
      let a = off, e = Math.min(off + len, size);
      while (a < e) {
        const c = Math.floor(a / bpc), ce = Math.min(e, (c + 1) * bpc);
        arr[c] += (ce - a) * w;
        a = ce;
      }
    };
    const paint = (off, len, c) => {
      let a = off, e = Math.min(off + len, size);
      while (a < e) {
        const i = Math.floor(a / bpc), ie = Math.min(e, (i + 1) * bpc), n = ie - a;
        R[i] += c[0] * n; G[i] += c[1] * n; B[i] += c[2] * n; W[i] += n;
        a = ie;
      }
    };
    const flag = (arr, off, len) => { for (let c = Math.floor(off / bpc); c <= Math.floor((off + len - 1) / bpc); c++) arr[c] = 1; };
    const C = colors, b = h.blocks, sites = bySite() ? h.sites || [] : null;
    const dim = (c) => c.map((x, k) => x + (C.free[k] - x) * 0.82);
    paint(0, b.length ? b[0] : size, C.meta);
    if (b.length) paint(h.tail, size - h.tail, C.meta);
    for (let i = 0; i < b.length; i += 4) {
      const off = b[i], len = b[i + 1], fl = b[i + 2];
      if (fl & FREE) { paint(off, len, C.free); continue; }
      const id = sites ? sites[i / 4] ?? -1 : -3;
      const c = sites ? siteColor(id) : C.used;
      paint(off, len, focus > -2 && id !== focus ? dim(c) : c);
      if (fl & CORRUPT) flag(bad, off, len);
      if (markPtr && h.start + off + 12 === markPtr) flag(mark, off, len);
      if (len >= 2 * bpc) edge[Math.floor(off / bpc)] = 1;
      const t = v.born.get(off + ":" + len);
      if (t) {
        const w = 1 - (now - t) / fade;
        if (w > 0 && w <= 1) add(fresh, off, len, w);
      }
    }
    for (const f of v.freed) {
      const w = 1 - (now - f.t) / fade;
      if (w > 0 && w <= 1) add(freed, f.off, f.len, w);
    }

    canvas.width = cols; canvas.height = rows;
    canvas.style.width = cols * CELL + "px"; canvas.style.height = rows * CELL + "px";
    const ctx = canvas.getContext("2d"), img = ctx.createImageData(cols, rows), px = img.data;
    for (let c = 0; c < cells; c++) {
      const tot = W[c] || 1;
      let r = R[c] / tot, g = G[c] / tot, bl = B[c] / tot;
      const fw = Math.min(1, fresh[c] / tot), dw = Math.min(1, freed[c] / tot);
      if (fw > 0) { r += (C.fresh[0] - r) * fw; g += (C.fresh[1] - g) * fw; bl += (C.fresh[2] - bl) * fw; }
      if (dw > 0) { r += (C.freed[0] - r) * dw; g += (C.freed[1] - g) * dw; bl += (C.freed[2] - bl) * dw; }
      if (edge[c]) { r *= 0.78; g *= 0.78; bl *= 0.78; }
      if (bad[c]) [r, g, bl] = C.corrupt;
      if (mark[c]) [r, g, bl] = C.text;
      const p = c * 4;
      px[p] = r; px[p + 1] = g; px[p + 2] = bl; px[p + 3] = 255;
    }
    ctx.putImageData(img, 0, 0);
  }

  function spark(v) {
    const canvas = v.el.querySelector("canvas.spark"), h = v.heap, key = h.start.toString(16);
    const pts = history.filter((e) => e.h[key]).map((e) => [e.t, ...e.h[key]]);
    const dpr = devicePixelRatio || 1, w = canvas.clientWidth, ht = canvas.clientHeight;
    canvas.width = w * dpr; canvas.height = ht * dpr;
    const ctx = canvas.getContext("2d");
    ctx.scale(dpr, dpr);
    const label = v.el.querySelector(".tl");
    if (pts.length < 2) { label.textContent = "collecting history…"; return; }
    let lo = Infinity, hi = -Infinity;
    for (const p of pts) { lo = Math.min(lo, p[2], p[1]); hi = Math.max(hi, p[1], p[2]); }
    if (hi - lo < 1024) { lo = Math.max(0, (hi + lo) / 2 - 512); hi = lo + 1024; }
    const t0 = pts[0][0], t1 = pts[pts.length - 1][0] || t0 + 1;
    const x = (t) => (t - t0) / (t1 - t0 || 1) * (w - 2) + 1, y = (b) => ht - 3 - (b - lo) / (hi - lo) * (ht - 6);
    const rgb = (c) => `rgb(${c.join(",")})`;
    ctx.lineWidth = 1.5; ctx.lineJoin = "round";
    for (const [idx, color, dash] of [[2, colors.fresh, [3, 3]], [1, colors.used, []]]) {
      ctx.strokeStyle = rgb(color); ctx.setLineDash(dash); ctx.beginPath();
      pts.forEach((p, i) => i ? ctx.lineTo(x(p[0]), y(p[idx])) : ctx.moveTo(x(p[0]), y(p[idx])));
      ctx.stroke();
    }
    if (at !== null && tr.t0) {
      const tc = (tr.t0 + cursorT) / 1000;   // the replay cursor on this live-snapshot trend
      if (tc >= t0 && tc <= t1) { ctx.fillStyle = rgb(colors.past); ctx.fillRect(Math.round(x(tc)) - 1, 0, 2, ht); }
    }
    const last = pts[pts.length - 1], mins = Math.round((t1 - t0) / 60);
    label.innerHTML = `<span style="color:var(--used)">━</span> free ${kib(last[1])} &nbsp;` +
      `<span style="color:var(--fresh)">┅</span> largest ${kib(last[2])}<br>` +
      `${kib(lo)}–${kib(hi)} · last ${mins ? mins + " min" : Math.round(t1 - t0) + " s"}`;
  }

  // What is under the pointer: a metadata region, or the biggest block in the cell.
  function hit(v, e) {
    const h = v.heap, rect = e.target.getBoundingClientRect();
    const col = Math.floor((e.clientX - rect.left) / CELL), row = Math.floor((e.clientY - rect.top) / CELL);
    const lo = (row * v.cols + col) * v.bpc, hi = lo + v.bpc, size = h.end - h.start;
    if (col >= v.cols || lo >= size) return null;
    const b = h.blocks, n = b.length / 4;
    // First block ending after lo.
    let a = 0, z = n;
    while (a < z) { const m = (a + z) >> 1; if (b[m * 4] + b[m * 4 + 1] <= lo) a = m + 1; else z = m; }
    if (!n || lo < b[0] || a >= n) {
      return { meta: lo < (b[0] ?? size) ? "heap + TLSF control" : "pool end", addr: h.start + lo };
    }
    let i = a, nUsed = 0, nFree = 0;
    for (; i < n && b[i * 4] < hi; i++) (b[i * 4 + 2] & FREE) ? nFree++ : nUsed++;
    let best = a;
    for (let j = a; j < i; j++) if (b[j * 4 + 1] > b[best * 4 + 1]) best = j;
    const off = b[best * 4], len = b[best * 4 + 1], fl = b[best * 4 + 2], req = b[best * 4 + 3];
    const block = h.start + off - 4;   // TLSF header (prev_phys_block)
    const used = !(fl & FREE), ev = h.ev?.[best] ?? -1;
    // The event log dates an allocation exactly; without it, use when the page first saw it.
    const born = !used ? undefined : ev >= 0 && ev < tr.n ? tr.t0 + tr.t[ev] : v.born.get(off + ":" + len);
    return { off, len, fl, req, block, used, nUsed, nFree, born, past: !!h.past,
             site: h.sites?.[best] ?? -1, stk: h.stk?.[best] ?? -1, ptr: block + (fl & POISONED ? 16 : 8) };
  }

  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  // "std::vector<A, std::allocator<A> >::vector(std::initializer_list<A>)" -> "std::vector<…>::vector()"
  function shortFn(fn) {
    let s = fn, prev;
    do { prev = s; s = s.replace(/<[^<>]*>/g, "‹›"); } while (s !== prev);
    return s.replace(/‹›/g, "<…>").replace(/\((?!\)).*\)( const)?$/, "()");
  }
  const shortLoc = (loc) => loc.replace(/ \(discriminator \d+\)$/, "").replace(/^.*\//, "");

  // The symbolized stack for a stack id: a value once fetched, else a Promise.
  function stackFor(id) {
    if (!stacks.has(id)) {
      stacks.set(id, api.stack(id).catch(() => null)
        .then((s) => { stacks.set(id, s); return s; }));
    }
    return stacks.get(id);
  }

  function stackHtml(s, full, siteId = -1) {
    if (s instanceof Promise) return `<div class="stk"><div class="hint">looking up the allocation stack…</div></div>`;
    if (!snap?.traced) {
      const why = snap?.traceNote || "Start QEMU with the heaptrace plugin to see where blocks were allocated.";
      return `<div class="stk"><div class="hint">${esc(why)}</div></div>`;
    }
    if (!s) return `<div class="stk"><div class="hint">No stack recorded for this allocation.</div></div>`;
    const fr = s.frames, site = fr[s.site];
    const loc = (f) => f.loc ? `<span class="loc" title="${esc(f.loc)}">${esc(shortLoc(f.loc))}</span>` : "";
    const swatch = bySite() && siteId >= 0 ? `<i style="background:${css(siteColor(siteId))}"></i>` : "";
    const by = site ? `<div class="by">${swatch}allocated by <b title="${esc(site.fn)}">${esc(shortFn(site.fn))}</b>${loc(site)}</div>` : "";
    const first = full ? 0 : s.caller, last = full ? fr.length : Math.min(fr.length, s.caller + 8);
    const items = fr.slice(first, last).map((f, k) => {
      const cls = first + k === s.site ? "site" : f.alloc ? "alloc" : f.lib ? "lib" : "";
      const inl = full ? f.inl.map((x) => `<div class="inl">inlined ${esc(x.fn)}${loc(x)}</div>`).join("") : "";
      return `<li class="${cls}"${full ? ` title="${hex(f.addr)}"` : ""}>${esc(full ? f.fn : shortFn(f.fn))}${loc(f)}${inl}</li>`;
    }).join("");
    const more = full ? "" : [s.caller ? `${s.caller} allocator frames hidden` : "",
      fr.length > last ? `${fr.length - last} more frames` : ""].filter(Boolean).join(" · ");
    return `<div class="stk">${by}<ol>${items}</ol>${more ? `<div class="more">${more}${full ? "" : " · click for the full stack"}</div>` : ""}</div>`;
  }

  function blockRows(x) {
    if (x.meta) return [["region", x.meta], ["address", hex(x.addr)]];
    const rows = [["state", x.fl & CORRUPT ? "allocated · CORRUPT tail canary" : x.used ? "allocated" : "free"],
                  ["block", hex(x.block)]];
    if (x.used) rows.push(["pointer", hex(x.ptr)]);
    rows.push([x.past ? "block size ≈" : "block size", fmt(x.len - 4) + " B"]);
    if (x.fl & POISONED) rows.push(["requested", fmt(x.req) + " B"]);
    if (x.used) {
      const ref = x.past ? tr.t0 + cursorT : snap?.time ? snap.time * 1000 : Date.now();
      rows.push(["age", x.born ? Math.max(0, (ref - x.born) / 1000).toFixed(x.past ? 3 : 0) + " s" : "before viewer"]);
    }
    if (x.nUsed + x.nFree > 1) rows.push(["in this cell", `${x.nUsed} allocated, ${x.nFree} free`]);
    return rows.map(([k, val]) => `<div class="row"><span>${k}</span><span class="num">${val}</span></div>`).join("");
  }

  let lastTip = null;
  function tip(v, e) {
    const t = $("tip"), x = hit(v, e);
    lastTip = [v, { target: e.target, clientX: e.clientX, clientY: e.clientY }];
    if (!x) { t.hidden = true; tipKey = null; return; }
    let html = blockRows(x);
    if (x.used && x.fl & POISONED) {
      const s = x.stk >= 0 ? stackFor(x.stk) : null;
      tipKey = x.ptr;
      if (s instanceof Promise) s.then(() => { if (tipKey === x.ptr && !t.hidden) tip(...lastTip); });
      html += stackHtml(s, false, x.site);
    }
    t.innerHTML = html;
    t.hidden = false;
    const tw = t.offsetWidth, th = t.offsetHeight;
    t.style.left = Math.min(e.clientX + 14, innerWidth - tw - 8) + "px";
    t.style.top = Math.max(8, e.clientY + 14 + th > innerHeight ? e.clientY - th - 10 : e.clientY + 14) + "px";
  }

  // Click a block to keep its full stack open under the map.
  function pin(v, x) {
    const d = v.el.querySelector(".detail");
    if (!x || !x.used) { d.hidden = true; return; }
    const s = (x.fl & POISONED) && x.stk >= 0 ? stackFor(x.stk) : null;
    d.innerHTML = `<div class="dh num"><b>${hex(x.ptr)}</b><span>${fmt(x.fl & POISONED ? x.req : x.len - 4)} B</span>
      <button title="Close">×</button></div>` + (x.fl & POISONED ? stackHtml(s, true, x.site) : "");
    d.querySelector("button").onclick = () => d.hidden = true;
    d.hidden = false;
    v.pinned = x.ptr;
    if (s instanceof Promise) s.then(() => { if (!d.hidden && v.pinned === x.ptr) pin(v, x); });
  }

  // ---- Polling -------------------------------------------------------------------

  async function poll() {
    clearTimeout(timer);
    let interval = 1;
    try {
      snap = await api.snapshot();
      interval = snap.interval || 1;
      if (!snap.error) {
        const last = history[history.length - 1];
        if (!last || last.t !== snap.time) {
          history.push({ t: snap.time, h: Object.fromEntries(snap.heaps.map((h) =>
            [h.start.toString(16), [h.infoFree, h.largest, h.infoMinFree]])) });
          if (history.length > 900) history.splice(0, history.length - 900);
        }
        await fetchEvents();
      }
    } catch (e) {
      snap = { error: "the viewer is not responding (" + (e?.message ?? e) + ")", heaps: snap?.heaps || [], time: Date.now() / 1000 };
      interval = 2;
    }
    render();
    if (!paused) timer = setTimeout(poll, interval * 1000);
  }

  // Fade the just-allocated/just-freed highlights between live snapshots.
  let lastFade = 0;
  function animate(ts) {
    if (ts - lastFade > 150 && snap && at === null) {
      lastFade = ts;
      const now = Date.now();
      for (const v of views.values()) {
        const active = v.freed.length || [...v.born.values()].some((t) => t && now - t < FADE + 200);
        if (active) draw(v);
      }
    }
    requestAnimationFrame(animate);
  }

  $("pause").onclick = () => {
    paused = !paused;
    $("pause").textContent = paused ? "Resume" : "Pause";
    if (paused) { clearTimeout(timer); if (snap) render(); } else poll();
  };
  addEventListener("resize", () => { if (snap) render(); });
  const themeChanged = () => { readColors(); if (snap) render(); };
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", themeChanged);
  new MutationObserver(themeChanged).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

  readColors();
  api.history().then((h) => history.push(...h)).catch(() => {}).finally(poll);
  requestAnimationFrame(animate);
}
