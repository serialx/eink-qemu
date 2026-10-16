/* Recent firmware in the browser: the files themselves, kept in this
   browser's IndexedDB so that after a reload Start runs the last one and the
   firmware card offers the others, as the desktop app's recent list does. */
import type { Settings } from './types';

/** As many as the desktop keeps (settings.rs MAX_RECENT). */
const KEEP = 6;

export interface KeptFirmware { id: string; name: string; size: number; modified: number | null; device: Settings['device']; used: number }
interface Row extends KeptFirmware { blob: Blob }

let database: Promise<IDBDatabase> | null = null;
function open(): Promise<IDBDatabase> {
  database ??= new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open('eink-qemu-firmware', 1);
    let late = false;
    const timer = setTimeout(() => { late = true; reject(new Error('The browser storage did not respond.')); }, 10000);
    request.onupgradeneeded = () => request.result.createObjectStore('files', { keyPath: 'id' });
    request.onsuccess = () => {
      clearTimeout(timer);
      const db = request.result;
      if (late) { db.close(); return; }
      db.onversionchange = () => { db.close(); database = null; };
      resolve(db);
    };
    request.onerror = () => { clearTimeout(timer); reject(request.error); };
  }).catch((error) => { database = null; throw error; });
  return database;
}

async function transaction<T>(mode: IDBTransactionMode, action: (store: IDBObjectStore, done: (value: T) => void) => void): Promise<T> {
  const db = await open();
  return new Promise<T>((resolve, reject) => {
    const tx = db.transaction('files', mode);
    let result: T;
    action(tx.objectStore('files'), (value) => { result = value; });
    tx.oncomplete = () => resolve(result);
    tx.onabort = () => reject(tx.error ?? new Error('Saving the firmware in this browser failed.'));
  });
}

const rows = () => transaction<Row[]>('readonly', (store, done) => {
  const request = store.getAll();
  request.onsuccess = () => done((request.result as Row[]).sort((a, b) => b.used - a.used));
});
const strip = ({ blob: _, ...info }: Row): KeptFirmware => info;

export const firmwareStore = {
  /** Newest first. */
  async list(): Promise<KeptFirmware[]> {
    return (await rows()).map(strip);
  },
  async file(id: string): Promise<File | null> {
    const row = (await rows()).find((r) => r.id === id);
    return row ? new File([row.blob], row.name, { lastModified: row.modified ?? Date.now() }) : null;
  },
  /** Keeps `file` as `id`, the newest, used with `device`; the oldest beyond KEEP go. */
  async keep(file: File, id: string, device: Settings['device']): Promise<void> {
    const kept = await rows();
    // A file kept before keeps its stored copy.
    const before = kept.find((r) => r.id === id);
    const row: Row = before ? { ...before, device, used: Date.now() }
      : { id, name: file.name, size: file.size, modified: file.lastModified || null, device, used: Date.now(), blob: file };
    await transaction<void>('readwrite', (store, done) => {
      store.put(row);
      for (const old of kept.filter((r) => r.id !== id).slice(KEEP - 1)) store.delete(old.id);
      done();
    });
  },
  /** The device last used with firmware `id`. */
  async device(id: string, device: Settings['device']): Promise<void> {
    await transaction<void>('readwrite', (store, done) => {
      const request = store.get(id);
      request.onsuccess = () => { if (request.result) store.put({ ...request.result, device }); done(); };
    });
  },
};
