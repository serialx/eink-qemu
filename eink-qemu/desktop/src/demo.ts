/* Mock content for layout work in a plain browser (pnpm dev, or ?demo): an
   SD listing, a console log and a running X4 Pro with a drawn page. */
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
  $('sd-save').textContent = '~/Library/Application Support/eink-qemu/sd.img';
  $('sd-save').title = '~/Library/Application Support/eink-qemu/sd.img';
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
}
