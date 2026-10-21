/* The desktop backend: native QEMU behind the Rust commands (src-tauri/src/lib.rs).
   Files are host paths from native dialogs and window drops; the screen is a
   canvas painted from the eink display backend's records (DESIGN.md). */
import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Backend, DropHandlers, Screen } from './backend';
import type { HeapEvents } from './heap-log';
import type { BridgeInspection, BridgeOperation, BridgePlan } from './bridge-types';
import { Panel } from './panel';
import {
  AppError, type Attached, type CardInfo, type FileInfo, type HeapHistoryPoint, type HeapPast, type HeapSite,
  type HeapSnapshot, type HeapStack, type MadeCard, type SdListing, type SessionInfo, type Settings, type Setup,
  type StartOptions, type UploadPlan,
} from './types';

async function call<T>(command: string, args?: Record<string, unknown> | Uint8Array, options?: { headers: Record<string, string> }): Promise<T> {
  try {
    return await invoke<T>(command, args as never, options);
  } catch (error) {
    const { code = 'EIO', message = String(error) } = (typeof error === 'object' && error) ? error as { code?: string; message?: string } : {};
    throw new AppError(code, message);
  }
}

const MiB = 1024 ** 2, GiB = 1024 ** 3;
let platform = '';

/* The screen: a canvas at the panel's resolution. core/src/display.rs
   sends one raw binary channel message per flush: `u32 1, width, height`
   for a size, or `u32 2, count` followed by each rect's x, y, w, h and gray8
   pixels. Rust keeps a copy of the panel and sends it first on attach, which
   can arrive before the page has made the screen: it waits here. */
class CanvasScreen implements Screen {
  el: HTMLCanvasElement;
  width: number;
  height: number;
  readonly ownPointer = false;
  private context: CanvasRenderingContext2D;
  private image: ImageData | null = null;

  constructor(machine: Machine, private readonly resized: () => void) {
    [this.width, this.height] = machine === 'x3' ? [528, 792] : [480, 800];
    this.el = document.createElement('canvas');
    this.el.width = this.width;
    this.el.height = this.height;
    this.el.setAttribute('aria-label', `${{ x3: 'X3', x4: 'X4', x4pro: 'X4 Pro', picco: 'BOOX Picco' }[machine]} display`);
    this.context = this.el.getContext('2d')!;
    this.context.fillStyle = '#f5f4ef';
    this.context.fillRect(0, 0, this.width, this.height);
  }
  resize(scale: number, exact: boolean) {
    this.el.style.width = `${this.width * scale}px`;
    this.el.style.height = `${this.height * scale}px`;
    this.el.style.imageRendering = exact ? 'pixelated' : 'auto';
  }
  canvas() { return this.el; }
  dispose() {}
  paint(buffer: ArrayBuffer) {
    const view = new DataView(buffer);
    const kind = view.getUint32(0, true);
    if (kind === 1) {
      const width = view.getUint32(4, true), height = view.getUint32(8, true);
      if (this.width !== width || this.height !== height) {
        this.width = width; this.height = height;
        this.el.width = width; this.el.height = height;
        this.resized();
      }
      this.context = this.el.getContext('2d')!;
      this.image = this.context.createImageData(width, height);
      this.image.data.fill(255);
      return;
    }
    if (kind !== 2) return;
    if (!this.image) {
      this.image = this.context.createImageData(this.width, this.height);
      this.image.data.fill(255);
    }
    const bytes = new Uint8Array(buffer);
    const count = view.getUint32(4, true);
    const stride = this.width, data = this.image.data;
    let at = 8, minX = Infinity, minY = Infinity, maxX = 0, maxY = 0;
    for (let n = 0; n < count; n++) {
      const x = view.getUint32(at, true), y = view.getUint32(at + 4, true);
      const w = view.getUint32(at + 8, true), h = view.getUint32(at + 12, true);
      at += 16;
      for (let row = 0; row < h; row++) {
        let target = ((y + row) * stride + x) * 4;
        const source = at + row * w;
        for (let col = 0; col < w; col++, target += 4) {
          const gray = bytes[source + col];
          data[target] = gray; data[target + 1] = gray; data[target + 2] = gray;
        }
      }
      at += w * h;
      minX = Math.min(minX, x); minY = Math.min(minY, y); maxX = Math.max(maxX, x + w); maxY = Math.max(maxY, y + h);
    }
    if (count) this.context.putImageData(this.image, 0, 0, minX, minY, maxX - minX, maxY - minY);
  }
}

let screen: CanvasScreen | null = null;
let screenUpdated: (blank: boolean) => void = () => {};
/** Display messages that arrived before the page made a screen. */
const early: ArrayBuffer[] = [];
/** The panel the display messages have drawn, for whether it is blank. */
const panel = new Panel();
function onDisplay(buffer: ArrayBuffer) {
  screenUpdated(panel.apply(buffer));
  if (screen) { screen.paint(buffer); return; }
  // A size starts the panel over: what came before it is stale.
  if (new DataView(buffer).getUint32(0, true) === 1) early.length = 0;
  if (early.length < 1024) early.push(buffer);
}

export const tauri: Backend = {
  bridge: {
    inspect: () => call<BridgeInspection>('bridge_inspect'),
    install: () => call<BridgeInspection>('bridge_install'),
    plan: (request) => call<BridgePlan>('bridge_plan', { request }),
    apply: (planId) => call<BridgeOperation>('bridge_apply', { planId }),
    keep: (operationId) => call<BridgeOperation>('bridge_keep', { operationId }),
    cancel: (operationId) => call<BridgeOperation>('bridge_cancel', { operationId }),
    verify: () => call<BridgeInspection>('bridge_verify'),
    async watch(handler) {
      let disposed = false, polling = false;
      const unlisten = await listen<BridgeOperation>('bridge-operation', event => { if (!disposed) handler(event.payload); });
      const timer = window.setInterval(async () => {
        if (disposed || polling) return;
        polling = true;
        try {
          const operation = await call<BridgeOperation | null>('bridge_operation');
          if (!disposed && operation) handler(operation);
        } catch { /* Inspection/actions surface errors; polling never installs support. */ }
        finally { polling = false; }
      }, 1000);
      return () => { disposed = true; window.clearInterval(timer); unlisten(); };
    },
  },
  can: { heap: true, cardOpen: true, cardNew: true, cardResize: true, cardRecent: true, cardSwitchRunning: true, cardReveal: true },
  /** All with the stock card's 32 KiB clusters (sd.rs). */
  cardSizes: [
    { size: 2050 * MiB, label: '2 GiB', note: 'The stock X3 card (2050 MiB): use it to compare timing with a real device' },
    { size: 4 * GiB, label: '4 GiB' },
    { size: 8 * GiB, label: '8 GiB' },
    { size: 16 * GiB, label: '16 GiB', note: 'The default' },
    { size: 32 * GiB, label: '32 GiB', note: 'The largest SDHC card' },
  ],
  cardDefaultSize: 16 * GiB,
  cardSizeNote: 'Cards are sparse files: an empty one takes a few MiB on disk and grows as files are written. ' +
    "All have the stock card's 32 KiB clusters.",

  async attach(handlers) {
    // Before attaching, so no session or log message is missed.
    await listen<SessionInfo>('session', (event) => handlers.session(event.payload));
    await listen('sd-reset', () => handlers.sdReset());
    const logs = new Channel<string[]>();
    logs.onmessage = (lines) => handlers.log(lines);
    const frames = new Channel<ArrayBuffer>();
    screenUpdated = blank => handlers.screen(blank);
    frames.onmessage = onDisplay;
    const state = await call<Attached>('attach', { logs, frames });
    platform = state.platform;
    return state;
  },
  note: (line) => void call<void>('note', { line }).catch(() => {}),
  settings: (patch) => call<Settings>('settings_set', { patch }),

  // Native dialogs run in Rust; host paths come back only as results.
  pickFirmware: () => call<Setup | null>('firmware_pick'),
  useFirmware: (path) => call<Setup>('firmware_use', { path }),
  recentFirmware: (path) => call<Setup>('firmware_recent', { path }),
  setup: (detect) => call<Setup>('firmware_setup', { detect }),
  readFirmware: async () => new Uint8Array(await call<ArrayBuffer>('firmware_read')),

  createScreen(machine, resized) {
    screen = new CanvasScreen(machine, resized);
    // The current panel came with attach; draw it on the screen made now.
    for (const message of early.splice(0)) screen.paint(message);
    const made = screen;
    made.dispose = () => { if (screen === made) screen = null; };
    return made;
  },
  start: (flash, options: StartOptions) =>
    call<SessionInfo>('start', flash, { headers: { 'x-options': JSON.stringify(options) } }),
  stop: () => call<void>('stop'),
  reset: () => call<void>('reset'),
  key: (key, down) => call<void>('key', { key, down }),
  touch: (x, y, down) => call<void>('touch', { x, y, down }),
  saveScreenshot: (png, name) => call<string | null>('screenshot_save', png, { headers: { 'x-name': name } }),

  recentCards: () => call<CardInfo[]>('card_recent'),
  useRecentCard: (path) => call<CardInfo>('card_use_recent', { path }),
  eraseCard: () => call<void>('card_erase'),
  pickOpenCard: () => call<CardInfo | null>('card_pick_open'),
  pickNewCard: (size) => call<MadeCard | null>('card_pick_new', { size }),
  resizeCard: (size) => call<MadeCard>('card_resize', { size }),
  pickCopyCard: () => call<string | null>('card_pick_copy'),
  revealCard: () => call<void>('card_reveal'),
  sdList: (path) => call<SdListing>('sd_list', { path }),
  sdPickSources: (folders = false) => call<string[]>('sd_pick_sources', { folders }),
  sdPlanUpload: (dir, sources) => call<UploadPlan>('sd_plan_upload', { dir, sources }),
  sdUpload: (dir, sources, confirmed) => call<UploadPlan>('sd_upload', { dir, sources, confirmed }),
  sdPickExport: (path) => call<string | null>('sd_pick_export', { path }),
  sdMkdir: (path) => call<void>('sd_mkdir', { path }),
  sdRename: (path, name) => call<void>('sd_rename', { path, name }),
  sdRemove: (path) => call<void>('sd_remove', { path }),

  heapPickElf: () => call<FileInfo | null>('heap_pick_elf'),
  heapUseElf: (path) => call<FileInfo>('heap_use_elf', { path }),
  heapForgetElf: () => call<void>('heap_forget_elf'),
  heapSnapshot: () => call<HeapSnapshot>('heap_snapshot'),
  heapSnapshotAt: (time) => call<HeapPast | null>('heap_snapshot_at', { time }),
  heapStack: (id) => call<HeapStack | null>('heap_stack', { id }),
  heapSites: (from) => call<HeapSite[]>('heap_sites', { from }),
  heapHistory: () => call<HeapHistoryPoint[]>('heap_history'),
  heapEvents: (gen, start, stacks, sites) => call<HeapEvents | null>('heap_events', { gen, start, stacks, sites }),
  heapWindow: () => call<void>('heap_window'),

  /* File drops: Tauri hands over paths and a position. macOS reports drop
     positions in points already (wry passes Cocoa coordinates through);
     other platforms report physical pixels. */
  async watchDrops(handlers: DropHandlers) {
    const scale = () => platform === 'macos' ? 1 : window.devicePixelRatio;
    await getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === 'enter' || payload.type === 'over') {
        handlers.paint(handlers.target(payload.position.x / scale(), payload.position.y / scale()));
      } else if (payload.type === 'drop') {
        handlers.paint(null);
        const files = payload.paths.map((path) => ({ ref: path, name: path.split(/[\\/]/).pop() ?? path }));
        handlers.drop(files, payload.position.x / scale(), payload.position.y / scale());
      } else handlers.paint(null);
    });
  },
  // Project links open in the browser, not in the app's WebView.
  openUrl: (url) => openUrl(url),
};

/** The commands the heap window uses directly (heap-window.ts). */
export const api = tauri;
