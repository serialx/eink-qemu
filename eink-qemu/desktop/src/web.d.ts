// Globals installed by the browser page's dependency-free modules.
type Machine = 'x3' | 'x4' | 'x4pro' | 'picco';
declare const FlashImage: {
  selectMachine(chip: number, selected?: string): Machine;
  appLayout(machine: Machine): { bootAsset: string; partitionAsset: string; nvsAsset: string | null };
  expandMerged(bytes: Uint8Array): Uint8Array | null | false;
  composeApp(app: Uint8Array, options: {
    machine: Machine; boot: ArrayBuffer | Uint8Array; partitions: ArrayBuffer | Uint8Array;
    nvs: ArrayBuffer | Uint8Array | null; selectOta?: boolean;
  }): Uint8Array;
};
declare const XOTA: {
  isXota(bytes: Uint8Array): boolean;
  decode(bytes: Uint8Array, maxAppSize?: number): Promise<Uint8Array>;
};
declare module '@web/flash-image.js';
declare module '@web/xota.js';

// eink-qemu/heapview.js, shared with heapview.py's page.
declare module '@heapview/heapview.css';
declare module '@heapview/heapview.js' {
  /** A snapshot as heapview.py's /snapshot returns it (time in seconds). */
  export interface Snapshot { time: number; heaps: unknown[]; error?: string; [key: string]: unknown }
  export interface HeapviewApi {
    snapshot(): Promise<Snapshot>;
    history(): Promise<unknown>;
    events(gen: number, start: number, stacks: number, sites: number): Promise<unknown>;
    stack(id: number): Promise<unknown>;
  }
  export function mount(root: HTMLElement, api: HeapviewApi): void;
}
