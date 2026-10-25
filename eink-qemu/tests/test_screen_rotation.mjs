// Run with: node --test eink-qemu/tests/test_screen_rotation.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { panelPoint, parseRotation, rotatedPanel } from '../desktop/src/screen-rotation.ts';

test('saved rotation accepts only supported quarter turns', () => {
  for (const angle of [0, 90, 180, 270]) assert.equal(parseRotation(String(angle)), angle);
  for (const value of [null, '', 'invalid', '-90', '45', '360', 'Infinity']) assert.equal(parseRotation(value), 0);
});

for (const [width, height] of [[528, 792], [480, 800]]) {
  test(`${width} × ${height}: all rotated corners remain inside the screen and PNG bounds`, () => {
    // Expected destinations of native top-left, top-right, bottom-right, bottom-left.
    const destinations = {
      0: [[0, 0], [1, 0], [1, 1], [0, 1]],
      90: [[1, 0], [1, 1], [0, 1], [0, 0]],
      180: [[1, 1], [0, 1], [0, 0], [1, 0]],
      270: [[0, 1], [0, 0], [1, 0], [1, 1]],
    };
    for (const scale of [0.375, 1, 2]) {
      const w = width * scale, h = height * scale;
      for (const rotation of [0, 90, 180, 270]) {
        const panel = rotatedPanel(w, h, rotation);
        assert.deepEqual([panel.width, panel.height], rotation % 180 ? [h, w] : [w, h]);
        const [a, b, c, d, e, f] = panel.matrix;
        const corners = [[0, 0], [w, 0], [w, h], [0, h]];
        corners.forEach(([x, y], i) => {
          const expected = destinations[rotation][i];
          assert.deepEqual([a * x + c * y + e, b * x + d * y + f],
            [expected[0] * panel.width, expected[1] * panel.height]);
        });
        // A touch at an interior point must reach the same firmware pixel at every zoom.
        const x = w * 0.25, y = h * 0.75;
        const point = panelPoint((a * x + c * y + e) / panel.width,
          (b * x + d * y + f) / panel.height, rotation);
        assert.deepEqual(point, [0.25, 0.75]);
      }
    }
  });
}

test('touch capture outside the display retains direction after rotation', () => {
  assert.deepEqual(panelPoint(-0.25, 1.5, 90), [1.5, 1.25]);
  assert.deepEqual(panelPoint(-0.25, 1.5, 270), [-0.5, -0.25]);
});
