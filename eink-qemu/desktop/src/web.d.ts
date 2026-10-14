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
