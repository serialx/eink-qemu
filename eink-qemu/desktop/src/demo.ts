/* Mock content for layout work in a plain browser (pnpm dev, or ?demo): an
   SD listing, a console log, a running X4 Pro with a drawn page and heaps
   that change with every snapshot. */
import type { Heap, HeapSite, HeapSnapshot, Setup } from './api';
/** Whether the mock firmware card has an ELF: without one, snapshots are unlabelled. */
let demoElf = true;
import { attachHeap, heapSession, setElf } from './heap';
import { attachSetup, paintSetup } from './setup';
const $ = (id: string) => document.getElementById(id)!;

interface Entry { name: string; size: number; isDir: boolean }
const entries: Entry[] = [
  { name: '.crosspoint', size: 0, isDir: true },
  { name: 'books', size: 0, isDir: true },
  { name: 'fonts', size: 0, isDir: true },
  { name: 'sleep', size: 0, isDir: true },
  { name: 'Alice in Wonderland.epub', size: 189_440, isDir: false },
  { name: 'Moby Dick.epub', size: 1_203_712, isDir: false },
  { name: 'Pride and Prejudice.epub', size: 731_136, isDir: false },
  { name: 'settings.bin', size: 1_024, isDir: false },
];

function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GiB`;
}
function icon(name: string) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'icon');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}

function renderSd() {
  const root = document.createElement('button');
  root.textContent = 'SD card';
  root.setAttribute('aria-current', 'location');
  root.disabled = true;
  $('sd-path').replaceChildren(root);
  $('sd-info').textContent = 'FAT32 · 61.2 MiB free of 63.9 MiB';
  $('sd-meter').style.width = '4%';
  const card = '/Users/me/Library/Application Support/net.serialx.eink-qemu/cards/', name = 'reading-test.img';
  $('sd-save').querySelector('.sd-dir')!.textContent = card;
  $('sd-save').querySelector('.sd-name')!.textContent = name;
  $('sd-save').title = card + name;
  $('sd-reveal').hidden = false;
  const rows = $('sd-rows');
  rows.replaceChildren();
  for (const entry of entries) {
    const row = document.createElement('tr');
    const name = document.createElement('td');
    const label = document.createElement('span');
    label.textContent = entry.name;
    if (entry.isDir) {
      const open = document.createElement('button');
      open.className = 'dir entry';
      open.type = 'button';
      open.append(icon('folder'), label);
      name.append(open);
    } else {
      const file = document.createElement('div');
      file.className = 'entry';
      file.append(icon('file'), label);
      name.append(file);
    }
    const size = document.createElement('td');
    size.className = 'size';
    size.textContent = entry.isDir ? '' : formatSize(entry.size);
    const ops = document.createElement('td');
    ops.className = 'ops';
    for (const [text, symbol] of [['Download', 'download'], ['Rename', 'pencil'], ['Delete', 'trash']]) {
      if (text === 'Download' && entry.isDir) continue;
      const button = document.createElement('button');
      button.type = 'button';
      button.title = text;
      button.setAttribute('aria-label', `${text} ${entry.name}`);
      button.append(icon(symbol));
      ops.append(button);
    }
    row.append(name, size, ops);
    rows.append(row);
  }
}

const keyHints: Record<string, string> = { Back: 'Esc', Confirm: 'Enter', Left: '←', Right: '→', Up: '↑', Down: '↓', Power: 'P', Home: 'Home' };
function renderKeys(machine: string) {
  const keys = $('keys');
  keys.replaceChildren();
  keys.dataset.machine = machine;
  const names = machine === 'x3' || machine === 'x4' ? ['Back', 'Confirm', 'Up', 'Down', 'Left', 'Right', 'Power']
    : machine === 'picco' ? ['Up', 'Down', 'Power'] : ['Home', 'Up', 'Down', 'Power'];
  $('key-hint').textContent = machine === 'x4pro' ? 'Keyboard: ↑ ↓ · Home · P · touch the screen' : 'Keyboard: arrows · Enter · Esc · P';
  for (const name of names) {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = name;
    button.dataset.kbd = keyHints[name];
    keys.append(button);
  }
}

/* A gray8 page as the eink backend would send it, drawn into the canvas. */
function renderScreen(width: number, height: number) {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d')!;
  ctx.fillStyle = '#f2f1ec';
  ctx.fillRect(0, 0, width, height);
  ctx.fillStyle = '#1a1a1a';
  ctx.font = '600 26px Lora, Georgia, serif';
  ctx.fillText('Chapter I', 44, 92);
  ctx.font = '600 20px Lora, Georgia, serif';
  ctx.fillText('Down the Rabbit-Hole', 44, 124);
  ctx.font = '18px Georgia, serif';
  const text = 'Alice was beginning to get very tired of sitting by her sister on the bank, and of having nothing to do: once or twice she had peeped into the book her sister was reading, but it had no pictures or conversations in it, “and what is the use of a book,” thought Alice “without pictures or conversations?” So she was considering in her own mind (as well as she could, for the hot day made her feel very sleepy and stupid), whether the pleasure of making a daisy-chain would be worth the trouble of getting up and picking the daisies, when suddenly a White Rabbit with pink eyes ran close by her.';
  let line = '', y = 172;
  for (const word of text.split(' ')) {
    const next = line ? `${line} ${word}` : word;
    if (ctx.measureText(next).width > width - 88) { ctx.fillText(line, 44, y); line = word; y += 30; }
    else line = next;
  }
  ctx.fillText(line, 44, y);
  ctx.fillStyle = '#6b6b6b';
  ctx.font = '14px Georgia, serif';
  ctx.fillText('Alice in Wonderland', 44, height - 34);
  ctx.fillText('3 / 142', width - 100, height - 34);
  $('screen').replaceChildren(canvas);

  // Fit: the largest size that fits the stage, as the page's default.
  const device = document.querySelector<HTMLElement>('.device')!;
  const fit = () => {
    const scale = Math.min((device.clientWidth - 68) / width, (device.clientHeight - 48) / height);
    canvas.style.width = `${Math.floor(width * scale)}px`;
    canvas.style.height = `${Math.floor(height * scale)}px`;
  };
  new ResizeObserver(fit).observe(device);
  fit();
}

const log = `ESP-ROM:esp32s3-20210327
Build:Mar 27 2021
rst:0x1 (POWERON),boot:0x8 (SPI_FAST_FLASH_BOOT)
SPIWP:0xee
mode:DIO, clock div:1
load:0x3fce3818,len:0x508
entry 0x403c9880
I (31) boot: ESP-IDF v5.3.2 2nd stage bootloader
I (31) boot: compile time Oct  2 2026 10:14:22
I (32) boot: Multicore bootloader
I (36) boot: chip revision: v0.2
I (52) boot: Partition Table:
I (61) boot:  0 nvs              WiFi data        01 02 00009000 00005000
I (68) boot:  1 otadata          OTA data         01 00 0000e000 00002000
I (75) boot:  2 app0             OTA app          00 10 00010000 00640000
I (101) boot: Loaded app from partition at offset 0x10000
[    212][I][main.cpp:58] setup(): CrossPoint 1.6.5 (x4pro)
[    214][I][Display.cpp:41] begin(): UC8279 rev 0x68, 528x792
[    388][I][SdCard.cpp:77] mount(): FAT32, 63.9 MiB, 512 B clusters
[    401][I][Library.cpp:112] scan(): 3 books
[    512][I][Reader.cpp:203] open(): /Alice in Wonderland.epub
[    846][I][Display.cpp:188] refresh(): full, 642 ms
`;

/* Heaps: TLSF-like block lists that allocate and free a little each time,
   each block from one of a few made-up allocation sites. */
const sites: HeapSite[] = [
  'Display::begin()', 'xTaskCreatePinnedToCore', 'ActivityManager::begin()', 'FontRegistry::discover()',
  'xQueueGenericCreate', 'Reader::open(char const*)',
  'std::_Function_handler<void (std::shared_ptr<epub::Chapter>), Reader::open(char const*)::{lambda(std::shared_ptr<epub::Chapter>)#1}>::_M_invoke(std::_Any_data const&, std::shared_ptr<epub::Chapter>&&)', 'GfxRenderer::insertFont(int)',
  'nvs::PageManager::load()', 'setup()', 'pthread_mutex_init', 'HalGPIO::begin()',
].map((fn, i) => ({ fn, loc: `/Users/me/crosspoint/src/activities/reader/very/deep/path/file${i}.cpp:${40 + i * 7}` }));
/** A made-up stack for a block from `site`: the allocator, the site, its callers. */
function mockStack(site: number) {
  const frame = (fn: string, loc: string, alloc = false, lib = false) => ({ addr: 0x42000000 + fn.length * 16, fn, loc, inl: [], alloc, lib });
  const idf = '/Users/me/.platformio/packages/framework-espidf/components';
  return {
    frames: [
      frame('multi_heap_malloc', `${idf}/heap/multi_heap_poisoning.c:243`, true, true),
      frame('heap_caps_malloc', `${idf}/heap/heap_caps.c:84`, true, true),
      frame('operator new(unsigned int)', '/builds/idf/crosstool-NG/libstdc++-v3/libsupc++/new_op.cc:50', true, true),
      { ...frame(sites[site].fn, sites[site].loc), inl: [{ fn: 'std::make_shared<epub::Chapter>()', loc: '/toolchain/include/c++/bits/shared_ptr.h:1010' }] },
      frame('ActivityManager::loop()', '/Users/me/crosspoint/src/ActivityManager.cpp:120'),
      frame('loop()', '/Users/me/crosspoint/src/main.cpp:512'),
      frame('loopTask(void*)', `${idf}/../framework-arduinoespressif32/cores/esp32/main.cpp:74`, false, true),
    ],
    caller: 3, site: 3,
  };
}
function mockHeap(name: string, caps: string[], start: number, size: number, avg: number, seed: number, fill = 0.9) {
  let state = seed;
  const random = () => (state = (state * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
  const pool = 0x3a0, sizes: [number, boolean, number][] = [];
  let site = 0;
  for (let used = pool; used < size * fill;) {
    const len = 16 + Math.floor(random() ** 3 * avg) * 4;
    if (random() < 0.4) site = Math.floor(random() ** 2 * sites.length);
    sizes.push([len, random() < 0.25, site]);
    used += len + 4;
  }
  const snapshot = (): Heap => {
    for (let i = 0; i < 6 && sizes.length; i++) {
      const k = Math.floor(random() * sizes.length);
      sizes[k][1] = !sizes[k][1];
    }
    const blocks: number[] = [], blockSites: number[] = [];
    let off = pool, free = 0, used = 0, largest = 0, nfree = 0;
    for (const [len, isFree, site] of sizes) {
      blocks.push(off, len + 4, isFree ? 1 : 2, isFree ? 0 : len - 12);
      blockSites.push(isFree ? -1 : site);
      if (isFree) { free += len; nfree++; largest = Math.max(largest, len); } else used += len;
      off += len + 4;
    }
    const rest = size - off - 16;
    blocks.push(off, rest + 4, 1, 0);
    blockSites.push(-1);
    free += rest; nfree++; largest = Math.max(largest, rest);
    return { name, caps, start, end: start + size, pool: pool - 4, tail: off + rest + 12, free, used, largest,
      count: sizes.length + 1, nfree, infoFree: free, infoMinFree: Math.round(free * 0.82), corrupt: 0, blocks,
      sites: blockSites, stk: blockSites };
  };
  return snapshot;
}

export function demo({ appendConsole }: { appendConsole: (text: string) => void }) {
  document.querySelector('.app')!.classList.add('live');
  ($('start') as HTMLButtonElement).disabled = true;
  ($('stop') as HTMLButtonElement).disabled = false;
  ($('reset') as HTMLButtonElement).disabled = false;
  ($('zoom-level') as HTMLSelectElement).disabled = false;
  ($('zoom-in') as HTMLButtonElement).disabled = false;
  ($('zoom-out') as HTMLButtonElement).disabled = false;
  ($('screenshot') as HTMLButtonElement).disabled = false;
  $('flash-name').textContent = 'crosspoint-1.6.5-x4pro.bin';
  $('flash-name').classList.remove('empty');
  $('machine').textContent = 'Xteink X4 Pro · 480 × 800';
  $('status').textContent = 'Running · Turbo (not timing-accurate) · reader open';
  renderSd();
  renderKeys('x4pro');
  renderScreen(480, 800);
  appendConsole(log);
  const heaps = [
    mockHeap('DRAM', ['32BIT', '8BIT', 'INTERNAL', 'DEFAULT'], 0x3fc9a000, 0x4c000, 220, 7),
    mockHeap('DRAM', ['32BIT', '8BIT', 'DMA', 'INTERNAL', 'DEFAULT'], 0x3fcf0000, 0x8000, 0, 5, 0),
    mockHeap('RTC', ['32BIT', '8BIT', 'INTERNAL', 'DEFAULT', 'RTCRAM'], 0x600fe000, 0x1e00, 24, 3),
    mockHeap('PSRAM', ['32BIT', '8BIT', 'SPIRAM', 'DEFAULT'], 0x3d000000, 0x800000, 9000, 11),
  ];
  // Every snapshot is kept, as Rust keeps one per trend point.
  const kept: HeapSnapshot[] = [];
  attachHeap({
    snapshot: async (): Promise<HeapSnapshot> => {
      const snap: HeapSnapshot = { time: Date.now() / 1000, heaps: heaps.map((h) => h()), missing: 0, took: 4,
        traced: demoElf, traceNote: null, elf: demoElf ? 'firmware.elf' : null, elfNote: null };
      if (!demoElf) for (const h of snap.heaps) { delete h.sites; delete h.stk; }
      kept.push(snap);
      if (kept.length > 900) kept.shift();
      return snap;
    },
    at: async (time: number) => {
      if (!kept.length) return null;
      let i = 0;
      kept.forEach((k, j) => { if (Math.abs(k.time - time) < Math.abs(kept[i].time - time)) i = j; });
      return { snapshot: kept[i], prev: kept[i - 1] ?? null };
    },
    stack: async (id: number) => id >= 0 && id < sites.length ? mockStack(id) : null,
    sites: async (from: number) => sites.slice(from),
    history: async () => [],
    openWindow: async () => {},
    pickElf: async () => ({ path: '/Users/me/crosspoint/.pio/build/x4pro/firmware.elf', name: 'firmware.elf', size: 31_457_280 }),
  }, { path: '/Users/me/crosspoint/.pio/build/x4pro/firmware.elf', name: 'firmware.elf', size: 31_457_280 });
  heapSession('running', 1);
  demoSetup();
}

/* The firmware card over two PlatformIO builds; choices change the mock. */
function demoSetup() {
  const build = (env: string, chip: 'ESP32-C3' | 'ESP32-S3', version: string, size: number, modified: number) => ({
    path: `/Users/me/crosspoint/.pio/build/${env}/firmware.bin`, name: 'firmware.bin', size,
    folder: `~/crosspoint/.pio/build/${env}`, parent: env, modified, chip, version, project: 'crosspoint',
  });
  const builds = {
    x4pro: build('x4pro', 'ESP32-S3', '1.6.5-37-g7394e40f-dirty', 5_955_968, Date.parse('2026-10-12T21:22:00')),
    default: build('default', 'ESP32-C3', '1.6.5-37-g7394e40f-dirty', 5_926_608, Date.parse('2026-10-12T21:33:00')),
  };
  const elfOf = (env: string) => ({ path: `/Users/me/crosspoint/.pio/build/${env}/firmware.elf`, name: 'firmware.elf', size: 88_223_200 });
  let setup: Setup = {
    settings: { device: 'x4pro', panel: 'uc8279-68', wifi: true, ghosting: true, turbo: true, firmware: builds.x4pro.path,
      card: null, elf: elfOf('x4pro').path, elfAuto: true },
    firmware: builds.x4pro, elf: elfOf('x4pro'), elfMatches: true, elfSkipped: null,
    recent: [
      { path: builds.x4pro.path, name: 'firmware.bin', parent: 'x4pro', device: 'x4pro', elf: true },
      { path: builds.default.path, name: 'firmware.bin', parent: 'default', device: 'x3', elf: true },
    ],
  };
  const paint = (next: Setup) => { setup = next; demoElf = !!next.elf; setElf(next.elf); paintSetup(setup, true); };
  const settings = (patch: Partial<Setup['settings']>) => paint({ ...setup, settings: { ...setup.settings, ...patch } });
  attachSetup({
    choose: () => {},
    recent: (path) => {
      const env = path.includes('/x4pro/') ? 'x4pro' : 'default';
      const entry = setup.recent.find((r) => r.path === path)!;
      paint({ ...setup, firmware: builds[env], elf: elfOf(env), settings: { ...setup.settings, device: entry.device, firmware: path, elfAuto: true },
        recent: [entry, ...setup.recent.filter((r) => r !== entry)] });
    },
    device: (device) => settings({ device }),
    panel: (panel) => settings({ panel }),
    option: (option, on) => settings({ [option]: on }),
    chooseElf: () => paint({ ...setup, elf: elfOf(setup.firmware!.parent), elfMatches: true, settings: { ...setup.settings, elfAuto: false } }),
    forgetElf: () => paint({ ...setup, elf: null, elfMatches: null, settings: { ...setup.settings, elf: null, elfAuto: false } }),
  });
  paint(setup);
}
