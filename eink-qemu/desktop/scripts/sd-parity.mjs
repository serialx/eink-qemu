// Read an SD image with the browser's fat.js and print what it sees, so the
// desktop's Rust explorer (fatfs) can be checked against it.
// usage: node scripts/sd-parity.mjs card.img
// Prints JSON: {type, free, total, order: {dir: [names in list order]},
// tree: {path: {dir: true} | {size, fnv1a}}}. fat.js validates the boot
// sector, FSInfo signatures, FAT mirroring and every cluster chain it reads;
// this script also checks every FSInfo copy's free count against the FAT.
import { readFileSync } from 'node:fs';
import FAT from '../../wasm/web/fat.mjs';

const fs = FAT.open(new Uint8Array(readFileSync(process.argv[2])));
const order = {}, tree = {};
// 32-bit FNV-1a, easy to recompute on the Rust side without extra crates.
function fnv1a(bytes) {
  let hash = 0x811c9dc5;
  for (const byte of bytes) hash = Math.imul(hash ^ byte, 0x01000193) >>> 0;
  return hash;
}
const join = (dir, name) => dir === '/' ? `/${name}` : `${dir}/${name}`;
function walk(dir) {
  const entries = fs.list(dir);
  order[dir] = entries.map(entry => entry.name);
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDir) { tree[path] = { dir: true }; walk(path); }
    else {
      const bytes = fs.readFile(path);
      if (bytes.length !== entry.size) throw new Error(`${path}: read ${bytes.length} of ${entry.size} bytes`);
      tree[path] = { size: entry.size, fnv1a: fnv1a(bytes) };
    }
  }
}
walk('/');
// FSInfo (every copy) must hold the FAT's free count; the next-free hint
// must name a cluster or be unset (fresh cards start it at 2).
const image = fs.image, view = new DataView(image.buffer, image.byteOffset, image.byteLength);
for (const offset of fs.fsInfo) {
  const free = view.getUint32(offset + 488, true), next = view.getUint32(offset + 492, true);
  if (free !== fs.freeClusters) throw new Error(`FSInfo at ${offset} says ${free} free clusters; the FAT has ${fs.freeClusters}`);
  if (next !== 0xffffffff && (next < 2 || next >= fs.clusterCount + 2)) throw new Error(`FSInfo at ${offset} has next-free hint ${next}`);
}
console.log(JSON.stringify({ type: fs.type, free: fs.freeBytes(), total: fs.totalBytes, order, tree }));
