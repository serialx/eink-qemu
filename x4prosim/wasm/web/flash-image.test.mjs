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

function app(chip = 9, length = 64) {
    const bytes = new Uint8Array(length).fill(0x42);
    bytes[0] = 0xe9;
    new DataView(bytes.buffer).setUint16(12, chip, true);
    return bytes;
}
function table(picco = false) {
    const bytes = new Uint8Array(0xc00).fill(0xff), view = new DataView(bytes.buffer);
    const entries = picco ? [
        [1, 2, 0x9000, 0xc000], [1, 0, 0x15000, 0x2000],
        [0, 0, 0x20000, 0xc0000], [0, 0x10, 0xe0000, 0xe70000],
        [1, 0x82, 0xf50000, 0xb0000]
    ] : [[1, 2, 0x9000, 0x5000], [1, 0, 0xe000, 0x2000], [0, 0x10, 0x10000, 0x640000]];
    entries.forEach(([type, subtype, offset, size], i) => {
        const pos = i * 32;
        view.setUint16(pos, 0x50aa, true); bytes[pos + 2] = type; bytes[pos + 3] = subtype;
        view.setUint32(pos + 4, offset, true); view.setUint32(pos + 8, size, true);
    });
    return bytes;
}
test('Picco is an explicit S3 board with its own composition assets', () => {
    assert.equal(FlashImage.selectMachine(9, 'picco'), 'picco');
    assert.throws(() => FlashImage.selectMachine(5, 'picco'), /conflicts/);
    assert.deepEqual(FlashImage.appLayout('picco'), {
        appOffset: 0xe0000, appSize: 0xe70000, otaOffset: 0x15000,
        bootAsset: 'bootloader-esp32s3.bin', partitionAsset: 'partitions-picco.bin', nvsAsset: null
    });
    assert.equal(FlashImage.appLayout('x3').nvsAsset, 'nvs-x3.bin');
    assert.equal(FlashImage.appLayout('x4').nvsAsset, null);
    assert.equal(FlashImage.appLayout('x4pro').appOffset, 0x10000);
    assert.throws(() => FlashImage.appLayout('unknown'), /Unknown device/);
});
test('Picco composition selects valid ota_0 and keeps app and recovery bytes intact', () => {
    const bytes = app(9, 0x640001), boot = app(), partitions = table(true);
    const flash = FlashImage.composeApp(bytes, {machine: 'picco', boot, partitions});
    assert.equal(flash.length, 0x1000000);
    assert.deepEqual(flash.subarray(0, boot.length), boot);
    assert.deepEqual(flash.subarray(0x8000, 0x8c00), partitions);
    assert.deepEqual(flash.subarray(0xe0000, 0xe0000 + bytes.length), bytes);
    assert.ok(flash.subarray(0x20000, 0xe0000).every(byte => byte === 255));
    assert.ok(flash.subarray(0x9000, 0x15000).every(byte => byte === 255));
    assert.ok(flash.subarray(0xf50000).every(byte => byte === 255));
    const view = new DataView(flash.buffer);
    assert.equal(view.getUint32(0x15000, true), 1);
    assert.equal(view.getUint32(0x15018, true), 2);
    assert.equal(view.getUint32(0x1501c, true), 0x4743989a);
    assert.ok(flash.subarray(0x15020, 0x17000).every(byte => byte === 255));
});
test('composition validates app capacity and bundled assets', () => {
    const options = {machine: 'picco', boot: app(), partitions: table(true)};
    assert.throws(() => FlashImage.composeApp(app(9, 0xe70001), options), /partition/);
    assert.throws(() => FlashImage.composeApp(app(5), options), /conflicts/);
    assert.throws(() => FlashImage.composeApp(app(), {...options, boot: app(5)}), /boot assets/);
    assert.throws(() => FlashImage.composeApp(app(), {...options, partitions: table()}), /partition/);
});
test('CrossPoint and XOTA composition retain existing offsets and boot selection', () => {
    const bytes = app(), options = {machine: 'x4pro', boot: app(), partitions: table()};
    const flash = FlashImage.composeApp(bytes, options);
    assert.deepEqual(flash.subarray(0x10000, 0x10000 + bytes.length), bytes);
    assert.ok(flash.subarray(0xe000, 0x10000).every(byte => byte === 255));
    const xota = FlashImage.composeApp(bytes, {...options, selectOta: true});
    assert.equal(new DataView(xota.buffer).getUint32(0xe018, true), 2);
    const nvs = new Uint8Array(0x5000).fill(0x37);
    const x3 = FlashImage.composeApp(app(5), {machine: 'x3', boot: app(5), partitions: table(), nvs});
    assert.deepEqual(x3.subarray(0x9000, 0xe000), nvs);
});
test('merged Picco images preserve a populated recovery slot and original OTA selection', () => {
    const bytes = app(), flash = FlashImage.composeApp(bytes, {
        machine: 'picco', boot: app(), partitions: table(true)
    });
    flash.fill(0xff, 0x15000, 0x17000);
    flash.set(bytes, 0x20000);
    flash[0xf50000] = 42;
    const merged = flash.subarray(0, 0xf50001), expanded = FlashImage.expandMerged(merged);
    assert.deepEqual(expanded.subarray(0, merged.length), merged);
    assert.ok(expanded.subarray(merged.length).every(byte => byte === 255));
});
