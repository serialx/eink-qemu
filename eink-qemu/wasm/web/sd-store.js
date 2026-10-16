/* One saved card per hosted application path. Transactions replace the whole
 * image atomically, so a failed save leaves the previous snapshot intact.
 * Every save numbers the card anew. A save made from an older number than the
 * saved one fails with ESTALE: another tab of the page saved since, and its
 * card is not overwritten. The other tabs hear of each save. */
'use strict';
const sdStore = (() => {
    const key = new URL('.', location.href).pathname;
    const channel = typeof BroadcastChannel === 'function' ? new BroadcastChannel(`eink-qemu-sd:${key}`) : null;
    let database;
    function open() {
        if (!database) database = new Promise((resolve, reject) => {
            const request = indexedDB.open('eink-qemu-sd', 1);
            // A store that never answers (a blocked upgrade, a stuck browser)
            // must not hold the page's card forever.
            let late = false;
            const timer = setTimeout(() => {
                late = true;
                reject(new Error('The browser storage did not respond; reload the page.'));
            }, 10000);
            request.onupgradeneeded = () => request.result.createObjectStore('cards');
            request.onsuccess = () => {
                clearTimeout(timer);
                const db = request.result;
                if (late) { db.close(); return; }
                db.onversionchange = () => { db.close(); database = null; };
                resolve(db);
            };
            request.onerror = () => { clearTimeout(timer); reject(request.error); };
        }).catch(error => { database = null; throw error; });
        return database;
    }
    // `action` gets the store, a setter for the result and a way to fail.
    async function transaction(mode, action) {
        const db = await open();
        return new Promise((resolve, reject) => {
            const tx = db.transaction('cards', mode);
            let result, failure = null;
            action(tx.objectStore('cards'), value => { result = value; }, error => { failure = error; tx.abort(); });
            tx.oncomplete = () => resolve(result);
            tx.onabort = () => reject(failure || tx.error || new Error('SD save aborted.'));
            tx.onerror = () => {}; // onabort reports the transaction's failure.
        });
    }
    // Cards saved before revisions were a bare Blob.
    const record = value => value instanceof Blob ? {blob: value, revision: 0} : value;
    return {
        /** The saved card and its number, or null. */
        async load() {
            const saved = await transaction('readonly', (store, done) => {
                const request = store.get(key);
                request.onsuccess = () => done(request.result && record(request.result));
            });
            return saved ? {bytes: new Uint8Array(await saved.blob.arrayBuffer()), revision: saved.revision} : null;
        },
        /** Saves over card `revision` (0: none), or over any card when
            `force`; resolves to the new card's number. */
        async save(bytes, revision, force = false) {
            // Capture now: callers can mutate or transfer the source immediately.
            const blob = new Blob([bytes]);
            const next = await transaction('readwrite', (store, done, fail) => {
                const request = store.get(key);
                request.onsuccess = () => {
                    const saved = request.result ? record(request.result).revision : 0;
                    if (!force && saved !== revision) {
                        fail(Object.assign(new Error('Another tab of this page saved a newer SD card.'), {code: 'ESTALE'}));
                        return;
                    }
                    store.put({blob, revision: saved + 1}, key);
                    done(saved + 1);
                };
            });
            channel?.postMessage({revision: next});
            return next;
        },
        /** Calls `listener(revision)` when another tab saves the card. */
        onSaved(listener) {
            channel?.addEventListener('message', event => listener(event.data.revision));
        }
    };
})();
