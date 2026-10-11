/* Build with build-browser-net.sh, then node --test this file. No host sockets. */
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {Duplex} from 'node:stream';
import tls from 'node:tls';
import http from 'node:http';
import https from 'node:https';
import {createHash} from 'node:crypto';
const {createBrowserNetwork, replyDatagram, parseRequest} = await import(
    new URL('../../build-wasm-deps/browser-net/build/browser-network.mjs', import.meta.url));
const assets = new URL('../../build-wasm-deps/wifi-assets/', import.meta.url);
const requests = [];
const localFetch = async (url, options) => {
    requests.push(String(url));
    assert.equal(new URL(url).protocol, 'file:', 'no network access is allowed in this test');
    if (String(url).endsWith('fail.cpfont')) throw new Error('simulated fetch failure');
    try { const data = await readFile(url); return new Response(data, {headers: {'Content-Length': String(data.length)}}); }
    catch { return new Response('missing', {status: 404}); }
};
function checksum(data) {
    let sum = 0;
    for (let i = 0; i < data.length; i += 2) sum += (data[i] << 8) | (data[i + 1] || 0);
    while (sum >>> 16) sum = (sum & 65535) + (sum >>> 16);
    return ~sum & 65535;
}
const mac = Buffer.from([0x52, 0x54, 0, 1, 2, 3]);
const gatewayMac = Buffer.from([0x52, 0x55, 0, 0x12, 0x34, 0x02]);
const ip = Buffer.from([10, 0, 2, 15]), gateway = Buffer.from([10, 0, 2, 2]);
// Minimal lossless Ethernet test peer. Production TCP is entirely in lwIP.
class Guest extends Duplex {
    constructor(network, port, {pollMs = 2, stallAfter = Infinity, stallMs = 100} = {}) {
        super(); this.network = network; this.port = port; this.seq = 1000; this.ack = 0;
        this.received = 0; this.stallAfter = stallAfter; this.stallMs = stallMs; this.stalledUntil = 0;
        const arp = Buffer.alloc(42);
        gatewayMac.copy(arp); mac.copy(arp, 6); arp.writeUInt16BE(0x806, 12);
        arp.writeUInt16BE(1, 14); arp.writeUInt16BE(0x800, 16); arp[18] = 6; arp[19] = 4;
        arp.writeUInt16BE(1, 20); mac.copy(arp, 22); ip.copy(arp, 28); gateway.copy(arp, 38);
        network.send(arp);
        this.packet(2); this.seq++;
        this.poller = setInterval(() => this.drain(), pollMs);
    }
    setTimeout() { return this; }
    setNoDelay() { return this; }
    setKeepAlive() { return this; }
    _read() {}
    _write(data, encoding, callback) {
        for (let offset = 0; offset < data.length; offset += 1460) {
            const part = data.subarray(offset, offset + 1460);
            this.packet(0x18, part); this.seq += part.length;
        }
        callback();
    }
    _destroy(error, callback) { clearInterval(this.poller); callback(error); }
    packet(flags, data = Buffer.alloc(0)) {
        const p = Buffer.alloc(54 + data.length);
        gatewayMac.copy(p); mac.copy(p, 6); p.writeUInt16BE(0x800, 12);
        p[14] = 0x45; p.writeUInt16BE(p.length - 14, 16); p[22] = 64; p[23] = 6;
        ip.copy(p, 26); gateway.copy(p, 30); p.writeUInt16BE(checksum(p.subarray(14, 34)), 24);
        p.writeUInt16BE(40000, 34); p.writeUInt16BE(this.port, 36);
        p.writeUInt32BE(this.seq >>> 0, 38); p.writeUInt32BE(this.ack >>> 0, 42);
        p[46] = 0x50; p[47] = flags; p.writeUInt16BE(32768, 48); data.copy(p, 54);
        const pseudo = Buffer.concat([ip, gateway, Buffer.from([0, 6, (20 + data.length) >> 8, (20 + data.length) & 255]), p.subarray(34)]);
        p.writeUInt16BE(checksum(pseudo), 50);
        this.network.send(Buffer.concat([p, Buffer.alloc(22)]));
    }
    drain() {
        if (performance.now() < this.stalledUntil) return;
        let frame;
        while ((frame = this.network.take())) {
            const p = Buffer.from(frame);
            if (p.readUInt16BE(12) !== 0x800 || p[23] !== 6) continue;
            const tcp = 14 + (p[14] & 15) * 4;
            const flags = p[tcp + 13], sequence = p.readUInt32BE(tcp + 4);
            if (flags & 4) { this.destroy(new Error('TCP reset')); return; }
            if (flags & 2) {
                this.ack = sequence + 1; this.packet(0x10); this.emit('connect'); continue;
            }
            const payload = p.subarray(tcp + (p[tcp + 12] >> 4) * 4, 14 + p.readUInt16BE(16));
            const skip = this.ack - sequence;
            if (payload.length && skip >= 0 && skip < payload.length) {
                this.ack += payload.length - skip;
                this.received += payload.length - skip;
                this.push(payload.subarray(skip));
            }
            if (flags & 1) { this.ack++; this.push(null); }
            if (payload.length || flags & 1) this.packet(0x10);
            if (this.received >= this.stallAfter) {
                this.stallAfter = Infinity;
                this.stalledUntil = performance.now() + this.stallMs;
                return;
            }
        }
    }
}
async function requestFont(secure, path, options = {}, fetchResource = localFetch, guestOptions) {
    const network = await createBrowserNetwork({assets, fetchResource, log() {}});
    const guest = new Guest(network, secure ? 443 : 80, guestOptions);
    const agent = secure ? new https.Agent() : new http.Agent();
    agent.createConnection = () => secure ? tls.connect({socket: guest, rejectUnauthorized: false,
        servername: 'github.com', ...options}) : guest;
    try {
        await new Promise(resolve => guest.once('connect', resolve));
        return await new Promise((resolve, reject) => {
            const request = (secure ? https : http).get({host: 'github.com', path, agent}, response => {
                const chunks = [];
                response.on('data', chunk => chunks.push(chunk));
                response.on('error', reject);
                response.on('end', () => resolve({status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks)}));
            });
            request.on('error', reject);
            request.setTimeout(15000, () => request.destroy(new Error('HTTP timeout')));
        });
    } finally { agent.destroy(); guest.destroy(); network.close(); }
}
const base = '/crosspoint-reader/crosspoint-fonts/releases/download/sd-fonts-m1-b4/';
for (const secure of [false, true]) {
    test(`real ${secure ? 'TLS' : 'TCP'} font stream has exact bytes, without sockets`, {timeout: 20000}, async () => {
        const response = await requestFont(secure, base + 'Alef_12.cpfont');
        assert.equal(response.status, 200);
        const expected = await readFile(new URL('Alef_12.cpfont', assets));
        assert.equal(createHash('sha256').update(response.body).digest('hex'), createHash('sha256').update(expected).digest('hex'));
    });
}
test('TLS 1.2 firmware compatibility', {timeout: 20000}, async () => {
    const response = await requestFont(true, base + 'fonts.json', {maxVersion: 'TLSv1.2'});
    assert.equal(response.status, 200);
    assert.equal(JSON.parse(response.body).version, 1);
});
test('guest that validates server certificates rejects the local TLS endpoint', {timeout: 20000}, async () => {
    await assert.rejects(requestFont(true, base + 'fonts.json', {rejectUnauthorized: true}), /self-signed certificate/);
});
test('DNS answers A locally, returns no AAAA, and rejects malformed labels', () => {
    const q = Buffer.from('12340100000100000000000003666f6f03636f6d0000010001', 'hex');
    const answer = replyDatagram(1, q);
    assert.deepEqual([...answer.slice(-4)], [...gateway]);
    q[q.length - 3] = 28;
    assert.equal(replyDatagram(1, q).length, q.length);
    q[12] = 200;
    assert.equal(replyDatagram(1, q), null);
});
test('DHCP offer/ACK and browser clock NTP', () => {
    const q = Buffer.alloc(244); q.set([1, 1, 6]); q.writeUInt32BE(0x63825363, 236); q.set([53, 1, 1, 255], 240);
    const offer = replyDatagram(0, q);
    assert.deepEqual([...offer.slice(16, 20)], [...ip]);
    assert.equal(offer[242], 2);
    q[242] = 3; assert.equal(replyDatagram(0, q)[242], 5);
    const ntp = Buffer.alloc(48); ntp[0] = 0x23; ntp.fill(42, 40);
    const reply = replyDatagram(2, ntp, 1700000000000);
    assert.deepEqual([...reply.slice(24, 32)], [...ntp.slice(40)]);
    assert.equal(new DataView(reply.buffer).getUint32(40), 3908988800);
});
test('HTTP parser rejects ambiguous requests and unsupported uploads', () => {
    assert.equal(parseRequest('GET /fonts.json HTTP/1.1\r\nHost: github.com', true).url.href, 'https://github.com/fonts.json');
    for (const request of ['POST / HTTP/1.1\r\nHost: x', 'GET / HTTP/1.1\r\nHost: x\r\nHost: y',
        'GET / HTTP/1.1\r\nHost: x@y', 'GET / HTTP/1.1\r\nHost: x\r\nContent-Length: 4']) {
        assert.throws(() => parseRequest(request, true));
    }
});
test('DHCP discover from 0.0.0.0 traverses Ethernet/IP/UDP and gets an offer', async () => {
    const network = await createBrowserNetwork({assets, fetchResource: localFetch, log() {}});
    try {
        const p = Buffer.alloc(42 + 244);
        p.fill(255, 0, 6); mac.copy(p, 6); p.writeUInt16BE(0x800, 12);
        p[14] = 0x45; p.writeUInt16BE(p.length - 14, 16); p[22] = 64; p[23] = 17;
        p.fill(255, 30, 34); p.writeUInt16BE(checksum(p.subarray(14, 34)), 24);
        p.writeUInt16BE(68, 34); p.writeUInt16BE(67, 36); p.writeUInt16BE(p.length - 34, 38);
        p.set([1, 1, 6], 42); mac.copy(p, 70);
        p.writeUInt32BE(0x63825363, 278); p.set([53, 1, 1, 255], 282);
        network.send(p);
        const frames = [];
        let frame;
        while ((frame = network.take())) frames.push(Buffer.from(frame));
        const reply = frames.find(p => p.readUInt16BE(12) === 0x800 && p[23] === 17);
        assert.ok(reply, 'DHCP server must accept a zero source IP');
        assert.equal(reply.readUInt16BE(36), 68);
        assert.deepEqual([...reply.subarray(58, 62)], [...ip]);
        assert.equal(reply[284], 2);
    } finally { network.close(); }
});
test('a blocked download returns HTTP failure rather than an empty successful file', {timeout: 20000}, async () => {
    const response = await requestFont(true, base + 'Alef_12.cpfont', {}, async (url, options) => {
        if (String(url).endsWith('.cpfont')) throw new Error('CORS blocked');
        return localFetch(url, options);
    });
    assert.equal(response.status, 502);
    assert.match(response.body.toString(), /Download unavailable/);
});
test('stream failure aborts a partial response', {timeout: 20000}, async () => {
    await assert.rejects(requestFont(true, base + 'Alef_12.cpfont', {}, async (url, options) => {
        if (!String(url).endsWith('.cpfont')) return localFetch(url, options);
        return new Response(new ReadableStream({start(controller) {
            controller.enqueue(new Uint8Array(8192));
            setTimeout(() => controller.error(new Error('truncated source')), 100);
        }}));
    }), /reset|aborted|hang up/i);
});
test('closing the runtime aborts an outstanding fetch and clears queued frames', {timeout: 10000}, async () => {
    let started;
    const fetching = new Promise(resolve => { started = resolve; });
    let signal;
    const network = await createBrowserNetwork({assets, log() {}, fetchResource: (url, options) => {
        if (!String(url).endsWith('.cpfont')) return localFetch(url, options);
        signal = options.signal;
        started();
        return new Promise((resolve, reject) => signal.addEventListener('abort', () => reject(new Error('cancelled')), {once: true}));
    }});
    const guest = new Guest(network, 80);
    try {
        await new Promise(resolve => guest.once('connect', resolve));
        guest.write(`GET ${base}Alef_12.cpfont HTTP/1.1\r\nHost: github.com\r\n\r\n`);
        await fetching;
        network.close();
        assert.equal(signal.aborted, true);
        assert.equal(network.connected, false);
        assert.equal(network.take(), undefined);
        network.close(); // idempotent
    } finally { guest.destroy(); network.close(); }
});
test('unknown-length responses stream with valid chunk framing', {timeout: 20000}, async () => {
    const response = await requestFont(true, base + 'Alef_12.cpfont', {}, async (url, options) => {
        const original = await localFetch(url, options);
        return String(url).endsWith('.cpfont') ? new Response(original.body) : original;
    });
    assert.deepEqual(response.body, await readFile(new URL('Alef_12.cpfont', assets)));
});
test('TLS download resumes intact after the guest stops acknowledging packets', {timeout: 20000}, async () => {
    const response = await requestFont(true, base + 'Alef_12.cpfont', {}, localFetch,
        {stallAfter: 65536, stallMs: 150});
    assert.deepEqual(response.body, await readFile(new URL('Alef_12.cpfont', assets)));
});
test('network throughput benchmark', {skip: !process.env.BENCH_NETWORK, timeout: 60000}, async t => {
    const expected = Buffer.alloc(2 * 1024 * 1024);
    for (let i = 0; i < expected.length; i++) expected[i] = (i * 31 + (i >> 8)) & 255;
    const fetchResource = (url, options) => String(url).endsWith('.cpfont') ?
        new Response(expected, {headers: {'Content-Length': String(expected.length)}}) : localFetch(url, options);
    for (const secure of [false, true]) {
        const samples = [];
        for (let i = 0; i < 2; i++) {
            const start = performance.now();
            const response = await requestFont(secure, base + 'Alef_12.cpfont', {}, fetchResource, {pollMs: 10});
            const seconds = (performance.now() - start) / 1000;
            assert.deepEqual(response.body, expected);
            samples.push({seconds, mibPerSecond: expected.length / 1048576 / seconds});
        }
        t.diagnostic(JSON.stringify({protocol: secure ? 'TLS' : 'TCP', bytes: expected.length, pollMs: 10, samples}));
    }
});
