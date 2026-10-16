// The panel as the display messages of core/src/display.rs draw it, in gray
// values and apart from any canvas, so whether it is blank is judged on the
// whole picture: a refresh's rectangles can be one shade (a region cleared to
// white) while the rest of the panel still shows a drawn screen.

/** Display messages, little-endian: size `u32 1, u32 width, u32 height`;
    frame `u32 2, u32 count`, then per rect `u32 x, y, w, h` and w*h pixels. */
export class Panel {
  width = 0;
  height = 0;
  pixels = new Uint8Array(0);

  /** Apply a display message and return whether the panel is now one value
      throughout (blank before the firmware draws, a refresh's flash). */
  apply(buffer: ArrayBuffer): boolean {
    const view = new DataView(buffer), bytes = new Uint8Array(buffer);
    const kind = view.getUint32(0, true);
    if (kind === 1) {
      // A size starts the panel over, white like a new screen.
      this.width = view.getUint32(4, true);
      this.height = view.getUint32(8, true);
      this.pixels = new Uint8Array(this.width * this.height).fill(255);
      return true;
    }
    if (kind !== 2) return true;
    // A frame before any size: only its own rectangles can tell.
    if (!this.pixels.length) return !rectsShowSomething(view, bytes);
    const count = view.getUint32(4, true);
    let at = 8;
    for (let n = 0; n < count; n++) {
      const x = view.getUint32(at, true), y = view.getUint32(at + 4, true);
      const w = view.getUint32(at + 8, true), h = view.getUint32(at + 12, true);
      at += 16;
      // display.rs rejects rectangles off the panel; skip one all the same.
      if (x + w <= this.width && y + h <= this.height) {
        for (let row = 0; row < h; row++) {
          this.pixels.set(bytes.subarray(at + row * w, at + (row + 1) * w), (y + row) * this.width + x);
        }
      }
      at += w * h;
    }
    const pixels = this.pixels, first = pixels[0];
    for (let i = 1; i < pixels.length; i++) if (pixels[i] !== first) return false;
    return true;
  }
}

/** Whether a frame's rectangles are not all one value. */
function rectsShowSomething(view: DataView, bytes: Uint8Array) {
  const count = view.getUint32(4, true);
  let at = 8, first = -1;
  for (let n = 0; n < count; n++) {
    const area = view.getUint32(at + 8, true) * view.getUint32(at + 12, true);
    at += 16;
    for (let i = at; i < at + area; i++) {
      if (first < 0) first = bytes[i];
      else if (bytes[i] !== first) return true;
    }
    at += area;
  }
  return false;
}
