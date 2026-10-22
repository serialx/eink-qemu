import assert from 'node:assert/strict';
import test from 'node:test';
import { coordinateBridgeLaunch } from '../desktop/src/bridge-launch.ts';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('launch waits for an outstanding transport inspection and preserves its arguments', async () => {
  const inspected = deferred(), entered = deferred();
  const calls = [];
  const backend = coordinateBridgeLaunch({
    start: async (...args) => { calls.push(args); return 'running'; },
    bridge: { inspect: () => { entered.resolve(); return inspected.promise; } },
  });
  const inspection = backend.bridge.inspect();
  await entered.promise;
  const flash = new Uint8Array([1, 2]), options = { wifi: true, wifiMode: 'bridge' };
  const launch = backend.start(flash, options);
  await Promise.resolve();
  assert.deepEqual(calls, []);
  inspected.resolve('ready');
  assert.equal(await inspection, 'ready');
  assert.equal(await launch, 'running');
  assert.deepEqual(calls, [[flash, options]]);
});

test('inspection waits for launch while stop, cancellation and recovery polling remain available', async () => {
  const started = deferred(), entered = deferred();
  let inspections = 0;
  const backend = coordinateBridgeLaunch({
    start: () => { entered.resolve(); return started.promise; },
    stop: async () => 'stopped',
    bridge: {
      inspect: async () => { inspections++; return 'ready'; },
      cancel: async id => id,
      watch: async handler => { handler('applying'); return () => {}; },
    },
  });
  const launch = backend.start(new Uint8Array(), { wifi: true, wifiMode: 'bridge' });
  await entered.promise;
  const inspection = backend.bridge.inspect();
  assert.equal(await backend.stop(), 'stopped');
  assert.equal(await backend.bridge.cancel('operation'), 'operation');
  let progress;
  await backend.bridge.watch(value => { progress = value; });
  assert.equal(progress, 'applying');
  assert.equal(inspections, 0);
  started.resolve('running');
  await launch;
  assert.equal(await inspection, 'ready');
});

test('failed inspection does not suppress the authoritative launch error or poison later requests', async () => {
  const inspectionError = new Error('inspection failed'), launchError = new Error('bridge missing');
  const backend = coordinateBridgeLaunch({
    start: async () => { throw launchError; },
    bridge: { inspect: async () => { throw inspectionError; } },
  });
  await assert.rejects(backend.bridge.inspect(), error => error === inspectionError);
  await assert.rejects(backend.start(new Uint8Array(), { wifi: true, wifiMode: 'bridge' }), error => error === launchError);
  await assert.rejects(backend.bridge.inspect(), error => error === inspectionError);
});

test('NAT and Wi-Fi-off launches do not wait for unrelated bridge inspection', async () => {
  const inspected = deferred(), entered = deferred();
  const backend = coordinateBridgeLaunch({
    start: async () => 'running',
    bridge: { inspect: () => { entered.resolve(); return inspected.promise; } },
  });
  const inspection = backend.bridge.inspect();
  await entered.promise;
  assert.equal(await backend.start(new Uint8Array(), { wifi: true, wifiMode: 'nat' }), 'running');
  assert.equal(await backend.start(new Uint8Array(), { wifi: false, wifiMode: 'bridge' }), 'running');
  inspected.resolve('ready');
  await inspection;
});
