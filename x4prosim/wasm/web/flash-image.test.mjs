// node --test x4prosim/wasm/web/flash-image.test.mjs
import assert from 'node:assert/strict';
import {test} from 'node:test';
import './flash-image.js';

function merged(chip = 9) {
    const data = new Uint8Array(0x10100).fill(0xff), view = new DataView(data.buffer);
    for (const offset of [0, 0x10000]) {
        data[offset] = 0xe9; view.setUint16(offset + 12, chip, true);
    }
    view.setUint16(0x8000, 0x50aa, true); data[0x8002] = 0;
    view.setUint32(0x8004, 0x10000, true); view.setUint32(0x8008, 0x7e0000, true);
    data[0x9000] = 42;
    return data;
}
test('merged C3 and S3 images preserve all bytes and gain an erased tail', () => {
    for (const chip of [5, 9]) {
        const input = merged(chip), flash = FlashImage.expandMerged(input);
        assert.equal(flash.length, 0x1000000);
        assert.deepEqual(flash.subarray(0, input.length), input);
        assert.ok(flash.subarray(input.length).every(byte => byte === 255));
    }
});
test('ordinary app images are not identified as merged', () => {
    const data = merged(); data[0x8000] = 0;
    assert.equal(FlashImage.expandMerged(data), null);
    assert.equal(FlashImage.expandMerged(new Uint8Array(24)), null);
});
test('rejects missing applications, wrong chips and truncated tables', () => {
    const data = merged(); data[0x10000] = 0xff;
    assert.throws(() => FlashImage.expandMerged(data), /application/);
    data[0x10000] = 0xe9; data[0x1000c] = 5;
    assert.throws(() => FlashImage.expandMerged(data), /application/);
    assert.throws(() => FlashImage.expandMerged(data.subarray(0, 0x8010)), /Truncated/);
});
test('rejects overlapping and out-of-range partitions', () => {
    const data = merged(), view = new DataView(data.buffer);
    data.set(data.subarray(0x8000, 0x8020), 0x8020);
    assert.throws(() => FlashImage.expandMerged(data), /partition table/);
    data.fill(0xff, 0x8020, 0x8040);
    view.setUint32(0x8008, 0x1000000, true);
    assert.throws(() => FlashImage.expandMerged(data), /partition table/);
});

test('device selection requires a C3 board choice and validates the chip', () => {
    assert.equal(FlashImage.selectMachine(9, 'auto'), 'x4pro');
    assert.equal(FlashImage.selectMachine(5, 'x3'), 'x3');
    assert.equal(FlashImage.selectMachine(5, 'x4'), 'x4');
    assert.throws(() => FlashImage.selectMachine(5, 'auto'), /X3 or X4/);
    assert.throws(() => FlashImage.selectMachine(9, 'x4'), /conflicts/);
    assert.throws(() => FlashImage.selectMachine(5, 'x4pro'), /conflicts/);
    assert.throws(() => FlashImage.selectMachine(261, 'x4'), /Unsupported/);
    assert.throws(() => FlashImage.selectMachine(9, 'typo'), /Unknown device/);
});
