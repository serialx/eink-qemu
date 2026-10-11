/* lwIP and TLS stay in this page; fetch supplies HTTP response bodies.
 * SPDX-License-Identifier: GPL-2.0-or-later */
import createStack from './browser-net.mjs';
const encoder = new TextEncoder();
const decoder = new TextDecoder('latin1');
const gateway = [10, 0, 2, 2];

export function replyDatagram(service, request, now = Date.now()) {
    const view = new DataView(request.buffer, request.byteOffset, request.byteLength);
    if (service === 0) {
        if (request.length < 240 || request[0] !== 1 || request[1] !== 1 || request[2] !== 6 ||
            view.getUint32(236) !== 0x63825363) return null;
        let type;
        for (let i = 240; i < request.length;) {
            const tag = request[i++];
            if (tag === 255) break;
            if (tag === 0) continue;
            const length = request[i++];
            if (length === undefined || i + length > request.length) return null;
            if (tag === 53 && length === 1) type = request[i];
            i += length;
        }
        if (type !== 1 && type !== 3) return null;
        const reply = new Uint8Array(300);
        reply.set(request.subarray(0, 44));
        reply[0] = 2;
        reply.set([0, 0, 0, 0], 12); // ciaddr
        reply.set([10, 0, 2, 15], 16);
        reply.set(gateway, 20);
        reply.set([99, 130, 83, 99, 53, 1, type === 1 ? 2 : 5,
            54, 4, ...gateway, 1, 4, 255, 255, 255, 0,
            3, 4, ...gateway, 6, 4, ...gateway, 51, 4, 0, 1, 81, 128, 255], 236);
        return reply;
    }
    if (service === 1) {
        if (request.length < 17 || request[2] & 0x80 || view.getUint16(4) !== 1) return null;
        let end = 12;
        while (end < request.length && request[end]) {
            const length = request[end];
            if (length > 63 || end + length + 1 >= request.length) return null;
            end += length + 1;
        }
        end += 5;
        if (end > request.length) return null;
        const isA = view.getUint16(end - 4) === 1 && view.getUint16(end - 2) === 1;
        const reply = new Uint8Array(end + (isA ? 16 : 0));
        reply.set(request.subarray(0, end));
        reply[2] = 0x81; reply[3] = 0x80;
        reply.fill(0, 6, 12); reply[7] = isA ? 1 : 0;
        if (isA) reply.set([0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, ...gateway], end);
        return reply;
    }
    if (service === 2) {
        if (request.length < 48 || (request[0] & 7) !== 3) return null;
        const reply = new Uint8Array(48);
        reply.set([0x24, 1, 6, 0xec]);
        reply.set([76, 79, 67, 76], 12); // LOCL: browser clock, not an upstream NTP server.
        reply.set(request.subarray(40, 48), 24);
        const v = new DataView(reply.buffer);
        for (const offset of [16, 32, 40]) {
            v.setUint32(offset, Math.floor(now / 1000) + 2208988800);
            v.setUint32(offset + 4, (now % 1000) / 1000 * 4294967296);
        }
        return reply;
    }
    return null;
}

export function parseRequest(header, tls) {
    const [line, ...lines] = header.split('\r\n');
    const match = /^(GET|HEAD) (\/\S*) HTTP\/1\.[01]$/.exec(line);
    if (!match) throw new Error('Only HTTP GET and HEAD downloads are supported');
    const headers = new Headers();
    for (const line of lines) {
        if (!line) continue;
        const colon = line.indexOf(':');
        if (colon < 1) throw new Error('Malformed HTTP header');
        const name = line.slice(0, colon).toLowerCase();
        if (headers.has(name)) throw new Error('Duplicate HTTP header');
        headers.set(name, line.slice(colon + 1).trim());
    }
    const host = headers.get('host');
    if (!host || /[\s/@#?\\]/.test(host) || headers.has('transfer-encoding') ||
        (headers.has('content-length') && headers.get('content-length') !== '0')) {
        throw new Error('Invalid download request');
    }
    const url = new URL(`${tls ? 'https' : 'http'}://${host}${match[2]}`);
    if (url.username || url.password || url.hash) throw new Error('Invalid download URL');
    return {url, method: match[1], headers};
}

export async function createBrowserNetwork({assets = new URL('./wifi-assets/', import.meta.url),
    fetchResource = fetch, log = console.info, stackFactory = createStack} = {}) {
    const sessions = new Map(), frames = [];
    let bytes = 0, closed = false, stack, timer;
    const assetBase = new URL(assets);
    const indexResponse = await fetchResource(new URL('index.json', assetBase));
    if (!indexResponse.ok) throw new Error('Font assets are missing. Run prepare-wifi-assets.py before packaging.');
    const routes = (await indexResponse.json()).urls;
    if (!routes || typeof routes !== 'object') throw new Error('Invalid Wi-Fi asset index');
    function shutdownSession(id) {
        const session = sessions.get(id);
        if (!session) return;
        sessions.delete(id);
        session.abort.abort();
        session.wake?.();
    }
    stack = await stackFactory({
        printErr: line => log(String(line)),
        onFrame(frame) {
            if (!closed && bytes + frame.length <= 1024 * 1024) {
                frames.push(frame); bytes += frame.length;
            }
        },
        onOpen(id, tls) { sessions.set(id, {tls, header: '', started: false, abort: new AbortController()}); },
        onClose: shutdownSession,
        onData(id, data) {
            const session = sessions.get(id);
            if (!session || session.started) return;
            session.header += decoder.decode(data);
            const end = session.header.indexOf('\r\n\r\n');
            if (session.header.length > 16384 || end >= 0) {
                session.started = true;
                // Never reenter the stack from its receive callback.
                queueMicrotask(() => serve(id, session, end));
            }
        },
        onDatagram(service, request) { stack.udpReply = replyDatagram(service, request); }
    });
    const scratch = stack._malloc(4096);
    if (!scratch || stack._net_init() !== 0) throw new Error('Could not initialize browser Wi-Fi');
    function pump() {
        if (closed) return;
        stack._net_tick();
        for (const session of sessions.values()) { session.wake?.(); session.wake = null; }
    }
    // ACKs and application writes drive progress; the timer only handles idle
    // timeouts/retransmissions. Defer input work until the stack has returned.
    let pumpQueued = false;
    function schedulePump() {
        if (pumpQueued || closed) return;
        pumpQueued = true;
        queueMicrotask(() => { pumpQueued = false; pump(); });
    }
    timer = setInterval(pump, 10);
    async function write(id, session, data) {
        for (let offset = 0; offset < data.length;) {
            if (!sessions.has(id) || closed) throw new Error('Connection closed');
            const part = data.subarray(offset, offset + 4096);
            stack.HEAPU8.set(part, scratch);
            const written = stack._net_write(id, scratch, part.length);
            if (written < 0) throw new Error('Connection closed');
            if (written) {
                offset += written;
                // Flush now so another block can fill available TCP capacity.
                // A full send buffer keeps the pending block and suspends us
                // until an ACK (or the maintenance timer) pumps it again.
                pump();
            } else await new Promise(resolve => { session.wake = resolve; });
        }
    }
    async function serve(id, session, end) {
        let sentHeaders = false;
        try {
            if (end < 0 || end > 16384) throw new Error('HTTP headers exceed 16 KiB');
            const request = parseRequest(session.header.slice(0, end), session.tls);
            session.header = '';
            // Static mirrors also cover firmware that downgrades redirects to HTTP.
            const key = new URL(request.url); key.protocol = 'https:';
            const route = Object.hasOwn(routes, key.href) ? routes[key.href] : null;
            let target = request.url;
            if (route) {
                target = new URL(route, assetBase);
                if (target.origin !== assetBase.origin || !target.pathname.startsWith(assetBase.pathname)) {
                    throw new Error('Asset route escapes the static asset directory');
                }
            }
            const headers = new Headers();
            for (const name of ['range', 'accept']) {
                if (request.headers.has(name)) headers.set(name, request.headers.get(name));
            }
            const response = await fetchResource(target, {method: request.method, headers,
                signal: session.abort.signal, credentials: 'omit', redirect: 'follow'});
            log(`Browser Wi-Fi: ${request.method} ${request.url.hostname}${request.url.pathname} → ${response.status}${route ? ' (static asset)' : ''}`);
            const statusText = response.statusText.replace(/[\r\n]/g, '');
            let head = `HTTP/1.1 ${response.status} ${statusText}\r\nConnection: close\r\n`;
            for (const name of ['content-type', 'content-range']) {
                const value = response.headers.get(name);
                if (value) head += `${name}: ${value.replace(/[\r\n]/g, '')}\r\n`;
            }
            const noBody = request.method === 'HEAD' || [204, 304].includes(response.status);
            // Same-origin static responses expose Content-Encoding, so their
            // uncompressed Content-Length is safe to preserve for progress.
            // Cross-origin fetch can hide encoding headers: use chunks there.
            const lengthHeader = route && !response.headers.has('content-encoding') ?
                response.headers.get('content-length') : null;
            const length = lengthHeader !== null && /^\d+$/.test(lengthHeader) &&
                Number.isSafeInteger(Number(lengthHeader)) ? Number(lengthHeader) : null;
            const chunked = !noBody && length === null;
            head += length !== null ? `Content-Length: ${length}\r\n\r\n` :
                chunked ? 'Transfer-Encoding: chunked\r\n\r\n' : '\r\n';
            let received = 0;
            await write(id, session, encoder.encode(head));
            sentHeaders = true;
            if (!noBody && response.body) {
                const reader = response.body.getReader();
                try {
                    while (true) {
                        const {done, value} = await reader.read();
                        if (done) break;
                        if (!value.length) continue;
                        received += value.length;
                        if (length !== null && received > length) throw new Error('Response exceeds Content-Length');
                        if (chunked) await write(id, session, encoder.encode(value.length.toString(16) + '\r\n'));
                        await write(id, session, value);
                        if (chunked) await write(id, session, encoder.encode('\r\n'));
                    }
                } finally { await reader.cancel().catch(() => {}); }
            }
            if (!noBody && length !== null && received !== length) throw new Error('Truncated download');
            if (chunked) await write(id, session, encoder.encode('0\r\n\r\n'));
            stack._net_end(id);
        } catch (error) {
            if (!sessions.has(id) || closed) return;
            log(`Browser Wi-Fi download failed: ${error.message}`);
            if (sentHeaders) stack._net_abort(id);
            else {
                const body = encoder.encode('Download unavailable. Check static assets or the source server\'s CORS policy.\n');
                try {
                    await write(id, session, encoder.encode(`HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: ${body.length}\r\n\r\n`));
                    await write(id, session, body);
                    stack._net_end(id);
                } catch { stack._net_abort(id); }
            }
        }
    }
    return {
        get connected() { return !closed; },
        send(frame) {
            if (closed || frame.length < 14 || frame.length > 2048) return;
            stack.HEAPU8.set(frame, scratch);
            stack._net_input(scratch, frame.length);
            schedulePump();
        },
        take() { const frame = frames.shift(); if (frame) bytes -= frame.length; return frame; },
        reset() {
            for (const id of [...sessions.keys()]) stack._net_abort(id);
            frames.length = 0; bytes = 0;
        },
        close() {
            if (closed) return;
            closed = true;
            clearInterval(timer);
            for (const id of [...sessions.keys()]) stack._net_abort(id);
            stack._free(scratch);
            frames.length = 0; bytes = 0;
        }
    };
}
