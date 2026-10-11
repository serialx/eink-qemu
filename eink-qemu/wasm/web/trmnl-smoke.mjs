/* TRMNL X4 acceptance over Chrome CDP (Node 22+, no npm packages).
 * Start serve.py and an isolated headless Chrome as for smoke.mjs, then:
 * TRMNL_FIRMWARE=/path/FW1.8.17.bin node trmnl-smoke.mjs http://127.0.0.1:8000/
 * Optional CDP_PORT, EVIDENCE_DIR, REFERENCE_PPM (native X4 with offline radio).
 * Exercises the selector, firmware preservation, panel, Reset and Stop.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createHash} from 'node:crypto';
const target = new URL(process.argv[2]);
target.searchParams.set('wifi', 'off');
const base = target.href;
const firmware = process.env.TRMNL_FIRMWARE;
assert(firmware, 'Set TRMNL_FIRMWARE to the merged FW1.8.17.bin image');
const evidence = process.env.EVIDENCE_DIR || 'build-wasm/evidence/trmnl';
fs.mkdirSync(evidence, {recursive: true});
const pages = await (await fetch(`http://127.0.0.1:${process.env.CDP_PORT || 9224}/json`)).json();
const ws = new WebSocket(pages.find(p => p.type === 'page').webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, {once: true}));
let seq = 0;
const calls = new Map(), errors = [];
ws.addEventListener('message', e => {
    const m = JSON.parse(e.data);
    if (m.id) {
        const c = calls.get(m.id); calls.delete(m.id);
        if (m.error) c.reject(m.error); else c.resolve(m.result);
    } else if (m.method === 'Runtime.exceptionThrown') errors.push(m.params);
});
const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++seq; calls.set(id, {resolve, reject}); ws.send(JSON.stringify({id, method, params}));
});
async function evaluate(expression) {
    const r = await call('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true});
    if (r.exceptionDetails) throw Error(JSON.stringify(r.exceptionDetails));
    return r.result.value;
}
const delay = ms => new Promise(r => setTimeout(r, ms));
async function wait(expression) {
    for (let i = 0; i < 600; i++) {
        assert.deepEqual(errors, []);
        try { if (await evaluate(expression)) return; }
        catch (e) { if (!/context.*destroyed|Cannot find context/.test(e.message)) throw e; }
        await delay(100);
    }
    throw Error(`Timed out: ${expression}; ${await evaluate('document.querySelector("#console")?.textContent')}`);
}
async function choose(value) {
    await evaluate(`document.querySelector('#device').value = '${value}'; document.querySelector('#device').dispatchEvent(new Event('change'));`);
}
async function picker() {
    const {root} = await call('DOM.getDocument');
    const {nodeId} = await call('DOM.querySelector', {nodeId: root.nodeId, selector: '#flash'});
    await call('DOM.setFileInputFiles', {nodeId, files: [firmware]});
}
try {
    await call('Runtime.enable'); await call('Page.enable'); await call('DOM.enable');
    await call('Emulation.setDeviceMetricsOverride', {width: 1440, height: 1100, deviceScaleFactor: 1, mobile: false});
    const initial = new URL(base); initial.searchParams.set('device', 'auto');
    await call('Page.navigate', {url: initial.href});
    await wait('document.querySelector("#device") && typeof start === "function"');
    await evaluate(`window.addEventListener('eink-qemu-flash-ready', e => {
        window.flashHashPromise = crypto.subtle.digest('SHA-256', e.detail.flash.slice(0)).then(b =>
            [...new Uint8Array(b)].map(n => n.toString(16).padStart(2, '0')).join(''));
    });`);
    await picker();
    await wait('document.querySelector("#status").textContent.includes("Choose X3 or X4")');
    assert.equal(await evaluate('!!document.querySelector("iframe")'), false);
    await choose('x4pro');
    await wait('document.querySelector("#status").textContent.includes("conflicts")');
    assert.equal(await evaluate('!!document.querySelector("iframe")'), false);
    await choose('x4');
    await wait('document.querySelector("#console").textContent.includes(" + softAP")');
    await delay(2000);
    const expected = Buffer.alloc(0x1000000, 255); fs.readFileSync(firmware).copy(expected);
    assert.equal(await evaluate('flashHashPromise'), createHash('sha256').update(expected).digest('hex'));
    assert.equal(await evaluate('document.querySelector("#device").disabled'), true);
    const frame = 'document.querySelector("iframe")';
    assert.deepEqual(await evaluate(`({machine:${frame}.dataset.machine, width:${frame}.contentDocument.querySelector('canvas').width, height:${frame}.contentDocument.querySelector('canvas').height})`), {machine:'x4', width:480, height:800});
    const win = `${frame}.contentWindow`;
    await evaluate(`${win}.command('stop\\nscreendump /x4.ppm -f ppm panel')`);
    await wait(`(() => {try {return ${win}.Module.FS.stat('/x4.ppm').size === 480*800*3+15;} catch {return false;}})()`);
    const pixels = Buffer.from(await evaluate(`Array.from(${win}.Module.FS.readFile('/x4.ppm'))`));
    fs.writeFileSync(`${evidence}/x4-browser.ppm`, pixels);
    if (process.env.REFERENCE_PPM) {
        const hash = bytes => createHash('sha256').update(bytes).digest('hex');
        assert.equal(hash(pixels), hash(fs.readFileSync(process.env.REFERENCE_PPM)), 'Native panel differs');
    }
    assert(pixels.subarray(15).some(v => v < 128), 'Panel is blank');
    const shot = await call('Page.captureScreenshot', {format:'png'});
    fs.writeFileSync(`${evidence}/x4-browser.png`, Buffer.from(shot.data, 'base64'));
    await evaluate(`${win}.command('cont'); document.querySelector('#reset').click()`);
    await wait('document.querySelector("#console").textContent.split(" + softAP").length >= 3');
    assert(!await evaluate('document.querySelector("#console").textContent.includes("assert failed:")'));
    await evaluate(`document.querySelector('#stop').click()`);
    await wait('!document.querySelector("iframe") && !document.querySelector("#device").disabled');
    await call('Page.navigate', {url: base});
    await wait('document.querySelector("#device")?.value === "x4"');
    assert.deepEqual(errors, []);
    console.log('PASS: C3 ambiguity, chip mismatch, X4 boot, preserved firmware, reset, stop, remembered selection');
} finally { ws.close(); }
