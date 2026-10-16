import { defineConfig, type Plugin } from 'vite';
import { createReadStream, existsSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const web = fileURLToPath(new URL('../wasm/web', import.meta.url));
const host = process.env.TAURI_DEV_HOST;

/* One front end, two builds. The default is the desktop's: Tauri serves
   dist/ and the heap window is a second page. `--mode web` is the browser
   page: relative URLs for static hosting, the page's classic scripts
   (fat.js, sd-ops.js, sd-store.js, the isolation service worker) loaded by
   index.html, and in development the runtime's files served from
   ../wasm/web and a wasm package (EINK_WEB_DIST, default
   ../../build-wasm-32limit/web-dist) behind the isolation headers. */
const isolation = {
  'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp', 'Cross-Origin-Resource-Policy': 'same-origin',
};
const types: Record<string, string> = {
  '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html; charset=utf-8',
  '.json': 'application/json', '.txt': 'text/plain; charset=utf-8', '.md': 'text/markdown; charset=utf-8', '.css': 'text/css',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.bin': 'application/octet-stream', '.img': 'application/octet-stream',
};
function browserPage(): Plugin {
  const dist = process.env.EINK_WEB_DIST || fileURLToPath(new URL('../../build-wasm-32limit/web-dist', import.meta.url));
  return {
    name: 'eink-qemu-browser-page',
    // Before Vite's own pass, which rewrites the theme.js URL for the base.
    transformIndexHtml: {
      order: 'pre',
      handler(html) {
        const injected = html.replace(/<script src="\.?\/theme\.js"><\/script>/, (tag) =>
          `<script src="coi-serviceworker.js"></script>\n  ${tag}\n` +
          '  <script src="fat.js"></script>\n  <script src="sd-ops.js"></script>\n  <script src="sd-store.js"></script>');
        if (injected === html) throw new Error('index.html: theme.js script tag not found; the browser page needs its classic scripts');
        return injected;
      },
    },
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const name = decodeURIComponent((req.url ?? '/').split('?')[0]).replace(/^\/+/, '');
        const extension = name.slice(name.lastIndexOf('.'));
        if (!name || !(extension in types) || name.includes('..')) return next();
        for (const dir of [web, dist]) {
          const file = join(dir, name);
          if (!existsSync(file) || !statSync(file).isFile()) continue;
          res.setHeader('Content-Type', types[extension]);
          res.setHeader('Cache-Control', 'no-cache');
          // This runs before Vite's own middlewares, which add server.headers.
          for (const [name, value] of Object.entries(isolation)) res.setHeader(name, value);
          createReadStream(file).pipe(res);
          return;
        }
        next();
      });
    },
  };
}

export default defineConfig(({ mode }) => {
  const browser = mode === 'web';
  return {
    clearScreen: false,
    base: browser ? './' : '/',
    // The browser page's stylesheet, fonts, marks and dependency-free modules
    // are imported from ../wasm/web through the @web alias, so both front
    // ends share one copy.
    resolve: { alias: { '@web': web } },
    plugins: browser ? [browserPage()] : [],
    server: {
      port: browser ? 1421 : 1420,
      strictPort: true,
      host: host || false,
      fs: { allow: [fileURLToPath(new URL('..', import.meta.url))] },
      watch: { ignored: ['**/src-tauri/**'] },
      headers: browser ? isolation : {},
    },
    envPrefix: ['VITE_', 'TAURI_ENV_*'],
    build: {
      outDir: browser ? 'dist-web' : 'dist',
      target: browser ? 'es2022' : process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
      minify: process.env.TAURI_ENV_DEBUG ? false : 'esbuild',
      sourcemap: !!process.env.TAURI_ENV_DEBUG,
      rollupOptions: {
        input: Object.fromEntries((browser ? ['main'] : ['main', 'heap']).map((page) =>
          [page, fileURLToPath(new URL(page === 'main' ? 'index.html' : 'heap.html', import.meta.url))])),
      },
    },
  };
});
