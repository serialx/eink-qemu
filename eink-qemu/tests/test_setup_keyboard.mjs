// Run with: node --test eink-qemu/tests/test_setup_keyboard.mjs
import assert from 'node:assert/strict';
import test from 'node:test';

test('firmware card consumes Escape before other document listeners, wherever focus moved', async (t) => {
  const previous = globalThis.document;
  const document = new EventTarget();
  const elements = new Map();
  class Element extends EventTarget {
    hidden = true;
    attributes = new Map();
    setAttribute(name, value) { this.attributes.set(name, value); }
    focus() { document.activeElement = this; }
  }
  document.getElementById = (id) => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  globalThis.document = document;
  t.after(() => {
    if (previous) globalThis.document = previous;
    else delete globalThis.document;
  });
  await import('../desktop/src/setup.ts');
  const chip = document.getElementById('flash'), card = document.getElementById('setup-card');
  const seen = [];
  document.addEventListener('keydown', (event) => seen.push(event.defaultPrevented));
  const escape = () => document.dispatchEvent(Object.assign(new Event('keydown', { cancelable: true }), { key: 'Escape' }));

  for (const focus of [card, document.getElementById('reset'), document.getElementById('console-filter')]) {
    chip.dispatchEvent(new Event('click'));
    assert.equal(card.hidden, false);
    focus.focus();
    escape();
    assert.equal(card.hidden, true);
    assert.equal(document.activeElement, chip);
    assert.equal(chip.attributes.get('aria-expanded'), 'false');
    assert.equal(seen.at(-1), true);
  }

  escape();
  assert.equal(seen.at(-1), false); // Closed cards leave Escape to the device or another control.
});
