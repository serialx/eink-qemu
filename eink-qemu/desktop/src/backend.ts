/* What the page needs from whatever runs the emulator: native QEMU behind
   the desktop's Rust commands (backend-tauri.ts), or the WebAssembly build in
   a runtime iframe (backend-browser.ts). app.ts drives one of them and never
   knows which, beyond what `can` says it offers. */
import type {
  Attached, CardInfo, FileInfo, HeapHistoryPoint, HeapPast, HeapSite, HeapSnapshot, HeapStack, MadeCard, SdListing,
  SessionInfo, Settings, Setup, StartOptions, UploadPlan,
} from './types';
import type { HeapEvents } from './heap-log';

/** A file the backend handed the page: a host path on the desktop, a token
    for a File or a dropped entry in a browser. Only the backend reads it. */
export interface FileRef { ref: string; name: string }

export interface Capabilities {
  /** The Heap tab and the firmware's ELF: reading the guest's memory needs native QEMU. */
  heap: boolean;
  /** The card menu's items. */
  cardOpen: boolean;
  cardNew: boolean;
  cardResize: boolean;
  /** Showing the card in the file manager. */
  cardReveal: boolean;
}

/** A blank card the backend can make. */
export interface CardSize { size: number; label: string; note?: string }

export interface AttachHandlers {
  log(lines: string[]): void;
  /** The panel showed new pixels, or went blank (for readiness: a quiet,
      drawn screen ends a boot). */
  screen(blank: boolean): void;
  session(info: SessionInfo): void;
  /** The card changed under the running guest, which is being reset. */
  sdReset(): void;
  /** The open card, or how it is kept, changed; `changed` when it is another
      image, which the explorer then lists afresh. */
  card(card: CardInfo | null, changed: boolean): void;
  /** Something went wrong outside a command: shown in the status bar and logged. */
  problem(message: string): void;
}

/** The device's screen inside #screen: a canvas the backend paints, or the
    runtime's own iframe. */
export interface Screen {
  el: HTMLElement;
  width: number;
  height: number;
  /** Pointer events on the element reach the emulator by themselves. */
  ownPointer: boolean;
  /** Lay the element out at `scale` css px per panel px; whole physical pixels stay sharp. */
  resize(scale: number, exact: boolean): void;
  /** The panel's pixels, for a screenshot. */
  canvas(): HTMLCanvasElement | null;
  dispose(): void;
}

export interface DropHandlers {
  /** What a drop at this point (css px) would do, or null. */
  target(x: number, y: number): 'sd' | 'firmware' | null;
  paint(target: 'sd' | 'firmware' | null): void;
  drop(files: FileRef[], x: number, y: number): void;
}

export interface Backend {
  readonly can: Capabilities;
  /** The blank cards on offer; one size needs no choice. */
  readonly cardSizes: CardSize[];
  readonly cardDefaultSize: number;
  readonly cardSizeNote: string;

  attach(handlers: AttachHandlers): Promise<Attached>;
  /** A line for the backend's own log, where it keeps one. */
  note(line: string): void;
  settings(patch: Partial<Pick<Settings, 'device' | 'panel' | 'wifi' | 'ghosting' | 'turbo'>>): Promise<Settings>;

  pickFirmware(): Promise<Setup | null>;
  useFirmware(ref: string): Promise<Setup>;
  recentFirmware(path: string): Promise<Setup>;
  setup(detect: boolean): Promise<Setup>;
  readFirmware(): Promise<Uint8Array>;

  createScreen(machine: Machine, resized: () => void): Screen;
  start(flash: Uint8Array, options: StartOptions): Promise<SessionInfo>;
  stop(): Promise<void>;
  reset(): Promise<void>;
  key(name: string, down: boolean): Promise<void>;
  touch(x: number, y: number, down: boolean): Promise<void>;
  saveScreenshot(png: Uint8Array, name: string): Promise<string | null>;

  pickOpenCard(): Promise<CardInfo | null>;
  pickNewCard(size: number): Promise<MadeCard | null>;
  resizeCard(size: number): Promise<MadeCard>;
  pickCopyCard(): Promise<string | null>;
  revealCard(): Promise<void>;
  sdList(path: string): Promise<SdListing>;
  sdPickSources(): Promise<string[]>;
  sdPlanUpload(dir: string, sources: string[]): Promise<UploadPlan>;
  sdUpload(dir: string, sources: string[], confirmed: string[]): Promise<UploadPlan>;
  sdPickExport(path: string): Promise<string | null>;
  sdMkdir(path: string): Promise<void>;
  sdRename(path: string, name: string): Promise<void>;
  sdRemove(path: string): Promise<void>;

  heapPickElf(): Promise<FileInfo | null>;
  heapUseElf(ref: string): Promise<FileInfo>;
  heapForgetElf(): Promise<void>;
  heapSnapshot(): Promise<HeapSnapshot>;
  heapSnapshotAt(time: number): Promise<HeapPast | null>;
  heapStack(id: number): Promise<HeapStack | null>;
  heapSites(from: number): Promise<HeapSite[]>;
  heapHistory(): Promise<HeapHistoryPoint[]>;
  heapEvents(gen: number, start: number, stacks: number, sites: number): Promise<HeapEvents | null>;
  heapWindow(): Promise<void>;

  watchDrops(handlers: DropHandlers): Promise<void>;
  /** Opens a project link outside the page; absent where the browser does that itself. */
  openUrl?(url: string): Promise<void>;
}
