// Run with: node --test eink-qemu/tests/test_panel.mjs
// The desktop app's panel (desktop/src/panel.ts): whether the panel is blank
// is judged on everything drawn so far, not on one refresh's rectangles.
import assert from 'node:assert/strict';
import test from 'node:test';
import { Panel } from '../desktop/src/panel.ts';

function size(width, height) {
  return new Uint32Array([1, width, height]).buffer;
}

/** A frame message: rects of [x, y, w, h, gray]. */
function frame(...rects) {
  const length = 8 + rects.reduce((sum, [, , w, h]) => sum + 16 + w * h, 0);
  const buffer = new ArrayBuffer(length), view = new DataView(buffer), bytes = new Uint8Array(buffer);
  view.setUint32(0, 2, true);
  view.setUint32(4, rects.length, true);
  let at = 8;
  for (const [x, y, w, h, gray] of rects) {
    [x, y, w, h].forEach((v, i) => view.setUint32(at + 4 * i, v, true));
    at += 16;
    bytes.fill(gray, at, at + w * h);
    at += w * h;
  }
  return buffer;
}

test('a size starts a blank white panel', () => {
  const panel = new Panel();
  assert.equal(panel.apply(size(4, 4)), true);
  assert.equal(panel.apply(frame([0, 0, 4, 4, 255])), true);
});

test('a solid rectangle over a drawn screen leaves it drawn', () => {
  const panel = new Panel();
  panel.apply(size(4, 4));
  assert.equal(panel.apply(frame([0, 0, 2, 2, 0])), false);
  // A region cleared to white: its own pixels are one shade.
  assert.equal(panel.apply(frame([2, 2, 2, 2, 255])), false);
});

test('a full flash, then clearing what was drawn, is blank', () => {
  const panel = new Panel();
  panel.apply(size(4, 4));
  panel.apply(frame([1, 1, 1, 1, 0]));
  assert.equal(panel.apply(frame([0, 0, 4, 4, 0])), true);
  assert.equal(panel.apply(frame([1, 1, 1, 1, 255])), false);
  assert.equal(panel.apply(frame([0, 0, 4, 4, 255])), true);
});

test('a new size forgets the last screen', () => {
  const panel = new Panel();
  panel.apply(size(4, 4));
  panel.apply(frame([0, 0, 1, 1, 0]));
  assert.equal(panel.apply(size(4, 4)), true);
  assert.equal(panel.apply(frame([3, 3, 1, 1, 255])), true);
});

test('before any size, the rectangles alone decide', () => {
  const panel = new Panel();
  assert.equal(panel.apply(frame([0, 0, 2, 1, 255])), true);
  assert.equal(panel.apply(frame([0, 0, 1, 1, 255], [1, 0, 1, 1, 0])), false);
});
