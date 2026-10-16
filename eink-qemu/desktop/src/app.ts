/* The page: firmware card, SD card explorer, device stage, console and
   status bar over a Backend (backend.ts), which is native QEMU behind the
   desktop's Rust commands or the WebAssembly build in a runtime iframe. */
import type { Backend, DropHandlers, FileRef, Screen } from './backend';
import { appendLines, clearConsole } from './console';
import { choose, confirm, dialogOpen, prompt } from './dialog';
import { prepareFlash } from './firmware';
import { attachHeap, heapSession, setElf, setHeapAvailable, showPane, useElf } from './heap';
import { Readiness } from './readiness';
import { attachSetup, closeSetup, paintSetup } from './setup';
import { AppError, type CardInfo, type FileInfo, type SdEntry, type SessionInfo, type Settings, type Setup } from './types';

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

let api: Backend;
let session: SessionInfo = { state: 'idle', machine: null, turbo: false, exit: null, run: 0, seq: -1 };
let platform = '';
let settings: Settings;
/** The firmware card: firmware, device, ELF, recent firmware. */
let setup: Setup | null = null;
let firmware: FileInfo | null = null;
let card: CardInfo | null = null;
let pending = false; // Preparing firmware before the session starts.
let screen: Screen | null = null;

const status = (text: string) => { $('status').textContent = text; };
const timingLabel = () => session.turbo ? 'Turbo (not timing-accurate)' : 'Accurate timing';
const deviceLabel = () => session.machine ? shortNames[session.machine] : 'Device';
const errorMessage = (error: unknown) => (error as Error)?.message ?? String(error);
/* Readiness from the firmware log and the screen. window.bootTimeSeconds is
   a hook for the browser acceptance tests. */
const readiness = new Readiness(status, deviceLabel, timingLabel);
const sessionLive = () => session.state === 'running' || session.state === 'starting';
function watchLog(lines: string[]) {
  // Late lines from a run that has ended must not overwrite its status.
  if (!sessionLive()) return;
  readiness.feed(lines);
  window.bootTimeSeconds = readiness.bootSeconds;
}
function watchScreen(blank: boolean) {
  if (sessionLive()) readiness.screen(blank);
}
// A quiet screen ends the boot of firmware without a profile; no event marks it.
setInterval(() => {
  if (!sessionLive()) return;
  readiness.tick();
  window.bootTimeSeconds = readiness.bootSeconds;
}, 1000);
function resetting(prefix = '') {
  readiness.restart(true);
  window.bootTimeSeconds = null;
  status(`${prefix}Resetting…`);
}

/* Run state ------------------------------------------------------------ */

function paintControls() {
  const idle = session.state === 'idle' && !pending;
  const running = session.state === 'running';
  appEl.classList.toggle('live', session.state !== 'idle' || pending);
  paintSetup(setup, idle);
  button('start').disabled = !idle;
  button('stop').disabled = !(running || session.state === 'starting');
  button('reset').disabled = !running;
  button('sd').disabled = !idle;
  const live = running || session.state === 'starting';
  for (const id of ['zoom-in', 'zoom-out', 'zoom-level', 'screenshot']) ($(id) as HTMLButtonElement).disabled = !live || !screen;
  if (live && screen) resizeScreen();
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
  if ((info.state === 'starting' || info.state === 'running') && info.machine && !screen) {
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
  api.note(`Error: ${message}`);
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
  readiness.restart();
  window.bootTimeSeconds = null;
  paintControls();
  try {
    status('Loading firmware…');
    const input = await api.readFirmware();
    const { flash, machine, composed, composition: composedFrom } = await prepareFlash(input, settings.device, status);
    const composition = composedFrom ? `${composedFrom} · ` : '';
    if (composed) {
      appendLines([composition.replace(/ · $/, '')]);
      api.note(composition.replace(/ · $/, ''));
    }
    buttons(machine);
    createScreen(machine);
    status(`${composition}Booting ${bootNames[machine]} · ${settings.turbo ? 'Turbo (not timing-accurate)' : 'Accurate timing'}…`);
    // Observers (the acceptance tests) can inspect the exact bytes before the backend takes them.
    window.dispatchEvent(new CustomEvent('eink-qemu-flash-ready', { detail: { flash, machine, composed } }));
    readiness.restart();
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

/* Screen ----------------------------------------------------------------- */

function createScreen(machine: Machine) {
  screen?.dispose();
  screen = api.createScreen(machine, resizeScreen);
  $('screen').replaceChildren(screen.el);
  if (!screen.ownPointer) attachTouch(screen.el, machine);
  resizeScreen();
}

function removeScreen(message: string) {
  releaseTouch?.();
  screen?.dispose();
  screen = null;
  $('screen').style.width = '';
  $('screen').style.height = '';
  $('screen').textContent = message;
  $('keys').replaceChildren();
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
  const width = screen!.width, height = screen!.height;
  const padding = (element: Element, axis: 'x' | 'y') => {
    const style = getComputedStyle(element);
    return axis === 'x' ? parseFloat(style.paddingLeft) + parseFloat(style.paddingRight)
      : parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
  };
  const style = getComputedStyle(appEl);
  const device = document.querySelector('.device')!, bezel = document.querySelector('.bezel')!;
  let availableWidth: number, availableHeight: number;
  if (style.display === 'grid') {
    // The stage shrinks to the device, so measure what the side panels leave.
    availableWidth = appEl.clientWidth - parseFloat(style.getPropertyValue('--sd-max')) - parseFloat(style.getPropertyValue('--output-min'));
    availableHeight = document.querySelector<HTMLElement>('.stage')!.clientHeight -
      document.querySelector<HTMLElement>('.stage-bar')!.offsetHeight - $('keys').offsetHeight -
      padding(device, 'y') - padding(bezel, 'y');
  } else {
    // One column (a phone): the page scrolls, so only the width limits the screen.
    availableWidth = device.clientWidth;
    availableHeight = Infinity;
  }
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
  if (!screen) return;
  const layout = screenLayout(), scale = screenScale(layout);
  $('screen').style.width = `${layout.width * scale}px`;
  $('screen').style.height = `${layout.height * scale}px`;
  const pixels = scale * layout.dpr, exact = Math.abs(pixels - Math.round(pixels)) < 1e-6;
  // Whole pixels stay sharp; a fractional Fit scale reads better smoothed.
  screen.resize(scale, exact);
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
  if (!screen) return;
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
  const canvas = screen?.canvas();
  if (!canvas || !session.machine) return;
  const machine = session.machine;
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, 'image/png'));
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
// The X3/X4's four keys below the screen, left to right, are named by what
// they do in CrossPoint (where they can be remapped); other firmware gives
// them its own meanings.
const frontKeys = ['Back', 'Confirm', 'Left', 'Right'];
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
    const front = machine === 'x3' || machine === 'x4' ? frontKeys.indexOf(name) : -1;
    if (front >= 0) element.title = `${name} (${keyHints[name]}) · front key ${front + 1} of 4, from the left`;
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

function attachTouch(target: HTMLElement, machine: Machine) {
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
  // Typing in a field and the SD card panel keep their keys.
  if (target.closest('input, textarea, select, .sd, [role=dialog]')) return;
  if (session.state !== 'running') return;
  event.preventDefault();
  if (!event.repeat) press(name, owner, true);
}

/* Firmware and card ------------------------------------------------------- */

function applySetup(next: Setup) {
  setup = next;
  settings = next.settings;
  firmware = next.firmware;
  setElf(next.elf);
  paintControls();
}

/** The card again after the ELF changed elsewhere, without finding one. */
async function refreshSetup() {
  try { applySetup(await api.setup(false)); } catch (error) { fail(errorMessage(error)); }
}

async function useFirmware(choice: Promise<Setup | null>) {
  try {
    const chosen = await choice;
    if (!chosen) return;
    applySetup(chosen);
    if (chosen.elfSkipped) status(`${chosen.elfSkipped} beside the firmware is from another build; the Heap tab needs this build's ELF.`);
    // Choosing a firmware starts it; Start reruns the same file after Stop.
    await start();
  } catch (error) { fail(errorMessage(error)); }
}

async function chooseFirmware() {
  if (session.state !== 'idle' || pending) return;
  closeSetup();
  await useFirmware(api.pickFirmware());
}

/** The card line: a file's path, or (in a browser) how the card is kept. */
function paintCard() {
  const text = card ? card.note ?? card.path : 'No SD card is open: use the card menu.';
  const line = $('sd-save');
  line.classList.toggle('note', !card || !!card.note);
  // The folder gives way to the file name when the path is too long.
  const cut = card && !card.note ? Math.max(card.path.lastIndexOf('/'), card.path.lastIndexOf('\\')) + 1 : 0;
  line.querySelector('.sd-dir')!.textContent = cut ? card!.path.slice(0, cut) : '';
  line.querySelector('.sd-name')!.textContent = cut ? card!.path.slice(cut) : text;
  line.title = card?.path || '';
  line.classList.toggle('error', !card || !!card.error);
  $('sd-reveal').hidden = !card || !api.can.cardReveal;
}
function onCard(next: CardInfo | null, changed: boolean) {
  card = next;
  paintCard();
  if (changed) {
    sd.cwd = '/';
    void sdRefresh();
  } else sdPaint();
}

const MiB = 1024 ** 2, GiB = 1024 ** 3;
const sizeText = (bytes: number) => bytes % GiB === 0 ? `${bytes / GiB} GiB` : `${Math.round(bytes / MiB)} MiB`;

async function cardMenu() {
  if (session.state !== 'idle' || pending) return;
  // A press on the button closed the open menu; its click must not reopen it.
  if (menuJustClosed) { menuJustClosed = false; return; }
  const choice = await chooseCardAction();
  try {
    if (choice === 'resize') { await resizeCard(); return; }
    let chosen: CardInfo | null = null;
    if (choice === 'open') chosen = await api.pickOpenCard();
    else if (choice === 'new') {
      let size = api.cardDefaultSize;
      if (api.cardSizes.length > 1) {
        const picked = await choose('New blank card', api.cardSizeNote,
          api.cardSizes.map((c) => ({ value: String(c.size), label: c.label, note: c.note })), String(api.cardDefaultSize), 'Choose where…');
        if (!picked) return;
        size = Number(picked);
      }
      const made = await api.pickNewCard(size);
      if (!made) return;
      chosen = made.card;
      if (made.made.note) status(made.made.note);
    }
    if (!chosen) return;
    onCard(chosen, true);
  } catch (error) { sdError(error); }
}

/** Copy the card's files into a new card of a chosen size, in its place. */
async function resizeCard() {
  if (!card) return;
  const now = card.size, used = sd.ready ? sd.total - sd.free : 0;
  const choices = api.cardSizes.map((c) => {
    const small = c.size < used * 1.02 + 8 * MiB;
    return {
      value: String(c.size), label: c.label, disabled: c.size === now || small,
      note: c.size === now ? 'The current size' : small ? `Too small for the ${formatSize(used)} on the card` : c.note,
    };
  });
  const start = choices.find((c) => !c.disabled && Number(c.value) > now) ?? choices.find((c) => !c.disabled);
  if (!start) return;
  const size = await choose('Resize the SD card',
    `${card.name} is ${sizeText(now)}. Its folders and files are copied into a new card of the chosen size, which takes its place. ${api.cardSizeNote}`,
    choices, start.value, 'Resize');
  if (!size) return;
  await sdRun(async () => {
    sdStatus(`Copying the card into ${sizeText(Number(size))}…`);
    const made = await api.resizeCard(Number(size));
    if (made.card) { card = made.card; paintCard(); }
    await sdRefresh();
    sdStatusFlash(`The SD card is now ${sizeText(made.made.size)}`);
  });
}

let menuJustClosed = false;
function chooseCardAction(): Promise<'open' | 'new' | 'resize' | null> {
  return new Promise((resolve) => {
    const menu = $('card-menu');
    const anchor = button('sd');
    menu.hidden = false;
    menu.querySelector<HTMLButtonElement>('[data-card="resize"]')!.disabled = !card;
    const items = [...menu.querySelectorAll<HTMLButtonElement>('button:not(:disabled):not([hidden])')];
    const close = (value: 'open' | 'new' | 'resize' | null) => {
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
    menu.querySelector<HTMLButtonElement>('[data-card="resize"]')!.onclick = () => close('resize');
    anchor.setAttribute('aria-expanded', 'true');
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('keydown', keys, true);
    menu.addEventListener('focusout', leave);
    items[0]?.focus();
  });
}

async function copyCard() {
  await sdRun(async () => {
    const path = await api.pickCopyCard();
    if (path) sdStatusFlash(`Saved a copy to ${path}`);
  });
}

/* SD card explorer: operations run in the backend. While the emulator runs,
   it pauses the guest around them and resets the device after a change so
   the firmware mounts the card afresh. */
// ready: the last listing worked. An unreadable card can still be used by
// the emulator, but the explorer only allows retrying and switching cards.
const sd = { cwd: '/', entries: [] as SdEntry[], free: 0, total: 0, busy: false, info: '', ready: false };
const sdPanel = document.querySelector<HTMLElement>('.sd')!;
const sdUsable = () => !sd.busy && !pending && !card?.busy && (session.state === 'idle' || session.state === 'running');
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
  if (!card) {
    sd.entries = [];
    sd.ready = false;
    sd.info = '';
    $('sd-rows').replaceChildren();
    $('sd-path').replaceChildren();
    $('sd-meter').style.width = '0';
    sdStatus('');
    sdPaint();
    return;
  }
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
  sd.total = listing.total;
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
  const invalid = !name || name.length > 255 || /[\x00-\x1f￿"*/:<>?\\|]/u.test(name) || /[ .]$/u.test(name) ||
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

/* File drops: the backend reports what landed where. Firmware drops onto the
   chooser or the device stage; files and folders drop onto the SD panel. */
function dropTarget(x: number, y: number): 'sd' | 'firmware' | null {
  const element = document.elementFromPoint(x, y);
  if (element?.closest('.sd')) return sdUsable() && card && sd.ready ? 'sd' : null;
  if (element?.closest('.stage, .file-chip')) return session.state === 'idle' && !pending ? 'firmware' : null;
  return null;
}
function paintDrop(target: 'sd' | 'firmware' | null) {
  sdPanel.classList.toggle('drop', target === 'sd');
  document.querySelector('.stage')!.classList.toggle('firmware-drop', target === 'firmware');
  $('flash').classList.toggle('firmware-drop', target === 'firmware');
}

async function onDrop(files: FileRef[], x: number, y: number) {
  paintDrop(null);
  // An ELF goes to the heap view wherever it lands.
  if (files.length === 1 && /\.elf$/i.test(files[0].name) && api.can.heap) {
    await useElf(api.heapUseElf(files[0].ref));
    await refreshSetup();
    return;
  }
  const target = dropTarget(x, y);
  if (target === 'sd') {
    await sdRun(() => sdUpload(files.map((file) => file.ref)));
  } else if (target === 'firmware') {
    if (files.length !== 1 || !/\.(bin|xota)$/i.test(files[0].name)) {
      status('Drop one firmware file: a .bin, or an X4 Pro .xota update.');
      return;
    }
    await useFirmware(api.useFirmware(files[0].ref));
  }
}
const drops: DropHandlers = { target: dropTarget, paint: paintDrop, drop: (files, x, y) => void onDrop(files, x, y) };

/* Wiring ------------------------------------------------------------------ */

export async function boot(backend: Backend) {
  api = backend;
  const { can } = api;
  setHeapAvailable(can.heap);
  $('setup-elf-section').hidden = !can.heap;
  $('heap-open').hidden = !can.heap;
  $('card-menu').querySelector<HTMLElement>('[data-card="open"]')!.hidden = !can.cardOpen;
  $('card-menu').querySelector<HTMLElement>('[data-card="new"]')!.hidden = !can.cardNew;
  $('card-menu').querySelector<HTMLElement>('[data-card="resize"]')!.hidden = !can.cardResize;

  const state = await api.attach({
    log: (lines) => { appendLines(lines); watchLog(lines); },
    screen: watchScreen,
    session: onSession,
    sdReset: () => { releaseTouch?.(); resetting('SD card changed · '); },
    card: onCard,
    problem: fail,
  });
  settings = state.settings;
  firmware = state.firmware;
  card = state.card;
  if (state.session.seq > session.seq) session = state.session;
  attachHeap({
    snapshot: api.heapSnapshot, at: api.heapSnapshotAt, stack: api.heapStack, sites: api.heapSites, history: api.heapHistory,
    pickElf: api.heapPickElf, openWindow: api.heapWindow, elfChosen: () => void refreshSetup(),
  }, state.elf);
  heapSession(session.state, session.run);
  if (state.pane === 'heap' || state.pane === 'console') showPane(state.pane);
  if (state.pane === 'window') { showPane('heap'); void api.heapWindow(); }
  platform = state.platform;
  // The card reads the firmware, and takes up an ELF built beside it.
  try {
    setup = await api.setup(true);
    settings = setup.settings;
    firmware = setup.firmware;
    setElf(setup.elf);
  } catch (error) { state.problems.push(errorMessage(error)); }
  paintCard();
  if (session.machine && session.state !== 'idle') {
    // A reloaded WebView: the session kept running; the screen follows. Its
    // boot is long over: readiness waits for the next one.
    readiness.resume();
    buttons(session.machine);
    createScreen(session.machine);
    status(`${deviceLabel()} · ${timingLabel()} · ${session.state === 'running' ? 'Running' : 'Starting…'}`);
  } else status(firmware ? 'Start boots the chosen firmware.' : 'Choose a flash image to begin.');
  for (const problem of state.problems) {
    appendLines([`Error: ${problem}`]);
    api.note(`Error: ${problem}`);
  }
  if (state.problems.length) status(state.problems[0]);
  paintControls();
  void sdRefresh();
  if (state.autostart && firmware && session.state === 'idle') void start();

  const save = async (patch: Parameters<typeof api.settings>[0]) => {
    try {
      settings = await api.settings(patch);
      if (setup) setup = { ...setup, settings };
      paintControls();
      return true;
    } catch (error) { fail(errorMessage(error)); return false; }
  };
  // A new device or panel boots the firmware again; run options wait for Start.
  attachSetup({
    choose: () => void chooseFirmware(),
    recent: (path) => { if (session.state === 'idle' && !pending) void useFirmware(api.recentFirmware(path)); },
    device: async (device) => { if (await save({ device }) && firmware) void start(); },
    panel: async (panel) => { if (await save({ panel }) && firmware) void start(); },
    option: (option, on) => void save({ [option]: on }),
    chooseElf: async () => {
      const info = await api.heapPickElf().catch((error) => { fail(errorMessage(error)); return null; });
      if (info) await refreshSetup();
    },
    forgetElf: async () => {
      try { await api.heapForgetElf(); } catch (error) { fail(errorMessage(error)); }
      await refreshSetup();
    },
  });
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
  window.addEventListener('resize', resizeScreen);
  watchPixelRatio();

  document.addEventListener('keydown', deviceKeyboard);
  document.addEventListener('keyup', deviceKeyboard);
  window.addEventListener('blur', releaseAll);

  button('sd').addEventListener('click', () => void cardMenu());
  button('download').addEventListener('click', () => void copyCard());
  const reveal = platform === 'macos' ? 'Show in Finder' : platform === 'windows' ? 'Show in File Explorer' : 'Show in folder';
  button('sd-reveal').setAttribute('aria-label', reveal);
  button('sd-reveal').title = reveal;
  button('sd-reveal').addEventListener('click', () => void api.revealCard().catch((error) => fail(errorMessage(error))));
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

  await api.watchDrops(drops);

  // Project links open outside the page where the backend asks to (the app's WebView).
  const openUrl = api.openUrl;
  if (openUrl) document.addEventListener('click', (event) => {
    const link = (event.target as Element).closest<HTMLAnchorElement>('a[href^="https://"]');
    if (!link) return;
    event.preventDefault();
    openUrl(link.href).catch((error) => fail(errorMessage(error)));
  });
}
