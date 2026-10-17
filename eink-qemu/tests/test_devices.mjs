// Run with: node --test eink-qemu/tests/test_devices.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { deviceOf, devicesOf } from '../desktop/src/devices.ts';

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
