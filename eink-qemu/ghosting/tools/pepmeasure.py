# usage: pepmeasure.py shot.png: ghost depth of the erased text after the cursor, as a fraction of white-black
import sys
from PIL import Image
im = Image.open(sys.argv[1]).convert('L')
def px(x0, y0, x1, y1): return sorted(im.getpixel((x, y)) for x in range(x0, x1) for y in range(y0, y1))
bg = px(150, 230, 400, 280); black = px(60, 40, 240, 70)
white, blk = bg[len(bg) // 2], black[0]
erase = px(40, 150, 140, 195)        # where "pep" was (field starts at x~36, y~150..195)
print(f"white {white} black {blk} erased-min {erase[0]} p2 {erase[len(erase)//50]} depth {(white - erase[0]) / (white - blk):.3f}")
