/* Typed wrappers over the Rust commands (src-tauri/src/lib.rs). */
import { Channel, invoke } from '@tauri-apps/api/core';

export interface Exit { kind: 'stopped' | 'failed' | 'crashed'; reason: string | null }
export interface SessionInfo {
  state: 'idle' | 'starting' | 'running' | 'stopping';
  machine: Machine | null;
  turbo: boolean;
  exit: Exit | null;
}
export interface Settings {
  device: 'auto' | Machine;
  panel: 'uc8279-68' | 'uc8179';
  wifi: boolean;
  ghosting: boolean;
  turbo: boolean;
  firmware: string | null;
  card: string | null;
}
export interface FileInfo { path: string; name: string; size: number }
export interface Attached {
  session: SessionInfo;
  settings: Settings;
  card: FileInfo | null;
  firmware: FileInfo | null;
  platform: string;
  autostart: boolean;
}
export interface SdEntry { name: string; size: number; isDir: boolean; mtime: number | null }
export interface SdListing { path: string; entries: SdEntry[]; free: number; total: number; fsType: string }
export interface UploadPlan { files: number; bytes: number; conflicts: string[] }
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
  settings: (patch: Partial<Pick<Settings, 'device' | 'panel' | 'wifi' | 'ghosting' | 'turbo'>>) => call<Settings>('settings_set', { patch }),
  chooseFirmware: (path: string) => call<FileInfo>('firmware_choose', { path }),
  readFirmware: async () => new Uint8Array(await call<ArrayBuffer>('firmware_read')),
  start: (flash: Uint8Array, options: StartOptions) =>
    call<SessionInfo>('start', flash, { headers: { 'x-options': JSON.stringify(options) } }),
  stop: () => call<void>('stop'),
  reset: () => call<void>('reset'),
  key: (key: string, down: boolean) => call<void>('key', { key, down }),
  touch: (x: number, y: number, down: boolean) => call<void>('touch', { x, y, down }),
  openCard: (path: string) => call<FileInfo>('card_open', { path }),
  newCard: (path: string) => call<FileInfo>('card_new', { path }),
  copyCard: (dest: string) => call<void>('card_copy', { dest }),
  sdList: (path: string) => call<SdListing>('sd_list', { path }),
  sdPlanUpload: (dir: string, sources: string[]) => call<UploadPlan>('sd_plan_upload', { dir, sources }),
  sdUpload: (dir: string, sources: string[]) => call<UploadPlan>('sd_upload', { dir, sources }),
  sdExport: (path: string, dest: string) => call<void>('sd_export', { path, dest }),
  sdMkdir: (path: string) => call<void>('sd_mkdir', { path }),
  sdRename: (path: string, name: string) => call<void>('sd_rename', { path, name }),
  sdRemove: (path: string) => call<void>('sd_remove', { path }),
  saveScreenshot: (png: Uint8Array, path: string) =>
    call<void>('screenshot_save', png, { headers: { 'x-path': encodeURIComponent(path) } }),
};
