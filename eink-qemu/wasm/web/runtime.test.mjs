import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import {test} from 'node:test';

async function boot(machine, panel, turbo = false) {
    const listeners = {}, scripts = [];
    const window = {addEventListener: (name, fn) => { listeners[name] = fn; }};
    const parent = {location: {search: ''}, postMessage() {}};
    const context = vm.createContext({window, parent, TextDecoder, URLSearchParams,
        location: {origin: 'http://localhost'},
        document: {querySelector: () => ({}), addEventListener() {},
            createElement: () => ({}), body: {append: script => scripts.push(script.src)}}});
    vm.runInContext(readFileSync(new URL('./runtime.js', import.meta.url), 'utf8'), context);
    await listeners.message({source: parent, origin: 'http://localhost',
        data: {type: 'boot', machine, panel, turbo, wifi: false}});
    return {args: Array.from(window.Module.arguments), scripts};
}

test('panel selection reaches QEMU without changing CPU or input machine', async () => {
    for (const machine of ['x3', 'x4', 'x4pro', 'picco']) {
        for (const panel of [undefined, 'uc8179', 'uc8279-68']) {
            const {args, scripts} = await boot(machine, panel);
            const expected = machine === 'x4pro' ? `x4pro,panel=${panel || 'uc8279-68'}` : machine;
            assert.equal(args[args.indexOf('-machine') + 1], expected);
            assert.ok(args.includes(`shift=${machine === 'picco' ? 2 : 0},sleep=on`));
            assert.equal(scripts[0], `qemu-system-${['x3', 'x4'].includes(machine) ? 'riscv32' : 'xtensa'}.js`);
        }
    }
});

test('turbo overrides measured panel power and refresh waits', async () => {
    const {args} = await boot('x4pro', 'uc8279-68', true);
    assert.ok(args.includes('dwc_sdmmc.timing=off'));
    for (const name of ['pon-ms', 'pof-ms', 'busy-ms']) assert.ok(args.includes(`uc8179.${name}=1`));
});

test('new pixels on the canvas tell the page; repaints and blank frames do not', async () => {
    const sent = [], timers = [], canvas = {};
    class CanvasRenderingContext2D { putImageData() {} }
    const context = vm.createContext({window: {addEventListener() {}}, TextDecoder, URLSearchParams,
        parent: {location: {search: ''}, postMessage: message => sent.push(message.type)},
        location: {origin: 'http://localhost'}, CanvasRenderingContext2D, Uint32Array,
        setTimeout: fn => { timers.push(fn); return timers.length; },
        document: {querySelector: () => canvas, addEventListener() {}, createElement: () => ({}), body: {append() {}}}});
    vm.runInContext(readFileSync(new URL('./runtime.js', import.meta.url), 'utf8'), context);
    const ctx = Object.assign(new CanvasRenderingContext2D(), {canvas});
    const frame = (fill, ink = fill) => { const data = new Uint8ClampedArray(64).fill(fill); data.fill(ink, 0, 4); return {data}; };
    const flush = () => timers.splice(0).forEach(fn => fn());
    const screens = () => sent.filter(type => type === 'screen').length;
    ctx.putImageData(frame(255), 0, 0); flush();
    assert.equal(screens(), 0, 'the blank panel before the firmware draws');
    ctx.putImageData(frame(255, 0), 0, 0); flush();
    ctx.putImageData(frame(255, 0), 0, 0); flush();
    assert.equal(screens(), 1);
    // Another canvas is not the panel.
    CanvasRenderingContext2D.prototype.putImageData.call({canvas: {}}, frame(255, 7), 0, 0); flush();
    ctx.putImageData(frame(0), 0, 0); flush();
    ctx.putImageData(frame(255, 9), 0, 0); flush();
    assert.equal(screens(), 2);
});
