/* The desktop app: the browser page's app.js, with native QEMU behind the
   Rust commands instead of the WebAssembly runtime iframe. */
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { openUrl } from '@tauri-apps/plugin-opener';
import { api, AppError, type FileInfo, type SdEntry, type SessionInfo, type Settings } from './api';
import { appendLines, clearConsole } from './console';
import { confirm, dialogOpen, prompt } from './dialog';
import { prepareFlash } from './firmware';
import { attachHeap, heapSession, showPane, useElf } from './heap';

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const button = (id: string) => $<HTMLButtonElement>(id);
const appEl = document.querySelector<HTMLElement>('.app')!;

const machineNames: Record<Machine, string> = {
  x3: 'Xteink X3 · 528 × 792', x4: 'Xteink X4 · 480 × 800', x4pro: 'Xteink X4 Pro · 480 × 800', picco: 'BOOX Picco · 480 × 800',
};
const shortNames: Record<Machine, string> = { x3: 'X3', x4: 'X4', x4pro: 'X4 Pro', picco: 'BOOX Picco' };
const bootNames: Record<Machine, string> = {
  x3: 'X3 (ESP32-C3)', x4: 'X4 (ESP32-C3)', x4pro: 'X4 Pro (ESP32-S3)', picco: 'BOOX Picco (ESP32-S3)',
};

let session: SessionInfo = { state: 'idle', machine: null, turbo: false, exit: null, run: 0, seq: -1 };
let platform = '';
let settings: Settings;
let firmware: FileInfo | null = null;
let card: FileInfo | null = null;
let pending = false; // Preparing firmware before the session starts.
let canvas: HTMLCanvasElement | null = null;
let milestones = 0, startedAt = 0, lastDrawAt = 0, bootSeconds: number | null = null, sdFailed = false;

const status = (text: string) => { $('status').textContent = text; };
const timingLabel = () => session.turbo ? 'Turbo (not timing-accurate)' : 'Accurate timing';
const deviceLabel = () => session.machine ? shortNames[session.machine] : 'Device';
const errorMessage = (error: unknown) => (error as Error)?.message ?? String(error);

/* Run state ------------------------------------------------------------ */

const optionIds = ['device', 'panel', 'flash', 'turbo', 'ghosting', 'wifi'];
function paintControls() {
  const idle = session.state === 'idle' && !pending;
  const running = session.state === 'running';
  appEl.classList.toggle('live', session.state !== 'idle' || pending);
  for (const id of optionIds) ($(id) as HTMLButtonElement | HTMLInputElement | HTMLSelectElement).disabled = !idle;
  button('start').disabled = !idle;
  button('stop').disabled = !(running || session.state === 'starting');
  button('reset').disabled = !running;
  button('sd').disabled = !idle;
  const live = running || session.state === 'starting';
  for (const id of ['zoom-in', 'zoom-out', 'zoom-level', 'screenshot']) ($(id) as HTMLButtonElement).disabled = !live || !canvas;
  if (live && canvas) resizeScreen();
  $('machine').textContent = session.machine ? machineNames[session.machine] : 'No device running';
  sdPaint();
}

function onSession(info: SessionInfo) {
  // Events and command results can arrive out of order: keep the newest.
  if (info.seq <= session.seq) return;
  const previous = session.state;
  session = info;
  heapSession(info.state, info.run);
  if (info.state !== 'running') releaseAll();
  // A WebView reloaded during startup learns the board here.
  if ((info.state === 'starting' || info.state === 'running') && info.machine && !canvas) {
    buttons(info.machine);
    createScreen(info.machine);
  }
  if (info.state === 'running' && previous !== 'running') {
    status(`${deviceLabel()} · ${timingLabel()} · Booting…`);
    void sdRefresh();
  }
  if (info.state === 'idle' && previous !== 'idle') {
    const reason = info.exit?.reason?.replace(/\.$/, '') ?? 'unknown reason';
    if (info.exit?.kind === 'crashed') {
      removeScreen(`The emulator exited: ${reason}. Start again to boot with the current SD card.`);
      status(`The emulator exited: ${reason}.`);
    } else if (info.exit?.kind === 'failed' && previous !== 'starting') {
      // Ended by the app after an error (a failed reset or SD operation).
      removeScreen(`The emulator was stopped: ${reason}.`);
      status(`The emulator was stopped: ${reason}.`);
    } else if (info.exit?.kind === 'stopped') {
      removeScreen('Stopped. Start again to use the current SD card.');
      status('Stopped. Start boots again with your current SD card.');
    }
    void sdRefresh();
  }
  paintControls();
}

function fail(message: string) {
  status(message);
  appendLines([`Error: ${message}`]);
  void api.note(`Error: ${message}`);
}

async function start() {
  if (pending || session.state !== 'idle') return;
  if (!firmware) {
    await chooseFirmware();
    return;
  }
  if (!card) {
    fail('No SD card is open. Open an SD image or make a new card from the card menu.');
    return;
  }
  pending = true;
  clearConsole();
  milestones = 0; lastDrawAt = 0; bootSeconds = null; sdFailed = false; startedAt = performance.now();
  paintControls();
  try {
    status('Loading firmware…');
    const input = await api.readFirmware();
    const { flash, machine, composed } = await prepareFlash(input, settings.device, status);
    const composition = composed ? 'Composed 16 MiB flash from release app · ' : '';
    if (composed) {
      appendLines([composition.replace(/ · $/, '')]);
      void api.note(composition.replace(/ · $/, ''));
    }
    buttons(machine);
    createScreen(machine);
    status(`${composition}Booting ${bootNames[machine]} · ${settings.turbo ? 'Turbo (not timing-accurate)' : 'Accurate timing'}…`);
    startedAt = performance.now();
    pending = false;
    // The session events say how the start went; the result only adds
    // what an event may not have delivered yet.
    onSession(await api.start(flash, {
      machine, panel: settings.panel, wifi: settings.wifi, turbo: settings.turbo, ghosting: settings.ghosting,
    }));
    paintControls();
  } catch (error) {
    pending = false;
    if ((error as AppError).code !== 'ECANCELED') {
      removeScreen(errorMessage(error));
      fail(errorMessage(error));
    }
    paintControls();
  }
}

function resetting(prefix = '') {
  milestones = 0; lastDrawAt = 0; bootSeconds = null; sdFailed = false; startedAt = performance.now();
  status(`${prefix}Resetting…`);
}

/* Readiness from the firmware log, as on the page. */
// Panel refresh completions: UC8179/UC8279 boards log their DRF command,
// the X4 (SSD1677) a plain refresh.
const refreshDone = /Wait complete:\s+(?:(?:8179|8279|8279x4|X3)_DRF|refresh)\b/;
function watchLog(lines: string[]) {
  // Late lines from a run that has ended must not overwrite its status.
  if (session.state !== 'running' && session.state !== 'starting') return;
  for (const line of lines) {
    if (line.startsWith('ESP-ROM:') && milestones) {
      // The guest rebooted (a crash, a firmware reset): count again.
      milestones = 0; lastDrawAt = 0; bootSeconds = null; sdFailed = false; startedAt = performance.now();
      status(`${deviceLabel()} · ${timingLabel()} · Restarting…`);
    }
    // CrossPoint reports a card it cannot mount; its Home is then an error screen.
    if (/SD card initialization failed/.test(line)) sdFailed = true;
    // Stock X4 Pro firmware powers itself off unless Power is held at boot.
    if (/reject physical boot before init|POWERON without key/.test(line) && bootSeconds === null) {
      status(`${deviceLabel()} is off · hold Power (P) for a moment to turn it on`);
    }
    if (/main_task: Returned from app_main\(\)/.test(line) && bootSeconds === null) {
      status(`${deviceLabel()} · ${timingLabel()} · Firmware running`);
    }
    if (refreshDone.test(line)) {
      lastDrawAt = performance.now();
      if (++milestones === 3) {
        bootSeconds = (lastDrawAt - startedAt) / 1000;
        if (!sdFailed) status(`${deviceLabel()} · ${timingLabel()} · Home drawn in ${bootSeconds.toFixed(2)} s · finishing startup…`);
      }
    }
    if (line.includes('[MEM]')) {
      if (sdFailed) {
        status(`${deviceLabel()} · The firmware could not mount the SD card · check it in the SD card panel`);
      } else if (bootSeconds !== null) {
        status(`${deviceLabel()} · ${timingLabel()} · Ready · Home drawn in ${bootSeconds.toFixed(2)} s`);
      } else if (milestones && performance.now() - lastDrawAt > 3000) {
        // Firmware that resumes a book or an app draws fewer full refreshes
        // than a boot to Home; once the screen is quiet and it reports in, it runs.
        status(`${deviceLabel()} · ${timingLabel()} · Ready`);
      }
    }
  }
}

/* Screen ----------------------------------------------------------------- */

let image: ImageData | null = null;
let context: CanvasRenderingContext2D | null = null;

function createScreen(machine: Machine) {
  const [width, height] = machine === 'x3' ? [528, 792] : [480, 800];
  canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  canvas.setAttribute('aria-label', `${shortNames[machine]} display`);
  context = canvas.getContext('2d');
  context!.fillStyle = '#f5f4ef';
  context!.fillRect(0, 0, width, height);
  image = null;
  attachTouch(canvas, machine);
  $('screen').replaceChildren(canvas);
  resizeScreen();
}

function removeScreen(message: string) {
  releaseTouch?.();
  canvas = null; context = null; image = null;
  $('screen').style.width = '';
  $('screen').style.height = '';
  $('screen').textContent = message;
  $('keys').replaceChildren();
}

// Display messages that arrive before attach() returns (the current screen
// is sent first) wait until the session and canvas are known.
let attached = false;
const early: ArrayBuffer[] = [];
function onDisplay(buffer: ArrayBuffer) {
  if (!attached) {
    early.push(buffer);
    return;
  }
  const view = new DataView(buffer);
  const kind = view.getUint32(0, true);
  if (kind === 1) {
    const width = view.getUint32(4, true), height = view.getUint32(8, true);
    if (!canvas && session.machine) createScreen(session.machine);
    if (!canvas) return;
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
      resizeScreen();
    }
    context = canvas.getContext('2d');
    image = context!.createImageData(width, height);
    image.data.fill(255);
    return;
  }
  if (kind !== 2 || !canvas || !context) return;
  if (!image) {
    image = context.createImageData(canvas.width, canvas.height);
    image.data.fill(255);
  }
  const bytes = new Uint8Array(buffer);
  const count = view.getUint32(4, true);
  const stride = canvas.width, data = image.data;
  let at = 8, minX = Infinity, minY = Infinity, maxX = 0, maxY = 0;
  for (let n = 0; n < count; n++) {
    const x = view.getUint32(at, true), y = view.getUint32(at + 4, true);
    const w = view.getUint32(at + 8, true), h = view.getUint32(at + 12, true);
    at += 16;
    for (let row = 0; row < h; row++) {
      let target = ((y + row) * stride + x) * 4;
      const source = at + row * w;
      for (let col = 0; col < w; col++, target += 4) {
        const gray = bytes[source + col];
        data[target] = gray; data[target + 1] = gray; data[target + 2] = gray;
      }
    }
    at += w * h;
    minX = Math.min(minX, x); minY = Math.min(minY, y); maxX = Math.max(maxX, x + w); maxY = Math.max(maxY, y + h);
  }
  if (count) context.putImageData(image, 0, 0, minX, minY, maxX - minX, maxY - minY);
}

/* Screen size: 'fit' (fills the stage, may resample), 'auto' (the largest
   whole-pixel size up to 100% that fits the width) or a CSS scale in whole
   physical pixels. */
let zoomMode: 'fit' | 'auto' | number = 'fit';
try {
  const saved = localStorage.getItem('eink-qemu-zoom');
  zoomMode = saved === 'auto' ? 'auto' : Number(saved) || 'fit';
} catch {}

function screenLayout() {
  const dpr = window.devicePixelRatio || 1;
  const width = canvas!.width, height = canvas!.height;
  const padding = (element: Element, axis: 'x' | 'y') => {
    const style = getComputedStyle(element);
    return axis === 'x' ? parseFloat(style.paddingLeft) + parseFloat(style.paddingRight)
      : parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
  };
  const style = getComputedStyle(appEl);
  const device = document.querySelector('.device')!, bezel = document.querySelector('.bezel')!;
  // The stage shrinks to the device, so measure what the side panels leave.
  let availableWidth = appEl.clientWidth - parseFloat(style.getPropertyValue('--sd-max')) - parseFloat(style.getPropertyValue('--output-min'));
  const availableHeight = document.querySelector<HTMLElement>('.stage')!.clientHeight -
    document.querySelector<HTMLElement>('.stage-bar')!.offsetHeight - $('keys').offsetHeight -
    padding(device, 'y') - padding(bezel, 'y');
  availableWidth -= padding(device, 'x') + padding(bezel, 'x');
  const auto = Math.max(1, Math.min(Math.floor(dpr), Math.floor(availableWidth * dpr / width)));
  const fit = Math.max(0.25, Math.min(availableWidth / width, availableHeight / height));
  return { dpr, width, height, auto, fit, max: Math.max(auto, 2 * Math.ceil(dpr)) };
}
type Layout = ReturnType<typeof screenLayout>;
function screenScale(layout: Layout) {
  if (zoomMode === 'fit') return layout.fit;
  if (zoomMode === 'auto') return layout.auto / layout.dpr;
  return Math.min(layout.max, Math.max(1, Math.round(zoomMode * layout.dpr))) / layout.dpr;
}
function zoomOptions(layout: Layout, scale: number) {
  const percent = (value: number) => `${Math.round(value * 100)}%`;
  const options: [string, string][] = [['fit', `Fit · ${percent(layout.fit)}`], ['auto', `Auto · ${percent(layout.auto / layout.dpr)}`]];
  for (let pixels = 1; pixels <= layout.max; pixels++) options.push([String(pixels / layout.dpr), percent(pixels / layout.dpr)]);
  const selected = zoomMode === 'fit' || zoomMode === 'auto' ? zoomMode : String(scale);
  $('zoom-level').replaceChildren(...options.map(([value, label]) => new Option(label, value, false, value === selected)));
}
function resizeScreen() {
  if (!canvas) return;
  const layout = screenLayout(), scale = screenScale(layout);
  $('screen').style.width = `${layout.width * scale}px`;
  $('screen').style.height = `${layout.height * scale}px`;
  canvas.style.width = `${layout.width * scale}px`;
  canvas.style.height = `${layout.height * scale}px`;
  const pixels = scale * layout.dpr, exact = Math.abs(pixels - Math.round(pixels)) < 1e-6;
  // Whole pixels stay sharp; a fractional Fit scale reads better smoothed.
  canvas.style.imageRendering = exact ? 'pixelated' : 'auto';
  zoomOptions(layout, scale);
  button('zoom-out').disabled = pixels <= 1 + 1e-6;
  button('zoom-in').disabled = pixels >= layout.max - 1e-6;
}
function setZoom(mode: typeof zoomMode) {
  zoomMode = mode;
  try {
    if (mode === 'fit') localStorage.removeItem('eink-qemu-zoom');
    else localStorage.setItem('eink-qemu-zoom', String(mode));
  } catch {}
  resizeScreen();
}
function stepZoom(direction: number) {
  if (!canvas) return;
  // Steps land on whole physical pixels, also when leaving Fit.
  const layout = screenLayout(), pixels = screenScale(layout) * layout.dpr;
  const next = direction > 0 ? Math.floor(pixels + 1e-6) + 1 : Math.ceil(pixels - 1e-6) - 1;
  setZoom(Math.min(layout.max, Math.max(1, next)) / layout.dpr);
}
function watchPixelRatio() {
  // Moving between monitors can change DPR without changing the layout width.
  matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`).addEventListener('change', () => {
    resizeScreen();
    watchPixelRatio();
  }, { once: true });
}

async function screenshot() {
  if (!canvas || !session.machine) return;
  const machine = session.machine;
  const blob = await new Promise<Blob | null>((resolve) => canvas!.toBlob(resolve, 'image/png'));
  if (!blob) return;
  try {
    const path = await api.saveScreenshot(new Uint8Array(await blob.arrayBuffer()), `${machine}-screen.png`);
    if (path) status(`Screenshot saved to ${path}`);
  } catch (error) { fail(errorMessage(error)); }
}

/* Keys and touch ---------------------------------------------------------- */

const keyboard: Record<string, string> = {
  Backspace: 'Back', Escape: 'Back', Enter: 'Confirm', NumpadEnter: 'Confirm', ArrowLeft: 'Left', ArrowRight: 'Right',
  ArrowUp: 'Up', ArrowDown: 'Down', KeyP: 'Power', Home: 'Home',
};
const keyHints: Record<string, string> = { Back: 'Esc', Confirm: 'Enter', Left: '←', Right: '→', Up: '↑', Down: '↓', Power: 'P', Home: 'Home' };
// Each held device key has owners (keyboard codes, pointer ids); it is
// released when the last owner lets go.
const held = new Map<string, Set<string>>();

function press(name: string, owner: string, down: boolean) {
  if (down && session.state !== 'running') return;
  const owners = held.get(name) ?? new Set<string>();
  const wasDown = owners.size > 0;
  if (down) owners.add(owner); else owners.delete(owner);
  if (owners.size) held.set(name, owners); else held.delete(name);
  const isDown = owners.size > 0;
  if (isDown !== wasDown) void api.key(name, isDown);
  for (const element of $('keys').querySelectorAll<HTMLElement>(`button[data-key="${name}"]`)) element.classList.toggle('held', isDown);
}
function releaseAll() {
  releaseTouch?.();
  for (const [name] of held) {
    held.delete(name);
    if (session.state === 'running') void api.key(name, false);
  }
  for (const element of $('keys').querySelectorAll('.held')) element.classList.remove('held');
}

function buttons(machine: Machine) {
  const keys = $('keys');
  keys.replaceChildren();
  keys.dataset.machine = machine;
  $('key-hint').textContent = machine === 'x3' || machine === 'x4' ? 'Keyboard: arrows · Enter · Esc · P'
    : machine === 'picco' ? 'Keyboard: ↑ ↓ · P · touch the screen' : 'Keyboard: ↑ ↓ · Home · P · touch the screen';
  const names = machine === 'x3' || machine === 'x4' ? ['Back', 'Confirm', 'Up', 'Down', 'Left', 'Right', 'Power']
    : machine === 'picco' ? ['Up', 'Down', 'Power'] : ['Home', 'Up', 'Down', 'Power'];
  for (const name of names) {
    const element = document.createElement('button');
    element.type = 'button';
    element.textContent = name;
    element.dataset.key = name;
    // The hint is CSS-generated so the button's text stays the key name.
    element.dataset.kbd = keyHints[name];
    element.addEventListener('pointerdown', (event) => {
      event.preventDefault();
      element.setPointerCapture(event.pointerId);
      press(name, `pointer:${event.pointerId}`, true);
    });
    for (const type of ['pointerup', 'pointercancel', 'lostpointercapture'] as const) {
      element.addEventListener(type, (event) => press(name, `pointer:${event.pointerId}`, false));
    }
    // Keyboard and assistive-technology activation.
    element.addEventListener('click', (event) => {
      if (event.detail === 0) {
        press(name, 'click', true);
        setTimeout(() => press(name, 'click', false), 150);
      }
    });
    keys.append(element);
  }
}

// Lifts the emulated finger, if one is down (lost capture, blur, reset, stop).
let releaseTouch: (() => void) | null = null;

function attachTouch(target: HTMLCanvasElement, machine: Machine) {
  releaseTouch?.();
  releaseTouch = null;
  if (machine !== 'x4pro' && machine !== 'picco') return;
  let active: number | null = null, frame = 0, last: [number, number] = [0, 0];
  releaseTouch = () => {
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
    if (active === null) return;
    active = null;
    void api.touch(last[0], last[1], false);
  };
  const point = (event: PointerEvent): [number, number] => {
    const rect = target.getBoundingClientRect();
    return [(event.clientX - rect.left) / rect.width, (event.clientY - rect.top) / rect.height];
  };
  target.style.touchAction = 'none';
  target.addEventListener('pointerdown', (event) => {
    if (active !== null || event.button !== 0 || session.state !== 'running') return;
    event.preventDefault();
    active = event.pointerId;
    target.setPointerCapture(event.pointerId);
    last = point(event);
    void api.touch(last[0], last[1], true);
  });
  target.addEventListener('pointermove', (event) => {
    if (event.pointerId !== active) return;
    last = point(event);
    // One move per animation frame keeps the monitor queue short.
    frame ||= requestAnimationFrame(() => {
      frame = 0;
      if (active !== null) void api.touch(last[0], last[1], true);
    });
  });
  const end = (event: PointerEvent) => {
    if (event.pointerId === active) releaseTouch?.();
  };
  target.addEventListener('pointerup', end);
  target.addEventListener('pointercancel', end);
  target.addEventListener('lostpointercapture', end);
}

function deviceKeyboard(event: KeyboardEvent) {
  const name = keyboard[event.code];
  if (!name) return;
  const owner = `keyboard:${event.code}`;
  // A release belongs to its press, even after focus or modifiers change.
  if (event.type === 'keyup') {
    if (held.get(name)?.has(owner)) { event.preventDefault(); press(name, owner, false); }
    return;
  }
  if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.isComposing || dialogOpen()) return;
  const target = event.target as HTMLElement;
  // As on the page: typing in a field and the SD card panel keep their keys.
  if (target.closest('input, textarea, select, .sd, [role=dialog]')) return;
  if (session.state !== 'running') return;
  event.preventDefault();
  if (!event.repeat) press(name, owner, true);
}

/* Firmware and card ------------------------------------------------------- */

function paintFirmware() {
  $('flash-name').textContent = firmware ? firmware.name : 'Choose a .bin or .xota…';
  $('flash-name').classList.toggle('empty', !firmware);
  $('flash').title = firmware ? firmware.path : 'Firmware .bin (app or merged), X4 Pro .xota update, or a 16 MiB flash dump';
}

async function useFirmware(choice: Promise<FileInfo | null>) {
  try {
    const chosen = await choice;
    if (!chosen) return;
    firmware = chosen;
    paintFirmware();
    // Choosing a firmware starts it; Start reruns the same file after Stop.
    await start();
  } catch (error) { fail(errorMessage(error)); }
}

async function chooseFirmware() {
  if (session.state !== 'idle' || pending) return;
  await useFirmware(api.pickFirmware());
}

function paintCard() {
  const text = card ? card.path : 'No SD card is open: use the card menu.';
  $('sd-save').textContent = text;
  $('sd-save').title = text;
  $('sd-save').classList.toggle('error', !card);
}

async function cardMenu() {
  if (session.state !== 'idle' || pending) return;
  // A press on the button closed the open menu; its click must not reopen it.
  if (menuJustClosed) { menuJustClosed = false; return; }
  const choice = await chooseCardAction();
  try {
    const chosen = choice === 'open' ? await api.pickOpenCard() : choice === 'new' ? await api.pickNewCard() : null;
    if (!chosen) return;
    card = chosen;
    paintCard();
    sd.cwd = '/';
    await sdRefresh();
  } catch (error) { sdError(error); }
}

let menuJustClosed = false;
function chooseCardAction(): Promise<'open' | 'new' | null> {
  return new Promise((resolve) => {
    const menu = $('card-menu');
    const anchor = button('sd');
    menu.hidden = false;
    const items = [...menu.querySelectorAll<HTMLButtonElement>('button')];
    const close = (value: 'open' | 'new' | null) => {
      menu.hidden = true;
      document.removeEventListener('pointerdown', outside, true);
      document.removeEventListener('keydown', keys, true);
      menu.removeEventListener('focusout', leave);
      anchor.setAttribute('aria-expanded', 'false');
      anchor.focus();
      resolve(value);
    };
    const outside = (event: Event) => {
      if (menu.contains(event.target as Node)) return;
      menuJustClosed = anchor.contains(event.target as Node);
      close(null);
    };
    // WebKit does not focus buttons on click: keep focus in the menu so a
    // click is not taken for focus leaving it.
    menu.onmousedown = (event) => event.preventDefault();
    const leave = (event: FocusEvent) => { if (event.relatedTarget && !menu.contains(event.relatedTarget as Node)) close(null); };
    // Escape closes; arrows, Home and End move between the items.
    const keys = (event: KeyboardEvent) => {
      const at = items.indexOf(document.activeElement as HTMLButtonElement);
      const move = { ArrowDown: at + 1, ArrowUp: at - 1, Home: 0, End: items.length - 1 }[event.key];
      if (event.key === 'Escape') { event.preventDefault(); close(null); } else if (move !== undefined) {
        event.preventDefault();
        items[(move + items.length) % items.length].focus();
      }
    };
    menu.querySelector<HTMLButtonElement>('[data-card="open"]')!.onclick = () => close('open');
    menu.querySelector<HTMLButtonElement>('[data-card="new"]')!.onclick = () => close('new');
    anchor.setAttribute('aria-expanded', 'true');
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('keydown', keys, true);
    menu.addEventListener('focusout', leave);
    items[0].focus();
  });
}

async function copyCard() {
  await sdRun(async () => {
    const path = await api.pickCopyCard();
    if (path) sdStatusFlash(`Saved a copy to ${path}`);
  });
}

/* SD card explorer: operations run in Rust on the image file. While the
   emulator runs, Rust pauses the guest around them and resets the device
   after a change so the firmware mounts the card afresh. */
// ready: the last listing worked. An unreadable card can still be used by
// the emulator, but the explorer only allows retrying and switching cards.
const sd = { cwd: '/', entries: [] as SdEntry[], free: 0, busy: false, info: '', ready: false };
const sdPanel = document.querySelector<HTMLElement>('.sd')!;
const sdUsable = () => !sd.busy && !pending && (session.state === 'idle' || session.state === 'running');
const joinPath = (dir: string, name: string) => dir === '/' ? `/${name}` : `${dir}/${name}`;

function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GiB`;
}
function icon(name: string) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'icon');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}
function sdStatus(text: string, error = false) {
  $('sd-info').textContent = text;
  $('sd-info').classList.toggle('error', error);
}
function sdStatusFlash(text: string) {
  sdStatus(text);
  setTimeout(() => { if ($('sd-info').textContent === text) sdStatus(sd.info); }, 4000);
}
function sdError(error: unknown) {
  const message = errorMessage(error);
  $('sd-error').textContent = message;
  appendLines([`SD card: ${message}`]);
}
function sdPaint() {
  const on = sdUsable();
  for (const element of sdPanel.querySelectorAll<HTMLButtonElement>('button')) {
    if (element.id === 'sd' || element.closest('#card-menu')) continue;
    const needsListing = element.id !== 'sd-refresh' && element.id !== 'download';
    element.disabled = !on || element.hasAttribute('aria-current') || (needsListing && !sd.ready) || (!card && element.id === 'download');
  }
  sdPanel.classList.toggle('busy', sd.busy);
}

async function sdRun(action: () => Promise<void>) {
  // One explorer operation at a time; errors show in the panel and the log.
  if (!sdUsable()) return;
  sd.busy = true;
  sdPaint();
  $('sd-error').textContent = '';
  try { await action(); } catch (error) {
    sdError(error);
    await sdRefresh().catch(() => {});
  } finally {
    sd.busy = false;
    sdPaint();
  }
}

async function sdRefresh(path = sd.cwd): Promise<void> {
  if (session.state === 'starting' || session.state === 'stopping') return;
  let listing;
  try { listing = await api.sdList(path); } catch (error) {
    // The folder may have gone away (rename, delete, new card): fall back.
    if ((error as AppError).code === 'ENOENT' && path !== '/') return sdRefresh('/');
    sd.entries = [];
    sd.ready = false;
    $('sd-rows').replaceChildren();
    $('sd-path').replaceChildren();
    $('sd-meter').style.width = '0';
    const code = (error as AppError).code;
    sd.info = '';
    sdStatus(code === 'EINVAL' || code === 'ENOTSUP' ? `${errorMessage(error)} The emulator can still use this image.` : errorMessage(error), true);
    sdPaint();
    return;
  }
  sd.ready = true;
  sd.cwd = path;
  sd.entries = listing.entries;
  sd.free = listing.free;
  $('sd-meter').style.width = `${listing.total ? 100 * (1 - listing.free / listing.total) : 0}%`;
  sd.info = `${listing.fsType} · ${formatSize(listing.free)} free of ${formatSize(listing.total)}` +
    (session.state === 'running' ? ' · changes reset the device' : '');
  sdStatus(sd.info);
  sdRender();
}

function sdRender() {
  const crumbs = $('sd-path');
  crumbs.replaceChildren();
  const parts = sd.cwd.split('/').filter(Boolean);
  const root = document.createElement('button');
  root.type = 'button';
  root.textContent = 'SD card';
  root.dataset.path = '/';
  if (!parts.length) root.setAttribute('aria-current', 'location');
  crumbs.append(root);
  parts.forEach((part, index) => {
    const separator = document.createElement('span');
    separator.textContent = '/';
    const crumb = document.createElement('button');
    crumb.type = 'button';
    crumb.textContent = part;
    crumb.dataset.path = '/' + parts.slice(0, index + 1).join('/');
    if (index === parts.length - 1) crumb.setAttribute('aria-current', 'location');
    crumbs.append(separator, crumb);
  });

  const rows = $('sd-rows');
  rows.replaceChildren();
  for (const entry of sd.entries) {
    const row = document.createElement('tr');
    row.dataset.name = entry.name;
    if (entry.mtime) row.title = `Modified ${new Date(entry.mtime).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })}`;
    const name = document.createElement('td');
    name.className = 'name';
    const label = document.createElement('span');
    label.textContent = entry.name;
    if (entry.isDir) {
      const open = document.createElement('button');
      open.className = 'dir entry';
      open.dataset.action = 'open';
      open.type = 'button';
      open.append(icon('folder'), label);
      name.append(open);
    } else {
      const file = document.createElement('div');
      file.className = 'entry';
      file.append(icon('file'), label);
      name.append(file);
    }
    const size = document.createElement('td');
    size.className = 'size';
    size.textContent = entry.isDir ? '' : formatSize(entry.size);
    const actions = document.createElement('td');
    actions.className = 'ops';
    for (const [action, text, symbol] of [['download', 'Save to computer', 'download'], ['rename', 'Rename', 'pencil'], ['remove', 'Delete', 'trash']]) {
      if (action === 'download' && entry.isDir) continue;
      const control = document.createElement('button');
      control.type = 'button';
      control.dataset.action = action;
      control.title = text;
      control.setAttribute('aria-label', `${text} ${entry.name}`);
      control.append(icon(symbol));
      actions.append(control);
    }
    row.append(name, size, actions);
    rows.append(row);
  }
  if (!sd.entries.length) {
    const row = document.createElement('tr');
    row.className = 'empty';
    const cell = document.createElement('td');
    cell.colSpan = 3;
    cell.textContent = 'Empty folder';
    row.append(cell);
    rows.append(row);
  }
  sdPaint();
}

async function sdUpload(sources: string[]) {
  if (!sources.length || !card || !sd.ready) return;
  const plan = await api.sdPlanUpload(sd.cwd, sources);
  if (!plan.files) {
    sdStatusFlash('Nothing to copy: hidden files and links are skipped.');
    return;
  }
  if (plan.conflicts.length) {
    const ok = await confirm(
      plan.conflicts.length === 1 ? `Replace "${plan.conflicts[0].split('/').pop()}" on the SD card?` : `Replace ${plan.conflicts.length} existing files on the SD card?`,
      plan.conflicts.length > 1 ? plan.conflicts.slice(0, 5).join('\n') + (plan.conflicts.length > 5 ? '\n…' : '') : undefined,
      'Replace',
    );
    if (!ok) return;
  }
  sdStatus(`Copying ${plan.files === 1 ? '1 file' : `${plan.files} files`} (${formatSize(plan.bytes)})…`);
  await api.sdUpload(sd.cwd, sources, plan.conflicts);
  await sdRefresh();
}

function validName(name: string) {
  name = name.normalize('NFC');
  const invalid = !name || name.length > 255 || /[\x00-\x1f\uffff"*/:<>?\\|]/u.test(name) || /[ .]$/u.test(name) ||
    name === '.' || name === '..' || /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)/i.test(name);
  if (invalid) throw new AppError('EINVAL', `"${name}" is not a valid FAT file name.`);
  return name;
}

async function sdRowAction(action: string, name: string) {
  const entry = sd.entries.find((item) => item.name === name);
  if (!entry) return;
  const path = joinPath(sd.cwd, name);
  if (action === 'open') {
    await sdRefresh(path);
  } else if (action === 'download') {
    const dest = await api.sdPickExport(path);
    if (dest) sdStatusFlash(`Saved ${name}`);
  } else if (action === 'rename') {
    const target = await prompt(`Rename "${name}" to`, name, 'Rename');
    if (!target || target === name) return;
    await api.sdRename(path, validName(target));
    await sdRefresh();
  } else if (action === 'remove') {
    const ok = await confirm(entry.isDir ? `Delete the folder "${name}" and everything in it?` : `Delete "${name}"?`, undefined, 'Delete', true);
    if (!ok) return;
    await api.sdRemove(path);
    await sdRefresh();
  }
}

/* File drops: Tauri hands over paths and a position. Firmware drops onto the
   chooser or the device stage; files and folders drop onto the SD panel. */
function dropTarget(x: number, y: number): 'sd' | 'firmware' | null {
  // macOS reports drop positions in points already (wry passes Cocoa
  // coordinates through); other platforms report physical pixels.
  const scale = platform === 'macos' ? 1 : window.devicePixelRatio;
  const element = document.elementFromPoint(x / scale, y / scale);
  if (element?.closest('.sd')) return sdUsable() && card && sd.ready ? 'sd' : null;
  if (element?.closest('.stage, .file-chip')) return session.state === 'idle' && !pending ? 'firmware' : null;
  return null;
}
function paintDrop(target: 'sd' | 'firmware' | null) {
  sdPanel.classList.toggle('drop', target === 'sd');
  document.querySelector('.stage')!.classList.toggle('firmware-drop', target === 'firmware');
  $('flash').classList.toggle('firmware-drop', target === 'firmware');
}

async function onDrop(paths: string[], x: number, y: number) {
  paintDrop(null);
  // An ELF goes to the heap view wherever it lands.
  if (paths.length === 1 && /\.elf$/i.test(paths[0])) {
    await useElf(api.heapUseElf(paths[0]));
    return;
  }
  const target = dropTarget(x, y);
  if (target === 'sd') {
    await sdRun(() => sdUpload(paths));
  } else if (target === 'firmware') {
    if (paths.length !== 1 || !/\.(bin|xota)$/i.test(paths[0])) {
      status('Drop one firmware file: a .bin, or an X4 Pro .xota update.');
      return;
    }
    await useFirmware(api.useFirmware(paths[0]));
  }
}

/* Wiring ------------------------------------------------------------------ */

export async function boot() {
  // Before attaching, so no session or log message is missed.
  await listen<SessionInfo>('session', (event) => onSession(event.payload));
  await listen('sd-reset', () => {
    releaseTouch?.();
    resetting('SD card changed · ');
  });
  const state = await api.attach((lines) => {
    appendLines(lines);
    watchLog(lines);
  }, onDisplay);
  settings = state.settings;
  firmware = state.firmware;
  card = state.card;
  if (state.session.seq > session.seq) session = state.session;
  attachHeap({ snapshot: api.heapSnapshot, stack: api.heapStack, pickElf: api.heapPickElf, forgetElf: api.heapForgetElf, openWindow: api.heapWindow }, state.elf);
  heapSession(session.state, session.run);
  if (state.pane === 'heap' || state.pane === 'console') showPane(state.pane);
  if (state.pane === 'window') { showPane('heap'); void api.heapWindow(); }
  platform = state.platform;
  $<HTMLSelectElement>('device').value = settings.device;
  $<HTMLSelectElement>('panel').value = settings.panel;
  $<HTMLInputElement>('wifi').checked = settings.wifi;
  $<HTMLInputElement>('ghosting').checked = settings.ghosting;
  $<HTMLInputElement>('turbo').checked = settings.turbo;
  $('panel-choice').hidden = !['auto', 'x4pro'].includes(settings.device);
  paintFirmware();
  paintCard();
  if (session.machine && session.state !== 'idle') {
    // A reloaded WebView: the session kept running; the screen follows.
    buttons(session.machine);
    createScreen(session.machine);
    status(`${deviceLabel()} · ${timingLabel()} · ${session.state === 'running' ? 'Running' : 'Starting…'}`);
  } else status(firmware ? 'Start boots the chosen firmware.' : 'Choose a flash image to begin.');
  // The current screen came with attach; draw it on the canvas made above.
  attached = true;
  for (const message of early.splice(0)) onDisplay(message);
  for (const problem of state.problems) {
    appendLines([`Error: ${problem}`]);
    void api.note(`Error: ${problem}`);
  }
  if (state.problems.length) status(state.problems[0]);
  paintControls();
  void sdRefresh();
  if (state.autostart && firmware && session.state === 'idle') void start();

  const save = async (patch: Parameters<typeof api.settings>[0]) => {
    try { settings = await api.settings(patch); } catch (error) { fail(errorMessage(error)); }
  };
  $('device').addEventListener('change', async () => {
    await save({ device: $<HTMLSelectElement>('device').value as Settings['device'] });
    if (firmware) void start();
  });
  $('panel').addEventListener('change', async () => {
    await save({ panel: $<HTMLSelectElement>('panel').value as Settings['panel'] });
    if (firmware) void start();
  });
  for (const id of ['wifi', 'ghosting', 'turbo'] as const) {
    $(id).addEventListener('change', () => void save({ [id]: $<HTMLInputElement>(id).checked }));
  }
  $('flash').addEventListener('click', () => void chooseFirmware());
  document.querySelector('[data-for="flash"]')?.addEventListener('click', () => void chooseFirmware());
  button('start').addEventListener('click', () => void start());
  button('stop').addEventListener('click', () => {
    releaseAll();
    void api.stop().catch((error) => fail(errorMessage(error)));
  });
  button('reset').addEventListener('click', () => {
    releaseAll();
    resetting();
    void api.reset().catch((error) => fail(errorMessage(error)));
  });
  button('zoom-in').addEventListener('click', () => stepZoom(1));
  button('zoom-out').addEventListener('click', () => stepZoom(-1));
  $('zoom-level').addEventListener('change', () => {
    const value = $<HTMLSelectElement>('zoom-level').value;
    setZoom(value === 'fit' || value === 'auto' ? value : Number(value));
  });
  button('screenshot').addEventListener('click', () => void screenshot());
  for (const selector of ['.app', '.stage']) new ResizeObserver(resizeScreen).observe(document.querySelector(selector)!);
  watchPixelRatio();

  document.addEventListener('keydown', deviceKeyboard);
  document.addEventListener('keyup', deviceKeyboard);
  window.addEventListener('blur', releaseAll);

  button('sd').addEventListener('click', () => void cardMenu());
  button('download').addEventListener('click', () => void copyCard());
  button('sd-refresh').addEventListener('click', () => void sdRun(() => sdRefresh()));
  button('sd-upload').addEventListener('click', () => void sdRun(async () => {
    await sdUpload(await api.sdPickSources());
  }));
  button('sd-mkdir').addEventListener('click', () => void sdRun(async () => {
    const name = await prompt('New folder name', '', 'Create');
    if (!name) return;
    await api.sdMkdir(joinPath(sd.cwd, validName(name)));
    await sdRefresh();
  }));
  $('sd-path').addEventListener('click', (event) => {
    const crumb = (event.target as Element).closest<HTMLButtonElement>('button[data-path]');
    if (crumb && !crumb.hasAttribute('aria-current')) void sdRun(() => sdRefresh(crumb.dataset.path));
  });
  $('sd-rows').addEventListener('click', (event) => {
    const control = (event.target as Element).closest<HTMLButtonElement>('button[data-action]');
    if (!control) return;
    const name = control.closest('tr')!.dataset.name!;
    void sdRun(() => sdRowAction(control.dataset.action!, name));
  });

  await getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    if (payload.type === 'enter' || payload.type === 'over') paintDrop(dropTarget(payload.position.x, payload.position.y));
    else if (payload.type === 'drop') void onDrop(payload.paths, payload.position.x, payload.position.y);
    else paintDrop(null);
  });

  // Project links open in the browser, not in the app's WebView.
  document.addEventListener('click', (event) => {
    const link = (event.target as Element).closest<HTMLAnchorElement>('a[href^="https://"]');
    if (!link) return;
    event.preventDefault();
    openUrl(link.href).catch((error) => fail(errorMessage(error)));
  });
}
