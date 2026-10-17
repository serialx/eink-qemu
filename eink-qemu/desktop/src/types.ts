/* The shapes both front ends share: the session, settings, the firmware card,
   the SD card and the heap view. The desktop's Rust commands (src-tauri/src/lib.rs)
   answer in these shapes; the browser backend builds them itself. */

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
/** The open SD card: a file on disk, or (in a browser) the card saved in the
    browser, with a note on how it is kept and whether that failed. */
export interface CardInfo extends FileInfo { isDefault?: boolean; note?: string; error?: boolean; /** Still being loaded: the explorer waits. */ busy?: boolean }
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
  card: CardInfo | null;
  firmware: FileInfo | null;
  /** The heap view's ELF. */
  elf: FileInfo | null;
  platform: string;
  autostart: boolean;
  pane: string | null;
  problems: string[];
  cardNotices: string[];
}
/** A card just made or resized (src-tauri/src/lib.rs `MadeCard`); `note` is
    something the backend wants said about it. */
export interface MadeCard { card: CardInfo | null; made: { size: number; cluster: number; sparse: boolean; note?: string } }
export interface SdEntry { name: string; size: number; isDir: boolean; mtime: number | null }
export interface SdListing { path: string; entries: SdEntry[]; free: number; total: number; fsType: string }
export interface UploadPlan { files: number; bytes: number; conflicts: string[] }
/** One heap (core/src/heap.rs). `blocks` is flat: offset, length, flags,
    requested size; with an allocation log, each block's site, stack id and
    the event that allocated it (-1 where the log has none). */
export interface Heap {
  name: string; caps: string[]; start: number; end: number; pool: number; tail: number;
  free: number; used: number; largest: number; count: number; nfree: number;
  infoFree: number; infoMinFree: number; corrupt: number; blocks: number[];
  sites?: number[]; stk?: number[]; ev?: number[];
}
/** A snapshot of the heaps (core/src/heapview.rs `heap_snapshot`): time in seconds. */
export interface HeapSnapshot {
  time: number; heaps: Heap[]; missing: number; traced?: boolean;
  /** Why allocations are not recorded, when they are not. */
  traceNote?: string | null;
  /** Milliseconds the read took, the emulator paused for most of it. */
  took?: number;
  /** The ELF's name; null when the heaps were found without one. */
  elf?: string | null;
  /** Why the chosen ELF was not used (not the running firmware's, unreadable). */
  elfNote?: string | null;
  /** The run it was read from. */
  run?: number;
  /** The allocation log's generation and length, and false when an older plugin wrote it. */
  traceGen?: number; traceEvents?: number; traceFormat?: boolean;
}
/** A kept snapshot from the run's history and the one before it. */
export interface HeapPast { snapshot: HeapSnapshot; prev: HeapSnapshot | null }
/** One free-memory history point: time in seconds, and per heap (start in
    hex, no prefix) its free bytes, largest free block and lowest free. */
export interface HeapHistoryPoint { t: number; h: Record<string, [number, number, number]> }
/** An allocation site: the function that called the allocator, and where. */
export interface HeapSite { fn: string; loc: string }
/** A recorded allocation stack, symbolized (`heap_stack`): frames
    innermost first, with the calls inlined into each; `caller` is the first
    frame outside the allocator, `site` the one the block is attributed to. */
export interface HeapFrame { addr: number; fn: string; loc: string; inl: { fn: string; loc: string }[]; alloc: boolean; lib: boolean }
export interface HeapStack { frames: HeapFrame[]; caller: number; site: number }
export interface StartOptions { machine: Machine; panel: string; wifi: boolean; turbo: boolean; ghosting: boolean }

/** Errors from Rust carry a code (fat.js codes, EBUSY, EQEMU, …) and a message. */
export class AppError extends Error {
  constructor(readonly code: string, message: string) { super(message); }
}
