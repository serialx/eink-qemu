//! Heap view: the running firmware's ESP-IDF 5.x heaps, block by block.
//!
//! A port of `eink-qemu/heapview.py`'s snapshot. The firmware's ELF names
//! `registered_heaps`, the list of heaps; each refresh stops the guest,
//! copies every heap region, walks its TLSF block list and resumes. Under
//! -icount the virtual clock stops too, so the guest does not see the pause.
//!
//! The ELF must be the running app's: the guest's `esp_app_desc` carries the
//! SHA-256 of the ELF it was built from (or, without one, its version, name
//! and build time, which are compared instead). Without an ELF the heap list
//! is found by scanning internal DRAM for its entries (`find_list`).

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const EM_XTENSA: u16 = 94;
const EM_RISCV: u16 = 243;
/// More heaps than any ESP32 registers: a longer list is not a heap list.
const MAX_HEAPS: usize = 32;
/// The largest heap region read (the X4 Pro's PSRAM is 8 MiB).
const MAX_REGION: u32 = 32 * 1024 * 1024;
/// Snapshots retried when the guest was stopped mid-update.
const ATTEMPTS: u32 = 8;

// esp_app_desc_t (esp_app_desc.h)
const APP_DESC_SIZE: usize = 256;
const APP_DESC_MAGIC: u32 = 0xABCD_5432;
const APP_DESC_TEXT: std::ops::Range<usize> = 16..144;
const APP_DESC_SHA: std::ops::Range<usize> = 144..176;

/// MALLOC_CAP_* bits (esp_heap_caps.h).
const CAPS: [(u32, &str); 10] = [
    (0, "EXEC"), (1, "32BIT"), (2, "8BIT"), (3, "DMA"), (10, "SPIRAM"),
    (11, "INTERNAL"), (12, "DEFAULT"), (13, "IRAM_8BIT"), (14, "RETENTION"), (15, "RTCRAM"),
];
/// CONFIG_HEAP_POISONING_LIGHT canaries.
const HEAD_CANARY: u32 = 0xABBA_1234;
const TAIL_CANARY: u32 = 0xBAAD_5678;
/// Flags in a snapshot's block list.
pub const BLOCK_FREE: u32 = 1;
pub const BLOCK_POISONED: u32 = 2;
pub const BLOCK_CORRUPT: u32 = 4;

/// What the heap view needs from a firmware ELF.
#[derive(Debug)]
pub struct Elf {
    pub path: PathBuf,
    registered_heaps: u32,
    /// `esp_app_desc`'s address and its bytes in the ELF.
    app_desc: Option<(u32, Vec<u8>)>,
    sha256: [u8; 32],
    /// Why the heaptrace plugin cannot record this firmware's allocations.
    untraceable: Option<String>,
    /// Length and modification time when read: a rebuilt ELF is read again.
    stamp: (u64, Option<SystemTime>),
}

fn stamp(path: &Path) -> std::io::Result<(u64, Option<SystemTime>)> {
    let meta = std::fs::metadata(path)?;
    Ok((meta.len(), meta.modified().ok()))
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

impl Elf {
    pub fn load(path: &Path) -> Result<Elf, String> {
        let stamp = stamp(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let data = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let mut elf = Elf::parse(&data)?;
        elf.path = path.to_path_buf();
        elf.stamp = stamp;
        Ok(elf)
    }

    /// True when the file changed since it was read.
    pub fn stale(&self) -> bool {
        stamp(&self.path).map_or(true, |now| now != self.stamp)
    }

    fn parse(data: &[u8]) -> Result<Elf, String> {
        if data.get(..4) != Some(b"\x7fELF") || data.get(4) != Some(&1) || data.get(5) != Some(&1) {
            return Err("This is not a 32-bit little-endian ELF file.".into());
        }
        let malformed = || "The ELF file is truncated or malformed.".to_string();
        if !matches!(u16_at(data, 0x12), Some(EM_XTENSA | EM_RISCV)) {
            return Err("This ELF is not ESP32 firmware (Xtensa or RISC-V).".into());
        }
        let shoff = u32_at(data, 0x20).ok_or_else(malformed)? as usize;
        let shentsize = u16_at(data, 0x2E).ok_or_else(malformed)? as usize;
        let shnum = u16_at(data, 0x30).ok_or_else(malformed)? as usize;
        let shstrndx = u16_at(data, 0x32).ok_or_else(malformed)? as usize;
        if shentsize < 40 {
            return Err(malformed());
        }
        // (type, addr, offset, size, link)
        let sections = (0..shnum)
            .map(|i| {
                let at = shoff + i * shentsize;
                Some((u32_at(data, at + 4)?, u32_at(data, at + 12)?, u32_at(data, at + 16)? as usize, u32_at(data, at + 20)? as usize, u32_at(data, at + 24)? as usize))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(malformed)?;
        let c_str = |at: usize| data.get(at..).map(|s| &s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]);
        let section_names: Vec<&[u8]> = match sections.get(shstrndx) {
            Some(&(_, _, strtab, _, _)) => (0..shnum).filter_map(|i| c_str(strtab + u32_at(data, shoff + i * shentsize)? as usize)).collect(),
            None => Vec::new(),
        };
        let (mut heaps, mut desc) = (None, None);
        let mut hooks = [("poison_allocated_region", false), ("multi_heap_free", false), ("multi_heap_realloc", false)];
        for &(kind, _, offset, size, link) in &sections {
            if kind != 2 {
                continue; // SHT_SYMTAB
            }
            let strtab = sections.get(link).ok_or_else(malformed)?.2;
            for at in (offset..offset + size).step_by(16) {
                let (name, value) = (u32_at(data, at).ok_or_else(malformed)? as usize, u32_at(data, at + 4).ok_or_else(malformed)?);
                let name = data.get(strtab + name..).ok_or_else(malformed)?;
                let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
                match name {
                    b"registered_heaps" => heaps = Some(value),
                    b"esp_app_desc" => desc = Some(value),
                    _ => {
                        for hook in hooks.iter_mut().filter(|hook| hook.0.as_bytes() == name) {
                            hook.1 = true;
                        }
                    }
                }
            }
        }
        let registered_heaps = heaps.ok_or("This ELF has no registered_heaps symbol: choose the ELF of an ESP-IDF 5.x app, with symbols.")?;
        // The descriptor's bytes from the section that holds it (SHT_PROGBITS).
        let app_desc = desc.and_then(|addr| {
            let &(_, base, offset, _, _) = sections.iter().find(|&&(kind, base, _, size, _)| {
                kind == 1 && base <= addr && (addr - base) as usize + APP_DESC_SIZE <= size
            })?;
            let at = offset + (addr - base) as usize;
            Some((addr, data.get(at..at + APP_DESC_SIZE)?.to_vec()))
        });
        // What contrib/plugins/heaptrace.c needs: its hooks, and on RISC-V
        // unwind tables (Xtensa unwinds through its register windows).
        let untraceable = if !hooks[0].1 {
            Some("the firmware has no poison_allocated_region(); build it with CONFIG_HEAP_POISONING_LIGHT (the default)".to_string())
        } else if let Some((name, _)) = hooks.iter().find(|hook| !hook.1) {
            Some(format!("the ELF has no {name}()"))
        } else if u16_at(data, 0x12) == Some(EM_RISCV) && !section_names.iter().any(|n| *n == b".debug_frame" || *n == b".eh_frame") {
            Some("the ELF has no .debug_frame or .eh_frame to unwind RISC-V stacks".to_string())
        } else {
            None
        };
        Ok(Elf {
            path: PathBuf::new(),
            registered_heaps,
            app_desc,
            sha256: Sha256::digest(data).into(),
            untraceable,
            stamp: (0, None),
        })
    }
}

/// The guest, as the snapshot sees it: physical memory and the run state.
pub trait Guest {
    fn read(&mut self, addr: u32, size: u32) -> impl std::future::Future<Output = Result<Vec<u8>, String>> + Send;
    fn stop(&mut self) -> impl std::future::Future<Output = Result<(), String>> + Send;
    fn cont(&mut self) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

#[derive(Debug)]
pub enum HeapError {
    /// Stopping or resuming the guest failed: the run is in an unknown state.
    Guest(String),
    /// The running firmware is not the ELF's.
    Mismatch(String),
    /// Guest memory could not be read or did not hold a heap list.
    Read(String),
}

/// One heap. `blocks` is flat: offset from `start` (of the size field),
/// length (size field and payload), BLOCK_* flags, requested size.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Heap {
    pub name: &'static str,
    pub caps: Vec<&'static str>,
    pub start: u32,
    pub end: u32,
    /// Offset of the first block's header, after the heap and TLSF control.
    pub pool: u32,
    /// Offset of the end of the sentinel.
    pub tail: u32,
    pub free: u32,
    pub used: u32,
    pub largest: u32,
    pub count: u32,
    pub nfree: u32,
    /// multi_heap_info's own counters.
    pub info_free: u32,
    pub info_min_free: u32,
    pub corrupt: u32,
    pub blocks: Vec<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Milliseconds since the Unix epoch.
    pub time: u64,
    pub heaps: Vec<Heap>,
    /// Heaps caught mid-update in every attempt, left out.
    pub missing: usize,
}

/// What a run's snapshots learned: each heap's pool start by (region start,
/// TLSF control address), and without an ELF where the heap list is.
#[derive(Default)]
pub struct Pools {
    starts: HashMap<(u32, u32), u32>,
    list: Option<u32>,
}

impl Pools {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Where the heap list comes from: the ELF's `registered_heaps`, or a scan
/// of this internal DRAM range.
pub enum Lookup<'a> {
    Elf(&'a Elf),
    Scan(std::ops::Range<u32>),
}

/// Internal DRAM, where ESP-IDF keeps the heap list, by machine.
pub fn dram(machine: &str) -> std::ops::Range<u32> {
    match machine {
        "x3" | "x4" => 0x3FC8_0000..0x3FCE_0000,
        _ => 0x3FC8_8000..0x3FD0_0000,
    }
}

struct HeapEntry {
    caps: u32,
    start: u32,
    end: u32,
    handle: u32,
}

fn caps_names(caps: u32) -> Vec<&'static str> {
    CAPS.iter().filter(|(bit, _)| caps >> bit & 1 == 1).map(|&(_, name)| name).collect()
}

fn heap_label(start: u32, caps: u32) -> &'static str {
    if caps >> 10 & 1 == 1 {
        "PSRAM"
    } else if caps >> 15 & 1 == 1 || (0x5000_0000..0x5000_2000).contains(&start) || (0x600F_E000..0x6010_0000).contains(&start) {
        "RTC"
    } else if caps & 1 == 1 && caps >> 2 & 1 == 0 {
        "IRAM"
    } else {
        "DRAM"
    }
}

/// A TLSF block: header address, size, free.
type Block = (u32, u32, bool);

/// Walk the TLSF physical block list whose first size field is at `pool`,
/// up to the sentinel. None if the chain is inconsistent (a wrong guess, or
/// the guest stopped mid-update).
fn walk_pool(mem: &[u8], base: u32, end: u32, pool: u32) -> Option<Vec<Block>> {
    let (mut blocks, mut b, mut prev_free) = (Vec::new(), pool.checked_sub(4)?, false);
    loop {
        if b as u64 + 8 > end as u64 {
            return None;
        }
        let size = u32_at(mem, (b + 4 - base) as usize)?;
        let (bsize, free, pfree) = (size & !3, size & 1 == 1, size & 2 == 2);
        if pfree != prev_free {
            return None;
        }
        if bsize == 0 {
            // The sentinel ends the pool.
            return (!free && end - (b + 8) < 64).then_some(blocks);
        }
        if bsize < 12 {
            return None;
        }
        blocks.push((b, bsize, free));
        prev_free = free;
        b = b.checked_add(bsize)?.checked_add(4)?;
    }
}

/// The pool starts right after the TLSF control structure, whose size
/// depends on the IDF version and pool size: try each word until a walk fits.
fn find_pool(mem: &[u8], base: u32, end: u32, heap_data: u32) -> Option<(u32, Vec<Block>)> {
    let lo = heap_data.checked_add(8)? & !3;
    let hi = heap_data.saturating_add(16384).min(end.saturating_sub(8));
    (lo.max(base)..hi).step_by(4).find_map(|pool| {
        let size = u32_at(mem, (pool - base) as usize)?;
        if size & 2 != 0 || (size & !3) < 12 || (size & !3) > end - pool {
            return None;
        }
        walk_pool(mem, base, end, pool).filter(|blocks| !blocks.is_empty()).map(|blocks| (pool, blocks))
    })
}

/// A heap_t at `at`, with the offset of its fields: registered_heaps is an
/// SLIST of heap_t; CONFIG_HEAP_TASK_TRACKING prepends a name pointer.
/// multi_heap_register() puts the handle at the region start, which tells
/// the two layouts apart. Returns the entry, the next node and the layout.
fn entry_at(raw: &[u8]) -> Option<(HeapEntry, u32, usize)> {
    [0, 4].into_iter().find_map(|skip| {
        let word = |i: usize| u32_at(raw, skip + 4 * i).unwrap_or(0);
        let (start, end, handle) = (word(3), word(4), word(7));
        (start > 0 && start == handle && start < end && end - start <= MAX_REGION)
            .then(|| (HeapEntry { caps: word(0) | word(1) | word(2), start, end, handle }, word(8), skip))
    })
}

/// Without an ELF: the first node of the heap list in `mem` (read from
/// `base`). heap_caps_init() copies its heap_t array into the first
/// internal heap and links it into registered_heaps; the longest chain of
/// entries joined by `next`, starting at one no other entry points to, is
/// the list. Stale copies (the init's stack array) do not chain, and every
/// entry of a chain has the same layout, which rules out an entry read four
/// bytes off as the other layout.
pub fn find_list(mem: &[u8], base: u32) -> Option<u32> {
    let mut nodes: HashMap<u32, (u32, usize)> = HashMap::new();
    for at in (0..mem.len().saturating_sub(40)).step_by(4) {
        let Some((entry, next, skip)) = entry_at(&mem[at..at + 40]) else { continue };
        // MALLOC_CAP_INVALID (bit 31) never marks a registered heap.
        let caps_ok = entry.caps != 0 && entry.caps >> 31 == 0 && entry.start % 4 == 0;
        if caps_ok && next % 4 == 0 {
            nodes.insert(base + at as u32, (next, skip));
        }
    }
    let pointed: std::collections::HashSet<u32> = nodes.values().map(|&(next, _)| next).collect();
    let chain = |first: u32| {
        let layout = nodes[&first].1;
        let (mut n, mut len) = (first, 0);
        while let Some(&(next, skip)) = nodes.get(&n) {
            len += 1;
            if len > MAX_HEAPS || skip != layout { return 0; }
            n = next;
        }
        if n == 0 { len } else { 0 }
    };
    let mut heads: Vec<u32> = nodes.keys().copied().filter(|n| !pointed.contains(n)).collect();
    heads.sort_unstable();
    heads.into_iter().map(|n| (chain(n), n)).filter(|&(len, _)| len > 0).max_by_key(|&(len, n)| (len, std::cmp::Reverse(n))).map(|(_, n)| n)
}

async fn heap_list<G: Guest>(guest: &mut G, first: u32) -> Result<Vec<HeapEntry>, String> {
    let (mut heaps, mut node) = (Vec::new(), first);
    while node != 0 {
        if heaps.len() == MAX_HEAPS {
            return Err("The heap list does not end: is the ELF the running firmware's?".into());
        }
        let raw = guest.read(node, 40).await?;
        let Some((entry, next, _)) = entry_at(&raw) else {
            return Err(format!("The heap entry at 0x{node:08x} does not look like ESP-IDF 5.x."));
        };
        heaps.push(entry);
        node = next;
    }
    Ok(heaps)
}

async fn read_heap<G: Guest>(guest: &mut G, h: &HeapEntry, pools: &mut Pools) -> Result<Option<Heap>, String> {
    let (start, end) = (h.start, h.end);
    let mem = guest.read(start, end - start).await?;
    let info = |i: usize| u32_at(&mem, (h.handle - start) as usize + 4 * i).unwrap_or(0);
    let (info_free, info_min_free, heap_data) = (info(1), info(2), info(4));
    // The pool never moves, so once found a failed walk means the guest
    // stopped mid-update; rescanning then could lock onto a later block.
    let found = match pools.starts.get(&(start, heap_data)) {
        Some(&pool) => walk_pool(&mem, start, end, pool).map(|blocks| (pool, blocks)),
        None => find_pool(&mem, start, end, heap_data),
    };
    let Some((pool, walked)) = found.filter(|(_, blocks)| !blocks.is_empty()) else {
        return Ok(None);
    };
    pools.starts.insert((start, heap_data), pool);
    let word = |addr: u32| u32_at(&mem, (addr - start) as usize);
    let mut heap = Heap {
        name: heap_label(start, h.caps),
        caps: caps_names(h.caps),
        start,
        end,
        pool: pool - 4 - start,
        tail: 0,
        free: 0,
        used: 0,
        largest: 0,
        count: walked.len() as u32,
        nfree: 0,
        info_free,
        info_min_free,
        corrupt: 0,
        blocks: Vec::with_capacity(walked.len() * 4),
    };
    for &(b, bsize, free) in &walked {
        let (mut flags, mut req) = (if free { BLOCK_FREE } else { 0 }, 0);
        if free {
            heap.free += bsize;
            heap.largest = heap.largest.max(bsize);
            heap.nfree += 1;
        } else {
            heap.used += bsize;
            if let (Some(HEAD_CANARY), Some(requested)) = (word(b + 8), word(b + 12)) {
                if requested <= bsize - 12 {
                    req = requested;
                    flags |= BLOCK_POISONED;
                    if word(b + 16 + requested) != Some(TAIL_CANARY) {
                        flags |= BLOCK_CORRUPT;
                        heap.corrupt += 1;
                    }
                }
            }
        }
        // A block spans its size field and payload: [b + 4, b + 8 + bsize).
        heap.blocks.extend([b + 4 - start, bsize + 4, flags, req]);
    }
    let &(last, last_size, _) = walked.last().unwrap();
    heap.tail = last + last_size + 8 - start;
    Ok(Some(heap))
}

fn text(field: &[u8]) -> String {
    String::from_utf8_lossy(&field[..field.iter().position(|&b| b == 0).unwrap_or(field.len())]).into_owned()
}

/// "crosspoint 1.2.0, built Oct 14 2026 10:22:01"
fn describe(desc: &[u8]) -> String {
    format!("{} {}, built {} {}", text(&desc[48..80]), text(&desc[16..48]), text(&desc[96..112]), text(&desc[80..96]))
}

impl Elf {
    /// Whether an app descriptor is this ELF's: by the ELF hash the image
    /// carries, or without one by version, name and build time. None when
    /// `desc` is not a descriptor, or the ELF has none to compare.
    fn same_app(&self, desc: &[u8]) -> Option<bool> {
        let (_, ours) = self.app_desc.as_ref()?;
        if desc.len() < APP_DESC_SIZE || u32_at(desc, 0) != Some(APP_DESC_MAGIC) {
            return None;
        }
        let sha = &desc[APP_DESC_SHA];
        Some(if sha.iter().any(|&b| b != 0) { sha == self.sha256 } else { desc[APP_DESC_TEXT] == ours[APP_DESC_TEXT] })
    }

    /// Why allocations cannot be recorded for `flash` (a 16 MiB image) with
    /// this ELF, or None: one of its app partitions must be this ELF's.
    pub fn trace_problem(&self, flash: &[u8]) -> Option<String> {
        if let Some(problem) = &self.untraceable {
            return Some(problem.clone());
        }
        let ours = app_descs(flash).into_iter().any(|desc| self.same_app(desc) == Some(true));
        (!ours).then(|| "the ELF is not the chosen firmware's".to_string())
    }

    /// Whether this ELF built `image` (an app image, a merged image or a
    /// flash dump). None when either side has no descriptor to compare.
    pub fn matches(&self, image: &[u8]) -> Option<bool> {
        let answers: Vec<bool> = app_descs(image).into_iter().filter_map(|desc| self.same_app(desc)).collect();
        (!answers.is_empty()).then(|| answers.contains(&true))
    }
}

/// The app descriptors in a firmware file: an app image's own, else each app
/// partition's in a merged image or flash dump.
pub fn app_descs(image: &[u8]) -> Vec<&[u8]> {
    // The descriptor follows the image header and the first segment header.
    let desc = |at: usize| image.get(at + 32..at + 32 + APP_DESC_SIZE).filter(|d| u32_at(d, 0) == Some(APP_DESC_MAGIC));
    if let Some(own) = desc(0) {
        return vec![own];
    }
    // The partition table: 32-byte entries, magic 0x50AA, type 0 for apps.
    (0x8000..0x8C00).step_by(32)
        .map_while(|at| (u16_at(image, at)? == 0x50AA).then(|| (image[at + 2], u32_at(image, at + 4))))
        .filter_map(|(kind, offset)| desc(offset? as usize).filter(|_| kind == 0))
        .collect()
}

/// Compare the running app's descriptor with the ELF. Ok(false) while the
/// descriptor is not readable yet (flash not mapped early in boot).
async fn check_app<G: Guest>(guest: &mut G, elf: &Elf) -> Result<bool, HeapError> {
    let Some((addr, ours)) = &elf.app_desc else { return Ok(true) };
    let theirs = guest.read(*addr, APP_DESC_SIZE as u32).await.map_err(HeapError::Read)?;
    let Some(same) = elf.same_app(&theirs) else { return Ok(false) };
    if same {
        Ok(true)
    } else {
        let name = elf.path.file_name().map_or_else(|| "The ELF".into(), |n| n.to_string_lossy().into_owned());
        Err(HeapError::Mismatch(format!(
            "{name} is not the running firmware's ELF. It is {}; the emulator runs {}.",
            describe(ours),
            describe(&theirs)
        )))
    }
}

/// The heap list without an ELF: where it was found before, else a scan.
async fn scan_list<G: Guest>(guest: &mut G, range: &std::ops::Range<u32>, pools: &mut Pools) -> Result<Vec<HeapEntry>, String> {
    if let Some(first) = pools.list {
        if let Ok(list) = heap_list(guest, first).await {
            return Ok(list);
        }
    }
    let mem = guest.read(range.start, range.end - range.start).await?;
    pools.list = find_list(&mem, range.start);
    match pools.list {
        Some(first) => heap_list(guest, first).await,
        None => Ok(Vec::new()), // none registered yet
    }
}

/// Stop, read and walk every heap, resume. A heap caught mid-update is
/// retried after letting the guest run a little, then left out.
pub async fn snapshot<G: Guest>(guest: &mut G, lookup: &Lookup<'_>, pools: &mut Pools) -> Result<Snapshot, HeapError> {
    let mut attempt = 0;
    loop {
        guest.stop().await.map_err(HeapError::Guest)?;
        let read = async {
            let list = match lookup {
                Lookup::Elf(elf) => {
                    if !check_app(guest, elf).await? {
                        return Ok(Vec::new());
                    }
                    let first = u32_at(&guest.read(elf.registered_heaps, 4).await.map_err(HeapError::Read)?, 0).unwrap_or(0);
                    heap_list(guest, first).await
                }
                Lookup::Scan(range) => scan_list(guest, range, pools).await,
            }
            .map_err(HeapError::Read)?;
            let mut heaps = Vec::with_capacity(list.len());
            for entry in &list {
                heaps.push(read_heap(guest, entry, pools).await.map_err(HeapError::Read)?);
            }
            Ok(heaps)
        }
        .await;
        guest.cont().await.map_err(HeapError::Guest)?;
        attempt += 1;
        let heaps = read?;
        if heaps.iter().all(Option::is_some) || attempt == ATTEMPTS {
            let total = heaps.len();
            let heaps: Vec<Heap> = heaps.into_iter().flatten().collect();
            let time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
            return Ok(Snapshot { time, missing: total - heaps.len(), heaps });
        }
        tokio::time::sleep(Duration::from_millis(5 * attempt as u64)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const HEAD: u32 = 0x5000_0000; // &registered_heaps
    const NODE: u32 = 0x5000_0100; // heap_t
    const DESC: u32 = 0x3C00_0020; // &esp_app_desc
    const START: u32 = 0x3FC9_0000;
    const END: u32 = 0x3FC9_1000;
    const CONTROL: u32 = 0x3A4; // TLSF control size; the walker must find it

    struct Fake {
        mem: BTreeMap<u32, Vec<u8>>,
        log: Vec<&'static str>,
    }

    impl Guest for Fake {
        async fn read(&mut self, addr: u32, size: u32) -> Result<Vec<u8>, String> {
            for (&base, data) in &self.mem {
                if base <= addr && addr + size <= base + data.len() as u32 {
                    return Ok(data[(addr - base) as usize..(addr - base + size) as usize].to_vec());
                }
            }
            Err(format!("unmapped 0x{addr:x}"))
        }
        async fn stop(&mut self) -> Result<(), String> {
            self.log.push("stop");
            Ok(())
        }
        async fn cont(&mut self) -> Result<(), String> {
            self.log.push("cont");
            Ok(())
        }
    }

    fn put(region: &mut [u8], at: u32, values: &[u32]) {
        for (i, v) in values.iter().enumerate() {
            let at = (at - START) as usize + 4 * i;
            region[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    }

    /// blocks: (size, free, requested); the last free block takes the rest.
    fn build(blocks: &[(u32, bool, Option<u32>)], task_tracking: bool) -> Fake {
        let used: u32 = blocks.iter().map(|b| b.0 + 4).sum();
        let rest = (END - (START + 20 + CONTROL - 4) - 8 - used - 4) & !3;
        let blocks: Vec<_> = blocks.iter().copied().chain([(rest, true, None)]).collect();
        let mut region = vec![0u8; (END - START) as usize];
        let free: u32 = blocks.iter().filter(|b| b.1).map(|b| b.0).sum();
        // multi_heap_info: lock, free_bytes, minimum_free_bytes, pool_size, heap_data
        put(&mut region, START, &[0, free, free - 64, END - START - 20, START + 20]);
        let (mut b, mut prev_free) = (START + 20 + CONTROL - 4, 0);
        for &(size, free, req) in &blocks {
            put(&mut region, b + 4, &[size | free as u32 | prev_free << 1]);
            if let Some(req) = req {
                put(&mut region, b + 8, &[HEAD_CANARY, req]);
                put(&mut region, b + 16 + req, &[TAIL_CANARY]);
            }
            prev_free = free as u32;
            b += size + 4;
        }
        put(&mut region, b + 4, &[prev_free << 1]); // sentinel
        let mut node: Vec<u8> = [0x1802, 0x10_0000, 0, START, END, 0xB33F_FFFF, 0, START, 0].iter().flat_map(|v: &u32| v.to_le_bytes()).collect();
        if task_tracking {
            node.splice(0..0, 0x3C00_0000u32.to_le_bytes());
        }
        node.resize(40, 0);
        let mut mem = BTreeMap::new();
        mem.insert(HEAD, NODE.to_le_bytes().to_vec());
        mem.insert(NODE, node);
        mem.insert(START, region);
        Fake { mem, log: Vec::new() }
    }

    fn elf() -> Elf {
        Elf { path: "app.elf".into(), registered_heaps: HEAD, app_desc: None, sha256: [0; 32], untraceable: None, stamp: (0, None) }
    }

    fn desc(version: &str, sha: [u8; 32]) -> Vec<u8> {
        let mut desc = vec![0u8; APP_DESC_SIZE];
        desc[..4].copy_from_slice(&APP_DESC_MAGIC.to_le_bytes());
        desc[16..16 + version.len()].copy_from_slice(version.as_bytes());
        desc[48..54].copy_from_slice(b"reader");
        desc[APP_DESC_SHA].copy_from_slice(&sha);
        desc
    }

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(future)
    }

    #[test]
    fn walks_a_heap() {
        let mut guest = build(&[(64, false, Some(40)), (128, true, None), (32, false, Some(16)), (16, false, None)], false);
        let snap = run(snapshot(&mut guest, &Lookup::Elf(&elf()), &mut Pools::new())).unwrap();
        let [h] = &snap.heaps[..] else { panic!("{snap:?}") };
        assert_eq!(h.name, "DRAM");
        assert_eq!(h.caps, ["32BIT", "INTERNAL", "DEFAULT"]);
        assert_eq!(h.count, 5);
        assert_eq!(h.used, 64 + 32 + 16);
        assert_eq!(h.nfree, 2);
        assert_eq!(h.pool, 20 + CONTROL - 4);
        assert_eq!(h.blocks[0..4], [20 + CONTROL, 68, BLOCK_POISONED, 40]);
        assert_eq!(h.blocks[6], BLOCK_FREE);
        assert_eq!(h.blocks[12..16], [h.blocks[8] + 36, 20, 0, 0]); // unpoisoned allocation
        assert_eq!(h.largest, h.blocks[17] - 4);
        assert_eq!((h.corrupt, snap.missing), (0, 0));
        assert_eq!(guest.log, ["stop", "cont"]);
    }

    #[test]
    fn flags_a_corrupt_tail_canary() {
        let mut guest = build(&[(64, false, Some(40)), (64, false, Some(8))], false);
        guest.mem.get_mut(&START).unwrap()[(20 + CONTROL - 4 + 16 + 40) as usize] ^= 0xFF;
        let snap = run(snapshot(&mut guest, &Lookup::Elf(&elf()), &mut Pools::new())).unwrap();
        assert_eq!(snap.heaps[0].corrupt, 1);
        assert_ne!(snap.heaps[0].blocks[2] & BLOCK_CORRUPT, 0);
    }

    #[test]
    fn reads_the_task_tracking_layout() {
        let mut guest = build(&[(64, false, None)], true);
        assert_eq!(run(snapshot(&mut guest, &Lookup::Elf(&elf()), &mut Pools::new())).unwrap().heaps.len(), 1);
    }

    #[test]
    fn retries_a_torn_heap_then_leaves_it_out() {
        let mut guest = build(&[(64, false, None), (64, true, None)], false);
        let mut pools = Pools::new();
        assert_eq!(run(snapshot(&mut guest, &Lookup::Elf(&elf()), &mut pools)).unwrap().heaps.len(), 1); // learns the pool start
        // Clear the prev-free bit of the block after the free one, as a
        // half-finished free would.
        let next = (20 + CONTROL - 4 + 68 + 68 + 4) as usize;
        guest.mem.get_mut(&START).unwrap()[next] &= !2;
        guest.log.clear();
        let snap = run(snapshot(&mut guest, &Lookup::Elf(&elf()), &mut pools)).unwrap();
        assert!(snap.heaps.is_empty());
        assert_eq!(snap.missing, 1);
        assert_eq!(guest.log.len(), 2 * ATTEMPTS as usize);
        assert_eq!(guest.log.last(), Some(&"cont"));
    }

    #[test]
    fn checks_the_running_app() {
        let mut elf = elf();
        elf.sha256 = [7; 32];
        elf.app_desc = Some((DESC, desc("1.0", [0; 32])));
        let mut guest = build(&[(64, false, None)], false);
        // Flash not mapped yet: no heaps, no error.
        guest.mem.insert(DESC, vec![0; APP_DESC_SIZE]);
        assert!(run(snapshot(&mut guest, &Lookup::Elf(&elf), &mut Pools::new())).unwrap().heaps.is_empty());
        // The image's ELF hash decides.
        guest.mem.insert(DESC, desc("1.0", [7; 32]));
        assert_eq!(run(snapshot(&mut guest, &Lookup::Elf(&elf), &mut Pools::new())).unwrap().heaps.len(), 1);
        guest.mem.insert(DESC, desc("1.0", [8; 32]));
        let Err(HeapError::Mismatch(message)) = run(snapshot(&mut guest, &Lookup::Elf(&elf), &mut Pools::new())) else { panic!() };
        assert!(message.starts_with("app.elf is not the running firmware's ELF. It is reader 1.0"), "{message}");
        // Without a hash, the descriptor's text.
        guest.mem.insert(DESC, desc("1.0", [0; 32]));
        assert_eq!(run(snapshot(&mut guest, &Lookup::Elf(&elf), &mut Pools::new())).unwrap().heaps.len(), 1);
        guest.mem.insert(DESC, desc("1.1", [0; 32]));
        assert!(matches!(run(snapshot(&mut guest, &Lookup::Elf(&elf), &mut Pools::new())), Err(HeapError::Mismatch(_))));
        assert_eq!(guest.log.last(), Some(&"cont"));
    }

    #[test]
    fn finds_the_heap_list_without_an_elf() {
        const DRAM: u32 = 0x3FC8_0000;
        for task_tracking in [false, true] {
            let mut guest = build(&[(64, false, None), (32, false, None)], task_tracking);
            let node = guest.mem.remove(&NODE).unwrap();
            let mut dram = vec![0u8; 0x1000];
            // A stale copy that does not chain, then two linked entries, the
            // second a copy of the first ending the list.
            let at = |off: usize, dram: &mut Vec<u8>, next: u32| {
                dram[off..off + 40].copy_from_slice(&node);
                let n = if task_tracking { 36 } else { 32 };
                dram[off + n..off + n + 4].copy_from_slice(&next.to_le_bytes());
            };
            at(0x100, &mut dram, 0x1234_5678);
            at(0x800, &mut dram, DRAM + 0x900);
            at(0x900, &mut dram, 0);
            guest.mem.insert(DRAM, dram);
            assert_eq!(find_list(&guest.mem[&DRAM], DRAM), Some(DRAM + 0x800));
            let mut pools = Pools::new();
            let snap = run(snapshot(&mut guest, &Lookup::Scan(DRAM..DRAM + 0x1000), &mut pools)).unwrap();
            assert_eq!(snap.heaps.len(), 2, "task tracking {task_tracking}");
            assert_eq!(snap.heaps[0].used, 64 + 32);
            assert_eq!(pools.list, Some(DRAM + 0x800));
        }
        // Before the heaps are registered: none, and no error.
        let mut guest = build(&[(64, false, None)], false);
        guest.mem.insert(0x3FC8_0000, vec![0u8; 0x1000]);
        assert!(run(snapshot(&mut guest, &Lookup::Scan(0x3FC8_0000..0x3FC8_1000), &mut Pools::new())).unwrap().heaps.is_empty());
    }

    #[test]
    fn rejects_a_list_that_is_not_a_heap_list() {
        let mut guest = build(&[(64, false, None)], false);
        guest.mem.get_mut(&NODE).unwrap()[12..16].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(run(snapshot(&mut guest, &Lookup::Elf(&elf()), &mut Pools::new())), Err(HeapError::Read(_))));
        assert_eq!(guest.log, ["stop", "cont"]);
    }

    /// A minimal ELF: a symbol table, its strings, and a section holding
    /// esp_app_desc.
    fn tiny_elf(machine: u16, symbols: &[(&str, u32)]) -> Vec<u8> {
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 16];
        for (name, value) in symbols {
            symtab.extend((strtab.len() as u32).to_le_bytes());
            symtab.extend(value.to_le_bytes());
            symtab.extend([0u8; 8]);
            strtab.extend(name.as_bytes());
            strtab.push(0);
        }
        let rodata = desc("2.0", [0; 32]);
        let mut data = vec![0u8; 52];
        data[..6].copy_from_slice(b"\x7fELF\x01\x01");
        data[0x12..0x14].copy_from_slice(&machine.to_le_bytes());
        let (sym_at, str_at) = (data.len(), data.len() + symtab.len());
        let ro_at = str_at + strtab.len();
        data.extend(&symtab);
        data.extend(&strtab);
        data.extend(&rodata);
        let shoff = data.len();
        let section = |kind: u32, addr: u32, offset: usize, size: usize, link: u32| {
            [0, kind, 0, addr, offset as u32, size as u32, link, 0, 0, 0].iter().flat_map(|v: &u32| v.to_le_bytes()).collect::<Vec<u8>>()
        };
        data.extend(section(0, 0, 0, 0, 0));
        data.extend(section(2, 0, sym_at, symtab.len(), 2));
        data.extend(section(3, 0, str_at, strtab.len(), 0));
        data.extend(section(1, DESC, ro_at, rodata.len(), 0));
        data[0x20..0x24].copy_from_slice(&(shoff as u32).to_le_bytes());
        data[0x2E..0x30].copy_from_slice(&40u16.to_le_bytes());
        data[0x30..0x32].copy_from_slice(&4u16.to_le_bytes());
        data
    }

    #[test]
    fn reads_an_elf() {
        let data = tiny_elf(EM_RISCV, &[("main", 0x4200_0000), ("registered_heaps", HEAD), ("esp_app_desc", DESC)]);
        let elf = Elf::parse(&data).unwrap();
        assert_eq!(elf.registered_heaps, HEAD);
        let (addr, desc) = elf.app_desc.unwrap();
        assert_eq!((addr, describe(&desc).as_str()), (DESC, "reader 2.0, built  "));
        assert_eq!(elf.sha256, <[u8; 32]>::from(Sha256::digest(&data)));

        assert!(Elf::parse(&tiny_elf(EM_RISCV, &[("main", 0)])).unwrap_err().contains("registered_heaps"));
        assert!(Elf::parse(&tiny_elf(62, &[("registered_heaps", HEAD)])).unwrap_err().contains("not ESP32"));
        assert!(Elf::parse(b"MZ").is_err());
        let mut cut = data.clone();
        cut.truncate(200);
        assert!(Elf::parse(&cut).is_err());
    }
}
