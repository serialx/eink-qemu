import type { FirmwareInfo, Settings } from './types';

export const devicesOf = (chip: FirmwareInfo['chip']): Machine[] =>
  chip === 'ESP32-C3' ? ['x3', 'x4'] : chip === 'ESP32-S3' ? ['x4pro', 'picco'] : [];

/** Mirrors core/src/firmware.rs: a saved choice, a filename hint, then the chip default. */
export function deviceOf(chip: FirmwareInfo['chip'], path: string, remembered?: Settings['device']): Machine | null {
  const devices = devicesOf(chip);
  if (remembered && remembered !== 'auto' && devices.includes(remembered)) return remembered;
  const tokens = (path.split('/').pop() ?? '').toLowerCase().split(/[^a-z0-9]+/);
  if (chip === 'ESP32-C3' && tokens.includes('x4') && !tokens.includes('x3')) return 'x4';
  if (chip === 'ESP32-S3' && tokens.includes('picco')) return 'picco';
  return devices[0] ?? null;
}
