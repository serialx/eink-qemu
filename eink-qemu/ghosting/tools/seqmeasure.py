# usage: seqmeasure.py prefix: ghost depth in the old text-field area for prefix-{1,3..10}.png
import sys
from PIL import Image
def q(v, f): v = sorted(v); return v[int(len(v) * f)]
for n in [1, 3, 4, 5, 6, 7, 8, 9, 10]:
    im = Image.open(f'{sys.argv[1]}-{n}.png').convert('L')
    band = [im.getpixel((x, y)) for x in range(150, 330) for y in range(150, 196)]
    bg = q(band, 0.5)
    print(f'{n:>2}: bg {bg} depth {(bg - q(band, 0.02)) / (bg - 16):.3f} mean {(bg - sum(band) / len(band)) / (bg - 16):.4f}')
