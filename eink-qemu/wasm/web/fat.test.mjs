// Run: PATH=/opt/homebrew/bin:/opt/homebrew/sbin:$PATH node --test eink-qemu/wasm/web/fat.test.mjs
// Requires Python 3, dosfstools, and mtools; no npm packages or firmware.
import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createContext, runInContext, runInNewContext } from 'node:vm';
import FAT, { open, FatError, isValidName } from './fat.mjs';

const MiB = 1024 * 1024;
const work = mkdtempSync(join(tmpdir(), 'eink-qemu-fat-'));
after(() => rmSync(work, { recursive: true, force: true }));
const env = { ...process.env, PATH: `/opt/homebrew/bin:/opt/homebrew/sbin:${process.env.PATH}`, LC_ALL: 'en_US.UTF-8' };
function command(binary, args) {
  const result = spawnSync(binary, args, { env, encoding: 'utf8', maxBuffer: 2 * MiB });
  assert.equal(result.error, undefined, `${binary}: ${result.error?.message}`);
  assert.equal(result.status, 0, `${binary} ${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  return result.stdout;
}
const blankPath = join(work, 'blank.img');
command('python3', [fileURLToPath(new URL('../../mksd.py', import.meta.url)), blankPath, '64', '--cluster-size', '512']);
const blank = readFileSync(blankPath);
const partitionBase = blank.readUInt32LE(454) * 512;
let fixtureNumber = 0;
function fixture(superfloppy = false) {
  const image = new Uint8Array(superfloppy ? blank.subarray(partitionBase) : blank);
  const path = join(work, `disk-${fixtureNumber++}.img`);
  const fs = open(image);
  const base = superfloppy ? 0 : partitionBase;
  const view = new DataView(image.buffer);
  const bps = view.getUint16(base + 11, true), reserved = view.getUint16(base + 14, true);
  const fatSize = view.getUint32(base + 36, true) * bps;
  const fatBase = base + reserved * bps;
  function flush() { writeFileSync(path, image); }
  function verify() {
    const fs = result.fs;
    assert.equal(image.length, superfloppy ? blank.length - partitionBase : blank.length);
    assert.deepEqual(image.subarray(fatBase, fatBase + fatSize), image.subarray(fatBase + fatSize, fatBase + 2 * fatSize));
    const info = base + view.getUint16(base + 48, true) * bps;
    const backupInfo = info + view.getUint16(base + 50, true) * bps;
    let free = 0;
    for (let c = 2; c < fs.clusterCount + 2; c++) if (!(view.getUint32(fatBase + c * 4, true) & 0x0fffffff)) free++;
    assert.equal(fs.freeBytes(), free * fs.clusterSize);
    assert.equal(view.getUint32(info + 488, true), free);
    assert.equal(view.getUint32(backupInfo + 488, true), free);
    assert.equal(view.getUint32(info + 492, true), view.getUint32(backupInfo + 492, true));
    flush();
    // Independently enumerate and extract every live file after each batch.
    const visit = (directory, externalDirectory) => {
      const listing = command('mdir', ['-b', '-i', `${path}@@${base}`, `::${externalDirectory}`]);
      const cluster = fs.resolve(directory.split('/').filter(Boolean)).cluster;
      for (const entry of fs.list(directory)) {
        const full = `${directory === '/' ? '' : directory}/${entry.name}`;
        const external = `${externalDirectory === '/' ? '' : externalDirectory}/${fs.find(cluster, entry.name).shortName}`;
        if (!/[\uD800-\uDBFF]/.test(entry.name)) assert.ok(listing.includes(entry.name), `mdir lists ${full}`);
        if (entry.isDir) visit(full, external);
        else {
          const out = join(work, 'batch-extracted.bin');
          command('mcopy', ['-o', '-i', `${path}@@${base}`, `::${external}`, out]);
          assert.deepEqual(new Uint8Array(readFileSync(out)), fs.readFile(full), `mcopy bytes: ${full}`);
        }
      }
    };
    visit('/', '/');
    const partition = join(work, 'fsck-partition.img');
    writeFileSync(partition, image.subarray(base));
    const output = command('fsck.fat', ['-n', '-v', partition]);
    assert.doesNotMatch(output, /would be changed|dirty bit|unconnected|reclaimed|differences|corrupt/i);
    console.log(`fsck clean: ${output.trim().split('\n').at(-1).replace(partition, 'FAT32')}`);
    return output;
  }
  function extract(name) {
    flush();
    const out = join(work, 'extracted.bin');
    command('mcopy', ['-o', '-i', `${path}@@${base}`, `::${name}`, out]);
    return new Uint8Array(readFileSync(out));
  }
  const result = { fs, image, path, base, view, fatBase, fatSize, flush, verify, extract };
  return result;
}
function pattern(size, salt = 0) {
  const result = new Uint8Array(size);
  for (let i = 0; i < size; i++) result[i] = ((i * 31) ^ (i >>> 8) ^ salt) & 255;
  return result;
}
const digest = image => createHash('sha256').update(image).digest('hex');
function errorUnchanged(f, code, action) {
  const before = digest(f.image);
  assert.throws(action, error => error instanceof FatError && error.code === code);
  assert.equal(digest(f.image), before, `${code} must leave image byte-for-byte unchanged`);
}

test('classic script, side-effect import, module facade, metadata, and borrowed views', () => {
  assert.equal(FAT, globalThis.FAT);
  assert.equal(open, FAT.open);
  const context = {};
  runInNewContext(readFileSync(new URL('./fat.js', import.meta.url), 'utf8'), context);
  assert.equal(typeof context.FAT.open, 'function');
  const f = fixture();
  assert.equal(f.fs.type, 'FAT32');
  assert.equal(typeof f.fs.label, 'string');
  assert.equal(f.fs.totalBytes, f.fs.clusterSize * f.fs.clusterCount);
  assert.equal(f.fs.freeBytes(), f.fs.totalBytes - f.fs.clusterSize);
  assert.equal(f.fs.free(), f.fs.freeBytes());
  assert.deepEqual(f.fs.list('/'), []);
  assert.equal(f.fs.exists('/'), true);
  assert.equal(f.fs.exists('/missing/child'), false);
  const padded = new Uint8Array(blank.length + 32);
  padded.set(blank, 16);
  const sub = padded.subarray(16, 16 + blank.length);
  open(sub).writeFile('/offset.txt', Uint8Array.of(1, 2, 3));
  assert.deepEqual(open(sub).readFile('/OFFSET.TXT'), Uint8Array.of(1, 2, 3));
  assert.deepEqual(padded.subarray(0, 16), new Uint8Array(16));
  assert.deepEqual(padded.subarray(-16), new Uint8Array(16));
  assert.equal(open(blank.buffer.slice(blank.byteOffset, blank.byteOffset + blank.byteLength)).type, 'FAT32');
});

test('0/1/cluster/12 MiB/31 MiB writes, independent extraction, overwrite, and timestamps', () => {
  const f = fixture(), before = Date.now() - 2100;
  for (const size of [0, 1, f.fs.clusterSize, f.fs.clusterSize * 3, 12 * MiB, 31 * MiB]) {
    const data = pattern(size, size % 251), name = `/size-${size}.bin`;
    f.fs.writeFile(name, data);
    assert.deepEqual(f.fs.readFile(name), data);
    assert.deepEqual(f.extract(name), data);
    const entry = f.fs.list('/').find(e => e.name === name.slice(1));
    assert.deepEqual(Object.keys(entry).sort(), ['isDir', 'mtime', 'name', 'size']);
    assert.equal(entry.size, size); assert.equal(entry.isDir, false);
    assert.ok(entry.mtime instanceof Date);
    assert.ok(entry.mtime.getTime() >= before && entry.mtime.getTime() <= Date.now());
    f.verify();
  }
  const copy = f.fs.readFile('/size-1.bin'); copy[0] ^= 255;
  assert.notDeepEqual(f.fs.readFile('/size-1.bin'), copy);
  const free = f.fs.freeBytes();
  f.fs.writeFile('/SIZE-32505856.BIN', pattern(1, 71));
  assert.equal(f.fs.freeBytes(), free + 31 * MiB - f.fs.clusterSize);
  assert.deepEqual(f.extract('/size-32505856.bin'), pattern(1, 71));
  f.verify();
  f.fs.writeFile('/size-1.bin', new Uint8Array());
  assert.equal(f.fs.readFile('/size-1.bin').length, 0);
  f.verify();
});

test('mtools-created Korean VFAT, nested folders, and bytes read and edited by library', () => {
  const f = fixture(); f.flush();
  const source = join(work, 'source.bin'), data = pattern(2 * MiB + 17, 43);
  writeFileSync(source, data);
  command('mmd', ['-i', `${f.path}@@${f.base}`, '::한글 폴더']);
  command('mcopy', ['-i', `${f.path}@@${f.base}`, source, '::한글 폴더/외부에서 만든 책.epub']);
  f.image.set(readFileSync(f.path));
  f.fs = open(f.image);
  assert.deepEqual(f.fs.list('/').map(e => e.name), ['한글 폴더']);
  assert.deepEqual(f.fs.readFile('/한글 폴더/외부에서 만든 책.epub'), data);
  f.fs.writeFile('/한글 폴더/외부에서 만든 책.epub', pattern(15, 99));
  assert.deepEqual(f.extract('/한글 폴더/외부에서 만든 책.epub'), pattern(15, 99));
  f.verify();
});

test('UTF-16 names, numeric short alias collisions, sorted listing, and directory growth', () => {
  const f = fixture();
  f.fs.mkdir('/z folder'); f.fs.mkdir('/A folder');
  const names = ['해커와 화가.epub', '리더의 질문법.epub', 'multiple.dots.in.name.epub', '한국어 이름과 공백.epub', '책📚과😀.txt', 'éclair.txt', 'MixedCase.TXT', 'abcdefghijklm', '한'.repeat(255), 'a'.repeat(12) + '😀' + '나'.repeat(241)];
  for (let i = 0; i < 40; i++) names.push(`collision long name ${i}.txt`);
  for (const [i, name] of names.entries()) f.fs.writeFile(`/${name}`, Uint8Array.of(i));
  const entries = f.fs.list('/');
  assert.deepEqual(entries.slice(0, 2).map(e => e.name), ['A folder', 'z folder']);
  const files = entries.slice(2).map(e => e.name);
  assert.deepEqual(files, [...files].sort((a, b) => a.toUpperCase() < b.toUpperCase() ? -1 : 1));
  for (const [i, name] of names.entries()) assert.deepEqual(f.fs.readFile(`/${name}`), Uint8Array.of(i));
  for (const name of names) {
    // mtools does not combine UTF-16 surrogate pairs for command-line lookup.
    // Validate their bytes through the numeric alias, and inspect LFN units below.
    const externalName = /[\uD800-\uDBFF]/.test(name) ? f.fs.find(f.fs.root, name).shortName : name;
    assert.deepEqual(f.extract(`/${externalName}`), f.fs.readFile(`/${name}`));
  }
  for (const name of names) {
    const entry = f.fs.find(f.fs.root, name);
    const units = entry.slots.slice(0, -1).reverse().flatMap(p =>
      [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].map(n => f.view.getUint16(p + n, true)));
    assert.deepEqual(units.slice(0, name.length), Array.from({ length: name.length }, (_, i) => name.charCodeAt(i)));
  }
  f.flush();
  const aliases = command('mdir', ['-i', `${f.path}@@${f.base}`, '::']);
  assert.match(aliases, /COLLI~10|COLLI~11/);
  for (const name of names.filter(name => !/[\uD800-\uDBFF]/.test(name))) assert.ok(aliases.includes(name), `mdir lists ${name}`);
  f.verify();
  assert.deepEqual(open(f.image).list('/'), entries);
});

test('basename and case-only rename preserves nested contents and metadata', () => {
  const f = fixture();
  f.fs.mkdir('/Books'); f.fs.mkdir('/Books/한글');
  f.fs.writeFile('/Books/한글/원래 책.epub', pattern(9000));
  const before = f.fs.list('/Books/한글')[0].mtime;
  f.fs.rename('/Books/한글/원래 책.epub', '이름을 바꾼 책.epub');
  assert.equal(f.fs.exists('/Books/한글/원래 책.epub'), false);
  assert.equal(f.fs.list('/Books/한글')[0].mtime.getTime(), before.getTime());
  f.verify();
  f.fs.rename('/Books', 'BOOKS'); f.fs.rename('/BOOKS/한글', '새 폴더');
  assert.equal(f.fs.list('/')[0].name, 'BOOKS');
  assert.deepEqual(f.extract('/BOOKS/새 폴더/이름을 바꾼 책.epub'), pattern(9000));
  errorUnchanged(f, 'EINVAL', () => f.fs.rename('/BOOKS', '/other'));
  f.fs.mkdir('/Other');
  errorUnchanged(f, 'EEXIST', () => f.fs.rename('/BOOKS', 'other'));
  f.verify();
});

test('recursive removal frees chains, reuses space, and removes LFN slots', () => {
  const f = fixture(), initial = f.fs.freeBytes();
  f.fs.mkdir('/tree'); f.fs.mkdir('/tree/sub'); f.fs.mkdir('/tree/sub/deep');
  f.fs.writeFile('/tree/sub/deep/큰 책.epub', pattern(12 * MiB));
  f.fs.writeFile('/tree/empty', new Uint8Array());
  f.verify();
  f.fs.remove('/tree');
  assert.deepEqual(f.fs.list('/'), []);
  assert.equal(f.fs.freeBytes(), initial);
  f.verify();
  f.fs.writeFile('/reuse.bin', pattern(12 * MiB, 4));
  assert.deepEqual(f.extract('/reuse.bin'), pattern(12 * MiB, 4));
  f.fs.remove('/reuse.bin'); f.fs.mkdir('/tree');
  assert.equal(f.fs.freeBytes(), initial - f.fs.clusterSize);
  f.verify();
});

test('full disk rejects changes atomically, reuses overwrite clusters, and recovers freed space', () => {
  const f = fixture();
  for (let i = 0; i < f.fs.clusterSize / 32; i++) f.fs.writeFile(`/F${i}`, new Uint8Array());
  const data = pattern(f.fs.freeBytes(), 21);
  f.fs.writeFile('/F0', data);
  assert.equal(f.fs.freeBytes(), 0);
  f.verify();
  errorUnchanged(f, 'ENOSPC', () => f.fs.writeFile('/NEW.TXT', Uint8Array.of(2)));
  errorUnchanged(f, 'ENOSPC', () => f.fs.mkdir('/folder'));
  errorUnchanged(f, 'ENOSPC', () => f.fs.rename('/F0', 'long renamed file.bin'));
  errorUnchanged(f, 'ENOSPC', () => f.fs.writeFile('/F1', Uint8Array.of(2)));
  assert.deepEqual(f.fs.readFile('/F0'), data);
  f.fs.writeFile('/F0', pattern(data.length, 22));
  assert.deepEqual(f.extract('/F0'), pattern(data.length, 22));
  f.verify();
  f.fs.writeFile('/F0', pattern(data.length - f.fs.clusterSize, 23));
  assert.equal(f.fs.freeBytes(), f.fs.clusterSize);
  // A new long name needs directory growth as well as its data cluster.
  errorUnchanged(f, 'ENOSPC', () => f.fs.writeFile('/new long file.txt', Uint8Array.of(1)));
  f.verify();
  f.fs.remove('/F0');
  f.fs.mkdir('/folder'); f.fs.writeFile('/folder/restored', pattern(31 * MiB));
  assert.deepEqual(f.extract('/folder/restored'), pattern(31 * MiB));
  f.verify();
});

test('invalid names, collisions, wrong types, and missing parents preserve the image', () => {
  const f = fixture();
  f.fs.writeFile('/taken.txt', Uint8Array.of(1)); f.fs.mkdir('/folder');
  for (const name of ['', '.', '..', 'bad?', 'bad*', 'bad:', 'bad\\name', 'bad/name', 'bad\0name', 'bad\nname',
    'bad"', 'bad<', 'bad>', 'bad|', 'trailing.', 'trailing ', 'CON', 'nul.txt', 'LPT1', 'x'.repeat(256), '\ud800', '\udc00', '\uffff', null, 123]) {
    assert.equal(isValidName(name), false, String(name));
  }
  for (const name of ['한글.txt', 'a'.repeat(255), '.hidden', '책📚.epub', 'ok name']) assert.equal(isValidName(name), true);
  for (const name of ['bad?', 'bad\\name', 'trailing.', 'CON', 'x'.repeat(256), '\ud800']) {
    errorUnchanged(f, 'EINVAL', () => f.fs.writeFile(`/${name}`, Uint8Array.of(1)));
    errorUnchanged(f, 'EINVAL', () => f.fs.mkdir(`/${name}`));
    errorUnchanged(f, 'EINVAL', () => f.fs.rename('/taken.txt', name));
  }
  for (const path of ['', 'relative', '/folder/../escape', '/./taken.txt']) errorUnchanged(f, 'EINVAL', () => f.fs.list(path));
  errorUnchanged(f, 'EEXIST', () => f.fs.mkdir('/TAKEN.TXT'));
  errorUnchanged(f, 'EEXIST', () => f.fs.rename('/taken.txt', 'FOLDER'));
  errorUnchanged(f, 'EISDIR', () => f.fs.writeFile('/folder', Uint8Array.of(1)));
  errorUnchanged(f, 'EISDIR', () => f.fs.readFile('/folder'));
  errorUnchanged(f, 'ENOTDIR', () => f.fs.list('/taken.txt'));
  errorUnchanged(f, 'ENOTDIR', () => f.fs.mkdir('/taken.txt/sub'));
  errorUnchanged(f, 'ENOENT', () => f.fs.writeFile('/missing/file', Uint8Array.of(1)));
  errorUnchanged(f, 'ENOENT', () => f.fs.readFile('/missing'));
  errorUnchanged(f, 'ENOENT', () => f.fs.remove('/missing'));
  errorUnchanged(f, 'ENOENT', () => f.fs.rename('/missing', 'new'));
  for (const fn of [() => f.fs.mkdir('/'), () => f.fs.remove('/'), () => f.fs.rename('/', 'new'), () => f.fs.writeFile('/', new Uint8Array())]) {
    errorUnchanged(f, 'EINVAL', fn);
  }
  f.verify();
});

test('superfloppy interoperates in both directions', () => {
  const f = fixture(true);
  f.fs.mkdir('/folder'); f.fs.writeFile('/folder/한국어.txt', pattern(4000, 6));
  assert.deepEqual(f.extract('/folder/한국어.txt'), pattern(4000, 6));
  f.verify();
  const source = join(work, 'super-source.bin'); writeFileSync(source, pattern(1033, 9));
  command('mcopy', ['-i', f.path, source, '::outside.bin']);
  const loaded = open(readFileSync(f.path));
  assert.deepEqual(loaded.readFile('/outside.bin'), pattern(1033, 9));
  loaded.rename('/outside.bin', 'inside.bin');
  loaded.remove('/folder');
  f.image.set(loaded.image); f.fs = open(f.image); f.verify();
});

test('unsupported FAT12/16/exFAT, malformed geometry, partition tables, and corrupt FAT are rejected', () => {
  for (const [offset, value, width] of [
    [partitionBase + 11, 123, 2], [partitionBase + 13, 3, 1], [partitionBase + 16, 0, 1],
    [partitionBase + 17, 512, 2], [partitionBase + 22, 123, 2], [partitionBase + 42, 1, 2],
    [partitionBase + 32, 0xffffffff, 4], [partitionBase + 36, 1, 4], [partitionBase + 40, 128, 2],
    [partitionBase + 510, 0, 2], [450, 0x07, 1], [454, 0xffffffff, 4], [458, 0xffffffff, 4],
  ]) {
    const image = new Uint8Array(blank), v = new DataView(image.buffer);
    if (width === 1) v.setUint8(offset, value); else if (width === 2) v.setUint16(offset, value, true); else v.setUint32(offset, value, true);
    assert.throws(() => open(image), FatError);
  }
  assert.throws(() => open(new Uint8Array(10)), { code: 'ENOTSUP' });
  assert.throws(() => open(new Uint8Array(blank.subarray(0, -512))), FatError);
  assert.throws(() => open('wrong type'), { code: 'EINVAL' });
  const exfat = new Uint8Array(4096); exfat.set(new TextEncoder().encode('EXFAT   '), 3); exfat[510] = 85; exfat[511] = 170;
  assert.throws(() => open(exfat), { code: 'ENOTSUP' });
  const fat12 = join(work, 'fat12.img'); writeFileSync(fat12, new Uint8Array(1440 * 1024));
  command('mkfs.vfat', ['-F', '12', fat12]);
  assert.throws(() => open(readFileSync(fat12)), { code: 'ENOTSUP' });
  const fat16 = join(work, 'fat16.img'); writeFileSync(fat16, new Uint8Array(16 * MiB));
  command('mkfs.vfat', ['-F', '16', fat16]);
  assert.throws(() => open(readFileSync(fat16)), { code: 'ENOTSUP' });
  const f = fixture();
  f.view.setUint32(f.fatBase + f.fatSize + 12, 0x0fffffff, true);
  assert.throws(() => open(f.image), { code: 'EINVAL' });
});

test('cyclic file chains and recursive crosslinks fail before mutation', () => {
  const f = fixture();
  f.fs.writeFile('/file', pattern(1000));
  const first = f.fs.find(f.fs.root, 'file').cluster;
  f.fs.putFat(first, first);
  errorUnchanged(f, 'EINVAL', () => f.fs.readFile('/file'));
  errorUnchanged(f, 'EINVAL', () => f.fs.writeFile('/file', Uint8Array.of(2)));
  errorUnchanged(f, 'EINVAL', () => f.fs.remove('/file'));
  const g = fixture();
  g.fs.mkdir('/tree'); g.fs.writeFile('/tree/a', pattern(600)); g.fs.writeFile('/tree/b', pattern(600));
  const parent = g.fs.find(g.fs.root, 'tree'), a = g.fs.find(parent.cluster, 'a'), b = g.fs.find(parent.cluster, 'b');
  g.view.setUint16(b.p + 26, a.cluster, true); g.view.setUint16(b.p + 20, a.cluster >>> 16, true);
  errorUnchanged(g, 'EINVAL', () => g.fs.remove('/tree'));
});


test('directory end markers hide stale entries after insert and rename', () => {
  const f = fixture();
  const root = f.fs.offset(f.fs.root);
  // Legal on disk: entries after the first 0x00 slot are ignored, even if stale.
  const ghost = new Uint8Array(32);
  ghost.set(new TextEncoder().encode('GHOST   TXT')); ghost[11] = 32;
  for (let p = root + 32; p < root + f.fs.clusterSize; p += 32) f.image.set(ghost, p);
  f.fs.writeFile('/NEW.TXT', Uint8Array.of(1));
  assert.deepEqual(f.fs.list('/').map(e => e.name), ['NEW.TXT']);
  assert.equal(f.image[root + 32], 0);
  f.verify();
  f.fs.rename('/NEW.TXT', 'new longer name.txt');
  assert.deepEqual(f.fs.list('/').map(e => e.name), ['new longer name.txt']);
  f.verify();
});

test('Node Buffer input preserves rename metadata and allocation-failure atomicity', () => {
  const image = Buffer.from(blank), fs = open(image);
  fs.writeFile('/old.txt', pattern(1025));
  fs.rename('/old.txt', 'renamed.txt');
  assert.deepEqual(fs.readFile('/renamed.txt'), pattern(1025));
  fs.writeFile('/FILL', new Uint8Array(fs.freeBytes()));
  const before = digest(image);
  assert.throws(() => fs.writeFile('/renamed.txt', new Uint8Array(2 * MiB)), { code: 'ENOSPC' });
  assert.equal(digest(image), before);
  assert.deepEqual(fs.readFile('/renamed.txt'), pattern(1025));
  // Reading bytes out of the same image must snapshot before overwrite clears it.
  fs.writeFile('/renamed.txt', image.subarray(fs.offset(fs.find(fs.root, 'renamed.txt').cluster),
    fs.offset(fs.find(fs.root, 'renamed.txt').cluster) + 100));
  assert.deepEqual(fs.readFile('/renamed.txt'), pattern(100));
});


test('cross-realm Uint8Array/ArrayBuffer and shared views work without copies', () => {
  const foreign = runInNewContext('new Uint8Array(size)', { size: blank.length });
  foreign.set(blank);
  const fs = open(foreign);
  fs.mkdir('/iframe'); fs.writeFile('/iframe/book', runInNewContext('new Uint8Array([7, 8, 9])'));
  assert.deepEqual(open(foreign.buffer).readFile('/iframe/book'), Uint8Array.of(7, 8, 9));
  const shared = new Uint8Array(new SharedArrayBuffer(blank.length)); shared.set(blank);
  open(shared).writeFile('/shared', Uint8Array.of(4));
  assert.deepEqual(open(shared).readFile('/shared'), Uint8Array.of(4));
  assert.throws(() => open(new DataView(blank.buffer)), { code: 'EINVAL' });
  assert.throws(() => open(new Uint16Array(100)), { code: 'EINVAL' });
});

test('root volume label and dot/dotdot conventions interoperate', () => {
  const f = fixture(); f.flush();
  command('mlabel', ['-i', `${f.path}@@${f.base}`, '::BOOKS']);
  f.image.set(readFileSync(f.path)); f.fs = open(f.image);
  assert.equal(f.fs.label, 'BOOKS'); assert.deepEqual(f.fs.list('/'), []);
  f.fs.mkdir('/parent'); f.fs.mkdir('/parent/child');
  const parent = f.fs.find(f.fs.root, 'parent'), child = f.fs.find(parent.cluster, 'child');
  const parentOffset = f.fs.offset(parent.cluster), childOffset = f.fs.offset(child.cluster);
  assert.equal(f.view.getUint16(parentOffset + 26, true), parent.cluster);
  assert.equal(f.view.getUint16(parentOffset + 32 + 26, true), 0);
  assert.equal(f.view.getUint16(childOffset + 32 + 26, true), parent.cluster);
  f.verify();
});

test('explorer plans name what a write replaces, refuse file/folder clashes, and count reused space', () => {
  const f = fixture();
  // sd-ops.js is a classic script: its consts are reached by evaluating in its context.
  const context = createContext({ FAT });
  runInContext(readFileSync(new URL('./sd-ops.js', import.meta.url), 'utf8'), context);
  const ops = runInContext('({sdExecute, sdMutates, sdChanged})', context);
  const run = (op, args) => ops.sdExecute(f.image, op, args);
  f.fs.mkdir('/books'); f.fs.writeFile('/books/A.EPUB', pattern(3 * f.fs.clusterSize)); f.fs.writeFile('/TEXT.BIN', pattern(10));
  f.fs.mkdir('/text.dir');
  const plan = run('plan', { dirs: ['/books', '/new'], files: [
    { path: '/books/a.epub', size: 3 * f.fs.clusterSize }, { path: '/text.bin', size: 5 }, { path: '/new/b', size: 2 * f.fs.clusterSize + 1 },
  ] });
  // Nested and case-only matches are named as the card has them.
  assert.deepEqual([...plan.replaced], ['/books/A.EPUB', '/TEXT.BIN']);
  // A same-size replacement reuses its clusters: a new folder and three new clusters.
  assert.equal(plan.needed, 4 * f.fs.clusterSize);
  assert.equal(plan.free, f.fs.freeBytes());
  assert.throws(() => run('plan', { dirs: [], files: [{ path: '/TEXT.DIR', size: 1 }] }), { code: 'EISDIR' });
  assert.throws(() => run('plan', { dirs: ['/text.bin'], files: [{ path: '/text.bin/x', size: 1 }] }), { code: 'ENOTDIR' });
  assert.equal(ops.sdMutates('plan'), false);
  // A write that fails after its first file says the card changed.
  let failure;
  try { run('write', { dirs: [], files: [{ path: '/ok.bin', bytes: pattern(4) }, { path: '/text.dir', bytes: pattern(4) }] }); } catch (error) { failure = error; }
  assert.equal(failure.code, 'EISDIR');
  assert.equal(failure.changed, true);
  assert.equal(ops.sdChanged('write', failure), true);
  assert.equal(ops.sdChanged('write', Object.assign(new Error('x'), { code: 'EISDIR' })), false);
  assert.deepEqual(open(f.image).readFile('/ok.bin'), pattern(4));
  assert.equal(f.fs.stat('/books/a.epub').name, 'A.EPUB');
  assert.equal(f.fs.stat('/missing/x'), null);
  assert.equal(f.fs.stat('/TEXT.BIN/x'), null);
  // The operations opened their own volume on the image.
  f.fs = open(f.image);
  f.verify();
});
