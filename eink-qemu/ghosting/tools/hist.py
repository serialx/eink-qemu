# usage: hist.py a.png [b.png ...]: top gray levels, and count of "off-white" (200..239) and "off-black" (17..60) pixels
import zlib, struct, collections, sys
def load(fn):
    d = open(fn, 'rb').read(); i = 8; idat = b''
    while i < len(d):
        n, t = struct.unpack('>I4s', d[i:i+8]); c = d[i+8:i+8+n]
        if t == b'IHDR': w, h, bd, ct = struct.unpack('>IIBB', c[:10])
        if t == b'IDAT': idat += c
        i += 12 + n
    raw = zlib.decompress(idat); bpp = {2: 3, 6: 4, 0: 1}[ct]; st = w * bpp; prev = bytearray(st); p = 0; out = []
    for y in range(h):
        f = raw[p]; line = bytearray(raw[p+1:p+1+st]); p += 1 + st
        for x in range(st):
            a = line[x-bpp] if x >= bpp else 0; b = prev[x]; c = prev[x-bpp] if x >= bpp else 0
            if f == 1: line[x] = (line[x] + a) & 255
            elif f == 2: line[x] = (line[x] + b) & 255
            elif f == 3: line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                pp = a + b - c; pa, pb, pc = abs(pp-a), abs(pp-b), abs(pp-c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        out.append(bytes(line[0::bpp])); prev = line
    return out
for fn in sys.argv[1:]:
    rows = load(fn); H = collections.Counter()
    for r in rows: H.update(r)
    top = sorted(H.items(), key=lambda kv: -kv[1])[:5]
    offw = sum(v for k, v in H.items() if 200 <= k < 239); offb = sum(v for k, v in H.items() if 17 < k <= 60)
    print(f"{fn}: top {top} offwhite {offw} offblack {offb}")
