//! Where heap blocks came from: the heaptrace plugin's event log
//! (contrib/plugins/heaptrace.c), its stacks symbolized from the firmware's
//! DWARF, and allocation sites. A port of heapview.py's Trace, Symbolizer
//! and Sites; the heap window (desktop/src/heap-window.ts) replays the events.

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const MAGIC: &[u8; 8] = b"HTRC\x02\0\0\0";
#[cfg(test)]
const ALLOC: u8 = 1;
const FREE: u8 = 2;
const REALLOC: u8 = 3;
/// Events sent to the window per request.
const EVENTS_PER_REQUEST: usize = 100_000;

/// Frames inside the allocator (or free) itself; the first frame after them is the caller.
static ALLOCATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(multi_heap_\w*(alloc|free)|heap_caps_\w*(alloc|free)|aligned_or_unaligned_alloc|tlsf_|",
        r"poison_|malloc$|calloc$|realloc$|valloc$|free$|_malloc_r$|_calloc_r$|_realloc_r$|_free_r$|",
        r"strn?dup$|_strn?dup_r$|ps_|operator new|operator delete|std::__new_allocator<|__gnu_cxx::new_allocator<|std::allocator<|",
        r"std::allocator_traits<)"
    ))
    .unwrap()
});
/// Source outside the firmware's own tree: toolchain, ESP-IDF, Arduino core, libraries.
static LIBRARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/(\.platformio/packages|esp32-arduino-lib-builder|crosstool-NG|libdeps|managed_components|framework-[^/]+)/").unwrap()
});

/// Code addresses to functions and source lines, from the ELF's DWARF,
/// falling back to its symbol table.
pub struct Symbols {
    loader: Option<addr2line::Loader>,
    /// Sorted (address, size, demangled name) of every function symbol.
    functions: Vec<(u32, u32, String)>,
    cache: HashMap<u32, Vec<(String, String)>>,
}

/// Function symbols from an ELF's symbol table, sorted by address.
fn elf_functions(data: &[u8]) -> Vec<(u32, u32, String)> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let mut functions = Vec::new();
    let (Some(shoff), Some(shentsize), Some(shnum)) = (u32_at(0x20), u16_at(0x2E), u16_at(0x30)) else { return functions };
    let section = |i: usize| -> Option<(u32, usize, usize, usize)> {
        let at = shoff as usize + i * shentsize;
        Some((u32_at(at + 4)?, u32_at(at + 16)? as usize, u32_at(at + 20)? as usize, u32_at(at + 24)? as usize))
    };
    for i in 0..shnum {
        let Some((2, offset, size, link)) = section(i) else { continue }; // SHT_SYMTAB
        let Some((_, strtab, _, _)) = section(link) else { continue };
        for at in (offset..offset + size).step_by(16) {
            let (Some(name), Some(value), Some(size), Some(&info)) = (u32_at(at), u32_at(at + 4), u32_at(at + 8), data.get(at + 12)) else { break };
            if info & 0xF != 2 || value == 0 {
                continue; // STT_FUNC
            }
            let Some(name) = data.get(strtab + name as usize..) else { continue };
            let name = String::from_utf8_lossy(&name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())]).into_owned();
            let name = addr2line::demangle(&name, addr2line::gimli::DW_LANG_C_plus_plus).unwrap_or(name);
            functions.push((value, size, name));
        }
    }
    functions.sort();
    functions
}

impl Symbols {
    pub fn new(elf: &Path) -> Symbols {
        let functions = std::fs::read(elf).map(|data| elf_functions(&data)).unwrap_or_default();
        Symbols { loader: addr2line::Loader::new(elf).ok(), functions, cache: HashMap::new() }
    }

    /// The function symbol containing `addr`.
    fn function(&self, addr: u32) -> Option<&(u32, u32, String)> {
        let i = self.functions.partition_point(|f| f.0 <= addr).checked_sub(1)?;
        let f = &self.functions[i];
        (addr < f.0.saturating_add(f.1.max(1))).then_some(f)
    }

    /// (function, "file:line") for a code address, innermost inline first.
    fn lookup(&mut self, addr: u32) -> &[(String, String)] {
        if !self.cache.contains_key(&addr) {
            let mut chain = Vec::new();
            if let Some(Ok(mut frames)) = self.loader.as_ref().map(|loader| loader.find_frames(addr as u64)) {
                while let Ok(Some(frame)) = frames.next() {
                    let name = frame.function.as_ref().and_then(|f| f.demangle().ok()).map(|n| n.into_owned());
                    let loc = frame.location.as_ref().and_then(|l| Some(format!("{}:{}", l.file?, l.line?))).unwrap_or_default();
                    chain.push((name.unwrap_or_else(|| "??".into()), loc));
                }
            }
            let symbol = self.function(addr);
            match (chain.last_mut(), symbol) {
                // DWARF names a function without a linkage name (a static or
                // anonymous-namespace C++ function, or a clone) by its bare
                // name; the symbol table has the full one, as binutils prints it.
                (Some((name, _)), Some((_, _, full))) if name != "??" && !name.contains('(') && full.contains('(') && full.contains(name.as_str()) => {
                    *name = full.clone();
                }
                (Some((name, _)), _) if name != "??" => {}
                _ => {
                    let name = symbol.map_or_else(|| "??".into(), |(start, _, name)| format!("{name}+{:#x}", addr - start));
                    chain = vec![(name, String::new())];
                }
            }
            self.cache.insert(addr, chain);
        }
        &self.cache[&addr]
    }
}

#[derive(Serialize)]
pub struct Inline {
    #[serde(rename = "fn")]
    name: String,
    loc: String,
}

#[derive(Serialize)]
pub struct Frame {
    addr: u32,
    #[serde(rename = "fn")]
    name: String,
    loc: String,
    inl: Vec<Inline>,
    alloc: bool,
    lib: bool,
}

/// A stack's frames, the first frame outside the allocator (`caller`) and
/// the first of the firmware's own frames from there (`site`).
#[derive(Serialize)]
pub struct Stack {
    frames: Vec<Frame>,
    caller: usize,
    site: usize,
}

pub fn stack_frames(symbols: &mut Symbols, addrs: &[u32]) -> Stack {
    let mut frames = Vec::with_capacity(addrs.len());
    for &addr in addrs {
        // A return address points after the call; look up the call itself.
        let chain = symbols.lookup(addr.wrapping_sub(1));
        let (name, loc) = chain.last().cloned().unwrap_or_default(); // the outermost function
        frames.push(Frame {
            addr,
            alloc: ALLOCATOR.is_match(&name),
            lib: LIBRARY.is_match(&loc),
            inl: chain[..chain.len().saturating_sub(1)].iter().map(|(name, loc)| Inline { name: name.clone(), loc: loc.clone() }).collect(),
            name,
            loc,
        });
    }
    let caller = frames.iter().position(|f| !f.alloc).unwrap_or(frames.len());
    let site = (caller..frames.len()).find(|&i| !frames[i].lib && !frames[i].loc.is_empty()).unwrap_or(caller);
    Stack { frames, caller, site }
}

/// Allocation sites (the function a stack reports as "allocated by"),
/// numbered in the order they are first seen so a site keeps its number,
/// and its color in the window, while the ELF stays the same.
#[derive(Default)]
pub struct Sites {
    ids: HashMap<String, i32>,
    pub names: Vec<Value>,
}

impl Sites {
    pub fn of(&mut self, symbols: &mut Symbols, stack: &[u32]) -> i32 {
        let Stack { frames, site, .. } = stack_frames(symbols, stack);
        let (name, loc) = frames.get(site).map_or(("??".to_string(), String::new()), |f| (f.name.clone(), f.loc.clone()));
        if let Some(&id) = self.ids.get(&name) {
            return id;
        }
        let id = self.names.len() as i32;
        self.ids.insert(name.clone(), id);
        self.names.push(json!({"fn": name, "loc": loc}));
        id
    }
}

/// One allocation site's activity in a window (`Trace::window`).
#[derive(Debug, Clone, Serialize)]
pub struct SiteWindow {
    /// Index into `Sites::names`; -1 when the stack had no site.
    pub site: i32,
    pub allocs: u64,
    pub alloc_bytes: u64,
    /// Blocks allocated in the window and freed in it.
    pub freed: u64,
    pub freed_bytes: u64,
    /// Blocks allocated in the window and still live at its end.
    pub kept: u64,
    pub kept_bytes: u64,
    /// Blocks allocated before the window and freed in it.
    pub older_freed: u64,
    pub older_freed_bytes: u64,
}

impl SiteWindow {
    fn new(site: i32) -> Self {
        SiteWindow { site, allocs: 0, alloc_bytes: 0, freed: 0, freed_bytes: 0, kept: 0, kept_bytes: 0, older_freed: 0, older_freed_bytes: 0 }
    }
}

/// The plugin's log: after an 8-byte "HTRC" v2 header, records of u16 kind,
/// u16 frame count, u32 pointer, u32 size, u32 old pointer, u64 microseconds
/// since the epoch, then the return addresses. Kept as columns, with each
/// distinct stack stored once, and replayed into `live`.
pub struct Trace {
    path: PathBuf,
    /// Changes whenever the log starts over, so the window starts over too.
    pub gen: u64,
    pos: u64,
    rest: Vec<u8>,
    header: bool,
    /// False when the log comes from an older plugin.
    pub format_ok: bool,
    t: Vec<u64>,
    kind: Vec<u8>,
    ptr: Vec<u32>,
    size: Vec<u32>,
    old: Vec<u32>,
    stack: Vec<u32>,
    stack_ids: HashMap<Vec<u32>, u32>,
    stacks: Vec<Vec<u32>>,
    stack_site: Vec<i32>,
    /// pointer -> (size, stack id, event index)
    live: HashMap<u32, (u32, u32, u32)>,
}

impl Trace {
    pub fn new(path: PathBuf, gen: u64) -> Trace {
        Trace {
            path,
            gen,
            pos: 0,
            rest: Vec::new(),
            header: false,
            format_ok: true,
            t: Vec::new(),
            kind: Vec::new(),
            ptr: Vec::new(),
            size: Vec::new(),
            old: Vec::new(),
            stack: Vec::new(),
            stack_ids: HashMap::new(),
            stacks: Vec::new(),
            stack_site: Vec::new(),
            live: HashMap::new(),
        }
    }

    pub fn events_len(&self) -> usize {
        self.t.len()
    }

    pub fn traced(&self) -> bool {
        !self.t.is_empty()
    }

    /// Read what the plugin appended since the last call. A shorter file
    /// means it started over.
    pub fn update(&mut self, mut site_of: impl FnMut(&[u32]) -> i32) {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return };
        let Ok(len) = file.metadata().map(|m| m.len()) else { return };
        if len < self.pos {
            *self = Trace::new(std::mem::take(&mut self.path), self.gen + 1);
        }
        if len == self.pos || !self.format_ok || file.seek(SeekFrom::Start(self.pos)).is_err() {
            return;
        }
        let mut chunk = Vec::new();
        if file.read_to_end(&mut chunk).is_err() {
            return;
        }
        self.pos += chunk.len() as u64; // the file may have grown since the length was read
        let mut data = std::mem::take(&mut self.rest);
        data.extend_from_slice(&chunk);
        let mut off = 0;
        if !self.header {
            if data.len() < 8 {
                self.rest = data; // the header is not complete yet
                return;
            }
            self.header = true;
            if &data[..8] != MAGIC {
                self.format_ok = false;
                return;
            }
            off = 8;
        }
        let u16_at = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        while off + 24 <= data.len() {
            let n = u16_at(off + 2) as usize;
            if off + 24 + 4 * n > data.len() {
                break;
            }
            let kind = u16_at(off) as u8;
            let (ptr, size, old) = (u32_at(off + 4), u32_at(off + 8), u32_at(off + 12));
            let t = u64::from_le_bytes(data[off + 16..off + 24].try_into().unwrap());
            let frames: Vec<u32> = (0..n).map(|i| u32_at(off + 24 + 4 * i)).collect();
            off += 24 + 4 * n;
            let sid = match self.stack_ids.get(&frames) {
                Some(&sid) => sid,
                None => {
                    let sid = self.stacks.len() as u32;
                    self.stack_site.push(site_of(&frames));
                    self.stack_ids.insert(frames.clone(), sid);
                    self.stacks.push(frames);
                    sid
                }
            };
            let i = self.t.len() as u32;
            self.t.push(t);
            self.kind.push(kind);
            self.stack.push(sid);
            self.ptr.push(ptr);
            self.size.push(size);
            self.old.push(old);
            if kind == FREE {
                self.live.remove(&ptr);
            } else {
                if kind == REALLOC {
                    self.live.remove(&old);
                }
                self.live.insert(ptr, (size, sid, i));
            }
        }
        self.rest = data.split_off(off);
    }

    /// Events from index `start` as columns, times in microseconds after the
    /// first event, with the stack -> site entries and site names the caller
    /// lacks. A different `gen` starts the caller over.
    pub fn events(&self, gen: u64, start: usize, stacks: usize, sites: usize, names: &[Value]) -> Value {
        let (start, stacks) = if gen == self.gen { (start, stacks) } else { (0, 0) };
        let start = start.min(self.t.len());
        let end = self.t.len().min(start + EVENTS_PER_REQUEST);
        let t0 = self.t.first().copied().unwrap_or(0);
        json!({
            "gen": self.gen,
            "start": start,
            "total": self.t.len(),
            "t0": t0 as f64 / 1000.0,
            "t": self.t[start..end].iter().map(|t| t - t0).collect::<Vec<_>>(),
            "k": &self.kind[start..end],
            "p": &self.ptr[start..end],
            "s": &self.size[start..end],
            "o": &self.old[start..end],
            "st": &self.stack[start..end],
            "stackStart": stacks,
            "stackSite": self.stack_site.get(stacks..).unwrap_or_default(),
            "siteStart": sites,
            "sites": names.get(sites..).unwrap_or_default(),
        })
    }

    /// What happened between host times `from` and `to` (microseconds
    /// since the epoch, the log's clock), by allocation site: blocks
    /// allocated in the window, those of them still live at its end, and
    /// older blocks freed in it (attributed to the site that allocated them).
    pub fn window(&self, from: u64, to: u64) -> Vec<SiteWindow> {
        let mut by_site: HashMap<i32, SiteWindow> = HashMap::new();
        // pointer -> (size, site, allocated inside the window)
        let mut live: HashMap<u32, (u32, i32, bool)> = HashMap::new();
        for i in 0..self.t.len() {
            let t = self.t[i];
            if t > to {
                break;
            }
            let inside = t >= from;
            let site = self.stack_site.get(self.stack[i] as usize).copied().unwrap_or(-1);
            let (kind, ptr, size, old) = (self.kind[i], self.ptr[i], self.size[i], self.old[i]);
            let mut release = |p: u32, by_site: &mut HashMap<i32, SiteWindow>| {
                if let Some((bytes, owner, new)) = live.remove(&p) {
                    if inside {
                        let w = by_site.entry(owner).or_insert_with(|| SiteWindow::new(owner));
                        if new {
                            w.freed += 1;
                            w.freed_bytes += u64::from(bytes);
                        } else {
                            w.older_freed += 1;
                            w.older_freed_bytes += u64::from(bytes);
                        }
                    }
                }
            };
            if kind == FREE {
                release(ptr, &mut by_site);
                continue;
            }
            if kind == REALLOC {
                release(old, &mut by_site);
            }
            live.insert(ptr, (size, site, inside));
            if inside {
                let w = by_site.entry(site).or_insert_with(|| SiteWindow::new(site));
                w.allocs += 1;
                w.alloc_bytes += u64::from(size);
            }
        }
        for (bytes, site, new) in live.values() {
            if *new {
                let w = by_site.entry(*site).or_insert_with(|| SiteWindow::new(*site));
                w.kept += 1;
                w.kept_bytes += u64::from(*bytes);
            }
        }
        let mut sites: Vec<SiteWindow> = by_site.into_values().collect();
        sites.sort_by(|a, b| b.kept_bytes.cmp(&a.kept_bytes).then(b.alloc_bytes.cmp(&a.alloc_bytes)));
        sites
    }

    pub fn stack(&self, id: usize) -> Option<&[u32]> {
        self.stacks.get(id).map(Vec::as_slice)
    }

    /// The site, stack and allocation event of each block in a heap's block
    /// list, -1 where the log has no matching live allocation.
    pub fn label(&self, start: u32, blocks: &[u32]) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
        let (mut sites, mut stacks, mut born) = (Vec::new(), Vec::new(), Vec::new());
        for b in blocks.as_chunks::<4>().0 {
            // The list holds block offsets + 4; the user pointer is block + 16.
            let entry = (b[2] & crate::heap::BLOCK_POISONED != 0)
                .then(|| self.live.get(&(start + b[0] + 12)))
                .flatten()
                .filter(|entry| entry.0 == b[3]);
            match entry {
                Some(&(_, sid, i)) => {
                    sites.push(self.stack_site[sid as usize]);
                    stacks.push(sid as i32);
                    born.push(i as i32);
                }
                None => {
                    sites.push(-1);
                    stacks.push(-1);
                    born.push(-1);
                }
            }
        }
        (sites, stacks, born)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u16, ptr: u32, size: u32, old: u32, t: u64, frames: &[u32]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(kind.to_le_bytes());
        r.extend((frames.len() as u16).to_le_bytes());
        for v in [ptr, size, old] {
            r.extend(v.to_le_bytes());
        }
        r.extend(t.to_le_bytes());
        for f in frames {
            r.extend(f.to_le_bytes());
        }
        r
    }

    #[test]
    fn reads_the_log_as_it_grows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heaptrace.bin");
        let mut log = MAGIC.to_vec();
        log.extend(record(ALLOC as u16, 0x100, 16, 0, 1_000, &[1, 2]));
        log.extend(record(ALLOC as u16, 0x200, 32, 0, 2_000, &[1, 2]));
        let half = record(REALLOC as u16, 0x300, 64, 0x100, 3_000, &[3]);
        log.extend(&half[..10]);
        std::fs::write(&path, &log).unwrap();
        let mut trace = Trace::new(path.clone(), 1);
        let mut seen = Vec::new();
        trace.update(|stack| {
            seen.push(stack.to_vec());
            seen.len() as i32 - 1
        });
        assert_eq!(trace.events_len(), 2);
        assert_eq!(seen, [vec![1, 2]]); // one distinct stack
        // The rest of the realloc, then a free.
        log.extend(&half[10..]);
        log.extend(record(FREE as u16, 0x200, 0, 0, 4_000, &[4]));
        std::fs::write(&path, &log).unwrap();
        trace.update(|_| 7);
        assert_eq!(trace.events_len(), 4);
        assert_eq!(trace.live, HashMap::from([(0x300, (64, 1, 2))]));
        let ev = trace.events(1, 1, 1, 0, &[]);
        assert_eq!(ev["t"], json!([1000, 2000, 3000]));
        assert_eq!(ev["k"], json!([1, 3, 2]));
        assert_eq!(ev["stackSite"], json!([7, 7]));
        // Another generation starts the caller over.
        assert_eq!(trace.events(9, 3, 3, 0, &[])["start"], 0);
        // A shorter file is a new log.
        std::fs::write(&path, MAGIC).unwrap();
        trace.update(|_| 0);
        assert_eq!((trace.events_len(), trace.gen), (0, 2));
    }

    #[test]
    fn rejects_an_older_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heaptrace.bin");
        std::fs::write(&path, b"HTRC\x01\0\0\0").unwrap();
        let mut trace = Trace::new(path, 1);
        trace.update(|_| 0);
        assert!(!trace.format_ok);
    }

    #[test]
    fn labels_blocks_from_live_allocations() {
        let mut trace = Trace::new(PathBuf::new(), 1);
        trace.stacks = vec![vec![1]];
        trace.stack_site = vec![5];
        trace.live.insert(0x1000 + 0x20 + 12, (40, 0, 9));
        // A poisoned block whose request matches, one whose does not, a free one.
        let blocks = [0x20, 68, 2, 40, 0x64, 68, 2, 41, 0xa8, 68, 1, 0];
        assert_eq!(trace.label(0x1000, &blocks), (vec![5, -1, -1], vec![0, -1, -1], vec![9, -1, -1]));
    }

    /// heapview.py parity on a real log: EINK_QEMU_TEST_ELF and
    /// EINK_QEMU_TEST_TRACE name the firmware and the plugin's output; the
    /// events and site names print as heapview.py's /events and /stack would.
    #[test]
    #[ignore]
    fn symbolizes_a_real_log() {
        let elf = std::env::var("EINK_QEMU_TEST_ELF").unwrap();
        let mut symbols = Symbols::new(Path::new(&elf));
        let mut sites = Sites::default();
        let mut trace = Trace::new(std::env::var("EINK_QEMU_TEST_TRACE").unwrap().into(), 0);
        let started = std::time::Instant::now();
        trace.update(|stack| sites.of(&mut symbols, stack));
        eprintln!("{} events, {} stacks, {} sites in {:?}", trace.events_len(), trace.stacks.len(), sites.names.len(), started.elapsed());
        let stack = stack_frames(&mut symbols, &trace.stacks[3]);
        println!("{}", json!({"events": trace.events(0, 0, 0, 0, &sites.names), "stack3": stack}));
    }

    #[test]
    fn finds_the_caller_and_the_site() {
        let mut symbols = Symbols { loader: None, functions: Vec::new(), cache: HashMap::new() };
        let names = [
            ("multi_heap_malloc", "/x/.platformio/packages/framework-espidf/heap.c:1"),
            ("heap_caps_malloc", "/x/.platformio/packages/framework-espidf/heap.c:2"),
            ("std::vector<int>::push_back(int)", "/x/.platformio/packages/toolchain/vector:3"),
            ("Reader::open()", "/work/src/Reader.cpp:203"),
        ];
        for (i, (name, loc)) in names.iter().enumerate() {
            symbols.cache.insert(i as u32, vec![(name.to_string(), loc.to_string())]);
        }
        let stack = stack_frames(&mut symbols, &[1, 2, 3, 4]);
        assert_eq!((stack.caller, stack.site), (2, 3));
        assert!(stack.frames[0].alloc && stack.frames[2].lib && !stack.frames[3].lib);
        let mut sites = Sites::default();
        assert_eq!(sites.of(&mut symbols, &[1, 2, 3, 4]), 0);
        assert_eq!(sites.of(&mut symbols, &[2, 4]), 0);
        assert_eq!(sites.names[0]["fn"], "Reader::open()");
    }
}
