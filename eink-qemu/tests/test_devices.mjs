// Run with: node --test eink-qemu/tests/test_devices.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { deviceOf, devicesOf, selectedDevice } from '../desktop/src/devices.ts';

test('shared Rust/TypeScript matrix: 1056 filename, folder and remembered-choice cases', () => {
  const cases = JSON.parse(readFileSync(new URL('./device-cases.json', import.meta.url)));
  for (const [index, chip] of ['ESP32-C3', 'ESP32-S3'].entries()) {
    for (const [name, ...defaults] of cases.names) {
      for (const prefix of cases.prefixes) {
        for (const remembered of cases.remembered) {
          const path = `${prefix}${name}.bin`;
          const expected = devicesOf(chip).includes(remembered) ? remembered : defaults[index];
          assert.equal(deviceOf(chip, path, remembered), expected, `${chip} ${path} ${remembered}`);
        }
      }
    }
  }
});

test('legacy Auto resolves to the same board used for display and boot; explicit choices remain explicit', () => {
  for (const [chip, name, expected] of [
    ['ESP32-C3', 'crosspoint-x4.bin', 'x4'],
    ['ESP32-C3', 'crosspoint-x3-x4.bin', 'x3'],
    ['ESP32-S3', 'firmware-picco.bin', 'picco'],
    ['ESP32-S3', 'firmware.bin', 'x4pro'],
  ]) assert.equal(selectedDevice('auto', { chip, name }), expected);
  assert.equal(selectedDevice('x3', { chip: 'ESP32-C3', name: 'x4.bin' }), 'x3');
  assert.equal(selectedDevice('x4pro', { chip: 'ESP32-C3', name: 'x4.bin' }), 'x4pro');
  assert.equal(selectedDevice('auto', null), null);
});

test('filename hints and saved choices match core/src/firmware.rs', () => {
  for (const [chip, path, remembered, expected] of [
    ['ESP32-C3', 'crosspoint-1.6.5-x3.bin', undefined, 'x3'],
    ['ESP32-C3', 'crosspoint-1.6.5-x4.bin', undefined, 'x4'],
    ['ESP32-C3', 'crosspoint-1.6.5-x3-x4.bin', undefined, 'x3'],
    ['ESP32-C3', 'CROSSPOINT_X4.BIN', undefined, 'x4'],
    ['ESP32-C3', 'crosspoint-x4pro.bin', undefined, 'x3'],
    ['ESP32-C3', '/build/x4/firmware.bin', undefined, 'x3'],
    ['ESP32-C3', '/build/x3/firmware-x4.bin', undefined, 'x4'],
    ['ESP32-C3', 'picco.bin', undefined, 'x3'],
    ['ESP32-S3', 'firmware.bin', undefined, 'x4pro'],
    ['ESP32-S3', 'firmware-PICCO.bin', undefined, 'picco'],
    ['ESP32-S3', '/build/picco/firmware.bin', undefined, 'x4pro'],
    ['ESP32-S3', 'firmware-x3-x4.bin', undefined, 'x4pro'],
    ['ESP32-C3', 'x4.bin', 'x3', 'x3'],
    ['ESP32-C3', 'x3.bin', 'x4', 'x4'],
    ['ESP32-S3', 'picco.bin', 'x4pro', 'x4pro'],
    ['ESP32-C3', 'x4.bin', 'picco', 'x4'],
    ['ESP32-C3', 'x4.bin', 'auto', 'x4'],
  ]) assert.equal(deviceOf(chip, path, remembered), expected, `${chip} ${path} ${remembered}`);
});

test('only boards for the chip are offered, default first', () => {
  assert.deepEqual(devicesOf('ESP32-C3'), ['x3', 'x4']);
  assert.deepEqual(devicesOf('ESP32-S3'), ['x4pro', 'picco']);
  assert.deepEqual(devicesOf(null), []);
  assert.equal(deviceOf(null, 'x4.bin'), null);
});
