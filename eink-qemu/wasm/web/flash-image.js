/* Compose board-specific app images and preserve merged ESP flash images. */
'use strict';
globalThis.FlashImage = (() => {
    function selectMachine(chip, selected = 'auto') {
        if (chip !== 5 && chip !== 9) throw new Error(`Unsupported ESP chip ID ${chip}.`);
        const chips = {x3: 5, x4: 5, x4pro: 9, picco: 9};
        if (selected === 'auto') {
            if (chip === 5) throw new Error('Choose X3 or X4 in Device; both use ESP32-C3.');
            return 'x4pro';
        }
        if (!Object.hasOwn(chips, selected)) throw new Error(`Unknown device: ${selected}.`);
        if (chips[selected] !== chip) throw new Error(`Device ${selected} conflicts with this ESP32-${chip === 5 ? 'C3' : 'S3'} firmware.`);
        return selected;
    }
    function appLayout(machine) {
        if (!['x3', 'x4', 'x4pro', 'picco'].includes(machine)) {
            throw new Error(`Unknown device: ${machine}.`);
        }
        const picco = machine === 'picco', c3 = machine === 'x3' || machine === 'x4';
        return Object.freeze({
            appOffset: picco ? 0xe0000 : 0x10000,
            appSize: picco ? 0xe70000 : 0x640000,
            otaOffset: picco ? 0x15000 : 0xe000,
            bootAsset: `bootloader-${c3 ? 'esp32c3' : 'esp32s3'}.bin`,
            partitionAsset: picco ? 'partitions-picco.bin' : 'partitions.bin',
            nvsAsset: machine === 'x3' ? 'nvs-x3.bin' : null
        });
    }
    function readPartitions(bytes, offset = 0) {
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength), partitions = [];
        for (let pos = offset; pos < offset + 0xc00; pos += 32) {
            const magic = view.getUint16(pos, true);
            if (magic === 0xffff || magic === 0xebeb) break;
            const address = view.getUint32(pos + 4, true), size = view.getUint32(pos + 8, true);
            if (magic !== 0x50aa || !size || address < 0x9000 || address + size > 0x1000000 ||
                partitions.some(p => address < p.offset + p.size && p.offset < address + size)) {
                throw new Error('Invalid firmware partition table.');
            }
            partitions.push({type: bytes[pos + 2], subtype: bytes[pos + 3], offset: address, size});
        }
        return partitions;
    }
    function composeApp(input, {machine, boot, partitions, nvs = null, selectOta = false}) {
        const bytes = new Uint8Array(input), layout = appLayout(machine);
        if (bytes.length < 24 || bytes[0] !== 0xe9) throw new Error('Expected an ESP application image.');
        const chip = bytes[12] | bytes[13] << 8;
        selectMachine(chip, machine);
        if (bytes.length > layout.appSize) throw new Error(`App image exceeds the ${machine} app partition (${layout.appSize} bytes).`);
        boot = new Uint8Array(boot);
        partitions = new Uint8Array(partitions);
        nvs = nvs == null ? null : new Uint8Array(nvs);
        if (boot.length < 24 || boot.length > 0x8000 || boot[0] !== 0xe9 ||
            (boot[12] | boot[13] << 8) !== chip || partitions.length !== 0xc00 ||
            (layout.nvsAsset ? !nvs || nvs.length !== 0x5000 : nvs !== null)) {
            throw new Error('Invalid bundled boot assets. Run package-web.sh to regenerate the distribution.');
        }
        const entries = readPartitions(partitions);
        if (!entries.some(p => p.type === 0 && p.subtype === 0x10 &&
                p.offset === layout.appOffset && p.size === layout.appSize) ||
            !entries.some(p => p.type === 1 && p.subtype === 0 && p.offset === layout.otaOffset && p.size === 0x2000) ||
            (nvs && !entries.some(p => p.type === 1 && p.subtype === 2 && p.offset === 0x9000 && p.size === nvs.length))) {
            throw new Error(`Bundled partition table does not match ${machine}.`);
        }
        const flash = new Uint8Array(0x1000000).fill(0xff);
        flash.set(boot, 0);
        flash.set(partitions, 0x8000);
        if (nvs) flash.set(nvs, 0x9000);
        if (machine === 'picco' || selectOta) {
            // Select valid ota_0; Picco's factory recovery slot remains erased.
            const view = new DataView(flash.buffer);
            view.setUint32(layout.otaOffset, 1, true);
            view.setUint32(layout.otaOffset + 24, 2, true);
            view.setUint32(layout.otaOffset + 28, 0x4743989a, true);
        }
        flash.set(bytes, layout.appOffset);
        return flash;
    }
    function expandMerged(bytes) {
        if (bytes.length < 0x8002 || bytes[0] !== 0xe9 ||
            bytes[0x8000] !== 0xaa || bytes[0x8001] !== 0x50) return null;
        if (bytes.length > 0x1000000) throw new Error('Merged firmware exceeds 16 MiB flash.');
        if (bytes.length < 0x8c00) throw new Error('Truncated merged firmware partition table.');
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        const chip = view.getUint16(12, true);
        if (chip !== 5 && chip !== 9) throw new Error('Unsupported chip in merged firmware.');
        const partitions = readPartitions(bytes, 0x8000);
        const app = partitions.some(p => p.type === 0 && p.offset + 24 <= bytes.length &&
            bytes[p.offset] === 0xe9 && view.getUint16(p.offset + 12, true) === chip);
        if (!app) throw new Error('Merged firmware has no matching application image.');
        const flash = new Uint8Array(0x1000000).fill(0xff);
        flash.set(bytes);
        return flash;
    }
    return Object.freeze({expandMerged, selectMachine, appLayout, composeApp});
})();
