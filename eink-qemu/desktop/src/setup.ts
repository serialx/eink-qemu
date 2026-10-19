/* The top bar's firmware chip and its card: the firmware, the device that
   runs it, the ELF built with it, the X4 Pro's display panel and recent
   firmware; and the run options as icon toggles. Rust decides the device
   and finds the ELF (src-tauri/src/lib.rs `choose_firmware`); this paints
   what it reports and hands choices back. */
import type { Settings, Setup } from './types';
import { selectedDevice, devicesOf } from './devices.ts';

export type RunOption = 'wifi' | 'ghosting' | 'turbo';
export interface SetupHandlers {
  choose(): void;
  recent(path: string): void;
  device(device: Machine): void;
  panel(panel: Settings['panel']): void;
  option(option: RunOption, on: boolean): void;
  wifiMode(mode: Settings['wifiMode']): void;
  chooseElf(): void;
  forgetElf(): void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const names: Record<Machine, string> = { x3: 'X3', x4: 'X4', x4pro: 'X4 Pro', picco: 'BOOX Picco' };
const otherChip: Record<string, string> = {
  'ESP32-C3': 'X4 Pro and Picco take ESP32-S3 firmware', 'ESP32-S3': 'X3 and X4 take ESP32-C3 firmware',
};
const panels: Record<Settings['panel'], string> = { 'uc8279-68': 'UC8279 (0x68, default)', uc8179: 'UC8179' };
const options: RunOption[] = ['wifi', 'ghosting', 'turbo'];
export const hasGhosting = (device: Machine | null) => device === 'x3' || device === 'x4pro';

const chip = $<HTMLButtonElement>('flash');
const card = $('setup-card');
let state: Setup | null = null;
let idle = true;
let handlers: SetupHandlers | null = null;

const esc = (text: string) => text.replace(/[&<>"]/g, (c) => `&${({ '&': 'amp', '<': 'lt', '>': 'gt', '"': 'quot' } as Record<string, string>)[c]};`);
const icon = (name: string) => `<svg class="icon" aria-hidden="true"><use href="#i-${name}"/></svg>`;
/** PlatformIO and ESP-IDF name every build firmware.bin: its folder tells them apart. */
const label = (name: string, parent: string) => /^firmware\b/i.test(name) && parent ? `${parent}/${name}` : name;
const mib = (bytes: number) => `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
const when = (ms: number) => new Date(ms).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });

export function paintSetup(next: Setup | null, nextIdle: boolean) {
  state = next;
  idle = nextIdle;
  const firmware = next?.firmware ?? null;
  const device = next ? selectedDevice(next.settings.device, firmware) : null;

  // The chip: which firmware, on which device, with or without its ELF.
  $('flash-name').textContent = firmware ? label(firmware.name, firmware.parent) : 'Choose firmware…';
  $('flash-name').classList.toggle('empty', !firmware);
  const meta = $('flash-meta');
  meta.hidden = !firmware;
  if (next && firmware) {
    const elf = !next.elf ? '' : next.elfMatches === false
      ? `<span class="warn">${icon('alert')}ELF</span>` : `<span class="ok">${icon('check')}ELF</span>`;
    meta.innerHTML = [device ? esc(names[device]) : 'Choose a device', elf].filter(Boolean).join('<span aria-hidden="true">·</span>');
  }
  chip.title = firmware ? firmware.path || firmware.name : 'Firmware .bin (app or merged), X4 Pro .xota update, or a 16 MiB flash dump';

  // The file.
  $('setup-name').textContent = firmware ? firmware.name : 'No firmware chosen';
  $('setup-folder').textContent = firmware ? firmware.folder : 'A .bin (app or merged), an X4 Pro .xota update, or a 16 MiB flash dump.';
  $('setup-folder').title = firmware?.folder ?? '';
  $('setup-folder').hidden = !!firmware && !firmware.folder;
  $('setup-tags').innerHTML = !firmware ? '' : [
    firmware.chip, firmware.version && `<span class="mono" title="Version in the app descriptor">${esc(firmware.version)}</span>`,
    mib(firmware.size), firmware.modified && `<span title="Modified">${when(firmware.modified)}</span>`,
  ].filter(Boolean).map((tag) => `<span class="setup-tag grey">${tag}</span>`).join('');
  const choose = $<HTMLButtonElement>('setup-choose');
  choose.textContent = firmware ? 'Choose…' : 'Choose firmware…';
  choose.classList.toggle('primary', !firmware);
  choose.disabled = !idle;

  // The device: those that take the firmware's chip, all four before one is chosen.
  const offered = firmware?.chip ? devicesOf(firmware.chip) : (['x3', 'x4', 'x4pro', 'picco'] as Machine[]);
  $('setup-devices').innerHTML = offered.map((d) =>
    `<button type="button" role="radio" data-device="${d}" aria-checked="${d === device}"${idle ? '' : ' disabled'}>${names[d]}</button>`).join('');
  $('setup-device-note').textContent = firmware?.chip ? otherChip[firmware.chip] : '';

  // The ELF.
  const elf = next?.elf ?? null;
  $('setup-elf-name').textContent = elf ? elf.name : 'None';
  $('setup-elf-name').classList.toggle('empty', !elf);
  $('setup-elf-name').title = elf?.path ?? '';
  const tag = $('setup-elf-tag');
  const matches = next?.elfMatches;
  tag.hidden = !elf || (!next?.settings.elfAuto && matches == null);
  tag.className = `setup-tag${matches === false ? ' warn' : ''}`;
  tag.innerHTML = !elf ? '' : matches === false ? `${icon('alert')}another build`
    : next?.settings.elfAuto ? `${icon('check')}found beside it` : `${icon('check')}matches`;
  tag.title = !elf ? '' : matches === false ? 'The firmware was not built from this ELF.'
    : next?.settings.elfAuto && firmware ? `Found beside ${firmware.name}${matches ? ', and built with it' : ''}.` : 'The firmware was built from this ELF.';
  $('setup-elf-choose').textContent = elf ? 'Change…' : 'Choose…';
  $('setup-elf-forget').hidden = !elf;
  const note = $('setup-elf-note');
  note.textContent = next?.elfSkipped ? `${next.elfSkipped} beside the firmware is from another build, so it was left out.`
    : matches === false ? 'Rebuild, or choose the ELF this firmware was built with; until then allocations are not recorded.'
    : !elf ? 'The Heap tab names heaps and allocation sites from it. A firmware.elf beside the firmware is picked up by itself.' : '';
  note.hidden = !note.textContent;

  // The display panel, for the X4 Pro only.
  const panel = $<HTMLDetailsElement>('setup-panel');
  panel.hidden = device !== 'x4pro';
  if (next) {
    $('setup-panel-value').textContent = panels[next.settings.panel];
    for (const b of panel.querySelectorAll<HTMLButtonElement>('[data-panel]')) {
      b.setAttribute('aria-checked', String(b.dataset.panel === next.settings.panel));
      b.disabled = !idle;
    }
  }

  // Native Wi-Fi mode. The browser build removes this section.
  const network = $('setup-wifi');
  if (network) {
    const mode = next?.settings.wifiMode ?? 'nat';
    for (const b of network.querySelectorAll<HTMLButtonElement>('[data-wifi-mode]')) {
      b.setAttribute('aria-checked', String(b.dataset.wifiMode === mode));
      b.disabled = !idle;
    }
    $('setup-wifi-nat').hidden = mode !== 'nat';
    $('setup-wifi-bridge').hidden = mode !== 'bridge';
  }

  // Recent firmware, the chosen one marked.
  const recent = next?.recent ?? [];
  $('setup-recent-section').hidden = recent.length < 2;
  $('setup-recent').innerHTML = recent.map((r) => {
    const current = r.path === firmware?.path;
    const on = r.device === 'auto' ? '' : names[r.device];
    return `<button type="button" class="setup-recent-item${current ? ' current' : ''}" data-path="${esc(r.path)}" title="${esc(r.path)}"${idle && !current ? '' : ' disabled'}>` +
      `${current ? icon('check') : '<span class="icon"></span>'}<span class="mono">${esc(label(r.name, r.parent))}</span>` +
      `<span class="spacer"></span><span class="setup-note">${[on, r.elf && 'ELF'].filter(Boolean).join(' · ')}</span></button>`;
  }).join('');

  // The run options.
  $('ghosting').closest<HTMLElement>('.option')!.hidden = !hasGhosting(device);
  for (const option of options) {
    const on = next?.settings[option] ?? true;
    const toggle = $<HTMLButtonElement>(option);
    toggle.setAttribute('aria-pressed', String(on));
    toggle.disabled = !idle;
    $(`${option}-help`).querySelector('.tip-state')!.textContent = on ? '· on' : '· off';
  }
}

function open(show: boolean) {
  if (show === !card.hidden) return;
  card.hidden = !show;
  chip.setAttribute('aria-expanded', String(show));
  // The card takes focus, so Tab reaches its controls and Escape closes it.
  if (show) card.focus({ preventScroll: true });
}
export const closeSetup = () => open(false);

export function attachSetup(next: SetupHandlers) {
  handlers = next;
}

chip.addEventListener('click', () => open(card.hidden));
document.addEventListener('pointerdown', (event) => {
  if (!card.hidden && !(event.target as Element).closest('.setup')) open(false);
});
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && !card.hidden) {
    event.preventDefault();
    open(false);
    chip.focus();
    event.stopPropagation();
  }
}, true);
$('setup-choose').addEventListener('click', () => { open(false); handlers?.choose(); });
$('setup-devices').addEventListener('click', (event) => {
  const b = (event.target as Element).closest<HTMLButtonElement>('[data-device]');
  if (b && b.getAttribute('aria-checked') !== 'true') { open(false); handlers?.device(b.dataset.device as Machine); }
});
$('setup-panel').addEventListener('click', (event) => {
  const b = (event.target as Element).closest<HTMLButtonElement>('[data-panel]');
  if (b && b.getAttribute('aria-checked') !== 'true') { open(false); handlers?.panel(b.dataset.panel as Settings['panel']); }
});
$('setup-wifi')?.addEventListener('click', (event) => {
  const b = (event.target as Element).closest<HTMLButtonElement>('[data-wifi-mode]');
  if (idle && b && !b.disabled && b.getAttribute('aria-checked') !== 'true') {
    handlers?.wifiMode(b.dataset.wifiMode as Settings['wifiMode']);
  }
});
$('setup-recent').addEventListener('click', (event) => {
  const b = (event.target as Element).closest<HTMLButtonElement>('[data-path]');
  if (b) { open(false); handlers?.recent(b.dataset.path!); }
});
$('setup-elf-choose').addEventListener('click', () => handlers?.chooseElf());
$('setup-elf-forget').addEventListener('click', () => handlers?.forgetElf());
for (const option of options) {
  $(option).addEventListener('click', () => {
    if (state) handlers?.option(option, !state.settings[option]);
  });
}
