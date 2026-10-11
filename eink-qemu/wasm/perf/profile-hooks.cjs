// Loaded in every Node pthread worker; synchronous constructor costs only.
const fs = require('node:fs');
const path = require('node:path');
const {threadId} = require('node:worker_threads');
const file = path.join(process.env.TURBO_PROFILE_DIR, `modules-${threadId}.jsonl`);
const write = row => fs.appendFileSync(file, JSON.stringify({wall_us: (performance.timeOrigin + performance.now()) * 1000, ...row}) + '\n');
const modules = new WeakSet();
let compileMs = 0, instanceMs = 0, count = 0;
const OriginalModule = WebAssembly.Module;
write({event: 'start', threadId});
WebAssembly.Module = new Proxy(OriginalModule, {
    construct(target, args) {
        const start = performance.now();
        const module = Reflect.construct(target, args);
        if (OriginalModule.exports(module).some(e => e.name === 'start')) {
            modules.add(module);
            compileMs += performance.now() - start;
        }
        return module;
    }
});
WebAssembly.Instance = new Proxy(WebAssembly.Instance, {
    construct(target, args) {
        const start = performance.now();
        const instance = Reflect.construct(target, args);
        if (modules.has(args[0])) {
            instanceMs += performance.now() - start;
            count++;
            write({event: 'module', threadId, count, compileMs, instanceMs});
        }
        return instance;
    }
});
