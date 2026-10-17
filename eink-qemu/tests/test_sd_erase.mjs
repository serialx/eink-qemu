// Run: node --test eink-qemu/tests/test_sd_erase.mjs
// Uses the same independent mkfs/fsck tools as wasm/web/fat.test.mjs.
import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createContext, runInContext } from 'node:vm';
import FAT from '../wasm/web/fat.mjs';

const MiB = 1024 * 1024;
const work = mkdtempSync(join(tmpdir(), 'eink-sd-erase-'));
after(() => rmSync(work, { recursive: true, force: true }));
const context = createContext({ FAT });
runInContext(readFileSync(new URL('../wasm/web/sd-ops.js', import.meta.url), 'utf8'), context);
const { sdExecute, sdMutates, sdChanged } = runInContext('({sdExecute, sdMutates, sdChanged})', context);
function command(binary, args) {
  const result = spawnSync(binary, args, {
    env: { ...process.env, PATH: `/opt/homebrew/bin:/opt/homebrew/sbin:${process.env.PATH}` },
    encoding: 'utf8',
  });
  assert.equal(result.error, undefined, `${binary}: ${result.error?.message}`);
  assert.equal(result.status, 0, `${binary}: ${result.stdout}\n${result.stderr}`);
  return result.stdout;
}
function card(size = 64, cluster = 512) {
  const path = join(work, 'source.img');
  command('python3', [fileURLToPath(new URL('../mksd.py', import.meta.url)), path, `${size}`, '--cluster-size', `${cluster}`]);
  return new Uint8Array(readFileSync(path));
}
function verify(image, cluster) {
  const fs = FAT.open(image), view = new DataView(image.buffer, image.byteOffset, image.byteLength);
  assert.equal(fs.type, 'FAT32');
  assert.equal(fs.base, MiB);
  assert.equal(fs.clusterSize, cluster);
  assert.deepEqual(fs.list('/'), []);
  assert.equal(fs.freeBytes() + cluster, fs.totalBytes);
  assert.equal(image[450], 0x0c);
  assert.equal(view.getUint32(454, true), 2048);
  assert.equal(view.getUint32(458, true) * 512 + MiB, image.length);
  assert.deepEqual(image.subarray(MiB, MiB + 512), image.subarray(MiB + 6 * 512, MiB + 7 * 512));
  for (const offset of fs.fsInfo) assert.equal(view.getUint32(offset + 488, true), fs.clusterCount - 1);
  const path = join(work, 'volume.img');
  writeFileSync(path, image.subarray(MiB));
  assert.doesNotMatch(command('fsck.fat', ['-n', path]), /would be changed|dirty bit|unconnected|reclaimed|differences|corrupt/i);
  return fs;
}

test('erase preserves the existing array and cluster size, creates clean FAT32, and removes all files', () => {
  for (const [size, cluster] of [[64, 512], [128, 1024]]) {
    const source = card(size, cluster);
    // A borrowed subarray must not damage bytes elsewhere in its buffer.
    const backing = new Uint8Array(source.length + 32).fill(0xa5);
    const image = backing.subarray(16, backing.length - 16);
    image.set(source);
    const fs = FAT.open(image);
    fs.mkdir('/books'); fs.mkdir('/books/deep'); fs.mkdir('/empty');
    fs.writeFile('/books/deep/book.epub', new Uint8Array(200_000).fill(0x79));
    image.fill(0xa5, image.length - 512);
    const buffer = image.buffer;
    assert.equal(Object.keys(sdExecute(image, 'erase', {})).length, 0);
    assert.equal(image.buffer, buffer);
    assert.equal(image.length, size * MiB);
    assert.ok(backing.subarray(0, 16).every(b => b === 0xa5));
    assert.ok(backing.subarray(backing.length - 16).every(b => b === 0xa5));
    assert.ok(image.subarray(image.length - 512).every(b => b === 0));
    const fresh = verify(image, cluster);
    fresh.writeFile('/usable.txt', new Uint8Array([1, 2, 3]));
    assert.deepEqual(FAT.open(image).readFile('/usable.txt'), new Uint8Array([1, 2, 3]));
    sdExecute(image, 'erase', {});
    verify(image, cluster);
  }
  assert.equal(sdMutates('erase'), true);
});

test('erase converts a bare FAT32 volume to MBR without resizing it', () => {
  const image = card().slice(MiB), size = image.length;
  FAT.open(image).writeFile('/gone', new Uint8Array([1]));
  sdExecute(image, 'erase', {});
  assert.equal(image.length, size);
  verify(image, 512);
});

test('erase rejects invalid input or insufficient FAT32 geometry before changing bytes', () => {
  for (const image of [new Uint8Array(512), card(34).slice(MiB)]) {
    const before = image.slice();
    assert.throws(() => sdExecute(image, 'erase', {}), error => {
      assert.equal(error.code, 'ENOTSUP');
      assert.equal(sdChanged('erase', error), false);
      return true;
    });
    assert.deepEqual(image, before);
  }
});

test('erase marks failures after clearing bytes as changed even for a precheck error code', () => {
  const image = card(), original = Uint8Array.prototype.set;
  try {
    Uint8Array.prototype.set = function (...args) {
      if (this.buffer === image.buffer) throw Object.assign(new Error('injected write failure'), { code: 'EINVAL' });
      return original.apply(this, args);
    };
    assert.throws(() => sdExecute(image, 'erase', {}), error => {
      assert.equal(error.changed, true);
      assert.equal(sdChanged('erase', error), true);
      return true;
    });
  } finally {
    Uint8Array.prototype.set = original;
  }
  assert.ok(image.every(b => b === 0));
});
