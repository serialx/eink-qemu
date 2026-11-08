const keyboard: Record<string, string> = {
  Backspace: 'Back', Escape: 'Back', Enter: 'Confirm', NumpadEnter: 'Confirm', ArrowLeft: 'Left', ArrowRight: 'Right',
  ArrowUp: 'Up', ArrowDown: 'Down', KeyP: 'Power', Home: 'Home',
};
const keyHints: Record<string, string> = {
  Back: 'Esc / Backspace', Confirm: 'Enter', Left: '←', Right: '→', Up: '↑', Down: '↓', Power: 'P', Home: 'Home',
};

export function forwardKeyboard(event: KeyboardEvent, target: EventTarget) {
  if (!event.isTrusted) return; // Backend-generated device events go to SDL.
  const forwarded = new KeyboardEvent(event.type, event);
  target.dispatchEvent(forwarded);
  // Even rejected shortcuts must not reach SDL through a second route.
  event.stopImmediatePropagation();
  if (forwarded.defaultPrevented) event.preventDefault();
}

export function keyboardKey(machine: string | null, code: string): string | undefined {
  if ((machine === 'x4pro' || machine === 'x4classic') && (code === 'Escape' || code === 'KeyH')) return 'Home';
  return keyboard[code];
}

export function keyHint(machine: string, name: string): string {
  return (machine === 'x4pro' || machine === 'x4classic') && name === 'Home' ? 'Esc / H / Home' : keyHints[name];
}

export function keyTitle(machine: string, name: string): string {
  const title = `${name} (${keyHint(machine, name)})`;
  if (machine === 'x4classic' && ['Back', 'Confirm', 'Left', 'Right'].includes(name)) {
    return `${title} · shown in the eight-button layout; Classic GPIO mapping is not implemented`;
  }
  if (name === 'Power') return `${title} · P holds Power while the key is down; hold for a long press (many firmwares sleep or power off)`;
  const front = machine === 'x3' || machine === 'x4' ? ['Back', 'Confirm', 'Left', 'Right'].indexOf(name) : -1;
  return front >= 0 ? `${title} · front key ${front + 1} of 4, from the left` : title;
}

export function keyboardHint(machine: string): string {
  return machine === 'x3' || machine === 'x4' ? 'Keyboard: arrows · Enter · Esc / Backspace · hold P for a long press'
    : machine === 'picco' ? 'Keyboard: ↑ ↓ · hold P for a long press · touch the screen'
    : machine === 'x4classic' ? 'Keyboard: ↑ ↓ · Esc / H / Home · hold P for a long press · 8 on-screen controls'
    : 'Keyboard: ↑ ↓ · Esc / H / Home · hold P for a long press · touch the screen';
}
