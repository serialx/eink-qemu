/* Adapter lifecycle and optional Node bootstrap. */
for (const name of ['onExit', 'onAbort']) {
    const previous = Module[name];
    Module[name] = (...args) => { Module.browserNetwork?.close(); previous?.(...args); };
}
// Node uses the same in-process adapter and static assets, with no network relay.
if (typeof process === 'object' && process.versions?.node && process.env.WIFI_ASSETS) {
    Module.preRun = Module.preRun || [];
    Module.preRun.push(() => {
        addRunDependency('browser-network');
        Promise.all([import('node:url'), import('node:fs/promises'),
            import('./browser-network.mjs'), import('node:fs'), import('node:stream')
        ]).then(async ([url, fs, adapter, files, streams]) => {
            const assets = url.pathToFileURL(require('node:path').resolve(process.env.WIFI_ASSETS) + '/');
            Module.browserNetwork = await adapter.createBrowserNetwork({assets,
                fetchResource: async (target, options) => {
                    if (new URL(target).protocol !== 'file:') return fetch(target, options);
                    try {
                        const stat = await fs.stat(target);
                        return new Response(options?.method === 'HEAD' ? null :
                            streams.Readable.toWeb(files.createReadStream(target, {signal: options?.signal})),
                            {headers: {'Content-Length': String(stat.size)}});
                    }
                    catch { return new Response('Not found', {status: 404}); }
                }, log: line => console.error(line)});
            removeRunDependency('browser-network');
        }).catch(error => abort(String(error)));
    });
}
