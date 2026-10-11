# usage: photomeasure.py /path/to/series-  -> ghost depth in device photos series-{1,4..10}.jpg
# Regions are for this photo series' framing (portrait UI shot rotated 90 degrees):
# ghost band = erased " X4Ppppppp" at x 496..532, y 456..604; background strips beside it;
# black reference = the "Device Name" label (x 470..492, y 330..440, 1st percentile).
import sys, statistics as st
from PIL import Image
def q(v, f): v = sorted(v); return v[int(len(v) * f)]
for n in [1, 4, 5, 6, 7, 8, 9, 10]:
    im = Image.open(f'{sys.argv[1]}{n}.jpg').convert('L')
    band = [im.getpixel((x, y)) for x in range(496, 532) for y in range(456, 604)]
    bgs = [im.getpixel((x, y)) for x in range(536, 548) for y in range(456, 604)] + \
          [im.getpixel((x, y)) for x in range(482, 492) for y in range(456, 604)]
    lab = [im.getpixel((x, y)) for x in range(470, 492) for y in range(330, 440)]
    bg = st.median(bgs); blk = q(lab, 0.01)
    print(f'{n:>2}: bg {bg} black {blk} p2-depth {(bg - q(band, 0.02)) / (bg - blk):.3f} '
          f'mean-depth {(bg - st.mean(band)) / (bg - blk):.4f}')
