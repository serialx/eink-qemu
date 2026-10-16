/* The browser backend: the WebAssembly build of QEMU in a runtime iframe
   (../wasm/web/runtime.html), the SD card as a FAT32 image in page memory
   that fat.js edits and this browser keeps (sd-store.js), and files from
   pickers and drops. fat.js, sd-ops.js and sd-store.js are classic scripts
   the web build's index.html loads (vite.config.ts); they are globals here.

   The page owns the card image while the emulator is not running; the
   runtime iframe owns it (MEMFS /sd.img) from boot until the stop snapshot
   returns it. Explorer operations go to whichever owns it. */
import type { AttachHandlers, Backend, DropHandlers, FileRef, Screen } from './backend';
import {
  AppError, type CardInfo, type FirmwareInfo, type MadeCard, type SdListing, type SessionInfo,
  type Settings, type Setup, type StartOptions, type UploadPlan,
} from './types';

const params = new URLSearchParams(location.search);
const MACHINES: Machine[] = ['x3', 'x4', 'x4pro', 'picco'];
const PANELS: Settings['panel'][] = ['uc8279-68', 'uc8179'];
const shortNames: Record<Machine, string> = { x3: 'X3', x4: 'X4', x4pro: 'X4 Pro', picco: 'BOOX Picco' };
const MiB = 1024 ** 2;
const notSupported = (what: string) => Promise.reject(new AppError('ENOTSUP', `${what} is not available in the browser.`));
const errorMessage = (error: unknown) => (error as Error)?.message ?? String(error);

async function fetchBytes(url: string): Promise<Uint8Array> {
  const response = await fetch(url);
  if (!response.ok) throw new AppError('EIO', `Cannot load ${url}: HTTP ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}
function saveBlob(bytes: Uint8Array, name: string) {
  const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: 'application/octet-stream' }));
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/* Settings: the URL first (?device= ?panel= ?wifi=off ?ghosting=0 ?turbo=0),
   then what this browser saved, then the defaults. */
const KEYS = { device: 'eink-qemu-device', panel: 'eink-qemu-panel', wifi: 'eink-qemu-wifi', ghosting: 'eink-qemu-ghosting', turbo: 'eink-qemu-turbo' };
const saved = (key: string) => { try { return localStorage.getItem(key); } catch { return null; } };
function loadSettings(): Settings {
  const device = params.get('device') ?? saved(KEYS.device);
  const panel = params.get('panel') ?? saved(KEYS.panel);
  const on = (name: 'wifi' | 'ghosting' | 'turbo') => {
    const given = params.get(name);
    if (given !== null) return given !== '0' && given !== 'off';
    const kept = saved(KEYS[name]);
    return kept === null ? true : kept !== '0';
  };
  return {
    device: device === 'auto' || MACHINES.includes(device as Machine) ? device as Settings['device'] : 'auto',
    panel: PANELS.includes(panel as Settings['panel']) ? panel as Settings['panel'] : 'uc8279-68',
    wifi: on('wifi'), ghosting: on('ghosting'), turbo: on('turbo'),
    firmware: null, card: null, elf: null, elfAuto: false,
  };
}
let settings = loadSettings();

/* Files: pickers and drops hand the page a token; the File or entry stays here. */
type Source = { kind: 'file'; file: File } | { kind: 'entry'; entry: FileSystemEntry } | { kind: 'url'; url: string };
const sources = new Map<string, Source>();
let refs = 0;
function register(source: Source): FileRef {
  const ref = `file:${++refs}`;
  sources.set(ref, source);
  const name = source.kind === 'file' ? source.file.name : source.kind === 'entry' ? source.entry.name : source.url.split('/').pop() ?? source.url;
  return { ref, name };
}
function sourceOf(ref: string): Source {
  const source = sources.get(ref);
  if (!source) throw new AppError('ENOENT', 'That file is no longer available; choose it again.');
  return source;
}
const entryFile = (entry: FileSystemFileEntry) => new Promise<File>((resolve, reject) => entry.file(resolve, reject));
async function fileOf(ref: string): Promise<File> {
  const source = sourceOf(ref);
  if (source.kind === 'file') return source.file;
  if (source.kind === 'entry' && source.entry.isFile) return entryFile(source.entry as FileSystemFileEntry);
  throw new AppError('EISDIR', 'Choose a file, not a folder.');
}

/* Hidden file inputs, one per purpose, so a picker is one element automation
   can set files on. A pick waits for the input's next change or cancel. */
const inputs = { firmware: 'flash-file', card: 'sd-file', sd: 'sd-files' } as const;
function input(id: string, accept: string, multiple: boolean) {
  let element = document.getElementById(id) as HTMLInputElement | null;
  if (!element) {
    element = document.createElement('input');
    element.type = 'file';
    element.id = id;
    element.hidden = true;
    element.setAttribute('aria-hidden', 'true');
    element.tabIndex = -1;
    document.body.append(element);
  }
  element.accept = accept;
  element.multiple = multiple;
  return element;
}
function pick(id: string, accept: string, multiple = false): Promise<File[]> {
  const element = input(id, accept, multiple);
  return new Promise((resolve) => {
    let timer = 0;
    const done = (files: File[]) => {
      clearTimeout(timer);
      element.removeEventListener('change', onChange);
      element.removeEventListener('cancel', onCancel);
      window.removeEventListener('focus', onFocus);
      element.value = '';
      resolve(files);
    };
    const onChange = () => done([...element.files ?? []]);
    const onCancel = () => done([]);
    // Browsers without the cancel event: the window regains focus with no files.
    const onFocus = () => { timer = window.setTimeout(() => { if (!element.files?.length) done([]); }, 1000); };
    element.addEventListener('change', onChange);
    element.addEventListener('cancel', onCancel);
    window.addEventListener('focus', onFocus);
    element.click();
  });
}

/* The firmware: a picked or dropped file, or the ?flash= URL. */
interface Firmware { source: Source; info: FirmwareInfo; bytes: Uint8Array | null }
let firmware: Firmware | null = null;
const HEAD = 0xe0000 + 0x100;   // through the Picco app partition's descriptor

/** What the image header says, and the app descriptor's version and project. */
function describeFirmware(name: string, size: number, head: Uint8Array, modified: number | null): FirmwareInfo {
  const chipOf = (at: number) => ({ 5: 'ESP32-C3', 9: 'ESP32-S3' } as const)[head[at + 12] | (head[at + 13] << 8)] ?? null;
  let chip: FirmwareInfo['chip'] = null;
  if (size === 0x1000000) chip = head[12] === 5 ? 'ESP32-C3' : 'ESP32-S3';
  else if (head[0] === 0xe9 && head.length >= 14) chip = chipOf(0);
  else if (XOTA.isXota(head) || /\.xota$/i.test(name)) chip = 'ESP32-S3';
  if (!chip) throw new AppError('EINVAL', 'This file is not an ESP32-C3 or ESP32-S3 firmware image.');
  let version: string | null = null, project: string | null = null;
  // esp_app_desc_t follows the image header and the first segment header.
  for (const at of [32, 0x10000 + 32, 0xe0000 + 32]) {
    if (at + 80 > head.length) continue;
    if (head[at] !== 0x32 || head[at + 1] !== 0x54 || head[at + 2] !== 0xcd || head[at + 3] !== 0xab) continue;
    const text = (from: number) => {
      const bytes = head.subarray(at + from, at + from + 32);
      const end = bytes.indexOf(0);
      return new TextDecoder().decode(end < 0 ? bytes : bytes.subarray(0, end)).trim() || null;
    };
    version = text(16);
    project = text(48);
    break;
  }
  return { path: name, name, size, folder: '', parent: '', modified, chip, version, project };
}
async function useFile(file: File): Promise<Setup> {
  const head = new Uint8Array(await file.slice(0, HEAD).arrayBuffer());
  firmware = { source: { kind: 'file', file }, info: describeFirmware(file.name, file.size, head, file.lastModified || null), bytes: null };
  return setupOf();
}
const setupOf = (): Setup => ({
  settings: { ...settings, firmware: firmware?.info.name ?? null, card: card?.name ?? null },
  firmware: firmware?.info ?? null, elf: null, elfMatches: null, elfSkipped: null, recent: [],
});

/* The screen: the runtime iframe, which keeps the panel's resolution and is
   scaled as a whole. SDL inside it takes the pointer and the keyboard. */
class FrameScreen implements Screen {
  el: HTMLIFrameElement;
  width: number;
  height: number;
  readonly ownPointer = true;
  constructor(machine: Machine, resized: () => void) {
    [this.width, this.height] = machine === 'x3' ? [528, 792] : [480, 800];
    this.el = document.createElement('iframe');
    this.el.width = String(this.width);
    this.el.height = String(this.height);
    this.el.dataset.machine = machine;
    this.el.title = `${shortNames[machine]} display`;
    // The canvas exists once the runtime page loads; size it then too.
    this.el.addEventListener('load', resized);
  }
  resize(scale: number, exact: boolean) {
    // Keep SDL's viewport at the panel resolution: resizing it to the layout
    // would resample the framebuffer before the browser ever draws the canvas.
    this.el.style.transform = `scale(${scale})`;
    const canvas = this.canvas();
    if (canvas) canvas.style.imageRendering = exact ? '' : 'auto';
  }
  canvas() { return this.el.contentDocument?.querySelector('canvas') ?? null; }
  dispose() {
    if (frame === this) { frame = null; sdAbort('The emulator stopped.'); }
    this.el.remove();
  }
}

let handlers: AttachHandlers | null = null;
let frame: FrameScreen | null = null;
let session: SessionInfo = { state: 'idle', machine: null, turbo: false, exit: null, run: 0, seq: 0 };
function setSession(patch: Partial<SessionInfo>) {
  session = { ...session, ...patch, seq: session.seq + 1 };
  handlers?.session(session);
}
const post = (data: unknown, transfer: Transferable[] = []) => frame?.el.contentWindow?.postMessage(data, location.origin, transfer);
let bootData: Record<string, unknown> | null = null;
let starting: { resolve(): void; reject(error: Error): void } | null = null;
let readyTimer = 0;

/* The log, batched: a turbo boot prints thousands of lines a second. */
const pendingLog: string[] = [];
let logTimer = 0;
function log(line: string) {
  pendingLog.push(line);
  logTimer ||= window.setTimeout(() => { logTimer = 0; handlers?.log(pendingLog.splice(0)); }, 16);
}

/* The card. */
let image: Uint8Array | null = null;
let card: CardInfo | null = null;
let loading: Promise<void> | null = null;
let saving = 0, snapshotPending = false;
const calls = new Map<number, { resolve(result: unknown): void; reject(error: Error): void }>();
let sequence = 0;
let copyWanted: ((name: string) => void) | null = null;

function cardNote(note: string, error = false, changed = false) {
  if (!card) return;
  card = { ...card, note, error };
  handlers?.card(card, changed);
}
async function saveSD(bytes: Uint8Array) {
  const revision = ++saving;
  cardNote('Saving SD card in this browser…');
  try {
    await sdStore.save(bytes);
    if (revision === saving) cardNote('SD card saved in this browser.' + (session.state === 'running' ? ' Device changes save every 10 seconds; Stop saves now.' : ' Safe to refresh.'));
  } catch (error) {
    if (revision === saving) cardNote(`Could not save SD card: ${errorMessage(error)} Download the SD image to keep changes.`, true);
  }
}
/** Take up a card: a picked file or a same-origin URL, or the saved card
    when `restore` and there is one. The page hears of the card busy while
    it loads (the explorer waits), then of the card as another image. */
function loadCard(source: File | string, restore: boolean) {
  image = null;
  const name = source instanceof File ? source.name : source.split('/').pop() || 'sd.img';
  card = { path: '', name, size: 0, note: 'Loading SD image…', busy: true };
  handlers?.card(card, false);
  loading = (async () => {
    let kept: Uint8Array | null = null, restoreFailed: string | null = null;
    if (restore) {
      try { kept = await sdStore.load(); } catch (error) { restoreFailed = `Could not restore the saved SD card: ${errorMessage(error)} Using a temporary card.`; }
    }
    const bytes = kept ?? new Uint8Array(source instanceof File ? await source.arrayBuffer() : await fetchBytes(source));
    if (!bytes.byteLength || bytes.byteLength % 512) throw new AppError('EINVAL', 'SD image must contain a whole number of 512-byte sectors.');
    image = bytes;
    card = { path: '', name: kept ? 'sd.img' : name, size: bytes.byteLength, busy: true };
    if (kept) cardNote('Saved SD card restored from this browser.');
    else if (restoreFailed) cardNote(restoreFailed, true);
    else await saveSD(image);
    card = { ...card, busy: false };
    handlers?.card(card, true);
  })();
  loading.catch((error) => {
    card = { path: '', name, size: 0, note: `Could not load the SD image: ${errorMessage(error)}`, error: true };
    handlers?.card(card, true);
    handlers?.problem(`SD image: ${errorMessage(error)}`);
  }).finally(() => { loading = null; });
  return loading;
}
function autosave() {
  if (session.state !== 'running' || snapshotPending || calls.size) return;
  snapshotPending = true;
  post({ type: 'autosave' });
}
/** After a crash the card is as last saved. */
async function recoverCard() {
  image = null;
  card = { path: '', name: 'sd.img', size: 0, note: 'Reading the saved SD card…', busy: true };
  handlers?.card(card, false);
  try {
    const kept = await sdStore.load();
    if (!kept) throw new Error('nothing is saved');
    image = kept;
    card = { path: '', name: 'sd.img', size: kept.byteLength, note: 'The SD card is as last saved in this browser.' };
  } catch (error) {
    card = { path: '', name: 'sd.img', size: 0, note: `The SD card was lost with the emulator: ${errorMessage(error)}. Open an SD image or make a blank card.`, error: true };
  }
  handlers?.card(card, true);
}

/* Explorer operations: on the page's image while the emulator is stopped,
   otherwise in the runtime iframe, which pauses the guest, applies them to
   /sd.img and resets the device after a change (runtime.js). */
async function sdCall<T = unknown>(op: string, args: Record<string, unknown> = {}, transfer: Transferable[] = []): Promise<T> {
  if (loading) await loading.catch(() => {});
  if (frame) {
    if (session.state !== 'running') throw new AppError('EBUSY', 'The emulator is still starting.');
    const id = ++sequence;
    return new Promise<T>((resolve, reject) => {
      calls.set(id, { resolve: resolve as (result: unknown) => void, reject });
      post({ type: 'sd', id, op, args }, transfer);
    });
  }
  if (!image) throw new AppError('ENOENT', 'No SD card is loaded.');
  try {
    return sdExecute(image, op, args) as T;
  } catch (error) {
    throw new AppError((error as { code?: string }).code || 'EIO', errorMessage(error));
  } finally {
    // Failed multi-file uploads may still have changed part of the card.
    if (sdMutates(op)) await saveSD(image);
  }
}
async function sdResult(data: { id: number; ok: boolean; result?: unknown; error?: { code: string; message: string }; reset: boolean; image?: Uint8Array }) {
  const call = calls.get(data.id);
  calls.delete(data.id);
  if (data.reset) handlers?.sdReset();
  // The edited card is saved before the operation counts as done, so a
  // reload right after it finds the change.
  if (data.image) await saveSD(data.image);
  if (!call) return;
  if (data.ok) call.resolve(data.result);
  else call.reject(new AppError(data.error?.code ?? 'EIO', data.error?.message ?? 'SD card operation failed.'));
}
function sdAbort(reason: string) {
  for (const call of calls.values()) call.reject(new AppError('EIO', reason));
  calls.clear();
}

/* The run. */
function endRun(exit: SessionInfo['exit']) {
  clearTimeout(readyTimer);
  bootData = null;
  snapshotPending = false;
  sdAbort('The emulator stopped.');
  frame = null;
  setSession({ state: 'idle', machine: null, exit });
}
function onMessage({ source, origin, data }: MessageEvent) {
  if (!frame || source !== frame.el.contentWindow || origin !== location.origin || !data?.type) return;
  switch (data.type) {
    case 'ready': {
      clearTimeout(readyTimer);
      const boot = bootData;
      bootData = null;
      if (boot) post(boot, [boot.flash as ArrayBuffer, boot.sd as ArrayBuffer, boot.rom as ArrayBuffer]);
      break;
    }
    case 'running':
      setSession({ state: 'running' });
      starting?.resolve();
      starting = null;
      break;
    case 'log':
      log(data.line);
      break;
    case 'screen':
      // After the lines logged before it, as the native backend delivers them.
      if (logTimer) { clearTimeout(logTimer); logTimer = 0; handlers?.log(pendingLog.splice(0)); }
      handlers?.screen(data.blank === true);
      break;
    case 'boot-error': {
      // The runtime returns the card so a failed Wi-Fi start can be retried.
      image = new Uint8Array(data.sd);
      card = { path: '', name: card?.name ?? 'sd.img', size: image.byteLength, note: card?.note, error: card?.error };
      const message = String(data.message);
      endRun({ kind: 'failed', reason: message });
      handlers?.card(card, true);
      starting?.reject(new AppError('EQEMU', `Could not start Wi-Fi: ${message} Retry, or turn Wi-Fi off.`));
      starting = null;
      break;
    }
    case 'error':
      if (starting) {
        const message = String(data.message);
        endRun({ kind: 'failed', reason: message });
        void recoverCard();
        starting.reject(new AppError('EQEMU', message));
        starting = null;
      } else handlers?.problem(String(data.message));
      break;
    case 'exit':
      endRun({ kind: 'crashed', reason: `QEMU exited with status ${data.status}` });
      void recoverCard();
      break;
    case 'sd-result':
      void sdResult(data);
      break;
    case 'snapshot': {
      const bytes = data.bytes as Uint8Array;
      if (data.action === 'autosave') {
        saveSD(bytes).finally(() => { snapshotPending = false; });
      } else if (data.action === 'download') {
        void saveSD(bytes);
        saveBlob(bytes, 'sd.img');
        copyWanted?.('sd.img');
        copyWanted = null;
      } else {
        image = bytes;
        card = { path: '', name: card?.name ?? 'sd.img', size: bytes.byteLength };
        endRun({ kind: 'stopped', reason: null });
        void saveSD(bytes);
        handlers?.card(card, true);
      }
      break;
    }
  }
}

/* Uploads: a plan collects the files behind the refs (folders walked, hidden
   entries skipped) and names what would be replaced in the open folder. */
interface Item { path: string; file: File }
const plans = new Map<string, Item[]>();
async function collect(refsList: string[]): Promise<Item[]> {
  const items: Item[] = [];
  async function walk(entry: FileSystemEntry, prefix: string) {
    if (prefix && entry.name.startsWith('.')) return;
    if (entry.isFile) {
      items.push({ path: prefix + entry.name, file: await entryFile(entry as FileSystemFileEntry) });
    } else if (entry.isDirectory) {
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
        if (!batch.length) break;
        for (const child of batch) await walk(child, `${prefix}${entry.name}/`);
      }
    }
  }
  for (const ref of refsList) {
    const source = sourceOf(ref);
    if (source.kind === 'file') items.push({ path: source.file.name, file: source.file });
    else if (source.kind === 'entry') await walk(source.entry, '');
  }
  return items;
}
const joinPath = (dir: string, name: string) => dir === '/' ? `/${name}` : `${dir}/${name}`;
function fatName(name: string) {
  name = name.normalize('NFC');
  if (!FAT.isValidName(name)) throw new AppError('EINVAL', `"${name}" is not a valid FAT file name.`);
  return name;
}
const listing = (path: string) => sdCall<{ entries: { name: string; size: number; isDir: boolean; mtime: Date | null }[]; free: number; total: number; type: string }>('list', { path });

/* Drops: files and folders from the DataTransfer, as refs. Folders arrive
   through the entries API, which must be read before the first await. */
function dropped(transfer: DataTransfer): FileRef[] {
  const entries = [...transfer.items].map((item) => item.webkitGetAsEntry?.()).filter((entry): entry is FileSystemEntry => !!entry);
  if (entries.length) return entries.map((entry) => register({ kind: 'entry', entry }));
  return [...transfer.files].map((file) => register({ kind: 'file', file }));
}

export const browser: Backend = {
  can: { heap: false, cardOpen: true, cardNew: true, cardResize: false, cardReveal: false },
  cardSizes: [{ size: 64 * MiB, label: '64 MiB', note: 'The blank card bundled with this page' }],
  cardDefaultSize: 64 * MiB,
  cardSizeNote: 'The card lives in this browser: a card takes its full size in memory, so the bundled blank card is small. Open an SD image for a bigger one.',

  async attach(next) {
    handlers = next;
    window.addEventListener('message', onMessage);
    setInterval(autosave, 10000);
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'hidden') autosave(); });
    document.title = 'eink-qemu · Browser emulator';
    const problems: string[] = [];
    // ?flash=<url>: a same-origin firmware image, started as soon as the page opens.
    const flash = params.get('flash');
    if (flash) {
      try {
        const bytes = await fetchBytes(flash);
        const name = flash.split('/').pop() || flash;
        firmware = { source: { kind: 'url', url: flash }, info: describeFirmware(name, bytes.length, bytes.subarray(0, HEAD), null), bytes };
      } catch (error) { problems.push(`Firmware: ${errorMessage(error)}`); }
    }
    // An explicit ?sd= URL replaces the saved card (also useful for repeatable tests).
    void loadCard(params.get('sd') ?? 'blank-sd.img', !params.has('sd'));
    return {
      session, settings, card, firmware: firmware?.info ?? null, elf: null, platform: 'browser',
      autostart: !!firmware, pane: null, problems,
    };
  },
  note: () => {},
  async settings(patch) {
    settings = { ...settings, ...patch };
    try {
      for (const key of ['device', 'panel', 'wifi', 'ghosting', 'turbo'] as const) {
        if (!(key in patch)) continue;
        const value = settings[key];
        localStorage.setItem(KEYS[key], typeof value === 'boolean' ? (value ? '1' : '0') : value);
      }
    } catch {}
    return settings;
  },

  async pickFirmware() {
    const [file] = await pick(inputs.firmware, '.bin,.xota');
    return file ? useFile(file) : null;
  },
  useFirmware: async (ref) => useFile(await fileOf(ref)),
  recentFirmware: () => notSupported('Recent firmware'),
  setup: async () => setupOf(),
  async readFirmware() {
    if (!firmware) throw new AppError('ENOENT', 'Choose a firmware .bin, an X4 Pro .xota update, or a 16 MiB flash dump.');
    if (!firmware.bytes) {
      const source = firmware.source;
      firmware.bytes = source.kind === 'url' ? await fetchBytes(source.url)
        : new Uint8Array(await (source.kind === 'file' ? source.file : await entryFile(source.entry as FileSystemFileEntry)).arrayBuffer());
    }
    return firmware.bytes;
  },

  createScreen(machine, resized) {
    frame = new FrameScreen(machine, resized);
    return frame;
  },
  async start(flash, options: StartOptions) {
    if (!crossOriginIsolated) throw new AppError('EISOLATE', 'SharedArrayBuffer needs isolation headers. Serve this folder with serve.py on localhost or HTTPS.');
    if (!frame) throw new AppError('EINVAL', 'No screen to boot into.');
    if (loading) await loading.catch(() => {});
    if (!image) throw new AppError('ENOENT', 'No SD image is loaded.');
    const { machine } = options;
    const c3 = machine === 'x3' || machine === 'x4';
    const rom = await fetchBytes(c3 ? 'esp32c3-rom.bin' : 'esp32s3_rev0_rom.bin');
    // The flash goes over whole, so its buffer must hold nothing else.
    const bytes = flash.byteOffset === 0 && flash.byteLength === flash.buffer.byteLength ? flash : flash.slice();
    const sd = image.byteOffset === 0 && image.byteLength === image.buffer.byteLength ? image : image.slice();
    bootData = {
      type: 'boot', machine, panel: options.panel, flash: bytes.buffer, sd: sd.buffer, rom: rom.buffer,
      wifi: options.wifi, turbo: options.turbo, ghosting: options.ghosting,
    };
    // The runtime owns the card from here until the stop snapshot returns it.
    image = null;
    const booting = frame;
    return new Promise<SessionInfo>((resolve, reject) => {
      starting = { resolve: () => resolve(session), reject };
      setSession({ state: 'starting', machine, turbo: options.turbo, exit: null, run: session.run + 1 });
      readyTimer = window.setTimeout(() => {
        if (frame !== booting || !bootData) return;
        endRun({ kind: 'failed', reason: 'the runtime page did not load' });
        void recoverCard();
        starting?.reject(new AppError('EQEMU', 'The emulator runtime did not load. Run package-web.sh first.'));
        starting = null;
      }, 30000);
      booting.el.src = 'runtime.html';
    });
  },
  async stop() {
    if (!frame) return;
    if (session.state === 'starting') {
      // Give up on the boot; the card went to the runtime, so it is as last saved.
      endRun({ kind: 'stopped', reason: null });
      void recoverCard();
      starting?.reject(new AppError('ECANCELED', 'Stopped before the emulator started.'));
      starting = null;
      return;
    }
    if (session.state !== 'running') return;
    setSession({ state: 'stopping' });
    post({ type: 'stop' });
  },
  async reset() {
    if (session.state === 'running') post({ type: 'reset' });
  },
  async key(name, down) {
    const code = { Back: 'Backspace', Confirm: 'Enter', Left: 'ArrowLeft', Right: 'ArrowRight', Up: 'ArrowUp', Down: 'ArrowDown', Power: 'KeyP', Home: 'Home' }[name];
    if (code && session.state === 'running') post({ type: 'key', code, down });
  },
  async touch() {},   // SDL in the runtime iframe takes the pointer itself.
  async saveScreenshot(png, name) {
    saveBlob(png, name);
    return null;
  },

  async pickOpenCard() {
    const [file] = await pick(inputs.card, '.img');
    if (!file) return null;
    await loadCard(file, false);
    return card;
  },
  async pickNewCard(): Promise<MadeCard> {
    await loadCard('blank-sd.img', false);
    return { card, made: { size: 64 * MiB, cluster: 512, sparse: false } };
  },
  resizeCard: () => notSupported('Resizing the card'),
  async pickCopyCard() {
    if (frame && session.state === 'running') {
      return new Promise((resolve) => {
        copyWanted = resolve;
        post({ type: 'download' });
      });
    }
    if (loading) await loading.catch(() => {});
    if (!image) throw new AppError('ENOENT', 'No SD card is loaded.');
    saveBlob(image, 'sd.img');
    return 'sd.img';
  },
  revealCard: () => notSupported('Showing the card'),
  async sdList(path): Promise<SdListing> {
    const result = await listing(path);
    return {
      path, free: result.free, total: result.total, fsType: result.type,
      entries: result.entries.map((e) => ({ name: e.name, size: e.size, isDir: e.isDir, mtime: e.mtime ? e.mtime.getTime() : null })),
    };
  },
  async sdPickSources() {
    const files = await pick(inputs.sd, '', true);
    return files.map((file) => register({ kind: 'file', file }).ref);
  },
  async sdPlanUpload(dir, refsList): Promise<UploadPlan> {
    const items = await collect(refsList);
    plans.set(refsList.join('\n'), items);
    const { entries } = await listing(dir);
    const conflicts = items.filter(({ path }) => entries.some((e) => !e.isDir && e.name === path)).map(({ path }) => joinPath(dir, path));
    return { files: items.length, bytes: items.reduce((sum, { file }) => sum + file.size, 0), conflicts };
  },
  async sdUpload(dir, refsList): Promise<UploadPlan> {
    const items = plans.get(refsList.join('\n')) ?? await collect(refsList);
    plans.delete(refsList.join('\n'));
    const dirs = new Set<string>(), files: { path: string; bytes: Uint8Array }[] = [];
    for (const { path, file } of items) {
      const parts = path.split('/').filter(Boolean).map(fatName);
      for (let depth = 1; depth < parts.length; depth++) dirs.add(joinPath(dir, parts.slice(0, depth).join('/')));
      files.push({ path: joinPath(dir, parts.join('/')), bytes: new Uint8Array(await file.arrayBuffer()) });
    }
    const total = files.reduce((sum, { bytes }) => sum + bytes.length, 0);
    const { free } = await listing(dir);
    if (total > free) throw new AppError('ENOSPC', `Not enough space: ${(total / MiB).toFixed(1)} MiB to copy, ${(free / MiB).toFixed(1)} MiB free.`);
    await sdCall('write', { dirs: [...dirs], files }, files.map(({ bytes }) => bytes.buffer));
    return { files: files.length, bytes: total, conflicts: [] };
  },
  async sdPickExport(path) {
    const { bytes } = await sdCall<{ bytes: Uint8Array }>('read', { path });
    const name = path.split('/').pop() ?? 'file';
    saveBlob(bytes, name);
    return name;
  },
  sdMkdir: (path) => sdCall('mkdir', { path }),
  sdRename: (path, name) => sdCall('rename', { path, name }),
  sdRemove: (path) => sdCall('remove', { path }),

  heapPickElf: () => notSupported('The heap view'),
  heapUseElf: () => notSupported('The heap view'),
  heapForgetElf: () => notSupported('The heap view'),
  heapSnapshot: () => notSupported('The heap view'),
  heapSnapshotAt: () => notSupported('The heap view'),
  heapStack: () => notSupported('The heap view'),
  heapSites: () => notSupported('The heap view'),
  heapHistory: () => notSupported('The heap view'),
  heapEvents: () => notSupported('The heap view'),
  heapWindow: () => notSupported('The heap window'),

  async watchDrops(drops: DropHandlers) {
    const hasFiles = (event: DragEvent) => !!event.dataTransfer && [...event.dataTransfer.types].includes('Files');
    for (const type of ['dragenter', 'dragover'] as const) document.addEventListener(type, (event) => {
      if (!hasFiles(event)) return;
      const target = drops.target(event.clientX, event.clientY);
      drops.paint(target);
      // A file dropped anywhere else must not replace the page and a running device.
      event.preventDefault();
      event.dataTransfer!.dropEffect = target ? 'copy' : 'none';
    });
    document.addEventListener('dragleave', (event) => { if (!event.relatedTarget) drops.paint(null); });
    document.addEventListener('drop', (event) => {
      if (!hasFiles(event)) return;
      event.preventDefault();
      drops.paint(null);
      drops.drop(dropped(event.dataTransfer!), event.clientX, event.clientY);
    });
  },
};

/* For the browser acceptance tests (wasm/web/*-smoke.mjs): the explorer's
   operations and the card as the page holds them. */
window.einkQemu = {
  sdCall, image: () => image, saving: () => saving, snapshotPending: () => snapshotPending,
};
