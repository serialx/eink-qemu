/* One SD explorer operation on an in-memory FAT image. Shared by the page
 * (image owned by the page while the emulator is stopped) and the runtime
 * iframe (MEMFS /sd.img while the emulator runs). fat.js must be loaded. */
'use strict';
function sdExecute(image, op, args) {
    const fs = FAT.open(image);
    switch (op) {
    case 'list':
        return {entries: fs.list(args.path), free: fs.freeBytes(), total: fs.totalBytes, type: fs.type};
    case 'read':
        return {bytes: fs.readFile(args.path)};
    case 'plan': {
        // What a write of these folders and files would replace, refused up
        // front where a file meets a folder, and the space it needs beyond
        // the clusters it reuses.
        const clusters = size => Math.ceil(size / fs.clusterSize);
        const replaced = [];
        let needed = 0;
        for (const dir of args.dirs) {
            const entry = fs.stat(dir);
            if (!entry) needed++;
            else if (!entry.isDir) throw sdError('ENOTDIR', `${dir} is a file on the SD card, so a folder cannot be made there.`);
        }
        for (const {path, size} of args.files) {
            const entry = fs.stat(path);
            if (entry?.isDir) throw sdError('EISDIR', `${path} is a folder on the SD card; a file cannot replace it.`);
            // FAT names match case-insensitively: name the file as the card has it.
            if (entry) replaced.push(path.slice(0, path.lastIndexOf('/') + 1) + entry.name);
            needed += Math.max(0, clusters(size) - (entry ? clusters(entry.size) : 0));
        }
        return {replaced, needed: needed * fs.clusterSize, free: fs.freeBytes()};
    }
    case 'write': {
        // Parents come before children; folders dropped on the page are recreated.
        let changed = false;
        try {
            for (const dir of args.dirs) if (!fs.exists(dir)) { fs.mkdir(dir); changed = true; }
            for (const {path, bytes} of args.files) { fs.writeFile(path, bytes); changed = true; }
        } catch (error) {
            // The items before this one are on the card.
            if (changed) error.changed = true;
            throw error;
        }
        return {};
    }
    case 'mkdir':
        fs.mkdir(args.path);
        return {};
    case 'rename':
        fs.rename(args.path, args.name);
        return {};
    case 'remove':
        fs.remove(args.path);
        return {};
    default:
        throw new Error(`Unknown SD operation ${op}`);
    }
}
/* A snapshot of the card as the firmware left it: its FAT library can leave
 * the FSInfo free-cluster hint stale, which fsck reports. Correct the hint in
 * the copy that is saved or downloaded; cards that are not FAT32 stay as they are. */
function sdSyncInfo(image) {
    try { FAT.open(image).syncInfo(); } catch {}
}
function sdError(code, message) {
    return Object.assign(new Error(message), {code});
}
// Errors raised before any sector is written leave the card unchanged,
// unless the error says an earlier part of the operation changed it.
const sdPrecheckErrors = ['EEXIST', 'ENOENT', 'EINVAL', 'ENOTSUP', 'EISDIR', 'ENOTDIR', 'ENOTEMPTY'];
const sdMutates = op => !['list', 'read', 'plan'].includes(op);
const sdChanged = (op, error) => sdMutates(op) && (!!error.changed || !sdPrecheckErrors.includes(error.code));
