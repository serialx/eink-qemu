/* X4 Pro encrypted_v1 OTA container reader. Uses the browser's Web Crypto API.
 * Format reference (layout and channel constants):
 * https://github.com/crosspoint-reader/crosspoint-tools/blob/master/unlocker-tool/crates/unlocker-core/src/xota.rs
 * Firmware stays local and its decrypted application bytes are not modified.
 */
'use strict';
globalThis.XOTA = (() => {
    const metadataSize = 182, headerSize = 20 + metadataSize;
    const tables = [
        '2a28d5e48a2ba85804a5bf786c8c6391',
        'ea915660e42d5176c2333dea482a53fa'
    ];
    const isXota = bytes => bytes.length >= 4 &&
        bytes[0] === 0x58 && bytes[1] === 0x4f && bytes[2] === 0x54 && bytes[3] === 0x41;

    async function decode(bytes, maxAppSize = 0x640000) {
        if (!isXota(bytes) || bytes.length < headerSize + 24) {
            throw new Error('Invalid or truncated XOTA firmware file.');
        }
        const appSize = bytes.length - headerSize;
        if (appSize > maxAppSize) {
            throw new Error(`XOTA app exceeds the emulator app partition (${maxAppSize} bytes).`);
        }
        const cipher = {name: 'AES-CTR', counter: bytes.slice(4, 20), length: 128};
        for (let channel = 0; channel < tables.length; channel++) {
            const rawKey = Uint8Array.from(tables[channel].match(/../g),
                (hex, i) => parseInt(hex, 16) ^ ((17 * i - 0x5b) & 0xff));
            const key = await crypto.subtle.importKey('raw', rawKey, 'AES-CTR', false, ['decrypt']);
            const metadata = new Uint8Array(await crypto.subtle.decrypt(cipher, key,
                bytes.subarray(20, headerSize)));
            if (metadata[36] !== channel || metadata[37] !== 0 ||
                new DataView(metadata.buffer).getUint32(0, true) !== appSize) continue;

            // Metadata and body share one CTR stream; the body starts mid-block.
            const plaintext = new Uint8Array(await crypto.subtle.decrypt(cipher, key, bytes.subarray(20)));
            const app = plaintext.slice(metadataSize);
            const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', app));
            if (!digest.every((byte, i) => byte === metadata[4 + i])) {
                throw new Error('XOTA firmware checksum mismatch. The file may be damaged.');
            }
            if (app[0] !== 0xe9 || app[12] !== 9 || app[13] !== 0) {
                throw new Error('XOTA does not contain an ESP32-S3 firmware image for X4 Pro.');
            }
            return app;
        }
        throw new Error('Invalid or unsupported XOTA metadata (channel or firmware length).');
    }
    return Object.freeze({isXota, decode});
})();
