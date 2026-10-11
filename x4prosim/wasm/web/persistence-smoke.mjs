/* Node 22+, Chrome CDP and serve.py, as for smoke.mjs. Use an isolated browser
 * profile: this test replaces its saved card. Optional PICKER_FLASH exercises
 * running explorer changes, periodic snapshots and Stop with real firmware.
 * CDP_PORT=9224 node persistence-smoke.mjs http://127.0.0.1:8000/
 */
import assert from 'node:assert/strict';
import path from 'node:path';

const url = new URL(process.argv[2]);
url.searchParams.set('device', 'x3');
assert(!url.searchParams.has('flash') && !url.searchParams.has('sd'));
const pages = await (await fetch(`http://127.0.0.1:${process.env.CDP_PORT || 9224}/json`)).json();
const ws = new WebSocket(pages.find(page => page.type === 'page').webSocketDebuggerUrl);
await new Promise(resolve => ws.addEventListener('open', resolve, {once: true}));
let sequence = 0;
const calls = new Map(), errors = [];
ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
        const call = calls.get(message.id);
        calls.delete(message.id); clearTimeout(call.timer);
        if (message.error) call.reject(new Error(JSON.stringify(message.error)));
        else call.resolve(message.result);
    } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params);
});
function call(method, params = {}) {
    return new Promise((resolve, reject) => {
        const id = ++sequence;
        const timer = setTimeout(() => reject(new Error(`Timed out: ${method}`)), 120000);
        calls.set(id, {resolve, reject, timer});
        ws.send(JSON.stringify({id, method, params}));
    });
}
async function evaluate(expression) {
    const result = await call('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true});
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
}
async function waitFor(expression) {
    const deadline = Date.now() + 120000;
    while (Date.now() < deadline) {
        assert.deepEqual(errors, []);
        try { if (await evaluate(expression)) return; }
        catch (error) {
            if (!/context.*destroyed|Cannot find context/.test(error.message)) throw error;
        }
        await new Promise(resolve => setTimeout(resolve, 100));
    }
    throw new Error(`Timed out: ${expression}`);
}
async function navigate(target = url.href) {
    await call('Page.navigate', {url: target});
    await waitFor('document.querySelector("#sd-refresh") && !document.querySelector("#sd-refresh").disabled');
}
async function read(file) {
    return evaluate(`sdCall('read', {path: ${JSON.stringify(file)}}).then(r => new TextDecoder().decode(r.bytes))`);
}
async function write(file, text) {
    await evaluate(`sdRun(() => sdCall('write', {dirs: [], files: [{path: ${JSON.stringify(file)}, bytes: new TextEncoder().encode(${JSON.stringify(text)})}]}))`);
    assert.equal(await evaluate('document.querySelector("#sd-error").textContent'), '');
    assert.equal(await evaluate('document.querySelector("#sd-save").classList.contains("error")'), false);
}
try {
    await call('Runtime.enable'); await call('Page.enable'); await call('DOM.enable');
    const blank = new URL(url);
    blank.searchParams.set('sd', 'blank-sd.img');
    await navigate(blank.href);
    await write('/refresh.txt', 'preserved across refresh');
    await navigate();
    assert.match(await evaluate('document.querySelector("#sd-save").textContent'), /restored/);
    assert.equal(await read('/refresh.txt'), 'preserved across refresh');
    await evaluate("sdRun(() => sdCall('rename', {path: '/refresh.txt', name: 'renamed.txt'}))");
    await navigate();
    assert.equal(await read('/renamed.txt'), 'preserved across refresh');
    await evaluate("sdRun(() => sdCall('remove', {path: '/renamed.txt'}))");
    await navigate();
    assert.deepEqual(await evaluate("sdCall('list', {path: '/'}).then(r => r.entries.map(e => e.name))"), []);
    console.log('PASS: file contents, rename and deletion survive reload');

    // A failed transaction preserves the last good image and keeps editing usable.
    await write('/saved.txt', 'last successful save');
    await evaluate(`(() => {
        const put = IDBObjectStore.prototype.put;
        IDBObjectStore.prototype.put = function(...args) {
            const request = put.apply(this, args);
            this.transaction.abort();
            return request;
        };
    })()`);
    await evaluate("sdRun(() => sdCall('write', {dirs: [], files: [{path: '/unsaved.txt', bytes: new Uint8Array([1,2,3])}]}))");
    assert.equal(await evaluate('document.querySelector("#sd-save").classList.contains("error")'), true);
    assert.equal(await evaluate('document.querySelector("#sd-refresh").disabled'), false);
    await navigate();
    assert.equal(await read('/saved.txt'), 'last successful save');
    assert.equal(await evaluate("sdCall('list', {path: '/'}).then(r => r.entries.some(e => e.name === 'unsaved.txt'))"), false);
    console.log('PASS: aborted save is visible and preserves the previous saved card');

    const denied = await call('Page.addScriptToEvaluateOnNewDocument', {source:
        `Object.defineProperty(window, 'indexedDB', {get() { throw new DOMException('Storage disabled', 'SecurityError'); }});`});
    await navigate();
    assert.match(await evaluate('document.querySelector("#sd-save").textContent'), /Could not restore/);
    assert.equal(await evaluate('document.querySelector("#sd-refresh").disabled'), false);
    await call('Page.removeScriptToEvaluateOnNewDocument', {identifier: denied.identifier});
    await navigate();
    assert.equal(await read('/saved.txt'), 'last successful save');
    console.log('PASS: blocked storage falls back to a usable temporary card');

    if (process.env.PICKER_FLASH) {
        const runtimeWrite = file => evaluate(`new Promise(resolve => {
            const runtime = document.querySelector('iframe').contentWindow;
            runtime.whilePaused(() => {
                runtime.sdExecute(runtime.sdBytes(), 'write', {dirs: [], files: [{
                    path: ${JSON.stringify(file)}, bytes: new TextEncoder().encode('runtime-owned bytes')
                }]});
                resolve();
                return 'cont';
            });
        })`);
        const boot = async () => {
            const {root} = await call('DOM.getDocument');
            const {nodeId} = await call('DOM.querySelector', {nodeId: root.nodeId, selector: '#flash'});
            // Choosing the firmware starts the emulator.
            await call('DOM.setFileInputFiles', {nodeId, files: [path.resolve(process.env.PICKER_FLASH)]});
            await waitFor('document.querySelector("#status").textContent.includes(" · Ready · ")');
        };
        await boot();
        await write('/running.txt', 'edited while running');
        await navigate();
        assert.equal(await read('/running.txt'), 'edited while running');
        console.log('PASS: running explorer edit survives reload without Stop');
        await boot();
        // Write only to the running card, bypassing the explorer save path.
        await runtimeWrite('/periodic.txt');
        const revision = await evaluate('sdSaving');
        await waitFor(`sdSaving > ${revision} && !sdSnapshotPending`);
        await navigate();
        assert.equal(await read('/periodic.txt'), 'runtime-owned bytes');
        await boot();
        await runtimeWrite('/stop.txt');
        await evaluate('document.querySelector("#stop").click()');
        await waitFor('!document.querySelector("iframe") && document.querySelector("#sd-save").textContent.includes("Safe to refresh")');
        await navigate();
        assert.equal(await read('/running.txt'), 'edited while running');
        assert.equal(await read('/stop.txt'), 'runtime-owned bytes');
        console.log('PASS: periodic device snapshots and Stop save across reload');
    }
    await navigate(blank.href);
    await navigate();
    assert.deepEqual(await evaluate("sdCall('list', {path: '/'}).then(r => r.entries.map(e => e.name))"), []);
    assert.deepEqual(errors, []);
    console.log('PASS: explicit image URL replaces the saved card');
} finally {
    for (const call of calls.values()) clearTimeout(call.timer);
    ws.close();
}
