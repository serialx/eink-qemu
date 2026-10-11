// ES module facade for the same classic script used by the browser explorer.
import './fat.js';
export const { open, FatError, isValidName } = globalThis.FAT;
export default globalThis.FAT;
