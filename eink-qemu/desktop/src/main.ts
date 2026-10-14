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

/* Panel widths: the gutters at the stage's edges set the grid's variables. */
const widths = { sd: 300, output: 380 };
try { Object.assign(widths, JSON.parse(localStorage.getItem('eink-qemu-panels') || '{}')); } catch {}
const limits = { sd: [240, 420], output: [280, 720] } as const;
function applyWidths() {
  app.style.setProperty('--sd-w', `${widths.sd}px`);
  app.style.setProperty('--output-w', `${widths.output}px`);
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
      const delta = side === 'sd' ? e.clientX - startX : startX - e.clientX;
      widths[side] = Math.round(Math.min(limits[side][1], Math.max(limits[side][0], start + delta)));
      applyWidths();
    };
    const end = () => {
      gutter.classList.remove('active');
      app.classList.remove('resizing');
      gutter.removeEventListener('pointermove', move);
      try { localStorage.setItem('eink-qemu-panels', JSON.stringify(widths)); } catch {}
    };
    gutter.addEventListener('pointermove', move);
    gutter.addEventListener('lostpointercapture', end, { once: true });
  });
  gutter.addEventListener('dblclick', () => {
    widths[side] = side === 'sd' ? 300 : 380;
    applyWidths();
  });
}
applyWidths();

// Without the Tauri runtime (a plain browser on the dev server), the page
// shows mock content so the layout can be checked.
if ('__TAURI_INTERNALS__' in window && !new URLSearchParams(location.search).has('demo')) {
  void import('./app').then((app) => app.boot());
} else {
  demo({ appendConsole: (text: string) => appendLines(text.trimEnd().split('\n')) });
}
