/* Recognize merged ESP flash images whose erased tail was omitted. */
'use strict';
globalThis.FlashImage = (() => {
    function selectMachine(chip, selected = 'auto') {
        if (chip !== 5 && chip !== 9) throw new Error(`Unsupported ESP chip ID ${chip}.`);
        const chips = {x3: 5, x4: 5, x4pro: 9};
        if (selected === 'auto') {
            if (chip === 5) throw new Error('Choose X3 or X4 in Device; both use ESP32-C3.');
            return 'x4pro';
        }
        if (!Object.hasOwn(chips, selected)) throw new Error(`Unknown device: ${selected}.`);
        if (chips[selected] !== chip) throw new Error(`Device ${selected} conflicts with this ESP32-${chip === 5 ? 'C3' : 'S3'} firmware.`);
        return selected;
    }
    function expandMerged(bytes) {
        if (bytes.length < 0x8002 || bytes[0] !== 0xe9 ||
            bytes[0x8000] !== 0xaa || bytes[0x8001] !== 0x50) return null;
        if (bytes.length > 0x1000000) throw new Error('Merged firmware exceeds 16 MiB flash.');
        if (bytes.length < 0x8c00) throw new Error('Truncated merged firmware partition table.');
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        const chip = view.getUint16(12, true), partitions = [];
        if (chip !== 5 && chip !== 9) throw new Error('Unsupported chip in merged firmware.');
        for (let pos = 0x8000; pos < 0x8c00; pos += 32) {
            const magic = view.getUint16(pos, true);
            if (magic === 0xffff || magic === 0xebeb) break;
            const offset = view.getUint32(pos + 4, true), size = view.getUint32(pos + 8, true);
            if (magic !== 0x50aa || !size || offset < 0x9000 || offset + size > 0x1000000 ||
                partitions.some(p => offset < p.offset + p.size && p.offset < offset + size)) {
                throw new Error('Invalid merged firmware partition table.');
            }
            partitions.push({type: bytes[pos + 2], offset, size});
        }
        const app = partitions.some(p => p.type === 0 && p.offset + 24 <= bytes.length &&
            bytes[p.offset] === 0xe9 && view.getUint16(p.offset + 12, true) === chip);
        if (!app) throw new Error('Merged firmware has no matching application image.');
        const flash = new Uint8Array(0x1000000).fill(0xff);
        flash.set(bytes);
        return flash;
    }
    return Object.freeze({expandMerged, selectMachine});
})();
