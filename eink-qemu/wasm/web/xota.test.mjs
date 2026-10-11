// node --test eink-qemu/wasm/web/xota.test.mjs
// Synthetic firmware only; Node's cipher API builds independent test fixtures.
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {createCipheriv, createHash} from 'node:crypto';
import './xota.js';

const keys = ['8f9e123c63d1a344299bf0181d0ef035', '4f2791b80dd75a6aef0d728a39a8c05e'];
function appImage(size = 257) {
    const app = Buffer.alloc(size);
    for (let i = 0; i < size; i++) app[i] = i & 255;
    app[0] = 0xe9; app[12] = 9; app[13] = 0;
    return app;
}
function wrap(app, channel = 0, changeMetadata = () => {}) {
    const metadata = Buffer.alloc(182);
    metadata.writeUInt32LE(app.length);
    createHash('sha256').update(app).digest().copy(metadata, 4);
    metadata.writeUInt16LE(channel, 36);
    metadata.write('V7.6.11', 38);
    metadata.write('ESP32S3_X4_TL', 70);
    metadata.write('SSD1677', 94);
    changeMetadata(metadata);
    const iv = Buffer.from('ffeeddccbbaa99887766554433221100', 'hex');
    const cipher = createCipheriv('aes-128-ctr', Buffer.from(keys[channel], 'hex'), iv);
    return Buffer.concat([Buffer.from('XOTA'), iv,
        cipher.update(Buffer.concat([metadata, app])), cipher.final()]);
}

for (const channel of [0, 1]) {
    test(`channel ${channel}: preserves firmware across the partial CTR block`, async () => {
        const app = appImage(), file = wrap(app, channel), original = Buffer.from(file);
        assert.equal(XOTA.isXota(file), true);
        // Also exercise a view that does not start at offset zero in its buffer.
        const padded = Buffer.concat([Buffer.alloc(13), file]);
        assert.deepEqual(Buffer.from(await XOTA.decode(padded.subarray(13))), app);
        assert.deepEqual(file, original);
    });
}
test('rejects missing magic and truncated headers', async () => {
    for (const file of [Buffer.alloc(0), Buffer.from('XOTA'), Buffer.alloc(250)]) {
        await assert.rejects(XOTA.decode(file), /Invalid or truncated/);
    }
    assert.equal(XOTA.isXota(appImage()), false);
});
test('rejects truncated bodies, appended data and inconsistent metadata', async () => {
    const file = wrap(appImage());
    for (const invalid of [file.subarray(0, -1), Buffer.concat([file, Buffer.alloc(1)]),
        wrap(appImage(), 0, m => m.writeUInt32LE(123)),
        wrap(appImage(), 0, m => m.writeUInt16LE(1, 36))]) {
        await assert.rejects(XOTA.decode(invalid), /metadata/);
    }
});
test('rejects corrupted firmware and digest', async () => {
    const file = wrap(appImage());
    file[file.length - 1] ^= 1;
    await assert.rejects(XOTA.decode(file), /checksum mismatch/);
    await assert.rejects(XOTA.decode(wrap(appImage(), 1, m => m[4] ^= 1)), /checksum mismatch/);
});
test('enforces the application size limit', async () => {
    const file = wrap(appImage());
    await assert.rejects(XOTA.decode(file, 256), /exceeds/);
    assert.equal((await XOTA.decode(file, 257)).length, 257);
});
test('rejects non-S3 firmware even with a valid checksum', async () => {
    for (const [offset, value] of [[0, 0], [12, 5], [13, 1]]) {
        const app = appImage(); app[offset] = value;
        await assert.rejects(XOTA.decode(wrap(app)), /ESP32-S3/);
    }
});
