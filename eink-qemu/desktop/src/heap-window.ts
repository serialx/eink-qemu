/* The heap window: heapview.py's page (eink-qemu/heapview.js) in the app's
   colors, with the Rust commands in place of the viewer's HTTP endpoints. */
import '@heapview/heapview.css';
import './heap-window.css';
import { invoke } from '@tauri-apps/api/core';
import { mount, type Snapshot } from '@heapview/heapview.js';

let last: Snapshot | null = null;
mount(document.body, {
  async snapshot() {
    try {
      return last = await invoke<Snapshot>('heap_snapshot');
    } catch (error) {
      const { code = '', message = String(error) } = (typeof error === 'object' && error) ? error as { code?: string; message?: string } : {};
      // An SD card operation has the emulator for a moment: keep the last view.
      if (code === 'EBUSY' && last) return last;
      return { error: message, heaps: [], time: Date.now() / 1000 };
    }
  },
  history: () => invoke('heap_history'),
  events: (gen, start, stacks, sites) => invoke('heap_events', { gen, start, stacks, sites }),
  stack: (id) => invoke('heap_stack', { id }),
});

// The window's own errors go to the EINK_QEMU_LOG file, as the main window's do.
const report = (what: unknown) => void invoke('note', { line: `Error: heap window: ${(what as Error)?.message ?? what}` }).catch(() => {});
addEventListener('error', (event) => report(event.error ?? event.message));
addEventListener('unhandledrejection', (event) => report(event.reason));

// Follow the main window's theme toggle.
addEventListener('storage', (event) => {
  if (event.key !== 'eink-qemu-theme') return;
  if (event.newValue) document.documentElement.dataset.theme = event.newValue;
  else delete document.documentElement.dataset.theme;
});
