// Run with: node --test eink-qemu/tests/test_keyboard.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { forwardKeyboard, keyboardKey, keyboardHint, keyHint, keyTitle } from '../desktop/src/keyboard.ts';

test('X4 Pro accepts Esc, H and Home as its capacitive Home key', () => {
  for (const code of ['Escape', 'KeyH', 'Home']) assert.equal(keyboardKey('x4pro', code), 'Home');
  assert.equal(keyboardKey('x4pro', 'Backspace'), 'Back');
});

test('X3 and X4 keep Esc and Backspace as Back', () => {
  for (const machine of ['x3', 'x4']) {
    for (const code of ['Escape', 'Backspace']) assert.equal(keyboardKey(machine, code), 'Back');
    assert.equal(keyboardKey(machine, 'KeyH'), undefined);
  }
});

test('Picco keeps its existing bindings without the new Home aliases', () => {
  assert.equal(keyboardKey('picco', 'Escape'), 'Back');
  assert.equal(keyboardKey('picco', 'Backspace'), 'Back');
  assert.equal(keyboardKey('picco', 'KeyH'), undefined);
  assert.equal(keyboardKey('picco', 'Home'), 'Home'); // Ignored by the device, as before.
  assert.doesNotMatch(keyboardHint('picco'), /Esc|Home/);
});

test('all devices keep arrows, Enter and Power, and ignore unrelated keys', () => {
  for (const machine of ['x3', 'x4', 'x4pro', 'picco']) {
    for (const [code, name] of Object.entries({ ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right', Enter: 'Confirm', NumpadEnter: 'Confirm', KeyP: 'Power' })) {
      assert.equal(keyboardKey(machine, code), name);
    }
    assert.equal(keyboardKey(machine, 'KeyA'), undefined);
  }
});

test('shared hints expose Home aliases and firmware-neutral Power hold semantics', () => {
  assert.equal(keyHint('x4pro', 'Home'), 'Esc / H / Home');
  assert.match(keyboardHint('x4pro'), /Esc \/ H \/ Home/);
  assert.match(keyTitle('x4pro', 'Home'), /Esc \/ H \/ Home/);
  assert.equal(keyHint('x3', 'Back'), 'Esc / Backspace');
  for (const machine of ['x3', 'x4', 'x4pro', 'picco']) {
    assert.match(keyboardHint(machine), /hold P for a long press/);
    assert.match(keyTitle(machine, 'Power'), /while the key is down.*long press.*sleep or power off/);
    assert.doesNotMatch(keyTitle(machine, 'Power'), /CrossPoint/);
  }
});

test('iframe keys reach the page without a second physical route to SDL', async (t) => {
  // Node has EventTarget but no KeyboardEvent or trusted browser input.
  const previous = globalThis.KeyboardEvent;
  globalThis.KeyboardEvent = class extends Event {
    constructor(type, init = {}) {
      super(type, init);
      for (const name of ['code', 'key', 'ctrlKey', 'metaKey', 'altKey', 'shiftKey', 'repeat', 'isComposing']) this[name] = init[name];
    }
  };
  t.after(() => {
    if (previous) globalThis.KeyboardEvent = previous;
    else delete globalThis.KeyboardEvent;
  });

  for (const type of ['keydown', 'keyup']) {
    for (const modifier of ['ctrlKey', 'metaKey', 'altKey']) {
      await t.test(`${type}: rejected ${modifier} shortcut stays out of SDL`, () => {
        const runtime = new EventTarget(), page = new EventTarget();
        const received = [], sdl = [];
        runtime.addEventListener(type, (event) => forwardKeyboard(event, page));
        runtime.addEventListener(type, (event) => sdl.push(event));
        page.addEventListener(type, (event) => received.push(event)); // The page declines the shortcut.
        const key = new KeyboardEvent(type, { code: 'KeyP', key: 'p', [modifier]: true, repeat: true, bubbles: true, cancelable: true });
        Object.defineProperty(key, 'isTrusted', { value: true });
        runtime.dispatchEvent(key);
        assert.equal(received.length, 1);
        assert.equal(received[0].code, 'KeyP');
        assert.equal(received[0][modifier], true);
        assert.equal(received[0].repeat, true);
        assert.equal(received[0].bubbles, true);
        assert.equal(received[0].isTrusted, false);
        assert.deepEqual(sdl, []);
        assert.equal(key.defaultPrevented, false); // Browser shortcut defaults remain available.
      });
    }

    await t.test(`${type}: handled keys cancel the default and generated device events reach SDL once`, () => {
      const runtime = new EventTarget(), page = new EventTarget();
      const received = [], sdl = [];
      runtime.addEventListener(type, (event) => forwardKeyboard(event, page));
      runtime.addEventListener(type, (event) => sdl.push(event.code));
      page.addEventListener(type, (event) => {
        received.push(event.code);
        event.preventDefault();
        runtime.dispatchEvent(new KeyboardEvent(type, { code: 'Home' }));
      });
      const key = new KeyboardEvent(type, { code: 'KeyH', cancelable: true });
      Object.defineProperty(key, 'isTrusted', { value: true });
      runtime.dispatchEvent(key);
      assert.deepEqual(received, ['KeyH']);
      assert.deepEqual(sdl, ['Home']);
      assert.equal(key.defaultPrevented, true);
    });
  }
});
