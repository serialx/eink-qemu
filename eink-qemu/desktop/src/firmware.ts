/* The browser page's prepareFlash: release apps and X4 Pro .xota updates are
   composed into 16 MiB with the bundled boot assets (the Arduino-ESP32 16 MB
   partition table, or the Picco's); merged images and full dumps pass
   through. Images are validated before QEMU sees them. CrossPoint's NVS entry
   (its profile, eink-qemu/profiles/crosspoint.json) goes only into apps that
   carry its strings, on the boards it names. */
import '@web/xota.js';
import '@web/flash-image.js';
import crosspoint from '../../profiles/crosspoint.json' with { type: 'json' };

export interface Flash {
  flash: Uint8Array;
  machine: Machine;
  composed: boolean;
  /** How an app image was composed, for the status bar and the log; null when the image passed through. */
  composition: string | null;
}

const chipOf = (bytes: Uint8Array) => bytes[12] | (bytes[13] << 8);

async function bootAsset(name: string): Promise<Uint8Array> {
  const response = await fetch(`./boot/${name}`);
  if (!response.ok) {
    const local = ['localhost', '127.0.0.1', '[::1]'].includes(location.hostname);
    throw new Error(`Could not load the boot file ${name} (HTTP ${response.status}). Reload the page to try again.` +
      (local ? ' Serving a checkout? Run scripts/prepare-resources.sh first.' : ''));
  }
  return new Uint8Array(await response.arrayBuffer());
}

export async function prepareFlash(input: Uint8Array, device: string, progress: (text: string) => void): Promise<Flash> {
  let bytes = input;
  const xota = XOTA.isXota(bytes);
  if (xota) {
    progress('Reading and verifying XOTA firmware…');
    bytes = await XOTA.decode(bytes);
  }
  if (bytes.length === 0x1000000) {
    // The shared check covers boot magic, overlapping partitions and an
    // application for the boot chip; then every application is verified.
    if (!FlashImage.expandMerged(bytes)) throw new Error('This 16 MiB file is not an ESP flash image: no bootloader or partition table.');
    await validateFlash(bytes);
    return { flash: bytes, machine: FlashImage.selectMachine(chipOf(bytes), device), composed: false, composition: null };
  }
  const merged = !xota && FlashImage.expandMerged(bytes);
  if (merged) {
    await validateFlash(merged);
    return { flash: merged, machine: FlashImage.selectMachine(chipOf(merged), device), composed: false, composition: null };
  }
  if (bytes.length < 24 || bytes[0] !== 0xe9) {
    throw new Error('Choose a firmware .bin, an X4 Pro .xota update, or a 16 MiB flash dump (16777216 bytes).');
  }
  await validateApp(bytes);
  const chip = chipOf(bytes);
  if (chip !== 5 && chip !== 9) throw new Error(`Unsupported ESP chip ID ${chip}; expected ESP32-C3 (5) or ESP32-S3 (9).`);
  const machine = FlashImage.selectMachine(chip, device);
  const layout = FlashImage.appLayout(machine);
  progress('Composing 16 MiB flash image…');
  const seed = crosspoint.nvs_seed.machines.includes(machine) && FlashImage.hasCStrings(bytes, crosspoint.detect.image_strings);
  const [boot, partitions, nvs] = await Promise.all([
    bootAsset(layout.bootAsset), bootAsset(layout.partitionAsset), seed ? bootAsset('nvs-crosspoint.bin') : null,
  ]);
  const composition = `Composed 16 MiB flash from the ${xota ? 'XOTA update' : 'app'} with ${layout.partitionTable}` +
    (seed ? ` and ${crosspoint.title}'s NVS entry` : '');
  return { flash: FlashImage.composeApp(bytes, { machine, boot, partitions, nvs, selectOta: xota }), machine, composed: true, composition };
}

async function validateApp(bytes: Uint8Array): Promise<void> {
  const invalid = (reason: string): never => { throw new Error(`Invalid ESP application image: ${reason}.`); };
  if (bytes.length < 24 || bytes[0] !== 0xe9 || bytes[1] < 1 || bytes[1] > 16) invalid('bad header or segment count');
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 24, checksum = 0xef;
  for (let segment = 0; segment < bytes[1]; segment++) {
    if (offset + 8 > bytes.length) invalid('truncated segment header');
    const address = view.getUint32(offset, true), length = view.getUint32(offset + 4, true);
    offset += 8;
    if (length > 0x1000000 || offset + length > bytes.length || address + length > 0x100000000) invalid('truncated or invalid segment length');
    for (let i = offset; i < offset + length; i++) checksum ^= bytes[i];
    offset += length;
  }
  const checksumOffset = Math.floor(offset / 16) * 16 + 15;
  if (checksumOffset >= bytes.length) invalid('truncated checksum');
  if (bytes[checksumOffset] !== checksum) invalid('checksum mismatch');
  if (bytes[23] > 1) invalid('invalid appended-hash flag');
  if (bytes[23] === 1) {
    const end = checksumOffset + 1;
    if (end + 32 > bytes.length) invalid('truncated SHA-256 digest');
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes.slice(0, end)));
    if (!digest.every((value, i) => value === bytes[end + i])) invalid('SHA-256 mismatch');
  }
}

async function validateFlash(bytes: Uint8Array): Promise<void> {
  // The second-stage bootloader is an ESP image too, before the partition table.
  try { await validateApp(bytes.subarray(0, 0x8000)); } catch (error) {
    throw new Error((error as Error).message.replace('application image', 'bootloader'));
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let applications = 0;
  for (let offset = 0x8000; offset < 0x8c00; offset += 32) {
    const magic = view.getUint16(offset, true);
    if (magic === 0xffff || magic === 0xebeb) break;
    if (magic !== 0x50aa) throw new Error('Invalid firmware partition table.');
    const address = view.getUint32(offset + 4, true), size = view.getUint32(offset + 8, true);
    if (!size || address < 0x9000 || address + size > bytes.length) throw new Error('Invalid firmware partition bounds.');
    if (bytes[offset + 2] === 0 && bytes[address] === 0xe9) {
      if (chipOf(bytes.subarray(address)) !== chipOf(bytes)) throw new Error('An application in this flash image is for a different chip than its bootloader.');
      await validateApp(bytes.subarray(address, address + size));
      applications++;
    }
  }
  if (!applications) throw new Error('The flash image contains no valid ESP application.');
}
