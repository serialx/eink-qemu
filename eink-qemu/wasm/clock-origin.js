/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * Keep the pthread clock synchronized across realms without reading the
 * immutable time origin on every sample. This initializer runs separately in
 * the page and each worker; performance.now() remains local to that realm.
 * Leave Emscripten's non-pthread and AudioWorklet clock fallbacks unchanged.
 */
#if PTHREADS && !AUDIO_WORKLET
addToLibrary({
  $einkQemuTimeOrigin: 'performance.timeOrigin',
  emscripten_get_now__deps: ['$einkQemuTimeOrigin'],
  emscripten_get_now: () => einkQemuTimeOrigin + performance.now(),
});
#endif
