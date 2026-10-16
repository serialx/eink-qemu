/* Node 22+: browser acceptance test over Chrome's built-in DevTools protocol.
 * Start serve.py and headless Chrome --remote-debugging-port=9224 first.
 * node eink-qemu/wasm/web/smoke.mjs 'http://127.0.0.1:8000/?flash=flash.bin&sd=sd.img'
 * SAVE_SETTINGS=1 explicitly saves CrossPoint 1.6.5 settings on an empty card.
 * Required for X3: INPUT_SD (local source card, to compare the download).
 * Optional: CDP_PORT, CHROME_PID, EVIDENCE_DIR, TIMEOUT (seconds),
 * WEB_MACHINE=x4pro, EXPECT_SERVICE_WORKER=1, PICKER_FLASH/PICKER_SD, RELEASE_APP (local app for a ?flash= URL).
 * CPU_PROFILE=1 captures worker profiles split at Home; BENCH_ONLY=1 stops
 * after settled; REFERENCE_PPM pauses/captures at first MEM, before UI checks.
 * It reports exact equality or an explicit one-gray-level drift fallback.
 * HOME_CHECKPOINT=1 pauses and captures the third completed refresh;
 * REFERENCE_HOME_PPM enables that capture and requires exact pixels.
 * Checkpoint captures perturb wall timing; use separate runs for benchmarks.
 * REFERENCE_LOG also checks the first eight wait lines with sleep=off.
 * Add sleep=off to the page URL for deterministic timing measurements.
 * No npm packages are required. */
import fs from 'node:fs';
import {createHash} from 'node:crypto';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {execFileSync} from 'node:child_process';
async function sha256(file) {
    const hash = createHash('sha256');
    for await (const chunk of fs.createReadStream(file)) hash.update(chunk);
    return hash.digest('hex');
}
function comparePPM(actual, reference, maxLevelDelta) {
    const parse = bytes => {
        const header = /^P6\n(\d+) (\d+)\n255\n/.exec(bytes.subarray(0, 64).toString('ascii'));
        if (!header) throw new Error('Expected a QEMU P6/255 panel screendump');
        const width = Number(header[1]), height = Number(header[2]);
        const pixels = bytes.subarray(header[0].length);
        if (pixels.length !== width * height * 3) throw new Error('Incomplete panel screendump');
        return {width, height, pixels};
    };
    const a = parse(actual), b = parse(reference);
    if (a.width !== b.width || a.height !== b.height) throw new Error('Panel dimensions differ');
    let differingPixels = 0, differingBytes = 0, maxDelta = 0, grayscale = true;
    for (let i = 0; i < a.pixels.length; i += 3) {
        let changed = false;
        for (let channel = 0; channel < 3; channel++) {
            const delta = Math.abs(a.pixels[i + channel] - b.pixels[i + channel]);
            if (delta) { differingBytes++; changed = true; }
            maxDelta = Math.max(maxDelta, delta);
            grayscale &&= a.pixels[i + channel] === a.pixels[i] && b.pixels[i + channel] === b.pixels[i];
        }
        if (changed) differingPixels++;
    }
    return {differing_pixels: differingPixels, differing_bytes: differingBytes,
        max_level_delta: maxDelta, allowed_level_delta: maxLevelDelta,
        grayscale, passed: maxDelta === 0 || (grayscale && maxDelta <= maxLevelDelta)};
}
const evidence = process.env.EVIDENCE_DIR || 'build-wasm/evidence';
fs.mkdirSync(evidence, {recursive: true});
const releaseApp = process.env.RELEASE_APP ||
    (process.env.PICKER_FLASH && fs.statSync(process.env.PICKER_FLASH).size !== 0x1000000 ? process.env.PICKER_FLASH : null);
let expectedFlashHash;
if (releaseApp) {
    const output = path.resolve(evidence, 'composed-reference.bin');
    const composer = fileURLToPath(new URL('../../mkflash.py', import.meta.url));
    try {
        execFileSync(process.env.PYTHON || 'python3', [composer, path.resolve(releaseApp), output]);
        expectedFlashHash = await sha256(output);
    } finally {
        fs.rmSync(output, {force: true});
    }
}
let observerScript;
const pages = await (await fetch(`http://127.0.0.1:${process.env.CDP_PORT || 9224}/json`)).json();
const page = pages.find(page => page.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(resolve => ws.addEventListener('open', resolve, {once: true}));
let sequence = 0;
const calls = new Map(), errors = [];
const profiling = process.env.CPU_PROFILE === '1';
const workers = new Map();
let profilePhase = 'home';
let profileQueue = Promise.resolve();
async function endPhase(next) {
    const phase = profilePhase;
    profilePhase = next;
    for (const [session, target] of workers) {
        const {profile} = await call('Profiler.stop', {}, session);
        fs.writeFileSync(path.join(evidence, `${phase}-${target.targetId}.cpuprofile`), JSON.stringify(profile));
        if (next) await call('Profiler.start', {}, session);
    }
}

ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
        const callback = calls.get(message.id); calls.delete(message.id);
        if (message.error) callback.reject(message.error); else callback.resolve(message.result);
    } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params);
    else if (message.method === 'Target.attachedToTarget') {
        const {sessionId, targetInfo} = message.params;
        profileQueue = profileQueue.then(async () => {
            await call('Target.setAutoAttach', {autoAttach: true, waitForDebuggerOnStart: true, flatten: true}, sessionId);
            if (targetInfo.type === 'worker') {
                workers.set(sessionId, targetInfo);
                await call('Profiler.enable', {}, sessionId);
                await call('Profiler.setSamplingInterval', {interval: 1000}, sessionId);
                await call('Profiler.start', {}, sessionId);
            }
            await call('Runtime.runIfWaitingForDebugger', {}, sessionId);
        }).catch(error => errors.push(String(error)));
    } else if (profiling && message.method === 'Runtime.consoleAPICalled') {
        const mark = message.params.args?.[0]?.value;
        if (mark === 'smoke:home' || mark === 'smoke:settled') {
            profileQueue = profileQueue.then(() => endPhase(mark === 'smoke:home' ? 'settled' : null))
                .catch(error => errors.push(String(error)));
        }
    }
});
const call = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = ++sequence; calls.set(id, {resolve, reject});
    ws.send(JSON.stringify({id, method, params, sessionId}));
});
async function evaluate(expression) {
    const result = await call('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true});
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
}
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const deadline = Date.now() + Number(process.env.TIMEOUT || 600) * 1000;
async function waitFor(expression) {
    while (Date.now() < deadline) {
        if (errors.length) throw new Error(JSON.stringify(errors));
        try {
            const value = await evaluate(expression);
            if (value) return value;
        } catch (error) {
            // The isolation service worker reloads the first navigation.
            if (!/Execution context was destroyed|Cannot find context/.test(error.message || '')) throw error;
        }
        await delay(500);
    }
    throw new Error(`Timed out: ${expression}`);
}
async function screenshot(name) {
    const result = await call('Page.captureScreenshot', {format: 'png', captureBeyondViewport: false});
    fs.writeFileSync(path.join(evidence, name), Buffer.from(result.data, 'base64'));
}
const consoleText = 'document.querySelector("#console")?.textContent';
const canvas = 'document.querySelector("iframe").contentDocument.querySelector("canvas")';
const turboOn = 'document.querySelector("#turbo").getAttribute("aria-pressed") === "true"';
// The page's pickers are hidden file inputs the backend waits on once a
// button asks for a file: click through to the button, then set the files.
async function pickFile(clicks, input, ...files) {
    for (const selector of clicks) {
        await waitFor(`document.querySelector(${JSON.stringify(selector)}) && !document.querySelector(${JSON.stringify(selector)}).disabled`);
        await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
    }
    await waitFor(`document.querySelector(${JSON.stringify(input)})`);
    const {root} = await call('DOM.getDocument');
    const {nodeId} = await call('DOM.querySelector', {nodeId: root.nodeId, selector: input});
    await call('DOM.setFileInputFiles', {nodeId, files: files.map(file => path.resolve(file))});
}
const FIRMWARE_PICKER = [['#flash', '#setup-choose'], '#flash-file'];
const CARD_PICKER = [['#sd', '#card-menu [data-card=open]'], '#sd-file'];
const UPLOAD_PICKER = [['#sd-upload'], '#sd-files'];
async function checkDisplaySizes(width, height) {
    const results = [];
    // Fit (the default) may resample; Auto must keep whole physical pixels.
    const selectZoom = value => evaluate(`(() => { const s = document.querySelector('#zoom-level');
        s.value = ${JSON.stringify(value)}; s.dispatchEvent(new Event('change')); })()`);
    await selectZoom('auto');
    // Exercise Retina, fractional browser zoom, narrow layouts, and a viewport
    // too small for 1x (which must scroll rather than downsample the panel).
    for (const [viewport, dpr] of [[1400, 1], [1400, 2], [900, 1.5], [390, 3], [270, 1], [1400, 1.25]]) {
        await call('Emulation.setDeviceMetricsOverride', {
            width: viewport, height: 1380, deviceScaleFactor: dpr, mobile: false
        });
        await waitFor(`innerWidth === ${viewport} && devicePixelRatio === ${dpr}`);
        // Let ResizeObserver and the DPR media query settle, including SDL's
        // asynchronous response to any accidental iframe/canvas resize.
        await delay(500);
        const size = await evaluate(`(() => {
            const f = document.querySelector('iframe');
            const c = f.contentDocument.querySelector('canvas');
            const r = f.getBoundingClientRect();
            return {width: c.width, height: c.height,
                viewportWidth: f.contentWindow.innerWidth, viewportHeight: f.contentWindow.innerHeight,
                physicalWidth: r.width * devicePixelRatio, physicalHeight: r.height * devicePixelRatio,
                rendering: f.contentWindow.getComputedStyle(c).imageRendering};
        })()`);
        const scale = size.physicalWidth / width;
        if (size.width !== width || size.height !== height ||
            size.viewportWidth !== width || size.viewportHeight !== height ||
            size.rendering !== 'pixelated' || scale < 0.9999 ||
            Math.abs(scale - Math.round(scale)) > 0.0001 ||
            Math.abs(size.physicalHeight / height - scale) > 0.0001) {
            throw new Error(`Panel resampled at viewport=${viewport}, DPR=${dpr}: ${JSON.stringify(size)}`);
        }
        results.push({viewport, dpr, ...size});
    }
    await selectZoom('fit');
    return results;
}
const measurements = {host_cpu: os.cpus()[0].model, host_logical_cpus: os.cpus().length,
    host_ram_bytes: os.totalmem(), url: process.argv[2],
    browser: await call('Browser.getVersion')};
function checkPanel(ppm, referencePath, checkpoint = false) {
    const difference = comparePPM(ppm, fs.readFileSync(referencePath), checkpoint ? 0 : 1);
    difference.tolerance_fallback = difference.passed && difference.max_level_delta > 0;
    const name = checkpoint ? 'home_panel_difference' : 'panel_difference';
    measurements[name] = difference;
    fs.writeFileSync(path.join(evidence, `${name}.json`), JSON.stringify(difference, null, 2) + '\n');
    console.log(`${name}: ${JSON.stringify(difference)}`);
    if (!difference.passed) throw new Error(`Browser panel differs from ${referencePath}: ${JSON.stringify(difference)}`);
}
const homeCheckpoint = process.env.HOME_CHECKPOINT === '1' || !!process.env.REFERENCE_HOME_PPM;
const settledCheckpoint = !!process.env.REFERENCE_PPM;
let peakRSS = 0, peakCPU = 0;
const chromePID = Number(process.env.CHROME_PID);
const sampler = setInterval(() => {
    if (!chromePID) return;
    const rows = execFileSync('ps', ['-axo', 'pid=,ppid=,rss=,%cpu='], {encoding: 'utf8'})
        .trim().split('\n').map(line => line.trim().split(/\s+/).map(Number));
    const pids = new Set([chromePID]);
    for (let pass = 0; pass < 4; pass++) for (const row of rows) if (pids.has(row[1])) pids.add(row[0]);
    const owned = rows.filter(row => pids.has(row[0]));
    peakRSS = Math.max(peakRSS, owned.reduce((sum, row) => sum + row[2], 0));
    peakCPU = Math.max(peakCPU, owned.reduce((sum, row) => sum + row[3], 0));
}, 1000);
smoke: try {
    await call('Runtime.enable'); await call('Page.enable');
    if (profiling) await call('Target.setAutoAttach', {autoAttach: true, waitForDebuggerOnStart: true, flatten: true});
    const downloads = path.resolve(evidence, 'web-downloads');
    fs.mkdirSync(downloads, {recursive: true});
    fs.rmSync(path.join(downloads, 'sd.img'), {force: true});
    await call('Browser.setDownloadBehavior', {behavior: 'allow', downloadPath: downloads});
    await call('Emulation.setDeviceMetricsOverride', {width: 1400, height: 1380, deviceScaleFactor: 1, mobile: false});
    const start = Date.now();
    observerScript = (await call('Page.addScriptToEvaluateOnNewDocument', {source: String.raw`
        window.addEventListener('eink-qemu-flash-ready', ({detail}) => {
            window.smokeFlash = {composed: detail.composed, machine: detail.machine,
                status: document.querySelector('#status').textContent};
            window.smokeFlashHash = crypto.subtle.digest('SHA-256', detail.flash).then(hash =>
                Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, '0')).join(''));
        });
        window.addEventListener('message', ({origin, data}) => {
            if (origin !== location.origin || data.type !== 'log') return;
            window.smokeLogLines = (window.smokeLogLines || 0) + 1;
            if (/Wait complete:\s+(?:8179|8279|8279x4|X3)_DRF/.test(data.line)) {
                window.smokeRefreshes = (window.smokeRefreshes || 0) + 1;
                if (window.smokeRefreshes === 3) {
                    window.smokeHomeEpoch = Date.now();
                    if (${homeCheckpoint}) {
                        document.querySelector('iframe').contentWindow.command('stop\nscreendump /smoke-home.ppm panel');
                    }
                    console.debug('smoke:home');
                }
            }
            if (window.smokeHomeEpoch && !window.smokeSettledEpoch && data.line.includes('[MEM]')) {
                window.smokeSettledEpoch = Date.now();
                if (${settledCheckpoint}) {
                    window.smokeSettledLine = data.line;
                    document.querySelector('iframe').contentWindow.command('stop\nscreendump /smoke-settled.ppm panel');
                }
                console.debug('smoke:settled');
            }
        });
    `})).identifier;
    const target = new URL(process.argv[2]);
    if (!target.searchParams.has('device')) target.searchParams.set('device', process.env.WEB_MACHINE || 'x3');
    await call('Page.navigate', {url: target.href});
    // Do not count exceptions from the previous navigation.
    errors.length = 0;
    if (process.env.PICKER_FLASH) {
        await waitFor('crossOriginIsolated && window.einkQemu && document.querySelector("#flash")');
        await call('DOM.enable');
        // Choosing the firmware starts the emulator, so the SD card goes first.
        if (process.env.PICKER_SD) await pickFile(...CARD_PICKER, process.env.PICKER_SD);
        await pickFile(...FIRMWARE_PICKER, process.env.PICKER_FLASH);
        measurements.file_pickers = true;
    }
    await waitFor('crossOriginIsolated && document.querySelector("iframe")');
    if (releaseApp) {
        measurements.flash = await waitFor('window.smokeFlash');
        measurements.flash.sha256 = await evaluate('window.smokeFlashHash');
        measurements.flash.python_sha256 = expectedFlashHash;
        if (!measurements.flash.composed || !measurements.flash.status.includes('Composed 16 MiB flash')) {
            throw new Error('Release app did not report flash composition in the status text');
        }
        if (measurements.flash.sha256 !== expectedFlashHash) {
            throw new Error('Browser-composed flash differs from mkflash.py output');
        }
    }
    measurements.turbo = await evaluate(turboOn);
    if (measurements.turbo !== (new URL(process.argv[2]).searchParams.get('turbo') !== '0') ||
        !await evaluate('document.querySelector("#turbo").disabled')) {
        throw new Error('Turbo selection must follow the URL and stay fixed during a run');
    }
    const runtimeArgs = await waitFor('document.querySelector("iframe").contentWindow.Module?.arguments');
    measurements.runtime_args = runtimeArgs;
    const shift = process.env.WEB_MACHINE === 'picco' ? 2 : 0;
    const sleepOff = measurements.turbo || new URL(process.argv[2]).searchParams.get('sleep') === 'off';
    const icount = `shift=${shift},sleep=${sleepOff ? 'off' : 'on'}`;
    if (!runtimeArgs.includes(icount) ||
        runtimeArgs.includes('driver=ssi.esp32s3.gpspi,property=zero-wire-time,value=on') !== measurements.turbo) {
        throw new Error('Runtime timing options do not match the Turbo selection');
    }
    measurements.cross_origin_isolated = await evaluate('crossOriginIsolated');
    measurements.service_worker_controlled = await evaluate('!!navigator.serviceWorker.controller');
    if (process.env.EXPECT_SERVICE_WORKER === '1' && !measurements.service_worker_controlled) {
        throw new Error('Header-less hosting did not acquire a service worker');
    }
    measurements.webgl = await evaluate(`(() => {
        const gl = document.createElement('canvas').getContext('webgl');
        if (!gl) return null;
        const ext = gl.getExtension('WEBGL_debug_renderer_info');
        return {renderer: ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER)};
    })()`);
    if (!measurements.webgl) throw new Error('WebGL must be available for this acceptance test');
    if (homeCheckpoint) {
        await waitFor('window.smokeHomeEpoch');
        const size = process.env.WEB_MACHINE === 'x4pro' ? 480 * 800 * 3 + 15 : 528 * 792 * 3 + 15;
        await waitFor(`(() => { const fs = document.querySelector('iframe').contentWindow.Module.FS;
            return fs.analyzePath('/smoke-home.ppm').exists && fs.stat('/smoke-home.ppm').size === ${size}; })()`);
        const ppm = Buffer.from(await evaluate(`Array.from(document.querySelector('iframe').contentWindow.Module.FS.readFile('/smoke-home.ppm'))`));
        fs.writeFileSync(path.join(evidence, 'web-home.ppm'), ppm);
        fs.writeFileSync(path.join(evidence, 'web-home.log'), await evaluate(consoleText));
        measurements.home_checkpoint_sha256 = createHash('sha256').update(ppm).digest('hex');
        if (process.env.REFERENCE_HOME_PPM) checkPanel(ppm, process.env.REFERENCE_HOME_PPM, true);
        await evaluate(`document.querySelector('iframe').contentWindow.command('cont')`);
    }
    async function checkSettledPanel() {
        if (!settledCheckpoint) return;
        await waitFor('window.smokeSettledEpoch');
        const size = process.env.WEB_MACHINE === 'x4pro' ? 480 * 800 * 3 + 15 : 528 * 792 * 3 + 15;
        await waitFor(`(() => { const fs = document.querySelector('iframe').contentWindow.Module.FS;
            return fs.analyzePath('/smoke-settled.ppm').exists && fs.stat('/smoke-settled.ppm').size === ${size}; })()`);
        const ppm = Buffer.from(await evaluate(`Array.from(document.querySelector('iframe').contentWindow.Module.FS.readFile('/smoke-settled.ppm'))`));
        const prefix = process.env.WEB_MACHINE === 'x4pro' ? 'web-x4pro' : 'web';
        fs.writeFileSync(path.join(evidence, `${prefix}.ppm`), ppm);
        fs.writeFileSync(path.join(evidence, 'web-settled.log'), await evaluate(consoleText));
        measurements.panel_checkpoint = 'first-mem';
        measurements.panel_checkpoint_line = await evaluate('window.smokeSettledLine');
        measurements.ppm_sha256 = createHash('sha256').update(ppm).digest('hex');
        checkPanel(ppm, process.env.REFERENCE_PPM);
        await evaluate(`document.querySelector('iframe').contentWindow.command('cont')`);
    }
    if (process.env.WEB_MACHINE === 'x4pro') {
        // The runtime appears after the emulator download; from a remote host that takes seconds.
        await waitFor(`document.querySelector('iframe').contentWindow.Module?.FS?.analyzePath('/flash.bin').exists`);
        measurements.firmware = await evaluate(`(() => {
            const bytes = document.querySelector('iframe').contentWindow.Module.FS.readFile('/flash.bin');
            return [0, 0x10000].some(offset => bytes[offset] === 0xe9 &&
                bytes[offset + 12] === 9 && bytes[offset + 13] === 0);
        })()`);
        if (measurements.firmware) {
            await checkSettledPanel();
            measurements.home_milestone_s = await waitFor('window.bootTimeSeconds');
            measurements.page_load_to_home_s = ((await waitFor('window.smokeHomeEpoch')) - start) / 1000;
            measurements.settled_s = ((await waitFor('window.smokeSettledEpoch')) - start) / 1000;
            measurements.home_to_settled_s = measurements.settled_s - measurements.page_load_to_home_s;
            await screenshot('web-x4pro-home.png');
            if (!settledCheckpoint) {
                await evaluate(`document.querySelector('iframe').contentWindow.command('screendump /smoke.ppm panel')`);
                await waitFor(`document.querySelector('iframe').contentWindow.Module.FS.analyzePath('/smoke.ppm').exists`);
                const bytes = await evaluate(`Array.from(document.querySelector('iframe').contentWindow.Module.FS.readFile('/smoke.ppm'))`);
                const ppm = Buffer.from(bytes);
                fs.writeFileSync(path.join(evidence, 'web-x4pro.ppm'), ppm);
                measurements.ppm_sha256 = createHash('sha256').update(ppm).digest('hex');
            }
            const before = await evaluate(`${canvas}.toDataURL()`);
            await evaluate(`Array.from(document.querySelectorAll('#keys button')).find(b => b.textContent === 'Down').click()`);
            await waitFor(`${canvas}.toDataURL() !== ${JSON.stringify(before)}`);
            await delay(1500);
            await screenshot('web-x4pro-down.png');
            measurements.down_changed_canvas = true;
            const afterDown = await evaluate(`${canvas}.toDataURL()`);
            await evaluate(`${canvas}.focus({preventScroll:true})`);
            await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'ArrowUp', code: 'ArrowUp', windowsVirtualKeyCode: 38});
            await delay(measurements.turbo ? 40 : 150);
            await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'ArrowUp', code: 'ArrowUp', windowsVirtualKeyCode: 38});
            await waitFor(`${canvas}.toDataURL() !== ${JSON.stringify(afterDown)}`);
            await delay(1500);
            await screenshot('web-x4pro-up.png');
            measurements.keyboard_up_changed_canvas = true;
        } else {
            await waitFor(`${consoleText}?.includes('invalid header: 0x00000000')`);
            await delay(2000);
            measurements.rom_s = (Date.now() - start) / 1000;
        }
        measurements.canvas = await evaluate(`({width:${canvas}.width,height:${canvas}.height})`);
        if (measurements.canvas.width !== 480 || measurements.canvas.height !== 800) {
            throw new Error('X4 Pro canvas must retain the native 480x800 panel');
        }
        measurements.display_sizes = await checkDisplaySizes(480, 800);
        const beforeTouch = await evaluate(`${canvas}.toDataURL()`);
        // Home keeps Library selected after returning. The File Transfer and
        // Settings rows remain unselected throughout this flow on both blank
        // and populated cards. Compare them exactly: the book area can retain
        // selection ghosts even after moving selection away before the test.
        const homeRegion = `(() => {
            const c = document.createElement('canvas'); c.width = 480; c.height = 180;
            c.getContext('2d').drawImage(${canvas}, 0, -460); return c.toDataURL();
        })()`;
        const homePixels = await evaluate(homeRegion);
        fs.writeFileSync(path.join(evidence, 'web-x4pro-home-region.png'), Buffer.from(homePixels.split(',')[1], 'base64'));
        const logBeforeTouch = await evaluate(consoleText);
        const point = await evaluate(`(() => {
            const frame = document.querySelector('iframe');
            const f = frame.getBoundingClientRect();
            // Blank-card Home: Library row, in portrait panel coordinates.
            // The iframe can be scaled independently of its native viewport.
            return {x: f.x + f.width / 2, y: f.y + f.height * 420 / 800};
        })()`);
        await call('Input.dispatchMouseEvent', {type: 'mouseMoved', ...point});
        await call('Input.dispatchMouseEvent', {type: 'mousePressed', button: 'left', clickCount: 1, ...point});
        // Turbo advances guest hold timers faster than wall time.
        await delay(measurements.turbo ? 40 : 150);
        await call('Input.dispatchMouseEvent', {type: 'mouseReleased', button: 'left', clickCount: 1, ...point});
        if (measurements.firmware) {
            await waitFor(`${consoleText}?.slice(${logBeforeTouch.length}).includes('[LIB]')`);
            await waitFor(`${canvas}.toDataURL() !== ${JSON.stringify(beforeTouch)}`);
            await delay(1500);
            measurements.touch_changed_canvas = true;
            measurements.touch_opened_library = true;
        }
        measurements.pointer_delivered_without_error = true;
        await screenshot('web-x4pro.png');
        if (measurements.firmware) {
            for (const method of ['button', 'page-keyboard', 'canvas-keyboard']) {
                if (method === 'button') {
                    await evaluate(`Array.from(document.querySelectorAll('#keys button')).find(b => b.textContent === 'Home').click()`);
                } else {
                    await evaluate(method === 'canvas-keyboard' ? `${canvas}.focus({preventScroll:true})` :
                        `document.querySelector('#console').focus({preventScroll:true})`);
                    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Home', code: 'Home', windowsVirtualKeyCode: 36});
                    await delay(measurements.turbo ? 40 : 150);
                    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Home', code: 'Home', windowsVirtualKeyCode: 36});
                }
                await waitFor(`${homeRegion} === ${JSON.stringify(homePixels)}`);
                measurements[`${method.replaceAll('-', '_')}_home_returned_home`] = true;
                if (method !== 'canvas-keyboard') {
                    const beforeLibrary = await evaluate(`${canvas}.toDataURL()`);
                    await call('Input.dispatchMouseEvent', {type: 'mousePressed', button: 'left', clickCount: 1, ...point});
                    await delay(measurements.turbo ? 40 : 150);
                    await call('Input.dispatchMouseEvent', {type: 'mouseReleased', button: 'left', clickCount: 1, ...point});
                    await waitFor(`${canvas}.toDataURL() !== ${JSON.stringify(beforeLibrary)}`);
                    await delay(1500);
                    if (await evaluate(`${homeRegion} === ${JSON.stringify(homePixels)}`)) {
                        throw new Error('Touch did not reopen Library before testing Home');
                    }
                }
            }
            await screenshot('web-x4pro-home-control.png');
        }
    } else {
        await checkSettledPanel();
        measurements.home_milestone_s = await waitFor('window.bootTimeSeconds');
        measurements.page_load_to_home_s = ((await waitFor('window.smokeHomeEpoch')) - start) / 1000;
        measurements.canvas = await evaluate(`({width:${canvas}.width,height:${canvas}.height})`);
        if (measurements.canvas.width !== 528 || measurements.canvas.height !== 792) {
            throw new Error('X3 canvas must retain the native 528x792 panel');
        }
        console.log(`Home milestone: ${measurements.home_milestone_s.toFixed(3)} s`);
        // As in smoke.sh, wait for thumbnail generation to settle before input.
        await waitFor(`${consoleText}?.includes('[MEM]')`);
        measurements.display_sizes = await checkDisplaySizes(528, 792);
        measurements.settled_s = ((await waitFor('window.smokeSettledEpoch')) - start) / 1000;
        measurements.home_to_settled_s = measurements.settled_s - measurements.page_load_to_home_s;
        measurements.status = await evaluate('document.querySelector("#status").textContent');
        if (!measurements.status.includes(measurements.turbo ? 'Turbo (not timing-accurate)' : 'Accurate timing')) {
            throw new Error('Status must identify the selected timing mode');
        }
        measurements.log_lines = await evaluate('window.smokeLogLines');
        const log = await evaluate(consoleText);
        measurements.wait_lines = (log.match(/\[\d+\]\s+Wait complete:[^\r\n]+/g) || []).slice(0, 8);
        if (process.env.REFERENCE_LOG && sleepOff && !measurements.turbo) {
            const reference = (fs.readFileSync(process.env.REFERENCE_LOG, 'utf8')
                .match(/\[\d+\]\s+Wait complete:[^\r\n]+/g) || []).slice(0, 8);
            if (reference.length !== 8 || JSON.stringify(measurements.wait_lines) !== JSON.stringify(reference)) {
                throw new Error('Browser wait timestamps differ from reference');
            }
        }
        await profileQueue;
        if (errors.length) throw new Error(JSON.stringify(errors));
        if (profiling && !workers.size) throw new Error('No worker profiles captured');
        if (process.env.BENCH_ONLY === '1') {
            fs.writeFileSync(path.join(evidence, 'web.log'), await evaluate(consoleText));
            fs.writeFileSync(path.join(evidence, 'web-measurements.json'), JSON.stringify(measurements, null, 2) + '\n');
            console.log(JSON.stringify(measurements, null, 2));
            break smoke;
        }
        await screenshot('web-home.png');
        const before = await evaluate(`${canvas}.toDataURL()`);
        await evaluate(`Array.from(document.querySelectorAll('#keys button')).find(b => b.textContent === 'Down').click()`);
        await waitFor(`${canvas}.toDataURL() !== ${JSON.stringify(before)}`);
        await delay(1500);
        await screenshot('web-down.png');
        measurements.down_changed_canvas = true;
        // Exercise native keyboard events too (Up restores the previous selection).
        const afterDown = await evaluate(`${canvas}.toDataURL()`);
        await evaluate(`${canvas}.focus({preventScroll:true})`);
        await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'ArrowUp', code: 'ArrowUp', windowsVirtualKeyCode: 38});
        await delay(250);
        await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'ArrowUp', code: 'ArrowUp', windowsVirtualKeyCode: 38});
        await waitFor(`${canvas}.toDataURL() !== ${JSON.stringify(afterDown)}`);
        await delay(1500);
        await screenshot('web-keyboard-up.png');
        measurements.keyboard_up_changed_canvas = true;
        if (process.env.SAVE_SETTINGS === '1') {
            // CrossPoint 1.6.5 can boot a blank card without writing it.
            // Enter and leave Settings to explicitly persist its defaults.
            for (const name of ['Down', 'Down', 'Down', 'Confirm', 'Back']) {
                await evaluate(`Array.from(document.querySelectorAll('#keys button')).find(button => button.textContent === ${JSON.stringify(name)}).click()`);
                await delay(1500);
            }
            measurements.saved_settings = true;
        }
        await evaluate('document.querySelector("#download").click()');
        await waitFor('!document.querySelector("#download").disabled');
        while (!fs.existsSync(path.join(downloads, 'sd.img'))) {
            if (Date.now() > deadline) throw new Error('SD download did not complete');
            await delay(500);
        }
        measurements.downloaded_sd_bytes = fs.statSync(path.join(downloads, 'sd.img')).size;
        if (!process.env.INPUT_SD) throw new Error('Set INPUT_SD to check guest SD writes');
        if (fs.statSync(process.env.INPUT_SD).size !== measurements.downloaded_sd_bytes) {
            throw new Error('Downloaded SD file has the wrong size');
        }
        measurements.downloaded_sd_sha256 = await sha256(path.join(downloads, 'sd.img'));
        measurements.input_sd_sha256 = await sha256(process.env.INPUT_SD);
        if (measurements.input_sd_sha256 === measurements.downloaded_sd_sha256) {
            throw new Error('Downloaded SD did not change from the input');
        }
        measurements.sd_writes_preserved = true;
        const bootCount = await evaluate(`(${consoleText}.match(/ESP-ROM:/g)||[]).length`);
        await evaluate('document.querySelector("#reset").click()');
        await waitFor(`(${consoleText}.match(/ESP-ROM:/g)||[]).length > ${bootCount}`);
        measurements.reset_booted_rom = true;
    }
    // Stop snapshots the modified SD image and tears down all pthread workers.
    await evaluate('document.querySelector("#stop").click()');
    await waitFor('!document.querySelector("iframe")');
    if (await evaluate('document.querySelector("#turbo").disabled')) {
        throw new Error('Stop must allow changing Turbo before the next boot');
    }
    measurements.exported_sd_bytes = await evaluate('window.einkQemu.image().byteLength');
    measurements.stop_removed_runtime = true;
    measurements.sampled_peak_chrome_rss_kib = peakRSS;
    measurements.sampled_peak_chrome_cpu_percent = peakCPU;
    const prefix = process.env.WEB_MACHINE === 'x4pro' ? 'web-x4pro' : 'web';
    fs.writeFileSync(path.join(evidence, `${prefix}.log`), await evaluate(consoleText));
    fs.writeFileSync(path.join(evidence, `${prefix}-measurements.json`), JSON.stringify(measurements, null, 2) + '\n');
    if (errors.length) throw new Error(JSON.stringify(errors));
    console.log(JSON.stringify(measurements, null, 2));
    console.log('PASS: all browser smoke checks passed');
} catch (error) {
    fs.writeFileSync(path.join(evidence, 'web-failure.txt'), String(error.stack || error));
    fs.writeFileSync(path.join(evidence, 'web-failure-measurements.json'), JSON.stringify(measurements, null, 2) + '\n');
    try {
        fs.writeFileSync(path.join(evidence, 'web-failure.log'), await evaluate(consoleText) || '');
        await screenshot('web-failure.png');
    } catch { /* Keep the original failure if the target was lost. */ }
    throw error;
} finally {
    clearInterval(sampler);
    if (observerScript) await call('Page.removeScriptToEvaluateOnNewDocument', {identifier: observerScript});
    ws.close();
}
