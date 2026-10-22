import type { Backend } from './backend';

/** Inspection probes the native transport, which can be exclusive (Windows
 * TAP). Do not race a background inspection against this app's launch. Native
 * authorization, transaction locks and leases remain authoritative. */
export function coordinateBridgeLaunch(backend: Backend): Backend {
  if (!backend.bridge) return backend;
  let pending: Promise<unknown> = Promise.resolve();
  const serial = <T>(action: () => Promise<T>): Promise<T> => {
    const result = pending.then(action);
    pending = result.catch(() => {});
    return result;
  };
  return {
    ...backend,
    start: (flash, options) => options.wifi && options.wifiMode === 'bridge'
      ? serial(() => backend.start(flash, options))
      : backend.start(flash, options),
    bridge: {
      ...backend.bridge,
      inspect: () => serial(() => backend.bridge!.inspect()),
    },
  };
}
