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
    case 'write':
        // Parents come before children; folders dropped on the page are recreated.
        for (const dir of args.dirs) if (!fs.exists(dir)) fs.mkdir(dir);
        for (const {path, bytes} of args.files) fs.writeFile(path, bytes);
        return {};
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
// Errors raised before any sector is written leave the card unchanged.
const sdPrecheckErrors = ['EEXIST', 'ENOENT', 'EINVAL', 'ENOTSUP', 'EISDIR', 'ENOTDIR', 'ENOTEMPTY'];
const sdMutates = op => !['list', 'read'].includes(op);
