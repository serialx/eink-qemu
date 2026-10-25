/** Clockwise display rotation; the firmware's framebuffer stays unchanged. */
export type Rotation = 0 | 90 | 180 | 270;

export function parseRotation(value: unknown): Rotation {
  const angle = Number(value);
  return angle === 90 || angle === 180 || angle === 270 ? angle : 0;
}

/** Translate the rotated panel into positive coordinates, for CSS and PNGs. */
export function rotatedPanel(width: number, height: number, rotation: Rotation) {
  let matrix: [number, number, number, number, number, number];
  switch (rotation) {
    case 90: matrix = [0, 1, -1, 0, height, 0]; break;
    case 180: matrix = [-1, 0, 0, -1, width, height]; break;
    case 270: matrix = [0, -1, 1, 0, 0, width]; break;
    default: matrix = [1, 0, 0, 1, 0, 0];
  }
  const sideways = rotation === 90 || rotation === 270;
  return { width: sideways ? height : width, height: sideways ? width : height, matrix };
}

/** Normalized coordinates in the displayed rectangle back to the touch panel. */
export function panelPoint(x: number, y: number, rotation: Rotation): [number, number] {
  switch (rotation) {
    case 90: return [y, 1 - x];
    case 180: return [1 - x, 1 - y];
    case 270: return [1 - y, x];
    default: return [x, y];
  }
}
