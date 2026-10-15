/* Typed wrappers over the Rust commands (src-tauri/src/lib.rs). */
import { Channel, invoke } from '@tauri-apps/api/core';

export interface Exit { kind: 'stopped' | 'failed' | 'crashed'; reason: string | null }
export interface SessionInfo {
  state: 'idle' | 'starting' | 'running' | 'stopping';
  machine: Machine | null;
  turbo: boolean;
  exit: Exit | null;
  run: number;
  /** Increases with every change: older snapshots are ignored. */
  seq: number;
}
export interface Settings {
  device: 'auto' | Machine;
  panel: 'uc8279-68' | 'uc8179';
  wifi: boolean;
  ghosting: boolean;
  turbo: boolean;
  firmware: string | null;
  card: string | null;
  elf: string | null;
  /** The ELF was found beside the firmware rather than chosen. */
  elfAuto: boolean;
}
export interface FileInfo { path: string; name: string; size: number }
export interface FirmwareInfo extends FileInfo {
  /** The folder holding it (home as ~), and that folder's own name. */
  folder: string; parent: string;
  /** Milliseconds since the epoch. */
  modified: number | null;
  chip: 'ESP32-C3' | 'ESP32-S3' | null;
  version: string | null; project: string | null;
}
export interface RecentFirmware { path: string; name: string; parent: string; device: Settings['device']; elf: boolean }
/** The firmware card (src-tauri/src/lib.rs `Setup`). */
export interface Setup {
  settings: Settings;
  firmware: FirmwareInfo | null;
  elf: FileInfo | null;
  /** Whether the ELF built the firmware; null when that cannot be told. */
  elfMatches: boolean | null;
  /** An ELF beside the firmware that was left out as another build's. */
  elfSkipped: string | null;
  recent: RecentFirmware[];
}
export interface Attached {
  session: SessionInfo;
  settings: Settings;
  card: FileInfo | null;
  firmware: FileInfo | null;
  /** The heap view's ELF. */
  elf: FileInfo | null;
  platform: string;
  autostart: boolean;
  pane: string | null;
  problems: string[];
}
export interface SdEntry { name: string; size: number; isDir: boolean; mtime: number | null }
export interface SdListing { path: string; entries: SdEntry[]; free: number; total: number; fsType: string }
export interface UploadPlan { files: number; bytes: number; conflicts: string[] }
/** One heap (src-tauri/src/heap.rs). `blocks` is flat: offset, length, flags,
    requested size; with an allocation log, each block's site and stack id. */
export interface Heap {
  name: string; caps: string[]; start: number; end: number; pool: number; tail: number;
  free: number; used: number; largest: number; count: number; nfree: number;
  infoFree: number; infoMinFree: number; corrupt: number; blocks: number[];
  sites?: number[]; stk?: number[];
}
/** heapview.py's /snapshot shape (src-tauri/src/heapview.rs): time in seconds. */
export interface HeapSnapshot {
  time: number; heaps: Heap[]; missing: number; traced?: boolean;
  /** Why allocations are not recorded, when they are not. */
  traceNote?: string | null;
  /** Milliseconds the read took, the emulator paused for most of it. */
  took?: number;
}
/** One free-memory history point: time in seconds, and per heap (start in
    hex, no prefix) its free bytes, largest free block and lowest free. */
export interface HeapHistoryPoint { t: number; h: Record<string, [number, number, number]> }
/** An allocation site: the function that called the allocator, and where. */
export interface HeapSite { fn: string; loc: string }
/** A recorded allocation stack, symbolized (heapview.py's /stack): frames
    innermost first, with the calls inlined into each; `caller` is the first
    frame outside the allocator, `site` the one the block is attributed to. */
export interface HeapFrame { addr: number; fn: string; loc: string; inl: { fn: string; loc: string }[]; alloc: boolean; lib: boolean }
export interface HeapStack { frames: HeapFrame[]; caller: number; site: number }
export interface StartOptions { machine: Machine; panel: string; wifi: boolean; turbo: boolean; ghosting: boolean }

/** Errors from Rust carry a code (fat.js codes, EBUSY, EQEMU, …) and a message. */
export class AppError extends Error {
  constructor(readonly code: string, message: string) { super(message); }
}

async function call<T>(command: string, args?: Record<string, unknown> | Uint8Array, options?: { headers: Record<string, string> }): Promise<T> {
  try {
    return await invoke<T>(command, args as never, options);
  } catch (error) {
    const { code = 'EIO', message = String(error) } = (typeof error === 'object' && error) ? error as { code?: string; message?: string } : {};
    throw new AppError(code, message);
  }
}

export const api = {
  attach(onLog: (lines: string[]) => void, onDisplay: (message: ArrayBuffer) => void) {
    const logs = new Channel<string[]>();
    logs.onmessage = onLog;
    const frames = new Channel<ArrayBuffer>();
    frames.onmessage = onDisplay;
    return call<Attached>('attach', { logs, frames });
  },
  note: (line: string) => call<void>('note', { line }).catch(() => {}),
  settings: (patch: Partial<Pick<Settings, 'device' | 'panel' | 'wifi' | 'ghosting' | 'turbo'>>) => call<Settings>('settings_set', { patch }),
  // Native dialogs run in Rust; host paths come back only as results.
  pickFirmware: () => call<Setup | null>('firmware_pick'),
  useFirmware: (path: string) => call<Setup>('firmware_use', { path }),
  recentFirmware: (path: string) => call<Setup>('firmware_recent', { path }),
  setup: (detect: boolean) => call<Setup>('firmware_setup', { detect }),
  readFirmware: async () => new Uint8Array(await call<ArrayBuffer>('firmware_read')),
  start: (flash: Uint8Array, options: StartOptions) =>
    call<SessionInfo>('start', flash, { headers: { 'x-options': JSON.stringify(options) } }),
  stop: () => call<void>('stop'),
  reset: () => call<void>('reset'),
  key: (key: string, down: boolean) => call<void>('key', { key, down }),
  touch: (x: number, y: number, down: boolean) => call<void>('touch', { x, y, down }),
  pickOpenCard: () => call<FileInfo | null>('card_pick_open'),
  pickNewCard: () => call<FileInfo | null>('card_pick_new'),
  pickCopyCard: () => call<string | null>('card_pick_copy'),
  sdList: (path: string) => call<SdListing>('sd_list', { path }),
  sdPickSources: () => call<string[]>('sd_pick_sources'),
  sdPlanUpload: (dir: string, sources: string[]) => call<UploadPlan>('sd_plan_upload', { dir, sources }),
  sdUpload: (dir: string, sources: string[], confirmed: string[]) => call<UploadPlan>('sd_upload', { dir, sources, confirmed }),
  sdPickExport: (path: string) => call<string | null>('sd_pick_export', { path }),
  sdMkdir: (path: string) => call<void>('sd_mkdir', { path }),
  sdRename: (path: string, name: string) => call<void>('sd_rename', { path, name }),
  sdRemove: (path: string) => call<void>('sd_remove', { path }),
  heapPickElf: () => call<FileInfo | null>('heap_pick_elf'),
  heapUseElf: (path: string) => call<FileInfo>('heap_use_elf', { path }),
  heapForgetElf: () => call<void>('heap_forget_elf'),
  heapSnapshot: () => call<HeapSnapshot>('heap_snapshot'),
  heapStack: (id: number) => call<HeapStack | null>('heap_stack', { id }),
  heapSites: (from: number) => call<HeapSite[]>('heap_sites', { from }),
  heapHistory: () => call<HeapHistoryPoint[]>('heap_history'),
  heapWindow: () => call<void>('heap_window'),
  saveScreenshot: (png: Uint8Array, name: string) =>
    call<string | null>('screenshot_save', png, { headers: { 'x-name': name } }),
};
