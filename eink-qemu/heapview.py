#!/usr/bin/env python3
"""Live map of an ESP-IDF firmware's heaps in a running eink-qemu.

usage: heapview.py firmware.elf [--monitor SOCK] [--port N] [--interval S] [--once]
                   [--trace FILE] [--addr2line PATH]

Start the emulator first (run.sh exposes its monitor at /tmp/eink-qemu-mon.sock),
then open http://127.0.0.1:8765. firmware.elf must be the ELF of the running app:
its `registered_heaps` symbol is the list of heaps (ESP-IDF 5.x, TLSF allocator).
Each refresh stops the VM, copies every heap region with `pmemsave` and resumes;
under -icount the virtual clock stops too, so the guest does not see the pause.
The monitor socket takes one client: while this runs, use another `-monitor` for
anything else (run.sh passes extra QEMU arguments through).
--once prints one snapshot as text and exits.

Where blocks came from: load the heaptrace plugin when starting QEMU,
  -plugin build/contrib/plugins/libheaptrace.so,elf=firmware.elf
and hovering a block shows the stack that allocated it, symbolized with the
toolchain's addr2line (found on PATH or under ~/.platformio, or --addr2line).
The plugin logs every allocation and free, so the page can also replay the
heaps at any moment since boot and show allocation churn by site."""
from array import array
import argparse, bisect, glob, json, os, re, shutil, socket, struct, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
# The page, and the module and stylesheet it shares with the desktop app's heap window.
FILES = {"/": ("heapview.html", "text/html; charset=utf-8"),
         "/heapview.js": ("heapview.js", "text/javascript; charset=utf-8"),
         "/heapview.css": ("heapview.css", "text/css; charset=utf-8")}

# MALLOC_CAP_* bits (esp_heap_caps.h)
CAPS = {0: "EXEC", 1: "32BIT", 2: "8BIT", 3: "DMA", 10: "SPIRAM", 11: "INTERNAL",
        12: "DEFAULT", 13: "IRAM_8BIT", 14: "RETENTION", 15: "RTCRAM"}
HEAD_CANARY, TAIL_CANARY = 0xABBA1234, 0xBAAD5678     # CONFIG_HEAP_POISONING_LIGHT
BLOCK_FREE, BLOCK_POISONED, BLOCK_CORRUPT = 1, 2, 4    # flags in the JSON block list


def elf_symbol(path, name):
    """Address of a symbol in a 32-bit little-endian ELF (Xtensa or RISC-V)."""
    with open(path, "rb") as f:
        elf = f.read()
    if elf[:4] != b"\x7fELF" or elf[4] != 1 or elf[5] != 1:
        sys.exit(f"{path}: not a 32-bit little-endian ELF")
    shoff, = struct.unpack_from("<I", elf, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", elf, 0x2E)
    sections = [struct.unpack_from("<IIIIIIIIII", elf, shoff + i * shentsize) for i in range(shnum)]
    for sh in sections:
        if sh[1] != 2:          # SHT_SYMTAB
            continue
        strtab = sections[sh[6]]
        for off in range(sh[4], sh[4] + sh[5], 16):
            st_name, st_value = struct.unpack_from("<II", elf, off)
            end = elf.index(b"\0", strtab[4] + st_name)
            if elf[strtab[4] + st_name:end] == name.encode():
                return st_value
    sys.exit(f"{path}: no symbol {name} (is it an ESP-IDF 5.x app ELF with symbols?)")


class Trace:
    """The heaptrace plugin's event log: after an 8-byte "HTRC" v2 header,
    records of u16 kind, u16 frame count, u32 pointer, u32 size, u32 old
    pointer, u64 microseconds since the epoch, then the return addresses.
    Kept as columns, with each distinct stack stored once, and replayed into
    `live` (pointer -> (size, stack id, event index)). A replaced or shorter
    file means QEMU restarted, which starts a new generation."""
    MAGIC = b"HTRC\x02\0\0\0"
    ALLOC, FREE, REALLOC = 1, 2, 3

    def __init__(self, path, site_of=None):
        self.path, self.site_of, self.gen = path, site_of, 0
        self.lock = threading.Lock()
        self.reset(None)

    def reset(self, ino):
        self.pos, self.ino, self.rest, self.gen, self.format_ok = 0, ino, b"", self.gen + 1, True
        self.rest_header = False
        self.t, self.kind, self.stack = array("q"), array("B"), array("I")
        self.ptr, self.size, self.old = array("I"), array("I"), array("I")
        self.stack_ids, self.stacks, self.stack_site = {}, [], array("i")
        self.live = {}

    def intern(self, frames):
        sid = self.stack_ids.get(frames)
        if sid is None:
            sid = self.stack_ids[frames] = len(self.stacks)
            self.stacks.append(frames)
            self.stack_site.append(self.site_of(frames) if self.site_of else -1)
        return sid

    def update(self):
        with self.lock:
            try:
                st = os.stat(self.path)
            except OSError:
                return
            if st.st_ino != self.ino or st.st_size < self.pos:
                self.reset(st.st_ino)
            if st.st_size == self.pos or not self.format_ok:
                return
            with open(self.path, "rb") as f:
                f.seek(self.pos)
                chunk = f.read()
            self.pos += len(chunk)               # the file may have grown since the stat
            data = self.rest + chunk
            off = 0
            if not self.rest_header:
                if len(data) < 8:
                    self.rest = data              # header not complete yet
                    return
                self.rest_header = True
                if data[:8] != self.MAGIC:
                    self.format_ok = False        # an older plugin's log
                    return
                off = 8
            while off + 24 <= len(data):
                kind, n, ptr, size, old, t = struct.unpack_from("<HHIIIQ", data, off)
                if off + 24 + 4 * n > len(data):
                    break
                sid = self.intern(struct.unpack_from(f"<{n}I", data, off + 24))
                off += 24 + 4 * n
                i = len(self.t)
                self.t.append(t); self.kind.append(kind); self.stack.append(sid)
                self.ptr.append(ptr); self.size.append(size); self.old.append(old)
                if kind == self.FREE:
                    self.live.pop(ptr, None)
                else:
                    if kind == self.REALLOC:
                        self.live.pop(old, None)
                    self.live[ptr] = (size, sid, i)
            self.rest = data[off:]

    def events(self, gen, start, nstacks, limit=100000):
        """Events from index `start` as columns, times in microseconds after
        the first event, with the stack -> site entries the caller lacks."""
        with self.lock:
            if gen != self.gen:
                start, nstacks = 0, 0
            end = min(len(self.t), start + limit)
            t0 = self.t[0] if self.t else 0
            return dict(gen=self.gen, start=start, total=len(self.t), t0=t0 / 1000,
                        t=[x - t0 for x in self.t[start:end]], k=self.kind[start:end].tolist(),
                        p=self.ptr[start:end].tolist(), s=self.size[start:end].tolist(),
                        o=self.old[start:end].tolist(), st=self.stack[start:end].tolist(),
                        stackStart=nstacks, stackSite=self.stack_site[nstacks:].tolist())


# Frames inside the allocator (or free) itself; the first frame after them is the caller.
ALLOCATOR = re.compile(r"^(multi_heap_\w*(alloc|free)|heap_caps_\w*(alloc|free)|aligned_or_unaligned_alloc|tlsf_|"
                       r"poison_|malloc$|calloc$|realloc$|valloc$|free$|_malloc_r$|_calloc_r$|_realloc_r$|_free_r$|"
                       r"strn?dup$|_strn?dup_r$|ps_|operator new|operator delete|std::__new_allocator<|__gnu_cxx::new_allocator<|std::allocator<|"
                       r"std::allocator_traits<)")
# Source outside the firmware's own tree: toolchain, ESP-IDF, Arduino core, libraries.
LIBRARY = re.compile(r"/(\.platformio/packages|esp32-arduino-lib-builder|crosstool-NG|libdeps|"
                     r"managed_components|framework-[^/]+)/")


class Symbolizer:
    """addr2line in a long-lived process (loading DWARF is the slow part),
    falling back to the ELF symbol table."""
    TOOLS = {243: ["riscv32-esp-elf-addr2line"], 94: ["xtensa-esp32s3-elf-addr2line", "xtensa-esp-elf-addr2line"]}

    def __init__(self, elf, tool=None):
        with open(elf, "rb") as f:
            machine, = struct.unpack_from("<H", f.read(20), 18)
        self.tool = tool or self.find_tool(machine)
        self.proc, self.cache, self.lock = None, {}, threading.Lock()
        if self.tool:
            self.proc = subprocess.Popen([self.tool, "-e", elf, "-a", "-f", "-i", "-C"], stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, text=True, bufsize=1)
        self.funcs = elf_functions(elf)

    def find_tool(self, machine):
        for name in self.TOOLS.get(machine, []):
            found = shutil.which(name) or next(iter(
                sorted(glob.glob(os.path.expanduser(f"~/.platformio/packages/toolchain-*/bin/{name}"))) +
                sorted(glob.glob(os.path.expanduser(f"~/.espressif/tools/*/*/*/bin/{name}")))), None)
            if found:
                return found
        return None

    def lookup(self, addr):
        """[(function, "file:line")] for a code address, innermost inline first."""
        with self.lock:
            if addr in self.cache:
                return self.cache[addr]
            out = None
            if self.proc and self.proc.poll() is None:
                # Address 0 after the query marks where its output ends.
                self.proc.stdin.write(f"{addr:#x}\n0\n")
                self.proc.stdin.flush()
                self.proc.stdout.readline()            # the echoed address
                lines = []
                while True:
                    line = self.proc.stdout.readline().rstrip("\n")
                    if line == "0x00000000":
                        self.proc.stdout.readline()
                        self.proc.stdout.readline()
                        break
                    lines.append(line)
                out = [(lines[i], lines[i + 1]) for i in range(0, len(lines) - 1, 2)]
            if not out or out[0][0] == "??":
                i = bisect.bisect_right(self.funcs, (addr, float("inf"))) - 1
                if i >= 0 and addr < self.funcs[i][0] + max(self.funcs[i][1], 1):
                    out = [(f"{self.funcs[i][2]}+{addr - self.funcs[i][0]:#x}", "")]
                else:
                    out = [("??", "")]
            self.cache[addr] = out
            return out


def elf_functions(path):
    """Sorted (address, size, name) of every function symbol."""
    with open(path, "rb") as f:
        elf = f.read()
    shoff, = struct.unpack_from("<I", elf, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", elf, 0x2E)
    sections = [struct.unpack_from("<IIIIIIIIII", elf, shoff + i * shentsize) for i in range(shnum)]
    funcs = []
    for sh in sections:
        if sh[1] != 2:
            continue
        strtab = sections[sh[6]][4]
        for off in range(sh[4], sh[4] + sh[5], 16):
            st_name, st_value, st_size, st_info = struct.unpack_from("<IIIB", elf, off)
            if st_info & 0xF == 2 and st_value:
                end = elf.index(b"\0", strtab + st_name)
                funcs.append((st_value, st_size, elf[strtab + st_name:end].decode(errors="replace")))
    funcs.sort()
    return funcs


def stack_frames(sym, addrs):
    frames = []
    for addr in addrs:
        # A return address points after the call; look up the call itself.
        chain = sym.lookup(addr - 1)
        fn, loc = chain[-1]          # the outermost function; inlines are inside it
        frames.append(dict(addr=addr, fn=fn, loc=loc, inl=[dict(fn=f, loc=l) for f, l in chain[:-1]],
                           alloc=bool(ALLOCATOR.match(fn)), lib=bool(LIBRARY.search(loc))))
    caller = next((i for i, f in enumerate(frames) if not f["alloc"]), len(frames))
    site = next((i for i in range(caller, len(frames)) if not frames[i]["lib"] and frames[i]["loc"]), caller)
    return frames, caller, site


def describe_stack(sym, trace, sid):
    """Recorded stack number sid, symbolized, or None."""
    if not trace or not 0 <= sid < len(trace.stacks):
        return None
    frames, caller, site = stack_frames(sym, trace.stacks[sid])
    return dict(frames=frames, caller=caller, site=site)


class Sites:
    """Numbers allocation sites (the function describe_stack reports as
    "allocated by") in the order they are first seen, so a site keeps its
    number, and the page its color, for the life of the viewer."""

    def __init__(self, sym):
        self.sym, self.ids, self.names = sym, {}, []

    def of(self, stack):
        frames, _, site = stack_frames(self.sym, stack)
        f = frames[site] if site < len(frames) else dict(fn="??", loc="")
        sid = self.ids.get(f["fn"])
        if sid is None:
            sid = self.ids[f["fn"]] = len(self.names)
            self.names.append(dict(fn=f["fn"], loc=f["loc"]))
        return sid


def label_blocks(trace, h):
    """The site, stack and allocation event of each block in a heap's block
    list, -1 where the log has no matching live allocation."""
    sites, stacks, born, b = [], [], [], h["blocks"]
    for i in range(0, len(b), 4):
        entry = None
        if b[i + 2] & BLOCK_POISONED:
            # The list holds block offsets + 4; the user pointer is block + 16.
            entry = trace.live.get(h["start"] + b[i] + 12)
        if entry and entry[0] == b[i + 3]:
            sites.append(trace.stack_site[entry[1]])
            stacks.append(entry[1])
            born.append(entry[2])
        else:
            sites.append(-1); stacks.append(-1); born.append(-1)
    return sites, stacks, born


class Monitor:
    """HMP over a unix socket; every reply ends with the "(qemu) " prompt."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(path)
        self.sock.settimeout(10)
        self.buf = b""
        self._until_prompt()
        self.tmp = tempfile.mkdtemp(prefix="heapview-")

    def _until_prompt(self):
        while not self.buf.endswith(b"(qemu) "):
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("monitor closed (did QEMU exit?)")
            self.buf += chunk
        out, self.buf = self.buf, b""
        return out

    def cmd(self, line):
        self.sock.sendall(line.encode() + b"\n")
        out = self._until_prompt().decode(errors="replace").replace("\r", "")
        # The first line is the echoed command (with terminal escapes).
        return "\n".join(out.split("\n")[1:-1]).strip()

    def running(self):
        return "running" in self.cmd("info status")

    def read(self, addr, size):
        path = os.path.join(self.tmp, "mem")
        err = self.cmd(f'pmemsave 0x{addr:x} {size} "{path}"')
        if err:
            raise IOError(f"pmemsave 0x{addr:x}+{size}: {err}")
        with open(path, "rb") as f:
            data = f.read()
        if len(data) != size:
            raise IOError(f"pmemsave 0x{addr:x}+{size}: short read")
        return data


def caps_names(caps):
    return [n for bit, n in sorted(CAPS.items()) if caps >> bit & 1]


def heap_label(start, caps):
    if caps >> 10 & 1:
        return "PSRAM"
    if caps >> 15 & 1 or 0x50000000 <= start < 0x50002000 or 0x600FE000 <= start < 0x60100000:
        return "RTC"
    if caps & 1 and not caps >> 2 & 1:
        return "IRAM"
    return "DRAM"


def walk_pool(mem, base, end, pool):
    """Walk the TLSF physical block list whose first size field is at `pool`.
    Returns [(header_addr, size, free)] up to the sentinel, or None if the
    chain is inconsistent (wrong guess, or the guest stopped mid-update)."""
    blocks, b, prev_free = [], pool - 4, 0
    while True:
        if b + 8 > end:
            return None
        size, = struct.unpack_from("<I", mem, b + 4 - base)
        bsize, free, pfree = size & ~3, size & 1, size >> 1 & 1
        if pfree != prev_free:
            return None
        if bsize == 0:                       # sentinel ends the pool
            return blocks if not free and end - (b + 8) < 64 else None
        if bsize < 12 or bsize & 3:
            return None
        blocks.append((b, bsize, free))
        prev_free, b = free, b + bsize + 4


def find_pool(mem, base, end, heap_data):
    """The pool starts right after the TLSF control structure, whose size
    depends on the IDF version and pool size: try each word until a walk fits."""
    lo = heap_data + 8
    for pool in range(lo - lo % 4, min(heap_data + 16384, end - 8), 4):
        size, = struct.unpack_from("<I", mem, pool - base)
        if size & 2 or (size & ~3) < 12 or (size & ~3) > end - pool:
            continue
        blocks = walk_pool(mem, base, end, pool)
        if blocks:
            return pool, blocks
    return None, None


class Viewer:
    def __init__(self, elf, mon, interval, trace=None, sym=None):
        self.head = elf_symbol(elf, "registered_heaps")
        self.trace, self.sym, self.sites = trace, sym, None
        self.elf = os.path.basename(elf)
        self.mon, self.interval = mon, interval
        self.lock = threading.Lock()
        self.pools = {}          # (region start, TLSF control) -> pool address
        self.last, self.last_time = None, 0
        self.history = []

    def heap_list(self):
        """registered_heaps is an SLIST of heap_t; CONFIG_HEAP_TASK_TRACKING
        prepends a name pointer. multi_heap_register() puts the handle at the
        region start, which tells the two layouts apart."""
        first, = struct.unpack("<I", self.mon.read(self.head, 4))
        heaps, node = [], first
        while node and len(heaps) < 32:
            raw = self.mon.read(node, 40)
            for skip in (0, 4):
                c0, c1, c2, start, end, _, _, heap, nxt = struct.unpack_from("<9I", raw, skip)
                if 0 < start == heap < end:
                    break
            else:
                raise IOError(f"heap_t at 0x{node:x} does not look like ESP-IDF 5.x")
            heaps.append(dict(caps=c0 | c1 | c2, start=start, end=end, handle=heap))
            node = nxt
        return heaps

    def read_heap(self, h):
        start, end = h["start"], h["end"]
        mem = self.mon.read(start, end - start)
        lock, free_b, min_free, pool_size, heap_data = struct.unpack_from("<5I", mem, h["handle"] - start)
        # The pool never moves, so once found a failed walk means the guest
        # stopped mid-update; rescanning then could lock onto a later block.
        pool = self.pools.get((start, heap_data))
        if pool:
            blocks = walk_pool(mem, start, end, pool)
        else:
            pool, blocks = find_pool(mem, start, end, heap_data)
            if blocks:
                self.pools[start, heap_data] = pool
        if not blocks:
            return None
        flat, used, free, largest, nfree = [], 0, 0, 0, 0
        for b, bsize, is_free in blocks:
            req, flags = 0, BLOCK_FREE if is_free else 0
            if is_free:
                free += bsize
                largest = max(largest, bsize)
                nfree += 1
            else:
                used += bsize
                canary, req = struct.unpack_from("<II", mem, b + 8 - start)
                if canary == HEAD_CANARY and req <= bsize - 12:
                    flags |= BLOCK_POISONED
                    tail, = struct.unpack_from("<I", mem, b + 16 + req - start)
                    if tail != TAIL_CANARY:
                        flags |= BLOCK_CORRUPT
                else:
                    req = 0
            # A block spans its size field and payload: [b + 4, b + 8 + bsize).
            flat += [b + 4 - start, bsize + 4, flags, req]
        caps = h["caps"]
        return dict(name=heap_label(start, caps), caps=caps_names(caps), start=start, end=end,
                    pool=pool - 4 - start, tail=blocks[-1][0] + blocks[-1][1] + 8 - start,
                    free=free, used=used, largest=largest, count=len(blocks), nfree=nfree,
                    infoFree=free_b, infoMinFree=min_free, poolSize=pool_size,
                    corrupt=sum(1 for i in range(2, len(flat), 4) if flat[i] & BLOCK_CORRUPT), blocks=flat)

    def snapshot(self):
        """Stop, read and walk every heap, resume. A heap caught mid-update is
        retried after letting the guest run a little."""
        mon = self.mon
        was_running = mon.running()
        heaps = None
        try:
            for attempt in range(8):
                if was_running:
                    mon.cmd("stop")
                hl = self.heap_list()
                heaps = [self.read_heap(h) for h in hl]
                if all(heaps) or not was_running:
                    break
                mon.cmd("cont")
                time.sleep(0.005 * (attempt + 1))
        finally:
            if was_running:
                mon.cmd("cont")
        heaps = [h for h in heaps or [] if h]
        traced = False
        if self.trace:
            self.trace.update()
            with self.trace.lock:
                traced = bool(self.trace.t)
                for h in heaps if traced else []:
                    h["sites"], h["stk"], h["ev"] = label_blocks(self.trace, h)
        snap = dict(time=time.time(), elf=self.elf, running=was_running, heaps=heaps, traced=traced,
                    traceGen=self.trace.gen if self.trace else 0,
                    traceEvents=len(self.trace.t) if self.trace else 0,
                    traceFormat=self.trace.format_ok if self.trace else True,
                    interval=self.interval, missing=len(hl) - len(heaps) if heaps is not None else 0)
        self.history.append(dict(t=snap["time"], h={f'{h["start"]:x}': [h["infoFree"], h["largest"], h["infoMinFree"]]
                                                    for h in heaps}))
        del self.history[:-900]
        return snap

    def get(self):
        with self.lock:
            if self.last is None or time.time() - self.last_time >= self.interval * 0.8:
                try:
                    self.last = self.snapshot()
                except (OSError, ConnectionError) as e:
                    self.last = dict(time=time.time(), elf=self.elf, error=f"Cannot read the guest: {e}", heaps=[],
                                     interval=self.interval)
                self.last_time = time.time()
            return self.last


def print_snapshot(snap):
    if "error" in snap:
        sys.exit(snap["error"])
    if not snap["heaps"]:
        print("no heaps registered yet")
    print(f'{"heap":6} {"range":23} {"size":>8} {"used":>8} {"free":>8} {"largest":>8} {"min free":>8} {"blocks":>6}  caps')
    for h in snap["heaps"]:
        print(f'{h["name"]:6} {h["start"]:08x}-{h["end"]:08x} {h["end"] - h["start"]:8} {h["used"]:8} {h["infoFree"]:8} '
              f'{h["largest"]:8} {h["infoMinFree"]:8} {h["count"]:6}  {",".join(h["caps"])}'
              + (f'  {h["corrupt"]} CORRUPT' if h["corrupt"] else ""))
    if snap.get("missing"):
        print(f'{snap["missing"]} heap(s) could not be walked (guest mid-update?)')


def query(path):
    return dict(kv.split("=", 1) for kv in path.split("?", 1)[1].split("&") if "=" in kv)


def serve(viewer, port):
    class Handler(BaseHTTPRequestHandler):
        def send(self, body, ctype):
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path in FILES:
                name, ctype = FILES[self.path]
                with open(os.path.join(HERE, name), "rb") as f:
                    self.send(f.read(), ctype)
            elif self.path == "/snapshot":
                self.send(json.dumps(viewer.get(), separators=(",", ":")).encode(), "application/json")
            elif self.path == "/history":
                self.send(json.dumps(viewer.history, separators=(",", ":")).encode(), "application/json")
            elif self.path.startswith("/stack?"):
                try:
                    sid = int(query(self.path).get("id", "-1"))
                except ValueError:
                    return self.send_error(400)
                stack = describe_stack(viewer.sym, viewer.trace, sid) if viewer.sym else None
                self.send(json.dumps(stack).encode(), "application/json")
            elif self.path.startswith("/events?"):
                q = query(self.path)
                try:
                    gen, start, nstacks = (int(q.get(k, "0")) for k in ("gen", "start", "stacks"))
                    nsites = int(q.get("sites", "0"))
                except ValueError:
                    return self.send_error(400)
                if not viewer.trace:
                    return self.send(b"null", "application/json")
                viewer.trace.update()
                ev = viewer.trace.events(gen, start, nstacks)
                names = viewer.sites.names if viewer.sites else []
                ev.update(siteStart=nsites, sites=names[nsites:])
                self.send(json.dumps(ev, separators=(",", ":")).encode(), "application/json")
            else:
                self.send_error(404)

        def log_message(self, *args):
            pass

    httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"heapview: http://127.0.0.1:{port}", file=sys.stderr)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("elf")
    ap.add_argument("--monitor", default="/tmp/eink-qemu-mon.sock", help="HMP unix socket")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--interval", type=float, default=1.0, help="seconds between snapshots")
    ap.add_argument("--once", action="store_true", help="print one snapshot and exit")
    ap.add_argument("--trace", default="/tmp/eink-qemu-heaptrace.bin", help="heaptrace plugin output")
    ap.add_argument("--addr2line", help="toolchain addr2line for symbolizing stacks")
    args = ap.parse_args()
    try:
        mon = Monitor(args.monitor)
    except OSError as e:
        sys.exit(f"heapview: cannot connect to {args.monitor}: {e} (is eink-qemu running?)")
    sym = Symbolizer(args.elf, args.addr2line)
    if not sym.tool:
        print("heapview: no addr2line found; stacks show symbol names only", file=sys.stderr)
    sites = Sites(sym)
    viewer = Viewer(args.elf, mon, args.interval, Trace(args.trace, sites.of), sym)
    viewer.sites = sites
    if args.once:
        print_snapshot(viewer.get())
    else:
        serve(viewer, args.port)


if __name__ == "__main__":
    main()
