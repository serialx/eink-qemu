import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';

const web = fileURLToPath(new URL('../wasm/web', import.meta.url));
const heapview = fileURLToPath(new URL('..', import.meta.url));
const host = process.env.TAURI_DEV_HOST;

// The browser page's stylesheet, fonts, marks and dependency-free modules are
// imported from ../wasm/web through the @web alias, so both front ends share
// one copy; heapview.py's page module and stylesheet likewise through @heapview.
export default defineConfig({
  clearScreen: false,
  resolve: { alias: { '@web': web, '@heapview': heapview } },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    fs: { allow: [fileURLToPath(new URL('..', import.meta.url))] },
    watch: { ignored: ['**/src-tauri/**'] },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_*'],
  build: {
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
    minify: process.env.TAURI_ENV_DEBUG ? false : 'esbuild',
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('index.html', import.meta.url)),
        heap: fileURLToPath(new URL('heap.html', import.meta.url)),
      },
    },
  },
});
