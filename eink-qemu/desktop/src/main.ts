import './desktop.css';
import { demo } from './demo';
import { appendLines } from './console';

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const app = document.querySelector<HTMLElement>('.app')!;

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
  if (event.code === 'Escape' && closeTips()) event.preventDefault();
});

/* The X4 Pro panel choice only applies to that board. */
function showPanelChoice() {
  $('panel-choice').hidden = !['auto', 'x4pro'].includes($<HTMLSelectElement>('device').value);
}
$('device').addEventListener('change', showPanelChoice);
showPanelChoice();

/* Panel widths: the gutters at the stage's edges set the grid's variables.
   The stage keeps room for a device; widths shrink with the window. */
const defaults = { sd: 300, output: 380 };
const widths = { ...defaults };
try {
  const saved = JSON.parse(localStorage.getItem('eink-qemu-panels') || '{}');
  for (const side of ['sd', 'output'] as const) if (Number.isFinite(saved?.[side])) widths[side] = saved[side];
} catch {}
// The SD minimum fits its toolbar (Upload and four icon buttons).
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

// Without the Tauri runtime (a plain browser on the dev server), the page
// shows mock content so the layout can be checked.
if ('__TAURI_INTERNALS__' in window && !new URLSearchParams(location.search).has('demo')) {
  void import('./app').then((app) => app.boot());
} else {
  demo({ appendConsole: (text: string) => appendLines(text.trimEnd().split('\n')) });
}
