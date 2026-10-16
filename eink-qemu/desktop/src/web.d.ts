/// <reference types="vite/client" />
// Globals installed by the browser page's dependency-free modules
// (../wasm/web): imported as modules here, or loaded as classic scripts by
// the web build's index.html (vite.config.ts).
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
/* fat.js, sd-ops.js and sd-store.js, in the web build only. */
declare const FAT: { isValidName(name: string): boolean };
declare function sdExecute(image: Uint8Array, op: string, args: Record<string, unknown>): unknown;
declare function sdMutates(op: string): boolean;
declare function sdSyncInfo(image: Uint8Array): void;
declare const sdStore: {
  load(): Promise<{ bytes: Uint8Array; revision: number } | null>;
  save(bytes: Uint8Array, revision: number, force?: boolean): Promise<number>;
  onSaved(listener: (revision: number) => void): void;
};

interface Window {
  /** coi-serviceworker.js will reload the page once its worker is active. */
  coiReloading?: boolean;
  /** Seconds from boot to the Home screen; a hook for the acceptance tests. */
  bootTimeSeconds?: number | null;
  /** The browser backend's explorer and card, for the acceptance tests. */
  einkQemu?: {
    sdCall(op: string, args?: Record<string, unknown>): Promise<unknown>;
    image(): Uint8Array | null;
    saving(): number;
    snapshotPending(): boolean;
  };
}
