/* Synchronous, dependency-free FAT32 editor. Load as a script, or import for its
 * globalThis.FAT side effect in Node. open() borrows the supplied image: callers
 * must stop the guest and must not modify it while a filesystem is open.
 * Paths are absolute, case-insensitive, and cannot contain . or .. .
 * list(path) returns {name, size, isDir, mtime} entries, folders first.
 * rename(path, newName) changes a basename; remove(path) removes a whole tree.
 * totalBytes is usable cluster capacity, freeBytes() counts unallocated bytes.
 * Mutations throw FatError with codes; allocation failures leave bytes unchanged.
 */
(() => {
  'use strict';
  const EOC = 0x0fffffff;
  const LFN_POS = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
  const CP437 = 'ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ';
  class FatError extends Error {
    constructor(code, message) { super(message); this.name = 'FatError'; this.code = code; }
  }
  function fail(code, message) { throw new FatError(code, message); }
  function bytes(value) {
    // Normalize Node Buffer too: Buffer.slice() aliases memory, unlike Uint8Array.slice().
    if (ArrayBuffer.isView(value) && Object.prototype.toString.call(value) === '[object Uint8Array]') {
      return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    // The intrinsic brand check accepts ArrayBuffers from an iframe too.
    try {
      const length = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'byteLength').get.call(value);
      return new Uint8Array(value, 0, length);
    } catch { /* Not an ArrayBuffer. */ }
    fail('EINVAL', 'Expected an ArrayBuffer or Uint8Array');
  }
  function checksum(raw) {
    let sum = 0;
    for (let i = 0; i < 11; i++) sum = (((sum & 1) << 7) + (sum >> 1) + raw[i]) & 255;
    return sum;
  }
  function validName(name) {
    if (typeof name !== 'string' || !name || name.length > 255 || /[\x00-\x1f\uffff"*\/:<>?\\|]/u.test(name) || /[ .]$/u.test(name) ||
        name === '.' || name === '..' || /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)/i.test(name)) {
      fail('EINVAL', `Invalid FAT name: ${name}`);
    }
    for (let i = 0; i < name.length; i++) {
      const c = name.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff) {
        const next = name.charCodeAt(++i);
        if (!(next >= 0xdc00 && next <= 0xdfff)) fail('EINVAL', 'Unpaired UTF-16 surrogate');
      } else if (c >= 0xdc00 && c <= 0xdfff) fail('EINVAL', 'Unpaired UTF-16 surrogate');
    }
  }
  const fold = name => name.toUpperCase();
  function parts(path = '/') {
    if (typeof path !== 'string' || !path.startsWith('/')) fail('EINVAL', 'Path must be absolute');
    const result = path.split('/').filter(Boolean);
    result.forEach(validName);
    return result;
  }
  function timestamp(raw, creation = false) {
    const v = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
    const now = new Date();
    const year = Math.min(2107, Math.max(1980, now.getFullYear()));
    const date = ((year - 1980) << 9) | ((now.getMonth() + 1) << 5) | now.getDate();
    const time = (now.getHours() << 11) | (now.getMinutes() << 5) | (now.getSeconds() >> 1);
    if (creation) {
      raw[13] = (now.getSeconds() % 2) * 100 + Math.floor(now.getMilliseconds() / 10);
      v.setUint16(14, time, true); v.setUint16(16, date, true);
    }
    v.setUint16(18, date, true); v.setUint16(22, time, true); v.setUint16(24, date, true);
  }
  function modified(raw) {
    const v = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
    const date = v.getUint16(24, true), time = v.getUint16(22, true);
    return date ? new Date(1980 + (date >> 9), ((date >> 5) & 15) - 1, date & 31,
      time >> 11, (time >> 5) & 63, (time & 31) * 2) : null;
  }
  function setCluster(raw, cluster) {
    const v = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
    v.setUint16(20, cluster >>> 16, true); v.setUint16(26, cluster & 65535, true);
  }

  class Volume {
    constructor(image) {
      this.image = bytes(image);
      this.view = new DataView(this.image.buffer, this.image.byteOffset, this.image.byteLength);
      const u16 = offset => this.view.getUint16(offset, true);
      const u32 = offset => this.view.getUint32(offset, true);
      if (this.image.length < 512 || u16(510) !== 0xaa55) fail('ENOTSUP', 'Not a FAT32 disk image');
      let base = 0, limit = this.image.length;
      // A plausible BPB distinguishes a superfloppy from an MBR. FAT type is
      // determined from cluster count and BPB fields, never the label alone.
      const plausibleBPB = [512, 1024, 2048, 4096].includes(u16(11)) && this.image[13] !== 0 && u16(14) !== 0;
      if (!plausibleBPB) {
        const candidates = [];
        for (let i = 0; i < 4; i++) {
          const p = 446 + i * 16, type = this.image[p + 4];
          if (type === 0x0b || type === 0x0c) candidates.push(p);
        }
        if (candidates.length !== 1) fail('ENOTSUP', 'Expected one primary FAT32 MBR partition');
        const p = candidates[0];
        base = u32(p + 8) * 512; limit = base + u32(p + 12) * 512;
        if (base < 512 || limit > this.image.length || limit <= base || base + 512 > limit) {
          fail('EINVAL', 'FAT32 partition exceeds the image');
        }
      }
      this.base = base;
      const b16 = n => u16(base + n), b32 = n => u32(base + n);
      const bps = b16(11), spc = this.image[base + 13];
      const reserved = b16(14), copies = this.image[base + 16];
      const sectors = b32(32), fatSectors = b32(36);
      if (b16(510) !== 0xaa55 || ![512, 1024, 2048, 4096].includes(bps) ||
          !spc || spc > 128 || (spc & (spc - 1)) || !reserved || ![1, 2].includes(copies) ||
          b16(17) || b16(19) || b16(22) || b16(42) || !fatSectors || !sectors) {
        fail('ENOTSUP', 'Unsupported or invalid FAT32 boot sector (FAT12/16 and exFAT are unsupported)');
      }
      if (b16(40) & 0x80) fail('ENOTSUP', 'FAT32 with disabled FAT mirroring is unsupported');
      const dataSector = reserved + copies * fatSectors;
      const count = Math.floor((sectors - dataSector) / spc);
      if (count < 65525 || count >= 0x0ffffff5 || (count + 2) * 4 > fatSectors * bps ||
          base + sectors * bps > limit) fail('ENOTSUP', 'Invalid FAT32 geometry or truncated image');
      this.clusterSize = bps * spc;
      this.clusterCount = count;
      this.totalBytes = count * this.clusterSize;
      this.type = 'FAT32';
      this.label = String.fromCharCode(...this.image.subarray(base + 71, base + 82)).trim();
      this.fatBase = base + reserved * bps;
      this.fatSize = fatSectors * bps;
      this.copies = copies;
      this.dataBase = base + dataSector * bps;
      this.root = b32(44);
      this.checkCluster(this.root);
      this.fsInfo = [];
      const infoSector = b16(48), backupSector = b16(50);
      if (infoSector !== 0xffff && infoSector !== 0) {
        if (infoSector >= reserved) fail('EINVAL', 'Invalid FSInfo sector');
        this.fsInfo.push(base + infoSector * bps);
        if (backupSector !== 0 && backupSector !== 0xffff && backupSector + infoSector < reserved) {
          this.fsInfo.push(base + (backupSector + infoSector) * bps);
        }
        for (const offset of this.fsInfo) {
          if (u32(offset) !== 0x41615252 || u32(offset + 484) !== 0x61417272 || u32(offset + 508) !== 0xaa550000) {
            fail('EINVAL', 'Invalid FAT32 FSInfo signature');
          }
        }
      }
      this.freeClusters = 0;
      for (let c = 0; c < count + 2; c++) {
        const value = this.fat(c);
        for (let copy = 1; copy < copies; copy++) {
          if ((u32(this.fatBase + copy * this.fatSize + c * 4) & EOC) !== value) {
            fail('EINVAL', 'FAT copies disagree; repair the disk before editing');
          }
        }
        if (c >= 2 && value === 0) this.freeClusters++;
      }
      labelScan: for (const c of this.chain(this.root)) {
        const start = this.offset(c);
        for (let p = start; p < start + this.clusterSize; p += 32) {
          if (this.image[p] === 0) break labelScan;
          if (this.image[p] !== 0xe5 && this.image[p + 11] === 8) {
            this.label = Array.from(this.image.subarray(p, p + 11),
              c => c < 128 ? String.fromCharCode(c) : CP437[c - 128]).join('').trim();
            break labelScan;
          }
        }
      }
    }
    checkCluster(c) {
      if (!Number.isInteger(c) || c < 2 || c >= this.clusterCount + 2) fail('EINVAL', 'Invalid FAT cluster');
    }
    fat(c) { return this.view.getUint32(this.fatBase + c * 4, true) & EOC; }
    putFat(c, value) {
      const old = this.fat(c);
      if (!old && value) this.freeClusters--;
      if (old && !value) this.freeClusters++;
      for (let i = 0; i < this.copies; i++) {
        const p = this.fatBase + i * this.fatSize + c * 4;
        this.view.setUint32(p, (this.view.getUint32(p, true) & 0xf0000000) | value, true);
      }
    }
    offset(c) { this.checkCluster(c); return this.dataBase + (c - 2) * this.clusterSize; }
    chain(first) {
      if (!first) return [];
      const result = [], seen = new Set();
      let c = first;
      for (;;) {
        this.checkCluster(c);
        if (seen.has(c)) fail('EINVAL', 'Cyclic FAT cluster chain');
        seen.add(c); result.push(c);
        const next = this.fat(c);
        if (next >= 0x0ffffff8) return result;
        if (next < 2 || next >= 0x0ffffff0) fail('EINVAL', 'Broken FAT cluster chain');
        c = next;
      }
    }
    syncInfo() {
      let next = 0xffffffff;
      for (let c = 2; c < this.clusterCount + 2; c++) if (!this.fat(c)) { next = c; break; }
      for (const p of this.fsInfo) {
        this.view.setUint32(p + 488, this.freeClusters, true);
        this.view.setUint32(p + 492, next, true);
      }
    }
    allocatePlan(count) {
      if (count > this.freeClusters) fail('ENOSPC', 'Not enough free space on the SD card');
      const result = [];
      for (let c = 2; result.length < count && c < this.clusterCount + 2; c++) {
        if (!this.fat(c)) result.push(c);
      }
      return result;
    }
    link(clusters) {
      for (let i = 0; i < clusters.length; i++) this.putFat(clusters[i], clusters[i + 1] || EOC);
    }
    shortName(raw) {
      const decode = data => Array.from(data, c => c < 128 ? String.fromCharCode(c) : CP437[c - 128]).join('').trimEnd();
      const copy = raw.slice(0, 11);
      if (copy[0] === 5) copy[0] = 0xe5;
      let stem = decode(copy.subarray(0, 8)), ext = decode(copy.subarray(8));
      if (raw[12] & 8) stem = stem.toLowerCase();
      if (raw[12] & 16) ext = ext.toLowerCase();
      return stem + (ext ? `.${ext}` : '');
    }
    entries(cluster) {
      const result = [];
      let pending = [], expected = 0, sum = -1;
      for (const c of this.chain(cluster)) {
        const start = this.offset(c);
        for (let p = start; p < start + this.clusterSize; p += 32) {
          const raw = this.image.subarray(p, p + 32);
          if (raw[0] === 0) return result;
          if (raw[0] === 0xe5) { pending = []; expected = 0; continue; }
          if (raw[11] === 15) {
            const ordinal = raw[0] & 31;
            if (raw[0] & 64) { pending = []; expected = ordinal; sum = raw[13]; }
            if (!ordinal || ordinal > 20 || raw[0] & 0xa0 || ordinal !== expected || raw[13] !== sum ||
                raw[12] || this.view.getUint16(p + 26, true)) { pending = []; expected = 0; continue; }
            pending.push({ p, ordinal, text: LFN_POS.map(n => this.view.getUint16(p + n, true)) });
            expected--;
            continue;
          }
          let name = this.shortName(raw), slots = [p];
          if (pending.length && expected === 0 && checksum(raw) === sum) {
            const units = pending.slice().reverse().flatMap(e => e.text);
            const end = units.indexOf(0);
            const actual = end < 0 ? units : units.slice(0, end);
            if (actual.length <= 255 && !actual.includes(0xffff)) {
              name = String.fromCharCode(...actual); slots = pending.map(e => e.p).concat(p);
            }
          }
          pending = []; expected = 0;
          if (raw[11] & 8 || name === '.' || name === '..') continue;
          result.push({ name, shortName: this.shortName(raw), isDirectory: !!(raw[11] & 16),
            attributes: raw[11], size: this.view.getUint32(p + 28, true), modified: modified(raw),
            cluster: ((this.view.getUint16(p + 20, true) << 16) | this.view.getUint16(p + 26, true)) >>> 0,
            p, slots });
        }
      }
      return result;
    }
    find(cluster, name) {
      const key = fold(name);
      return this.entries(cluster).find(e => fold(e.name) === key || fold(e.shortName) === key);
    }
    resolve(names) {
      let entry = { cluster: this.root, isDirectory: true, name: '', path: '/' };
      for (const name of names) {
        if (!entry.isDirectory) fail('ENOTDIR', 'Path component is not a folder');
        entry = this.find(entry.cluster, name);
        if (!entry) fail('ENOENT', `Path does not exist: ${name}`);
      }
      return entry;
    }
    parent(path) {
      const names = parts(path), name = names.pop();
      if (!name) fail('EINVAL', 'Cannot modify the root directory');
      const dir = this.resolve(names);
      if (!dir.isDirectory) fail('ENOTDIR', 'Parent is not a folder');
      return { dir, name, names };
    }
    list(path = '/') {
      const names = parts(path), dir = this.resolve(names);
      if (!dir.isDirectory) fail('ENOTDIR', 'Not a folder');
      return this.entries(dir.cluster)
        .map(e => ({ name: e.name, size: e.isDirectory ? 0 : e.size, isDir: e.isDirectory, mtime: e.modified }))
        .sort((a, b) => Number(b.isDir) - Number(a.isDir) ||
          (fold(a.name) < fold(b.name) ? -1 : fold(a.name) > fold(b.name) ? 1 : 0));
    }
    readFile(path) {
      const e = this.resolve(parts(path));
      if (e.isDirectory) fail('EISDIR', 'Cannot read a folder as a file');
      const chain = this.chain(e.cluster);
      if (chain.length * this.clusterSize < e.size) fail('EINVAL', 'File chain is shorter than its size');
      const result = new Uint8Array(e.size);
      let done = 0;
      for (const c of chain) {
        const length = Math.min(this.clusterSize, e.size - done);
        if (length <= 0) break;
        result.set(this.image.subarray(this.offset(c), this.offset(c) + length), done); done += length;
      }
      return result;
    }
    alias(name, entries) {
      const occupied = new Set(entries.flatMap(e => [fold(e.shortName), fold(e.name)]));
      const dot = name.lastIndexOf('.'), stem = dot > 0 ? name.slice(0, dot) : name, ext = dot > 0 ? name.slice(dot + 1) : '';
      const legal = /^[A-Z0-9!#$%&'()@^_`{}~-]+$/;
      const upperStem = fold(stem), upperExt = fold(ext);
      const pack = (s, e) => Uint8Array.from((s.padEnd(8) + e.padEnd(3)), ch => ch.charCodeAt(0));
      if (upperStem.length <= 8 && upperExt.length <= 3 && legal.test(upperStem) && (!ext || legal.test(upperExt)) && !occupied.has(fold(name))) {
        return pack(upperStem, upperExt);
      }
      const clean = s => Array.from(fold(s), c => legal.test(c) ? c : (c === ' ' || c === '.' ? '' : '_')).join('');
      const cleanStem = clean(stem) || '_', cleanExt = clean(ext).slice(0, 3);
      for (let n = 1; n <= 9999999; n++) {
        const suffix = `~${n}`, candidate = cleanStem.slice(0, 8 - suffix.length) + suffix;
        if (!occupied.has(candidate + (cleanExt ? `.${cleanExt}` : ''))) return pack(candidate, cleanExt);
      }
      fail('ENOSPC', 'No short file aliases available');
    }
    records(name, alias, template) {
      const raw = template ? template.slice() : new Uint8Array(32);
      raw.set(alias); raw[12] = 0;
      const result = [];
      if (this.shortName(raw) !== name) {
        const units = Array.from({ length: name.length }, (_, i) => name.charCodeAt(i));
        const count = Math.ceil(units.length / 13), sum = checksum(alias);
        for (let i = count; i >= 1; i--) {
          const record = new Uint8Array(32), view = new DataView(record.buffer);
          record[0] = i | (i === count ? 64 : 0); record[11] = 15; record[13] = sum;
          for (let j = 0; j < 13; j++) {
            const k = (i - 1) * 13 + j;
            view.setUint16(LFN_POS[j], k < units.length ? units[k] : k === units.length ? 0 : 0xffff, true);
          }
          result.push(record);
        }
      }
      result.push(raw);
      return result;
    }
    slotPlan(cluster, count, reusable = []) {
      const chain = this.chain(cluster), allowed = new Set(reusable);
      let run = [], ended = false;
      for (const c of chain) {
        const base = this.offset(c);
        for (let p = base; p < base + this.clusterSize; p += 32) {
          if (this.image[p] === 0) ended = true;
          if (ended || this.image[p] === 0xe5 || allowed.has(p)) run.push(p);
          else run = [];
          if (run.length === count) {
            const next = p + 32 < base + this.clusterSize ? p + 32 :
              (chain[chain.indexOf(c) + 1] ? this.offset(chain[chain.indexOf(c) + 1]) : null);
            return { chain, slots: run, extra: 0, terminator: ended ? next : null };
          }
        }
      }
      return { chain, slots: run, extra: Math.ceil((count - run.length) * 32 / this.clusterSize) };
    }
    extendSlots(plan, clusters, count) {
      if (plan.terminator != null) {
        // Entries beyond 0x00 are unused but can contain old bytes. Clear the
        // remaining tail, including fragmented clusters, before exposing it.
        const index = plan.chain.findIndex(c => plan.terminator >= this.offset(c) &&
          plan.terminator < this.offset(c) + this.clusterSize);
        for (let i = index; i < plan.chain.length; i++) {
          const base = this.offset(plan.chain[i]);
          this.image.fill(0, i === index ? plan.terminator : base, base + this.clusterSize);
        }
      }
      if (clusters.length) {
        this.link(plan.chain.concat(clusters));
        for (const c of clusters) {
          const base = this.offset(c);
          this.image.fill(0, base, base + this.clusterSize);
          for (let p = base; p < base + this.clusterSize && plan.slots.length < count; p += 32) plan.slots.push(p);
        }
      }
      return plan.slots;
    }
    putRecords(slots, records) { records.forEach((raw, i) => this.image.set(raw, slots[i])); }
    writeFile(path, data) {
      data = bytes(data);
      if (data.length > 0xffffffff) fail('EINVAL', 'FAT32 files cannot exceed 4 GiB minus 1 byte');
      const { dir, name } = this.parent(path), existing = this.find(dir.cluster, name);
      if (existing?.isDirectory) fail('EISDIR', 'Cannot overwrite a folder');
      const old = existing ? this.chain(existing.cluster) : [];
      const needed = Math.ceil(data.length / this.clusterSize);
      let records, plan;
      if (!existing) {
        records = this.records(name, this.alias(name, this.entries(dir.cluster)));
        plan = this.slotPlan(dir.cluster, records.length);
      }
      const additional = Math.max(0, needed - old.length), extra = plan?.extra || 0;
      const allocation = this.allocatePlan(additional + extra);
      // Inputs may alias this.image. Snapshot before zeroing or freeing clusters.
      if (data.buffer === this.image.buffer) data = data.slice();
      const chain = old.slice(0, needed).concat(allocation.slice(0, additional));
      this.link(chain);
      for (const c of old.slice(needed)) this.putFat(c, 0);
      let done = 0;
      for (const c of chain) {
        const base = this.offset(c), length = Math.min(this.clusterSize, data.length - done);
        this.image.set(data.subarray(done, done + length), base);
        this.image.fill(0, base + length, base + this.clusterSize); done += length;
      }
      const raw = existing ? this.image.slice(existing.p, existing.p + 32) : records[records.length - 1];
      raw[11] |= 32; setCluster(raw, chain[0] || 0); timestamp(raw, !existing);
      new DataView(raw.buffer, raw.byteOffset, 32).setUint32(28, data.length, true);
      if (existing) this.image.set(raw, existing.p);
      else this.putRecords(this.extendSlots(plan, allocation.slice(additional), records.length), records);
      this.syncInfo();
    }
    mkdir(path) {
      const { dir, name } = this.parent(path);
      if (this.find(dir.cluster, name)) fail('EEXIST', 'Path already exists');
      const records = this.records(name, this.alias(name, this.entries(dir.cluster)));
      const plan = this.slotPlan(dir.cluster, records.length), allocation = this.allocatePlan(1 + plan.extra);
      const cluster = allocation[0], base = this.offset(cluster);
      this.putFat(cluster, EOC); this.image.fill(0, base, base + this.clusterSize);
      const raw = records[records.length - 1]; raw[11] = 16; setCluster(raw, cluster); timestamp(raw, true);
      const dot = raw.slice(); dot.fill(32, 0, 11); dot[0] = 46;
      this.image.set(dot, base);
      dot[1] = 46; setCluster(dot, dir.cluster === this.root ? 0 : dir.cluster);
      this.image.set(dot, base + 32);
      this.putRecords(this.extendSlots(plan, allocation.slice(1), records.length), records);
      this.syncInfo();
    }
    // Rename takes a basename, not a destination path; moving is not exposed.
    rename(source, newName) {
      validName(newName);
      const { dir, name } = this.parent(source), entry = this.find(dir.cluster, name);
      if (!entry) fail('ENOENT', 'Source does not exist');
      const target = this.find(dir.cluster, newName);
      if (target && target.p !== entry.p) fail('EEXIST', 'Destination already exists');
      if (newName === entry.name) return;
      const records = this.records(newName, this.alias(newName, this.entries(dir.cluster).filter(e => e.p !== entry.p)),
        this.image.subarray(entry.p, entry.p + 32));
      const plan = this.slotPlan(dir.cluster, records.length, entry.slots);
      const allocation = this.allocatePlan(plan.extra);
      for (const p of entry.slots) this.image[p] = 0xe5;
      this.putRecords(this.extendSlots(plan, allocation, records.length), records);
      this.syncInfo();
    }
    remove(path) {
      const { dir, name } = this.parent(path), entry = this.find(dir.cluster, name);
      if (!entry) fail('ENOENT', 'Path does not exist');
      const clusters = [], seen = new Set(), pending = [entry];
      while (pending.length) {
        const current = pending.pop(), chain = this.chain(current.cluster);
        for (const c of chain) {
          if (seen.has(c)) fail('EINVAL', 'Cross-linked directory or file');
          seen.add(c); clusters.push(c);
        }
        if (current.isDirectory) {
          const children = this.entries(current.cluster);
          for (const child of children) pending.push(child);
        }
      }
      for (const c of clusters) this.putFat(c, 0);
      for (const p of entry.slots) this.image[p] = 0xe5;
      this.syncInfo();
    }
    exists(path) {
      try { this.resolve(parts(path)); return true; }
      catch (error) { if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return false; throw error; }
    }
    freeBytes() { return this.freeClusters * this.clusterSize; }
    free() { return this.freeBytes(); }
  }
  globalThis.FAT = Object.freeze({
    open: image => new Volume(image), FatError,
    isValidName: name => { try { validName(name); return true; } catch { return false; } },
  });
})();
