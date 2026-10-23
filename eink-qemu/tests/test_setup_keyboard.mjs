// Run with: node --test eink-qemu/tests/test_setup_keyboard.mjs
import assert from 'node:assert/strict';
import test from 'node:test';

function installDocument(t) {
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
  return document;
}

test('firmware card consumes Escape before other document listeners, wherever focus moved', async (t) => {
  const document = installDocument(t);
  await import('../desktop/src/setup.ts?escape');
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

test('bridge setup returns focus to the visible firmware button after hiding its card', async (t) => {
  const document = installDocument(t);
  const { attachSetup } = await import('../desktop/src/setup.ts?bridge-focus');
  const chip = document.getElementById('flash'), card = document.getElementById('setup-card');
  let returnTarget;
  attachSetup({ manageBridge: trigger => { returnTarget = trigger; } });
  chip.dispatchEvent(new Event('click'));
  document.getElementById('setup-bridge-manage').dispatchEvent(new Event('click'));
  assert.equal(card.hidden, true);
  assert.equal(returnTarget, chip);
});

test('firmware card distinguishes unusable bridge selections from ready connections', async (t) => {
  const document = installDocument(t);
  const { paintBridgeInspection } = await import('../desktop/src/setup.ts?bridge-status');
  const inspection = { support:'ready', platform:'windows', adapters:[],
    capabilities:{create:true,existingBridge:false,reason:null},
    selection:{version:1,platform:'windows',adapterId:'ethernet',fingerprint:'one',resourceId:'a'.repeat(32),displayName:'Ethernet'},
    readiness:'ready', activeOperation:null, externalOverride:null };
  const status = document.getElementById('setup-bridge-status');
  for (const [readiness, label] of [
    ['needsSetup', /needs setup/i], ['disconnected', /disconnected/i],
    ['needsRepair', /needs repair/i], ['unsupported', /unsupported/i], ['inUse', /in use/i],
  ]) {
    paintBridgeInspection({...inspection, readiness});
    assert.match(status.textContent, /Ethernet/);
    assert.match(status.textContent, label);
    assert.doesNotMatch(status.textContent, /Join PICSimLabWifi/);
  }
  paintBridgeInspection(inspection);
  assert.match(status.textContent, /ready/i);
  assert.match(status.textContent, /Join PICSimLabWifi/);
  paintBridgeInspection({...inspection,externalOverride:'X4NET'});
  assert.match(status.textContent, /X4NET controls networking/);
  assert.doesNotMatch(status.textContent, /Join PICSimLabWifi/);
});
