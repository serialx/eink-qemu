/* Node 22+, raw CDP, no npm dependencies. Start serve.py and Chrome as for smoke.mjs.
 * PICKER_FLASH must be the X3 release app (tested with 1.6.5), EXPLORER_EPUB the
 * real demian epub, and EXPLORER_SMALL_EPUB a second, smaller epub. Sources are
 * copied into EVIDENCE_DIR; the test always starts with the packaged blank card.
 * CDP_PORT, EVIDENCE_DIR and TIMEOUT (seconds, default 180) follow smoke.mjs.
 * node explorer-smoke.mjs http://127.0.0.1:8000/  (no flash/sd query)
 * Requires mcopy, mdir and fsck.fat on PATH. The page defaults to turbo; add
 * ?turbo=0 for accurate-timing acceptance.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';

const url = new URL(process.argv[2]);
url.searchParams.set('device', 'x3');
assert(!url.searchParams.has('flash') && !url.searchParams.has('sd'), 'Use the picker and default blank card, without ?flash= or ?sd=');
assert(!url.searchParams.has('sleep'), 'Accurate acceptance requires default sleep pacing');
const evidence = path.resolve(process.env.EVIDENCE_DIR || 'build-wasm/evidence', 'explorer');
const fixtures = path.join(evidence, 'fixtures');
const downloads = path.join(evidence, 'downloads');
fs.mkdirSync(fixtures, {recursive: true});
fs.mkdirSync(downloads, {recursive: true});
function fixture(variable) {
    assert(process.env[variable], `Set ${variable}`);
    const source = path.resolve(process.env[variable]);
    const dest = path.join(fixtures, path.basename(source).normalize('NFC'));
    if (source !== dest) fs.copyFileSync(source, dest);
    return dest;
}
const flash = fixture('PICKER_FLASH');
const demian = fixture('EXPLORER_EPUB');
const small = fixture('EXPLORER_SMALL_EPUB');
const app = fs.readFileSync(flash);
assert(app.length < 0x640000 && app[0] === 0xe9 && app.readUInt16LE(12) === 5, 'PICKER_FLASH must be an ESP32-C3 release app');
const demianName = path.basename(demian), smallName = path.basename(small);
const renamed = 'demian-renamed.epub', folder = 'Acceptance books';
assert.equal(new Set([demianName, smallName, renamed, folder]).size, 4, 'Fixture names must be distinct');
// A stable ASCII name makes the added firmware row legible without SD fonts,
// and sorts after demian so the first two rows must remain pixel-identical.
const runningName = 'running-upload.epub';
const runningFile = path.join(fixtures, runningName);
assert(![small, demian, flash].includes(runningFile), 'Fixture conflicts with running-upload.epub');
fs.copyFileSync(small, runningFile);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const measurements = {url: url.href, turbo: url.searchParams.get('turbo') !== '0', states: {}, boots: {}, dialogs: []};
const started = Date.now(), deadline = started + Number(process.env.TIMEOUT || 180) * 1000;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const pages = await (await fetch(`http://127.0.0.1:${process.env.CDP_PORT || 9224}/json`)).json();
const page = pages.find(page => page.type === 'page');
assert(page, 'Chrome must have an open page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, {once: true});
    ws.addEventListener('error', reject, {once: true});
});
let sequence = 0, expectedDialog = null, observerScript;
let pageLoaded = false;
const observedErrors = [];
const calls = new Map(), errors = [];
function call(method, params = {}) {
    return new Promise((resolve, reject) => {
        const id = ++sequence;
        const timer = setTimeout(() => { calls.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, Math.max(1, deadline - Date.now()));
        calls.set(id, {resolve, reject, timer});
        ws.send(JSON.stringify({id, method, params}));
    });
}
ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
        const pending = calls.get(message.id);
        if (!pending) return;
        clearTimeout(pending.timer); calls.delete(message.id);
        if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
        else pending.resolve(message.result);
    } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params);
    else if (message.method === 'Page.javascriptDialogOpening') {
        const dialog = expectedDialog;
        expectedDialog = null;
        measurements.dialogs.push(message.params);
        if (!dialog || dialog.type !== message.params.type || !message.params.message.includes(dialog.message)) {
            errors.push({unexpectedDialog: message.params});
            call('Page.handleJavaScriptDialog', {accept: false}).catch(error => errors.push(String(error)));
        } else {
            call('Page.handleJavaScriptDialog', {accept: true, ...(dialog.text === undefined ? {} : {promptText: dialog.text})})
                .catch(error => errors.push(String(error)));
        }
    }
});
async function evaluate(expression) {
    const result = await call('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true});
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
}
async function waitFor(expression) {
    while (Date.now() < deadline) {
        assert.equal(errors.length, 0, JSON.stringify(errors));
        try {
            const value = await evaluate(expression);
            if (value) return value;
        } catch (error) {
            if (!/Execution context was destroyed|Cannot find context/.test(error.message)) throw error;
        }
        await delay(100);
    }
    throw new Error(`Timed out: ${expression}`);
}
const consoleText = 'document.querySelector("#console")?.textContent';
const frame = 'document.querySelector("iframe").contentWindow';
const rows = '[...document.querySelectorAll("#sd-rows tr[data-name]")].map(row => ({name: row.dataset.name, directory: !!row.querySelector("[data-action=open]")}))';
async function click(selector) {
    await waitFor(`document.querySelector(${JSON.stringify(selector)}) && !document.querySelector(${JSON.stringify(selector)}).disabled`);
    await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
}
function rowButton(name, action) {
    return `#sd-rows tr[data-name=${JSON.stringify(name)}] button[data-action=${action}]`;
}
async function dialogClick(selector, type, message, text) {
    assert.equal(expectedDialog, null);
    expectedDialog = {type, message, text};
    await click(selector);
    assert.equal(expectedDialog, null, 'Real button must open the expected JavaScript dialog');
}
async function picker(selector, files) {
    const {root} = await call('DOM.getDocument');
    const {nodeId} = await call('DOM.querySelector', {nodeId: root.nodeId, selector});
    assert(nodeId, `Missing ${selector}`);
    await call('DOM.setFileInputFiles', {nodeId, files});
}
async function listing(label, names, collision = false) {
    await waitFor('!document.querySelector("#sd-refresh").disabled');
    const error = await evaluate('document.querySelector("#sd-error").textContent');
    if (collision) assert.match(error, /exist/i, 'Rename collision must be visible');
    else assert.equal(error, '', `${label}: unexpected SD error`);
    const actual = await evaluate(rows);
    // The firmware creates its own hidden cache directory after boot.
    assert.deepEqual(actual.filter(row => row.name !== '.crosspoint').map(row => row.name).sort(), [...names].sort(), label);
    if (names.includes(folder)) assert(actual.find(row => row.name === folder)?.directory, 'New folder must render as a directory');
    measurements.states[label] = {rows: actual, error};
    console.log(`${label}: ${names.join(', ') || 'Empty folder'}`);
}
async function screenshot(name, selector) {
    const clip = await evaluate(`(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return {x:r.x + scrollX, y:r.y + scrollY, width:r.width, height:r.height, scale:1}; })()`);
    const shot = await call('Page.captureScreenshot', {format: 'png', clip, captureBeyondViewport: true});
    fs.writeFileSync(path.join(evidence, `${name}.png`), Buffer.from(shot.data, 'base64'));
}
async function panel(name) {
    // HMP reads the panel framebuffer directly; WebGL canvas snapshots can lag.
    await evaluate(`${frame}.command(${JSON.stringify(`screendump /${name}.ppm panel`)})`);
    await waitFor(`${frame}.Module.FS.analyzePath('/${name}.ppm').exists`);
    const base64 = await evaluate(`(() => { const b = ${frame}.Module.FS.readFile('/${name}.ppm'); let s = ''; for (let i = 0; i < b.length; i += 8192) s += String.fromCharCode(...b.subarray(i, i + 8192)); return btoa(s); })()`);
    const ppm = Buffer.from(base64, 'base64');
    fs.writeFileSync(path.join(evidence, `${name}.ppm`), ppm);
    const header = /^P6\s+(\d+)\s+(\d+)\s+255\s/.exec(ppm.toString('ascii', 0, 64));
    assert(header && Number(header[1]) === 528 && Number(header[2]) === 792, 'Expected native X3 panel');
    await screenshot(name, 'iframe');
    return ppm.subarray(header[0].length);
}
async function boot(label, action) {
    const start = Date.now();
    await action();
    await waitFor('window.bootTimeSeconds && document.querySelector("#status").textContent.includes(" · Ready · ")');
    assert.match(await evaluate(consoleText), /\[MEM\]/);
    const args = await evaluate(`${frame}.Module.arguments`);
    assert(args.includes(`shift=0,sleep=${measurements.turbo ? 'off' : 'on'}`), 'Runtime must use requested timing');
    measurements.boots[label] = {home_s: await evaluate('window.bootTimeSeconds'), settled_s: (Date.now() - start) / 1000};
    fs.writeFileSync(path.join(evidence, `${label}.log`), await evaluate(consoleText));
    console.log(`${label}: ${JSON.stringify(measurements.boots[label])}`);
}
async function openBrowser(name) {
    const before = await evaluate('window.explorerRefreshes || 0');
    const offset = (await evaluate(consoleText)).length;
    // Confirm is the actual on-screen Enter button. Its click sends a key pair.
    await evaluate('[...document.querySelectorAll("#keys button")].find(b => b.textContent === "Confirm").click()');
    await waitFor(`window.explorerRefreshes > ${before}`);
    await delay(400);
    const log = (await evaluate(consoleText)).slice(offset);
    fs.writeFileSync(path.join(evidence, `${name}.log`), log);
    measurements[`${name}_log`] = log;
    return panel(name);
}
function changedPixels(a, b, y0, y1) {
    let changed = 0;
    for (let i = y0 * 528 * 3; i < y1 * 528 * 3; i += 3) {
        if (!a.subarray(i, i + 3).equals(b.subarray(i, i + 3))) changed++;
    }
    return changed;
}
async function stop() {
    await click('#stop');
    await waitFor('!document.querySelector("iframe") && !document.querySelector("#start").disabled');
}
async function freshPage() {
    // The page observer is recreated on navigation. Keep each page's error
    // history in the driver so later reloads cannot erase the rename collision
    // (or hide an unexpected SD error) before the final assertion.
    if (pageLoaded) observedErrors.push(...await evaluate('window.explorerErrors || []'));
    await call('Page.navigate', {url: url.href});
    errors.length = 0;
    await waitFor('crossOriginIsolated && document.querySelector("#sd-refresh") && !document.querySelector("#sd-refresh").disabled');
    assert.equal(await evaluate('!!document.querySelector("iframe")'), false, 'Picker page must not auto-start');
    assert.equal(await evaluate('document.querySelector("#turbo").checked'), measurements.turbo);
    pageLoaded = true;
}
try {
    await call('Runtime.enable'); await call('Page.enable'); await call('DOM.enable');
    await call('Browser.setDownloadBehavior', {behavior: 'allow', downloadPath: downloads});
    await call('Emulation.setDeviceMetricsOverride', {width: 1500, height: 1400, deviceScaleFactor: 1, mobile: false});
    observerScript = (await call('Page.addScriptToEvaluateOnNewDocument', {source: `
        window.explorerStatuses = []; window.explorerErrors = [];
        window.addEventListener('DOMContentLoaded', () => {
            if (window !== top) return;
            for (const [id, history] of [['status', explorerStatuses], ['sd-error', explorerErrors]]) {
                new MutationObserver(() => { const value = document.getElementById(id).textContent; if (value) history.push(value); })
                    .observe(document.getElementById(id), {childList: true, subtree: true, characterData: true});
            }
        });
        window.addEventListener('message', ({origin, source, data}) => {
            if (origin !== location.origin || source !== document.querySelector('iframe')?.contentWindow || data.type !== 'log') return;
            if (/Wait complete:\\s+(?:8279|X3)_DRF/.test(data.line)) window.explorerRefreshes = (window.explorerRefreshes || 0) + 1;
        });
    `})).identifier;
    await freshPage();
    await picker('#sd', [path.resolve(process.env.INPUT_SD || 'build-wasm-32limit/web-dist/blank-sd.img')]);
    await listing('baseline-blank', []);
    // Choosing the firmware starts the emulator.
    await boot('baseline', () => picker('#flash', [flash]));
    const blank = await openBrowser('device-blank-browser');
    await stop();

    // Explicitly replace the saved card to discard the reference run's cache.
    await freshPage();
    await picker('#sd', [path.resolve(process.env.INPUT_SD || 'build-wasm-32limit/web-dist/blank-sd.img')]);
    await listing('state1-empty', []);
    assert.equal(await evaluate('document.querySelector("#sd-rows .empty").textContent'), 'Empty folder');
    await picker('#sd-files', [small, demian]);
    await listing('state1-uploaded', [smallName, demianName]);
    await dialogClick('#sd-mkdir', 'prompt', 'New folder name', folder);
    await listing('state1-folder', [smallName, demianName, folder]);
    await click(rowButton(folder, 'open'));
    await listing('state1-inside-folder', []);
    assert.equal(await evaluate('document.querySelector("#sd-path [aria-current]").textContent'), folder);
    await click('#sd-path button[data-path="/"]');
    await dialogClick(rowButton(demianName, 'rename'), 'prompt', `Rename "${demianName}"`, renamed);
    await listing('state1-renamed', [smallName, renamed, folder]);
    await dialogClick(rowButton(smallName, 'rename'), 'prompt', `Rename "${smallName}"`, renamed);
    await listing('state1-collision', [smallName, renamed, folder], true);
    await screenshot('explorer-collision', '.sd');
    await click('#sd-refresh');
    await listing('state1-collision-cleared', [smallName, renamed, folder]);
    await dialogClick(rowButton(smallName, 'remove'), 'confirm', `Delete "${smallName}"`);
    await listing('state1-final', [renamed, folder]);
    await screenshot('explorer-state1', '.sd');
    await freshPage();
    await listing('state1-restored', [renamed, folder]);

    await boot('state2', () => picker('#flash', [flash]));
    await click('#sd-refresh');
    await listing('state2', [renamed, folder]);
    const populated = await openBrowser('device-populated-browser');
    assert.equal(changedPixels(blank, populated, 35, 90), 0, 'The SD card title must match the blank file browser');
    measurements.blank_to_populated_pixels = changedPixels(blank, populated, 90, 700);
    assert(measurements.blank_to_populated_pixels > 500, 'Firmware file browser must differ from blank card in its content area');
    // Return Home through the real Back button before mutating the running card.
    const beforeBack = await evaluate('window.explorerRefreshes');
    await evaluate('[...document.querySelectorAll("#keys button")].find(b => b.textContent === "Back").click()');
    await waitFor(`window.explorerRefreshes > ${beforeBack}`);
    await evaluate('window.explorerStatuses.length = 0');
    await boot('running-upload', async () => {
        await picker('#sd-files', [runningFile]);
        await waitFor('window.explorerStatuses.includes("SD card changed · Resetting…")');
    });
    measurements.running_upload_statuses = await evaluate('window.explorerStatuses');
    await click('#sd-refresh');
    await listing('state2-uploaded', [renamed, runningName, folder]);
    const updated = await openBrowser('device-updated-browser');
    measurements.upload_changed_browser_pixels = changedPixels(populated, updated, 90, 700);
    assert.equal(changedPixels(populated, updated, 35, 185), 0, 'SD card title, folder and first epub must remain unchanged');
    measurements.upload_new_row_pixels = changedPixels(populated, updated, 185, 240);
    assert(measurements.upload_new_row_pixels > 500, 'Running upload must add a visible third row in the firmware file browser');
    await screenshot('explorer-state2', '.sd');

    const downloaded = path.join(downloads, 'sd.img');
    fs.rmSync(downloaded, {force: true});
    await click('#download');
    while (!fs.existsSync(downloaded) || fs.existsSync(`${downloaded}.crdownload`)) {
        assert(Date.now() < deadline, 'SD image download timed out'); await delay(100);
    }
    await waitFor('!document.querySelector("#download").disabled');
    const image = fs.readFileSync(downloaded);
    assert.equal(image.length, 64 * 1024 * 1024);
    const volume = `${downloaded}@@1048576`;
    const output = [];
    const command = (name, args) => {
        const result = execFileSync(name, args, {encoding: 'utf8', timeout: 15000});
        output.push(`$ ${name} ${args.join(' ')}\n${result}`);
        fs.writeFileSync(path.join(evidence, 'mtools-fsck.txt'), output.join('\n'));
        return result;
    };
    command('mdir', ['-i', volume, '::/']);
    measurements.files = [];
    for (const [name, source] of [[renamed, demian], [runningName, small]]) {
        const extracted = path.join(evidence, `extracted-${name}`);
        command('mcopy', ['-o', '-i', volume, `::/${name}`, extracted]);
        const bytes = fs.readFileSync(extracted), original = fs.readFileSync(source);
        assert(bytes.equals(original), `${name}: downloaded SD contents must equal the source byte for byte`);
        measurements.files.push({name, bytes: bytes.length, sha256: hash(bytes), source_sha256: hash(original)});
    }
    // fsck.fat takes a partition, unlike mtools' image@@offset syntax.
    const partition = path.join(evidence, 'sd-partition.img');
    assert.equal(image.readUInt32LE(454) * 512, 1048576, 'Expected packaged MBR partition offset');
    fs.writeFileSync(partition, image.subarray(1048576));
    measurements.fsck = command('fsck.fat', ['-n', partition]).trim();
    measurements.downloaded_sd_sha256 = hash(image);

    await stop();
    await listing('state3', [renamed, runningName, folder]);
    await dialogClick(rowButton(runningName, 'remove'), 'confirm', `Delete "${runningName}"`);
    await listing('state3-deleted', [renamed, folder]);
    await screenshot('explorer-state3', '.sd');
    await freshPage();
    await listing('state3-restored', [renamed, folder]);
    await boot('restarted', () => picker('#flash', [flash]));
    await click('#sd-refresh');
    await listing('restarted', [renamed, folder]);
    await screenshot('explorer-restarted', '.sd');
    await screenshot('device-restarted-home', 'iframe');
    observedErrors.push(...await evaluate('window.explorerErrors'));
    measurements.observed_errors = observedErrors;
    assert.equal(observedErrors.length, 1, `Only the intentional collision may set #sd-error: ${observedErrors}`);
    assert.match(observedErrors[0], /exist/i);
    await stop();
    assert.equal(errors.length, 0, JSON.stringify(errors));
    measurements.total_s = (Date.now() - started) / 1000;
    fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify(measurements, null, 2) + '\n');
    console.log(JSON.stringify(measurements, null, 2));
    console.log(`PASS: explorer browser acceptance (${measurements.total_s.toFixed(2)} s); evidence: ${evidence}`);
} catch (error) {
    fs.writeFileSync(path.join(evidence, 'failure.txt'), error.stack || String(error));
    fs.writeFileSync(path.join(evidence, 'measurements.json'), JSON.stringify(measurements, null, 2) + '\n');
    try {
        fs.writeFileSync(path.join(evidence, 'failure.log'), await evaluate(consoleText) || '');
        await screenshot('failure', 'body');
    } catch { /* Keep the original error if the page is gone. */ }
    throw error;
} finally {
    if (observerScript && Date.now() < deadline) await call('Page.removeScriptToEvaluateOnNewDocument', {identifier: observerScript});
    for (const pending of calls.values()) clearTimeout(pending.timer);
    ws.close();
}
