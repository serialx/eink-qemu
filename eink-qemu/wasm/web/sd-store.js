/* One saved card per hosted application path. Transactions replace the whole
 * image atomically, so a failed save leaves the previous snapshot intact. */
'use strict';
const sdStore = (() => {
    const key = new URL('.', location.href).pathname;
    let database;
    function open() {
        if (!database) database = new Promise((resolve, reject) => {
            const request = indexedDB.open('eink-qemu-sd', 1);
            request.onupgradeneeded = () => request.result.createObjectStore('cards');
            request.onsuccess = () => {
                const db = request.result;
                db.onversionchange = () => { db.close(); database = null; };
                resolve(db);
            };
            request.onerror = () => reject(request.error);
        }).catch(error => { database = null; throw error; });
        return database;
    }
    async function transaction(mode, action) {
        const db = await open();
        return new Promise((resolve, reject) => {
            const tx = db.transaction('cards', mode);
            const request = action(tx.objectStore('cards'));
            tx.oncomplete = () => resolve(request.result);
            tx.onabort = () => reject(tx.error || request.error || new Error('SD save aborted.'));
            tx.onerror = () => {}; // onabort reports the transaction's failure.
        });
    }
    return {
        async load() {
            const blob = await transaction('readonly', store => store.get(key));
            return blob ? new Uint8Array(await blob.arrayBuffer()) : null;
        },
        save(bytes) {
            // Capture now: callers can mutate or transfer the source immediately.
            const blob = new Blob([bytes]);
            return transaction('readwrite', store => store.put(blob, key));
        }
    };
})();
