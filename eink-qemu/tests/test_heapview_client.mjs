// Run with: node --test eink-qemu/tests/test_heapview_client.mjs
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

test('detached viewer fetches unsigned generations and restarts its event cursor', async () => {
  const elements = new Map();
  const context = new Proxy({}, { get: (o, key) => o[key] ?? (() => {}) });
  const element = () => ({
    dataset: {}, style: {}, classList: { toggle() {} },
    clientWidth: 20, clientHeight: 20,
    insertAdjacentHTML() {}, setAttribute() {},
    getContext: () => context,
  });
  const get = (id) => {
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  };
  let poll;
  const sandbox = vm.createContext({
    document: { documentElement: element(), getElementById: get,
      querySelectorAll: () => [], createElement: element },
    localStorage: { getItem: () => null },
    getComputedStyle: () => ({ getPropertyValue: () => '#112233' }),
    matchMedia: () => ({ addEventListener() {} }),
    MutationObserver: class { observe() {} },
    addEventListener() {}, requestAnimationFrame() {},
    clearTimeout() {}, setTimeout: (fn) => { poll = fn; return 1; },
    devicePixelRatio: 1,
  });
  const source = await readFile(new URL('../heapview.js', import.meta.url), 'utf8');
  vm.runInContext(source.replace('export function mount', 'function mount'), sandbox);
  let generation = 7;
  const requests = [];
  sandbox.mount(element(), {
    history: async () => [],
    snapshot: async () => ({ traced: true, traceGen: generation, traceEvents: 1,
      time: 1, heaps: [] }),
    events: async (gen, start, stacks, sites) => {
      requests.push([gen, start, stacks, sites]);
      // Match the desktop bridge's u64 contract, including the first request.
      if (!Number.isSafeInteger(gen) || gen < 0) throw new Error('expected u64');
      return { gen: generation, start: 0, t0: 1000, t: [0], k: [1], p: [4096],
        s: [32], o: [0], st: [0], stackStart: 0, stackSite: [0],
        siteStart: sites, sites: sites ? [] : [{ fn: 'allocate' }] };
    },
  });
  await new Promise(setImmediate);
  assert.equal(get('tm').hidden, false, get('banner').textContent);
  assert.match(get('tm-label').innerHTML, /1 events/);
  await poll();
  assert.equal(requests.length, 1, 'unchanged snapshots must not refetch events');
  generation = 8;
  await poll();
  assert.equal(requests.length, 2);
  assert.deepEqual(requests[1], [7, 0, 0, 1]);
  assert.equal(get('tm').hidden, false);
});
