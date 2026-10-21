import './app.css';
import './bridge.css';
import { appendLines } from './console';
import './heap';

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const app = document.querySelector<HTMLElement>('.app')!;
const params = new URLSearchParams(location.search);

/* Which backend runs the emulator: the desktop's Rust commands inside Tauri,
   the WebAssembly build in the web build (vite --mode web), or neither: the
   mock content of demo.ts, for layout work in a plain browser (pnpm dev, or
   ?demo). Markup meant for one of them is marked data-only. */
const backend = params.has('demo') ? 'demo' : import.meta.env.MODE === 'web' ? 'browser' : '__TAURI_INTERNALS__' in window ? 'tauri' : 'demo';
document.documentElement.dataset.backend = backend === 'browser' ? 'browser' : 'tauri';
for (const element of document.querySelectorAll<HTMLElement>('[data-only]')) {
  if (element.dataset.only !== document.documentElement.dataset.backend) element.remove();
}

/* Theme: the page's data-theme attribute over prefers-color-scheme. */
function currentTheme(): 'light' | 'dark' {
  const saved = document.documentElement.dataset.theme;
  if (saved === 'light' || saved === 'dark') return saved;
  return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}
function paintThemeButton() {
  const dark = currentTheme() === 'dark';
  $('theme').setAttribute('aria-label', dark ? 'Switch to light mode' : 'Switch to dark mode');
  $('theme').querySelector('use')!.setAttribute('href', dark ? '#i-sun' : '#i-moon');
}
$('theme').addEventListener('click', () => {
  const theme = currentTheme() === 'dark' ? 'light' : 'dark';
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem('eink-qemu-theme', theme); } catch {}
  paintThemeButton();
});
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', paintThemeButton);
paintThemeButton();

/* Option help: hover or keyboard focus shows it; a click pins it. */
function closeTips(): boolean {
  let open = false;
  for (const button of document.querySelectorAll('.info[aria-expanded="true"], .brand-button[aria-expanded="true"]')) {
    button.setAttribute('aria-expanded', 'false');
    open = true;
  }
  return open;
}
for (const button of document.querySelectorAll<HTMLElement>('.info, .brand-button')) {
  button.addEventListener('click', () => {
    const open = button.getAttribute('aria-expanded') !== 'true';
    closeTips();
    button.setAttribute('aria-expanded', String(open));
  });
}
document.addEventListener('click', (event) => {
  if (!(event.target as Element).closest('.option, .about')) closeTips();
});
document.addEventListener('keydown', (event) => {
  if (event.code !== 'Escape') return;
  let open = closeTips();
  // Help shown by hover or keyboard focus hides too, until they move on.
  for (const tip of document.querySelectorAll<HTMLElement>('.tip, .about-tip')) {
    const holder = tip.closest<HTMLElement>('.option, .about');
    if (!holder || holder.classList.contains('dismissed') || getComputedStyle(tip).visibility !== 'visible') continue;
    holder.classList.add('dismissed');
    open = true;
  }
  if (open) event.preventDefault();
});
for (const holder of document.querySelectorAll<HTMLElement>('.option, .about')) {
  for (const type of ['focusout', 'mouseleave']) holder.addEventListener(type, () => holder.classList.remove('dismissed'));
}

/* One column (a phone): the SD card and the console take turns below the stage. */
for (const tab of document.querySelectorAll<HTMLElement>('.tabs [role=tab]')) tab.addEventListener('click', () => {
  app.dataset.tab = tab.dataset.tab;
  for (const other of document.querySelectorAll('.tabs [role=tab]')) other.setAttribute('aria-selected', String(other === tab));
  if (tab.dataset.tab === 'console') $('console').scrollTop = $('console').scrollHeight;
});

/* Panel widths: the gutters at the stage's edges set the grid's variables.
   The stage keeps room for a device; widths shrink with the window. */
const defaults = { sd: 300, output: 380 };
const widths = { ...defaults };
try {
  const saved = JSON.parse(localStorage.getItem('eink-qemu-panels') || '{}');
  for (const side of ['sd', 'output'] as const) if (Number.isFinite(saved?.[side])) widths[side] = saved[side];
} catch {}
// The SD minimum fits both labelled upload actions.
const limits = { sd: [260, 420], output: [280, 720] } as const;
const MIN_STAGE = 360;
function clampWidths(changed: 'sd' | 'output' = 'output') {
  for (const side of ['sd', 'output'] as const) widths[side] = Math.round(Math.min(limits[side][1], Math.max(limits[side][0], widths[side])));
  // Give back from the other panel first, then from this one.
  const spare = app.clientWidth - MIN_STAGE;
  const other = changed === 'sd' ? 'output' : 'sd';
  let excess = widths.sd + widths.output - spare;
  for (const side of [other, changed] as const) {
    if (excess <= 0) break;
    const give = Math.min(excess, widths[side] - limits[side][0]);
    widths[side] -= give;
    excess -= give;
  }
}
function applyWidths(changed?: 'sd' | 'output') {
  clampWidths(changed);
  app.style.setProperty('--sd-w', `${widths.sd}px`);
  app.style.setProperty('--output-w', `${widths.output}px`);
}
function saveWidths() {
  try { localStorage.setItem('eink-qemu-panels', JSON.stringify(widths)); } catch {}
}
for (const gutter of document.querySelectorAll<HTMLElement>('.gutter')) {
  const side = gutter.dataset.side as 'sd' | 'output';
  gutter.addEventListener('pointerdown', (event) => {
    event.preventDefault();
    gutter.setPointerCapture(event.pointerId);
    gutter.classList.add('active');
    app.classList.add('resizing');
    const startX = event.clientX, start = widths[side];
    const move = (e: PointerEvent) => {
      widths[side] = start + (side === 'sd' ? e.clientX - startX : startX - e.clientX);
      applyWidths(side);
    };
    const end = () => {
      gutter.classList.remove('active');
      app.classList.remove('resizing');
      gutter.removeEventListener('pointermove', move);
      saveWidths();
    };
    gutter.addEventListener('pointermove', move);
    gutter.addEventListener('lostpointercapture', end, { once: true });
  });
  gutter.addEventListener('dblclick', () => {
    widths[side] = defaults[side];
    applyWidths(side);
    saveWidths();
  });
}
window.addEventListener('resize', () => applyWidths());
applyWidths();

const failed = (error: unknown) => {
  const message = (error as Error)?.message ?? String(error);
  $('status').textContent = `The app could not start: ${message}`;
  appendLines([`Error: the app could not start: ${message}`]);
};
if (backend === 'browser') {
  Promise.all([import('./backend-browser'), import('./app')]).then(([{ browser }, { boot }]) => boot(browser)).catch(failed);
} else if (backend === 'tauri' && import.meta.env.MODE !== 'web') {
  Promise.all([import('./backend-tauri'), import('./app')]).then(([{ tauri }, { boot }]) => boot(tauri)).catch(failed);
} else {
  void import('./demo').then(({ demo }) => demo({ appendConsole: (text: string) => appendLines(text.trimEnd().split('\n')) }));
}
