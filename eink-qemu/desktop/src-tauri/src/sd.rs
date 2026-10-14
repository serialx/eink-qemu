//! SD card explorer on the image file, with the `fatfs` crate. Synchronous:
//! callers run these on a blocking thread, with the guest stopped.
//!
//! CONTRACT (the session and commands depend on exactly these items):
//! - Paths on the card are absolute ("/", "/books/a.epub"), case-insensitive,
//!   never containing "." or ".." components.
//! - Names follow fat.js `validName`: 1..=255 chars, none of `\x00-\x1f "*/:<>?\|`
//!   or U+FFFF, no trailing space or dot, not "." / "..", not a DOS device
//!   name (CON PRN AUX NUL COM1-9 LPT1-9, with or without extension).
//! - The image may be a bare FAT volume or MBR-partitioned (first FAT
//!   partition, types 0x01 0x04 0x06 0x0b 0x0c 0x0e).
//! - Every mutating operation (upload, mkdir, rename, remove) is one
//!   transaction: its writes go to a journal and reach the image only after
//!   the whole operation, fatfs's unmount and the FSInfo update succeeded and
//!   the edited FAT still validates. Any earlier error leaves the image
//!   byte-identical. Read-only operations (list, plan_upload, export) never
//!   write the image.
//! - Errors carry fat.js-style codes: ENOENT EEXIST EINVAL ENOTDIR EISDIR
//!   ENOTEMPTY ENOSPC ENOTSUP EIO. `SdError::changed` is true only when
//!   writing a committed transaction to the image failed partway; the card
//!   may then be inconsistent. After `Ok` from a mutating operation, or an
//!   error with `changed`, the device must remount the card (reset).
//! - Cards whose FAT copies disagree, FAT32 cards with mirroring disabled,
//!   and FATs with broken, cross-linked or cyclic cluster chains are refused
//!   with ENOTSUP before anything is read through fatfs, as fat.js refuses
//!   them; so a damaged card cannot hang a listing.
//!
//! Notes on fatfs 0.3: it trusts the FSInfo free count, so free space is
//! counted from the FAT here and both FSInfo copies are rewritten after every
//! change, as fat.js does. Short names decode as code page 437, like fat.js;
//! new short names stay ASCII. Its long-name encoder only accepts characters
//! up to U+FFFF (no U+007F) and at most 255 UTF-8 bytes; such names are
//! rejected with EINVAL before anything is written.

use fatfs::{Dir, DirEntry, FileSystem, FsOptions, OemCpConverter};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SdError {
    pub code: &'static str,
    pub message: String,
    /// Writing a committed change to the image failed partway: the card may
    /// be inconsistent and must be remounted. False for every other error,
    /// which leaves the image byte-identical.
    pub changed: bool,
}

impl SdError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), changed: false }
    }
}

impl std::fmt::Display for SdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SdError {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SdEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    /// Modification time as milliseconds since the Unix epoch, reading the
    /// FAT timestamp as local time. None when the entry has no valid date.
    pub mtime: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SdListing {
    /// The listed folder, as requested.
    pub path: String,
    /// Folders first, then files; each group sorted case-insensitively.
    /// Volume labels, "." and ".." are omitted.
    pub entries: Vec<SdEntry>,
    pub free: u64,
    pub total: u64,
    /// "FAT12", "FAT16" or "FAT32".
    pub fs_type: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadPlan {
    pub files: usize,
    pub bytes: u64,
    /// Card paths of existing files the upload would replace.
    pub conflicts: Vec<String>,
}

/// Limits for host folders copied onto the card.
pub const MAX_UPLOAD_ENTRIES: usize = 10_000;
pub const MAX_UPLOAD_DEPTH: usize = 64;

const COPY_BUFFER: usize = 256 * 1024;
const MAX_FILE: u64 = 0xffff_ffff;

/// Whether `name` is a valid FAT long name (the rules above).
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.encode_utf16().count() > 255 || name == "." || name == ".." {
        return false;
    }
    if name.chars().any(|c| (c as u32) < 0x20 || c == '\u{ffff}' || "\"*/:<>?\\|".contains(c)) {
        return false;
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !device
}

/// A name valid for fat.js that fatfs 0.3 can also encode.
fn check_name(name: &str) -> Result<(), SdError> {
    if !valid_name(name) {
        return Err(SdError::new("EINVAL", format!("\"{name}\" is not a valid FAT file name.")));
    }
    if name.len() > 255 || name.chars().any(|c| c == '\u{7f}' || (c as u32) > 0xffff) {
        return Err(SdError::new(
            "EINVAL",
            format!("\"{name}\" cannot be written by the desktop SD explorer (too long, or characters outside the Basic Multilingual Plane)."),
        ));
    }
    Ok(())
}

fn fold(name: &str) -> String {
    name.to_uppercase()
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else { format!("{dir}/{name}") }
}

fn parts(path: &str) -> Result<Vec<&str>, SdError> {
    if !path.starts_with('/') {
        return Err(SdError::new("EINVAL", "Path must be absolute"));
    }
    let result: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    for part in &result {
        if !valid_name(part) {
            return Err(SdError::new("EINVAL", format!("\"{part}\" is not a valid FAT file name.")));
        }
    }
    Ok(result)
}

fn eio(error: io::Error) -> SdError {
    let message = error.to_string();
    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::AlreadyExists => "EEXIST",
        // A fixed FAT12/16 root folder that is full ends the write early.
        io::ErrorKind::WriteZero => "ENOSPC",
        _ if message.contains("No space left") => "ENOSPC",
        _ if message.contains("Directory not empty") => "ENOTEMPTY",
        _ if message.contains("Is a directory") => "EISDIR",
        _ if message.contains("Not a directory") => "ENOTDIR",
        _ if message.contains("File name") => "EINVAL",
        _ => "EIO",
    };
    let message = match code {
        "ENOSPC" => "Not enough free space on the SD card".to_string(),
        _ => format!("SD card: {message}"),
    };
    SdError::new(code, message)
}

fn host(path: &Path, error: io::Error) -> SdError {
    let code = if error.kind() == io::ErrorKind::NotFound { "ENOENT" } else { "EIO" };
    SdError::new(code, format!("{}: {error}", path.display()))
}

fn damaged(what: &str) -> SdError {
    SdError::new("ENOTSUP", format!("The SD card's file system is damaged ({what}); repair the card with a disk utility before editing it."))
}

/* Code page 437 for short names, as fat.js decodes them. */

const CP437_HIGH: [char; 128] = [
    '\u{00c7}', '\u{00fc}', '\u{00e9}', '\u{00e2}', '\u{00e4}', '\u{00e0}', '\u{00e5}', '\u{00e7}', '\u{00ea}', '\u{00eb}', '\u{00e8}', '\u{00ef}', '\u{00ee}', '\u{00ec}', '\u{00c4}', '\u{00c5}', '\u{00c9}', '\u{00e6}', '\u{00c6}', '\u{00f4}', '\u{00f6}', '\u{00f2}', '\u{00fb}', '\u{00f9}', '\u{00ff}', '\u{00d6}', '\u{00dc}', '\u{00a2}', '\u{00a3}', '\u{00a5}', '\u{20a7}', '\u{0192}', '\u{00e1}', '\u{00ed}', '\u{00f3}', '\u{00fa}', '\u{00f1}', '\u{00d1}', '\u{00aa}', '\u{00ba}', '\u{00bf}', '\u{2310}', '\u{00ac}', '\u{00bd}', '\u{00bc}', '\u{00a1}', '\u{00ab}', '\u{00bb}', '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}', '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255d}', '\u{255c}', '\u{255b}', '\u{2510}', '\u{2514}', '\u{2534}', '\u{252c}', '\u{251c}', '\u{2500}', '\u{253c}', '\u{255e}', '\u{255f}', '\u{255a}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256c}', '\u{2567}', '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256b}', '\u{256a}', '\u{2518}', '\u{250c}', '\u{2588}', '\u{2584}', '\u{258c}', '\u{2590}', '\u{2580}', '\u{03b1}', '\u{00df}', '\u{0393}', '\u{03c0}', '\u{03a3}', '\u{03c3}', '\u{00b5}', '\u{03c4}', '\u{03a6}', '\u{0398}', '\u{03a9}', '\u{03b4}', '\u{221e}', '\u{03c6}', '\u{03b5}', '\u{2229}', '\u{2261}', '\u{00b1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00f7}', '\u{2248}', '\u{00b0}', '\u{2219}', '\u{00b7}', '\u{221a}', '\u{207f}', '\u{00b2}', '\u{25a0}', '\u{00a0}',
];

#[derive(Debug)]
struct Cp437;

impl OemCpConverter for Cp437 {
    fn decode(&self, oem_char: u8) -> char {
        if oem_char < 0x80 { oem_char as char } else { CP437_HIGH[(oem_char - 0x80) as usize] }
    }
    fn encode(&self, uni_char: char) -> Option<u8> {
        // New short names stay ASCII; non-ASCII characters become '_' and
        // the long name carries them.
        if (uni_char as u32) < 0x80 { Some(uni_char as u8) } else { None }
    }
}

static CP437: Cp437 = Cp437;

fn options() -> FsOptions {
    FsOptions::new().oem_cp_converter(&CP437)
}

/* Volume geometry, read from the boot sector like fat.js. */

#[derive(Debug, Clone, Copy, PartialEq)]
enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone)]
struct Geometry {
    /// Partition start and end in the image file.
    base: u64,
    end: u64,
    fat_offset: u64,
    fat_bytes: u64,
    fats: u64,
    /// FAT12/16 fixed root folder: offset and entry count.
    root_offset: u64,
    root_entries: u64,
    data_offset: u64,
    root_cluster: u32,
    clusters: u64,
    cluster_size: u64,
    kind: FatKind,
    /// FAT32 ExtFlags: bit 7 set means mirroring is disabled.
    ext_flags: u64,
    /// FSInfo sectors (primary and backup) with valid signatures.
    fs_info: Vec<u64>,
}

impl Geometry {
    fn name(&self) -> &'static str {
        match self.kind {
            FatKind::Fat12 => "FAT12",
            FatKind::Fat16 => "FAT16",
            FatKind::Fat32 => "FAT32",
        }
    }
    fn total(&self) -> u64 {
        self.clusters * self.cluster_size
    }
    fn entries(&self) -> u64 {
        self.clusters + 2
    }
    fn fixed_root(&self) -> bool {
        self.kind != FatKind::Fat32
    }
}

fn u16_at(buf: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([buf[at], buf[at + 1]]) as u64
}
fn u32_at(buf: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as u64
}

fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0;
        while done < buf.len() {
            match std::os::windows::fs::FileExt::seek_read(file, &mut buf[done..], offset + done as u64)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => done += n,
            }
        }
        Ok(())
    }
}

fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0;
        while done < buf.len() {
            match std::os::windows::fs::FileExt::seek_write(file, &buf[done..], offset + done as u64)? {
                0 => return Err(io::ErrorKind::WriteZero.into()),
                n => done += n,
            }
        }
        Ok(())
    }
}

fn read_sector(file: &File, offset: u64) -> Result<[u8; 512], SdError> {
    let mut sector = [0u8; 512];
    read_exact_at(file, &mut sector, offset)
        .map_err(|_| SdError::new("ENOTSUP", "Not a FAT disk image (truncated boot sector)"))?;
    Ok(sector)
}

fn plausible_bpb(sector: &[u8; 512]) -> bool {
    matches!(u16_at(sector, 11), 512 | 1024 | 2048 | 4096) && sector[13] != 0 && u16_at(sector, 14) != 0
}

fn fs_info_valid(sector: &[u8; 512]) -> bool {
    u32_at(sector, 0) == 0x4161_5252 && u32_at(sector, 484) == 0x6141_7272 && u32_at(sector, 508) == 0xaa55_0000
}

fn geometry(file: &File) -> Result<Geometry, SdError> {
    let length = file.metadata().map_err(eio)?.len();
    let unsupported = |message: &str| SdError::new("ENOTSUP", message.to_string());
    if length < 512 {
        return Err(unsupported("Not a FAT disk image"));
    }
    let first = read_sector(file, 0)?;
    if u16_at(&first, 510) != 0xaa55 {
        return Err(unsupported("Not a FAT disk image"));
    }
    let (base, end) = if plausible_bpb(&first) {
        (0, length)
    } else {
        let partition = (0..4).map(|i| 446 + i * 16).find(|&p| matches!(first[p + 4], 0x01 | 0x04 | 0x06 | 0x0b | 0x0c | 0x0e));
        let Some(p) = partition else {
            return Err(unsupported("Expected a FAT partition in the MBR"));
        };
        let base = u32_at(&first, p + 8) * 512;
        let end = base + u32_at(&first, p + 12) * 512;
        if base < 512 || end > length || end <= base + 512 {
            return Err(SdError::new("EINVAL", "The FAT partition exceeds the image"));
        }
        (base, end)
    };
    let boot = read_sector(file, base)?;
    let bps = u16_at(&boot, 11);
    let spc = boot[13] as u64;
    let reserved = u16_at(&boot, 14);
    let fats = boot[16] as u64;
    let root_entries = u16_at(&boot, 17);
    let total = match u16_at(&boot, 19) {
        0 => u32_at(&boot, 32),
        n => n,
    };
    let fat_sectors = match u16_at(&boot, 22) {
        0 => u32_at(&boot, 36),
        n => n,
    };
    if u16_at(&boot, 510) != 0xaa55
        || !matches!(bps, 512 | 1024 | 2048 | 4096)
        || spc == 0
        || spc > 128
        || !spc.is_power_of_two()
        || reserved == 0
        || !matches!(fats, 1 | 2)
        || total == 0
        || fat_sectors == 0
    {
        return Err(unsupported("Unsupported or invalid FAT boot sector"));
    }
    let root_sectors = (root_entries * 32).div_ceil(bps);
    let data = reserved + fats * fat_sectors + root_sectors;
    if total <= data || base + total * bps > end {
        return Err(unsupported("Invalid FAT geometry or truncated image"));
    }
    let clusters = (total - data) / spc;
    let kind = if clusters < 4085 {
        FatKind::Fat12
    } else if clusters < 65525 {
        FatKind::Fat16
    } else {
        FatKind::Fat32
    };
    let fat_bits = match kind {
        FatKind::Fat12 => 12,
        FatKind::Fat16 => 16,
        FatKind::Fat32 => 32,
    };
    if (clusters + 2) * fat_bits > fat_sectors * bps * 8 {
        return Err(unsupported("Invalid FAT geometry"));
    }
    let mut fs_info = Vec::new();
    let mut root_cluster = 0;
    let mut ext_flags = 0;
    if kind == FatKind::Fat32 {
        root_cluster = u32_at(&boot, 44) as u32;
        if root_cluster < 2 || root_cluster as u64 >= clusters + 2 {
            return Err(unsupported("Invalid FAT32 root folder cluster"));
        }
        ext_flags = u16_at(&boot, 40);
        let info = u16_at(&boot, 48);
        let backup = u16_at(&boot, 50);
        if info != 0 && info != 0xffff && info < reserved {
            for sector in [Some(info), (backup != 0 && backup != 0xffff && backup + info < reserved).then_some(backup + info)]
                .into_iter()
                .flatten()
            {
                // Offsets are relative to the partition.
                let offset = sector * bps;
                if fs_info_valid(&read_sector(file, base + offset)?) {
                    fs_info.push(offset);
                }
            }
        }
    }
    Ok(Geometry {
        base,
        end,
        fat_offset: reserved * bps,
        fat_bytes: fat_sectors * bps,
        fats,
        root_offset: (reserved + fats * fat_sectors) * bps,
        root_entries,
        data_offset: data * bps,
        root_cluster,
        clusters,
        cluster_size: bps * spc,
        kind,
        ext_flags,
        fs_info,
    })
}

/* The journal: fatfs reads and writes the partition through it. Writes
   land in an overlay of 4 KiB chunks (kept in memory up to a budget, then
   spilled to an anonymous temporary file) and reach the image only on
   commit, so a failed operation leaves the image untouched. */

const CHUNK: u64 = 4096;
const DIRTY_IN_MEMORY: usize = 8192; // 32 MiB
const CLEAN_CACHE: usize = 4096; // 16 MiB

struct Journal {
    file: File,
    base: u64,
    len: u64,
    pos: u64,
    dirty: HashMap<u64, Vec<u8>>,
    spilled: HashMap<u64, u64>,
    spill: Option<File>,
    spill_end: u64,
    clean: HashMap<u64, Vec<u8>>,
}

impl Journal {
    fn new(file: File, base: u64, len: u64) -> Journal {
        Journal {
            file,
            base,
            len,
            pos: 0,
            dirty: HashMap::new(),
            spilled: HashMap::new(),
            spill: None,
            spill_end: 0,
            clean: HashMap::new(),
        }
    }

    fn is_dirty(&self) -> bool {
        !self.dirty.is_empty() || !self.spilled.is_empty()
    }

    fn chunk_len(&self, index: u64) -> usize {
        CHUNK.min(self.len - index * CHUNK) as usize
    }

    fn load(&self, index: u64) -> io::Result<Vec<u8>> {
        let mut data = vec![0u8; self.chunk_len(index)];
        read_exact_at(&self.file, &mut data, self.base + index * CHUNK)?;
        Ok(data)
    }

    fn unspill(&mut self, index: u64) -> io::Result<Option<Vec<u8>>> {
        let Some(offset) = self.spilled.remove(&index) else { return Ok(None) };
        let mut data = vec![0u8; self.chunk_len(index)];
        read_exact_at(self.spill.as_ref().unwrap(), &mut data, offset)?;
        Ok(Some(data))
    }

    fn spill_all(&mut self) -> io::Result<()> {
        if self.spill.is_none() {
            self.spill = Some(tempfile::tempfile()?);
        }
        let spill = self.spill.as_ref().unwrap();
        for (index, data) in self.dirty.drain() {
            write_all_at(spill, &data, self.spill_end)?;
            self.spilled.insert(index, self.spill_end);
            self.spill_end += CHUNK;
        }
        Ok(())
    }

    /// The overlay copy of a chunk, created from the image (or zeroes when
    /// the caller overwrites all of it) on first write.
    fn chunk_mut(&mut self, index: u64, whole: bool) -> io::Result<&mut Vec<u8>> {
        if !self.dirty.contains_key(&index) {
            let data = match self.unspill(index)? {
                Some(data) => data,
                None => match self.clean.remove(&index) {
                    Some(data) => data,
                    None if whole => vec![0u8; self.chunk_len(index)],
                    None => self.load(index)?,
                },
            };
            if self.dirty.len() >= DIRTY_IN_MEMORY {
                self.spill_all()?;
            }
            self.dirty.insert(index, data);
        }
        Ok(self.dirty.get_mut(&index).unwrap())
    }

    fn pread(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.read_exact(buf)
    }

    fn pwrite(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.write_all(buf)
    }

    /// Write the overlay to the image in offset order and sync it.
    fn commit(mut self) -> Result<(), SdError> {
        let failed = |error: io::Error| SdError {
            code: "EIO",
            message: format!("Writing the SD card image failed partway; the card may be inconsistent: {error}"),
            changed: true,
        };
        let mut indices: Vec<u64> = self.dirty.keys().chain(self.spilled.keys()).copied().collect();
        indices.sort_unstable();
        let mut run: Vec<u8> = Vec::with_capacity(1 << 20);
        let mut run_start = 0u64;
        let mut next = None;
        for index in indices {
            let data = match self.dirty.remove(&index) {
                Some(data) => data,
                None => self.unspill(index).map_err(failed)?.unwrap(),
            };
            if next != Some(index) || run.len() >= 1 << 20 {
                if !run.is_empty() {
                    write_all_at(&self.file, &run, self.base + run_start).map_err(failed)?;
                    run.clear();
                }
                run_start = index * CHUNK;
            }
            run.extend_from_slice(&data);
            next = Some(index + 1);
        }
        if !run.is_empty() {
            write_all_at(&self.file, &run, self.base + run_start).map_err(failed)?;
        }
        self.file.sync_data().map_err(failed)
    }
}

impl Read for Journal {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        let mut done = 0;
        while done < n {
            let at = self.pos + done as u64;
            let index = at / CHUNK;
            let offset = (at % CHUNK) as usize;
            let take = (n - done).min(self.chunk_len(index) - offset);
            if self.spilled.contains_key(&index) {
                self.chunk_mut(index, false)?;
            }
            if let Some(data) = self.dirty.get(&index).or_else(|| self.clean.get(&index)) {
                buf[done..done + take].copy_from_slice(&data[offset..offset + take]);
                done += take;
            } else if offset == 0 && take == self.chunk_len(index) {
                // A run of whole chunks that are neither dirty nor cached:
                // read them straight from the image.
                let mut last = index;
                let mut bytes = take;
                while done + bytes < n {
                    let following = last + 1;
                    let length = self.chunk_len(following);
                    if done + bytes + length > n
                        || self.dirty.contains_key(&following)
                        || self.spilled.contains_key(&following)
                        || self.clean.contains_key(&following)
                    {
                        break;
                    }
                    last = following;
                    bytes += length;
                }
                read_exact_at(&self.file, &mut buf[done..done + bytes], self.base + at)?;
                done += bytes;
            } else {
                let data = self.load(index)?;
                buf[done..done + take].copy_from_slice(&data[offset..offset + take]);
                if self.clean.len() >= CLEAN_CACHE {
                    self.clean.clear();
                }
                self.clean.insert(index, data);
                done += take;
            }
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for Journal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.pos >= self.len {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        let mut done = 0;
        while done < n {
            let at = self.pos + done as u64;
            let index = at / CHUNK;
            let offset = (at % CHUNK) as usize;
            let length = self.chunk_len(index);
            let take = (n - done).min(length - offset);
            let chunk = self.chunk_mut(index, offset == 0 && take == length)?;
            chunk[offset..offset + take].copy_from_slice(&buf[done..done + take]);
            done += take;
        }
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Journal {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => self.len as i128 + offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek to a negative offset"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

/* FAT validation: every operation reads the whole FAT once, checks the
   copies agree and that chains are sound, and counts free clusters. */

struct FatState {
    next: Vec<u32>,
    free: u64,
    first_free: Option<u64>,
    bad: u32,
    end: u32,
}

impl FatState {
    fn is_link(&self, value: u32) -> bool {
        value >= 2 && (value as usize) < self.next.len()
    }

    /// The clusters of a chain starting at `first` (none for an empty file).
    fn chain(&self, first: u32) -> Result<Vec<u32>, SdError> {
        let mut result = Vec::new();
        if first == 0 {
            return Ok(result);
        }
        if !self.is_link(first) || self.next[first as usize] == 0 || self.next[first as usize] == self.bad {
            return Err(SdError::new("EINVAL", "Broken FAT cluster chain"));
        }
        let mut cluster = first;
        loop {
            result.push(cluster);
            if result.len() > self.next.len() {
                return Err(SdError::new("EINVAL", "Cyclic FAT cluster chain"));
            }
            let value = self.next[cluster as usize];
            if value >= self.end {
                return Ok(result);
            }
            cluster = value;
        }
    }
}

fn decode_fat(kind: FatKind, raw: &[u8], entries: usize) -> Vec<u32> {
    match kind {
        FatKind::Fat12 => (0..entries)
            .map(|cluster| {
                let at = cluster * 3 / 2;
                let pair = u16::from_le_bytes([raw[at], raw[at + 1]]) as u32;
                if cluster & 1 == 1 { pair >> 4 } else { pair & 0xfff }
            })
            .collect(),
        FatKind::Fat16 => raw[..entries * 2].as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes([b[0], b[1]]) as u32).collect(),
        FatKind::Fat32 => raw[..entries * 4]
            .as_chunks::<4>().0.iter()
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x0fff_ffff)
            .collect(),
    }
}

fn fat_scan(journal: &mut Journal, geo: &Geometry) -> Result<FatState, SdError> {
    if geo.kind == FatKind::Fat32 && geo.ext_flags & 0x80 != 0 {
        return Err(SdError::new("ENOTSUP", "FAT32 with disabled FAT mirroring is unsupported"));
    }
    let entries = geo.entries() as usize;
    let used = match geo.kind {
        FatKind::Fat12 => (entries * 3).div_ceil(2) + 1,
        FatKind::Fat16 => entries * 2,
        FatKind::Fat32 => entries * 4,
    }
    .min(geo.fat_bytes as usize);
    let mut raw = vec![0u8; used.max(entries * 3 / 2 + 2)];
    journal.pread(geo.fat_offset, &mut raw[..used]).map_err(eio)?;
    let next = decode_fat(geo.kind, &raw, entries);
    for copy in 1..geo.fats {
        journal.pread(geo.fat_offset + copy * geo.fat_bytes, &mut raw[..used]).map_err(eio)?;
        if decode_fat(geo.kind, &raw, entries)[2..] != next[2..] {
            return Err(SdError::new("ENOTSUP", "FAT copies disagree; repair the card before editing"));
        }
    }
    let (bad, end) = match geo.kind {
        FatKind::Fat12 => (0xff7, 0xff8),
        FatKind::Fat16 => (0xfff7, 0xfff8),
        FatKind::Fat32 => (0x0fff_fff7, 0x0fff_fff8),
    };
    let mut state = FatState { next, free: 0, first_free: None, bad, end };
    let mut incoming = vec![0u8; entries];
    for cluster in 2..entries {
        let value = state.next[cluster];
        if value == 0 {
            state.free += 1;
            state.first_free.get_or_insert(cluster as u64);
        } else if value == bad || value >= end {
            continue;
        } else if !state.is_link(value) {
            return Err(damaged("a cluster chain points outside the card"));
        } else if state.next[value as usize] == 0 {
            return Err(damaged("a cluster chain runs into free space"));
        } else {
            incoming[value as usize] += 1;
            if incoming[value as usize] > 1 {
                return Err(damaged("cross-linked cluster chains"));
            }
        }
    }
    // Each cluster has at most one predecessor, so walking every chain from
    // its head visits each cluster once; an allocated cluster left unvisited
    // sits on a cycle.
    let mut visited = vec![false; entries];
    for head in 2..entries {
        let value = state.next[head];
        if value == 0 || value == bad || incoming[head] != 0 {
            continue;
        }
        let mut cluster = head;
        loop {
            visited[cluster] = true;
            let value = state.next[cluster];
            if value >= end || value == bad {
                break;
            }
            cluster = value as usize;
        }
    }
    if (2..entries).any(|c| !visited[c] && state.next[c] != 0 && state.next[c] != bad) {
        return Err(damaged("a cluster chain loops"));
    }
    Ok(state)
}

/// Rewrite both FAT32 FSInfo copies from the FAT, as fat.js `syncInfo` does.
fn sync_info(journal: &mut Journal, geo: &Geometry, state: &FatState) -> Result<(), SdError> {
    for &offset in &geo.fs_info {
        let mut sector = [0u8; 512];
        journal.pread(offset, &mut sector).map_err(eio)?;
        if !fs_info_valid(&sector) {
            continue;
        }
        sector[488..492].copy_from_slice(&(state.free as u32).to_le_bytes());
        sector[492..496].copy_from_slice(&state.first_free.map_or(0xffff_ffff, |c| c as u32).to_le_bytes());
        journal.pwrite(offset, &sector).map_err(eio)?;
    }
    Ok(())
}

/* Raw directory reading, for checks fatfs cannot express: the start
   clusters of a folder tree before it is removed. */

struct RawEntry {
    long: Option<String>,
    short: String,
    attr: u8,
    cluster: u32,
}

impl RawEntry {
    fn is_dir(&self) -> bool {
        self.attr & 0x10 != 0
    }
    fn matches(&self, key: &str) -> bool {
        self.long.as_deref().is_some_and(|long| fold(long) == key) || fold(&self.short) == key
    }
}

const LFN_POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

fn raw_folder(journal: &mut Journal, geo: &Geometry, fat: &FatState, cluster: Option<u32>) -> Result<Vec<RawEntry>, SdError> {
    let bytes = match cluster {
        None if geo.fixed_root() => {
            let mut bytes = vec![0u8; (geo.root_entries * 32) as usize];
            journal.pread(geo.root_offset, &mut bytes).map_err(eio)?;
            bytes
        }
        _ => {
            let first = cluster.unwrap_or(geo.root_cluster);
            let mut bytes = Vec::new();
            for c in fat.chain(first)? {
                let start = bytes.len();
                bytes.resize(start + geo.cluster_size as usize, 0);
                journal.pread(geo.data_offset + (c as u64 - 2) * geo.cluster_size, &mut bytes[start..]).map_err(eio)?;
            }
            bytes
        }
    };
    let mut result = Vec::new();
    let mut lfn: Vec<u16> = Vec::new();
    let mut lfn_checksum = None;
    for record in bytes.as_chunks::<32>().0 {
        if record[0] == 0 {
            break;
        }
        if record[0] == 0xe5 {
            lfn.clear();
            lfn_checksum = None;
            continue;
        }
        let attr = record[11];
        if attr & 0x3f == 0x0f {
            let sequence = (record[0] & 0x1f) as usize;
            if record[0] & 0x40 != 0 {
                lfn = vec![0xffff; sequence * 13];
                lfn_checksum = Some(record[13]);
            }
            if sequence >= 1 && sequence * 13 <= lfn.len() {
                for (i, &at) in LFN_POS.iter().enumerate() {
                    lfn[(sequence - 1) * 13 + i] = u16::from_le_bytes([record[at], record[at + 1]]);
                }
            }
            continue;
        }
        if attr & 0x08 != 0 {
            lfn.clear();
            lfn_checksum = None;
            continue;
        }
        let checksum = record[..11].iter().fold(0u8, |sum, &b| sum.rotate_right(1).wrapping_add(b));
        let long = (lfn_checksum == Some(checksum) && !lfn.is_empty()).then(|| {
            let units: Vec<u16> = lfn.iter().copied().take_while(|&u| u != 0 && u != 0xffff).collect();
            String::from_utf16_lossy(&units)
        });
        lfn.clear();
        lfn_checksum = None;
        let decode = |bytes: &[u8]| -> String {
            bytes.iter().map(|&b| CP437.decode(b)).collect::<String>().trim_end_matches(' ').to_string()
        };
        let mut base = record[..8].to_vec();
        if base[0] == 0x05 {
            base[0] = 0xe5;
        }
        let (stem, ext) = (decode(&base), decode(&record[8..11]));
        let short = if ext.is_empty() { stem } else { format!("{stem}.{ext}") };
        if short == "." || short == ".." {
            continue;
        }
        let low = u16_at(record, 26) as u32;
        let high = if geo.kind == FatKind::Fat32 { (u16_at(record, 20) as u32) << 16 } else { 0 };
        result.push(RawEntry { long, short, attr, cluster: high | low });
    }
    Ok(result)
}

/// Refuse to remove a tree whose clusters are shared or loop, as fat.js does.
fn check_tree(journal: &mut Journal, geo: &Geometry, fat: &FatState, names: &[&str]) -> Result<(), SdError> {
    let mut folder: Option<u32> = None;
    let mut target = None;
    for (i, name) in names.iter().enumerate() {
        let key = fold(name);
        let entry = raw_folder(journal, geo, fat, folder)?.into_iter().find(|e| e.matches(&key));
        let Some(entry) = entry else { return Ok(()) }; // fatfs reports ENOENT
        if i + 1 < names.len() {
            if !entry.is_dir() {
                return Ok(());
            }
            folder = Some(entry.cluster);
        } else {
            target = Some(entry);
        }
    }
    let Some(target) = target else { return Ok(()) };
    let mut seen = HashSet::new();
    let mut pending = vec![target];
    while let Some(entry) = pending.pop() {
        for cluster in fat.chain(entry.cluster)? {
            if !seen.insert(cluster) {
                return Err(SdError::new("EINVAL", "Cross-linked directory or file"));
            }
        }
        if entry.is_dir() {
            if entry.cluster == 0 {
                return Err(SdError::new("EINVAL", "Broken FAT cluster chain"));
            }
            pending.extend(raw_folder(journal, geo, fat, Some(entry.cluster))?);
        }
    }
    Ok(())
}

struct Stats {
    free: u64,
}

type Disk = Journal;

/// Mount the card through a journal and run `action`. For writes, the
/// journal is committed only if the action and fatfs's unmount succeed and
/// the edited FAT still validates; otherwise the image is untouched.
fn with_fs<R>(
    image: &Path,
    write: bool,
    pre: impl FnOnce(&mut Journal, &Geometry, &FatState) -> Result<(), SdError>,
    action: impl FnOnce(&FileSystem<&mut Disk>, &Geometry, &Stats) -> Result<R, SdError>,
) -> Result<R, SdError> {
    let file = OpenOptions::new().read(true).write(write).open(image).map_err(|e| host(image, e))?;
    let geo = geometry(&file)?;
    let mut journal = Journal::new(file, geo.base, geo.end - geo.base);
    let fat = fat_scan(&mut journal, &geo)?;
    pre(&mut journal, &geo, &fat)?;
    let stats = Stats { free: fat.free * geo.cluster_size };
    drop(fat);
    journal.pos = 0; // fatfs expects to start at the boot sector
    let value = {
        let fs = FileSystem::new(&mut journal, options())
            .map_err(|e| SdError::new("ENOTSUP", format!("Not a FAT file system the explorer can read: {e}")))?;
        let value = action(&fs, &geo, &stats)?;
        fs.unmount().map_err(eio)?;
        value
    };
    if !write || !journal.is_dirty() {
        return Ok(value);
    }
    let after = fat_scan(&mut journal, &geo).map_err(|e| {
        SdError::new("EIO", format!("The change was not saved because it would leave the card inconsistent: {}", e.message))
    })?;
    sync_info(&mut journal, &geo, &after)?;
    journal.commit()?;
    Ok(value)
}

fn no_check(_: &mut Journal, _: &Geometry, _: &FatState) -> Result<(), SdError> {
    Ok(())
}

/* Lookup helpers. */

type FsDir<'a, 'b> = Dir<'a, &'b mut Disk>;
type FsEntry<'a, 'b> = DirEntry<'a, &'b mut Disk>;

fn children<'a, 'b>(dir: &FsDir<'a, 'b>) -> Result<Vec<FsEntry<'a, 'b>>, SdError> {
    let mut result = Vec::new();
    for entry in dir.iter() {
        let entry = entry.map_err(eio)?;
        let name = entry.file_name();
        if name != "." && name != ".." {
            result.push(entry);
        }
    }
    Ok(result)
}

fn find<'a, 'b>(dir: &FsDir<'a, 'b>, name: &str) -> Result<Option<FsEntry<'a, 'b>>, SdError> {
    let key = fold(name);
    for entry in dir.iter() {
        let entry = entry.map_err(eio)?;
        let long = entry.file_name();
        if long == "." || long == ".." {
            continue;
        }
        if fold(&long) == key || fold(&entry.short_file_name()) == key {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

fn open_dir<'a, 'b>(fs: &'a FileSystem<&'b mut Disk>, names: &[&str]) -> Result<FsDir<'a, 'b>, SdError> {
    let mut dir = fs.root_dir();
    for name in names {
        match find(&dir, name)? {
            None => return Err(SdError::new("ENOENT", format!("Path does not exist: {name}"))),
            Some(entry) if !entry.is_dir() => return Err(SdError::new("ENOTDIR", format!("Not a folder: {name}"))),
            Some(entry) => dir = entry.to_dir(),
        }
    }
    Ok(dir)
}

/// The parent folder and the name of a non-root path.
fn split(path: &str) -> Result<(Vec<&str>, &str), SdError> {
    let mut names = parts(path)?;
    let Some(name) = names.pop() else {
        return Err(SdError::new("EINVAL", "Cannot modify the root directory"));
    };
    Ok((names, name))
}

fn mtime(entry: &FsEntry) -> Option<i64> {
    use chrono::TimeZone;
    let modified = entry.modified();
    let (date, time) = (modified.date, modified.time);
    if !(1..=12).contains(&date.month) || !(1..=31).contains(&date.day) || time.hour > 23 || time.min > 59 || time.sec > 59 {
        return None;
    }
    chrono::Local
        .with_ymd_and_hms(date.year as i32, date.month as u32, date.day as u32, time.hour as u32, time.min as u32, time.sec as u32)
        .earliest()
        .map(|t| t.timestamp_millis())
}

/// List a folder.
pub fn list(image: &Path, dir: &str) -> Result<SdListing, SdError> {
    let names = parts(dir)?;
    with_fs(image, false, no_check, |fs, geo, stats| {
        let folder = open_dir(fs, &names)?;
        let mut entries: Vec<SdEntry> = children(&folder)?
            .iter()
            .map(|entry| SdEntry {
                name: entry.file_name(),
                size: if entry.is_dir() { 0 } else { entry.len() },
                is_dir: entry.is_dir(),
                mtime: mtime(entry),
            })
            .collect();
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| fold(&a.name).cmp(&fold(&b.name))));
        Ok(SdListing { path: dir.to_string(), entries, free: stats.free, total: geo.total(), fs_type: geo.name().to_string() })
    })
}

/* Uploads. */

struct HostPlan {
    /// Card folders to create or merge into, parents first.
    dirs: Vec<String>,
    /// Folded paths of `dirs`.
    dir_keys: HashSet<String>,
    /// Host source, card path, size.
    files: Vec<(PathBuf, String, u64)>,
}

fn walk(source: &Path, card: &str, depth: usize, top: bool, plan: &mut HostPlan, seen: &mut HashMap<String, bool>, count: &mut usize) -> Result<(), SdError> {
    let meta = std::fs::symlink_metadata(source).map_err(|e| host(source, e))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let Some(name) = source.file_name().and_then(|n| n.to_str()) else {
        return Err(SdError::new("EINVAL", format!("{}: not a valid Unicode file name", source.display())));
    };
    // Hosts may hand over decomposed names (macOS file systems often do, Korean
    // especially); the card gets the composed form, as the browser page writes.
    let name: String = unicode_normalization::UnicodeNormalization::nfc(name).collect();
    let name = name.as_str();
    if !top && name.starts_with('.') {
        return Ok(());
    }
    if !meta.is_dir() && !meta.is_file() {
        return Ok(());
    }
    check_name(name)?;
    *count += 1;
    if *count > MAX_UPLOAD_ENTRIES {
        return Err(SdError::new("ENOTSUP", format!("Too many entries to copy (more than {MAX_UPLOAD_ENTRIES}).")));
    }
    let target = join(card, name);
    if let Some(&was_dir) = seen.get(&fold(&target)) {
        if !(was_dir && meta.is_dir()) {
            return Err(SdError::new("EINVAL", format!("Two sources would be copied to {target}.")));
        }
    }
    seen.insert(fold(&target), meta.is_dir());
    if meta.is_dir() {
        if depth > MAX_UPLOAD_DEPTH {
            return Err(SdError::new("ENOTSUP", format!("Folders nest more than {MAX_UPLOAD_DEPTH} levels deep.")));
        }
        if plan.dir_keys.insert(fold(&target)) {
            plan.dirs.push(target.clone());
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(source)
            .map_err(|e| host(source, e))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<_, _>>()
            .map_err(|e| host(source, e))?;
        entries.sort();
        for entry in entries {
            walk(&entry, &target, depth + 1, false, plan, seen, count)?;
        }
    } else {
        if meta.len() > MAX_FILE {
            return Err(SdError::new("EINVAL", format!("{name}: FAT files cannot exceed 4 GiB minus 1 byte.")));
        }
        plan.files.push((source.to_path_buf(), target, meta.len()));
    }
    Ok(())
}

fn host_plan(dir: &str, sources: &[PathBuf]) -> Result<HostPlan, SdError> {
    let mut plan = HostPlan { dirs: Vec::new(), dir_keys: HashSet::new(), files: Vec::new() };
    let mut seen = HashMap::new();
    let mut count = 0;
    for source in sources {
        walk(source, dir, 1, true, &mut plan, &mut seen, &mut count)?;
    }
    Ok(plan)
}

/// What an operation knows about a name in a folder.
#[derive(Clone, Copy)]
enum Known {
    /// An entry that was on the card, by its index in `Folder::entries`.
    Entry { index: usize, is_dir: bool, len: u64 },
    Created { dir: bool },
}

struct Folder<'a, 'b> {
    dir: FsDir<'a, 'b>,
    entries: Vec<FsEntry<'a, 'b>>,
    /// Folded long and short names.
    names: HashMap<String, Known>,
    /// Directory slots in use (estimated from the names).
    slots: u64,
}

impl<'a, 'b> Folder<'a, 'b> {
    fn load(dir: FsDir<'a, 'b>) -> Result<Self, SdError> {
        let entries = children(&dir)?;
        let mut names = HashMap::new();
        let mut slots = 0;
        for (index, entry) in entries.iter().enumerate() {
            slots += name_slots(&entry.file_name());
            let known = Known::Entry { index, is_dir: entry.is_dir(), len: entry.len() };
            names.insert(fold(&entry.short_file_name()), known);
            names.insert(fold(&entry.file_name()), known);
        }
        Ok(Folder { dir, entries, names, slots })
    }
}

/// Every card folder an operation touches, read once.
struct Index<'a, 'b> {
    fs: &'a FileSystem<&'b mut Disk>,
    /// Folded path -> the folder, or None when it does not exist (yet).
    folders: HashMap<String, Option<Folder<'a, 'b>>>,
}

impl<'a, 'b> Index<'a, 'b> {
    fn new(fs: &'a FileSystem<&'b mut Disk>) -> Self {
        Index { fs, folders: HashMap::new() }
    }

    fn folder(&mut self, path: &str) -> Result<Option<&mut Folder<'a, 'b>>, SdError> {
        let names = parts(path)?;
        let mut current = "/".to_string();
        if !self.folders.contains_key("/") {
            let root = Folder::load(self.fs.root_dir())?;
            self.folders.insert("/".into(), Some(root));
        }
        for name in names {
            let next = join(&current, name);
            let key = fold(&next);
            if !self.folders.contains_key(&key) {
                let parent = self.folders.get(&fold(&current)).unwrap().as_ref();
                let loaded = match parent.and_then(|p| p.names.get(&fold(name)).map(|known| (p, *known))) {
                    None => None,
                    Some((parent, Known::Entry { index, is_dir: true, .. })) => Some(Folder::load(parent.entries[index].to_dir())?),
                    Some((_, Known::Entry { .. })) | Some((_, Known::Created { dir: false })) => {
                        return Err(SdError::new("ENOTDIR", format!("A file is in the way of folder {next}.")));
                    }
                    // Created folders are inserted when they are made.
                    Some((_, Known::Created { dir: true })) => unreachable!(),
                };
                self.folders.insert(key.clone(), loaded);
            }
            current = next;
        }
        Ok(self.folders.get_mut(&fold(&current)).unwrap().as_mut())
    }

    fn lookup(&mut self, path: &str) -> Result<Option<Known>, SdError> {
        let (parents, name) = split(path)?;
        let parent = format!("/{}", parents.join("/"));
        Ok(self.folder(&parent)?.and_then(|folder| folder.names.get(&fold(name)).copied()))
    }

    fn create_dir(&mut self, path: &str) -> Result<(), SdError> {
        let (parents, name) = split(path)?;
        let parent = format!("/{}", parents.join("/"));
        let folder = self.folder(&parent)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {parent}")))?;
        let dir = folder.dir.create_dir(name).map_err(eio)?;
        folder.names.insert(fold(name), Known::Created { dir: true });
        folder.slots += name_slots(name);
        self.folders.insert(fold(path), Some(Folder { dir, entries: Vec::new(), names: HashMap::new(), slots: 0 }));
        Ok(())
    }
}

/// Directory slots a name takes: the short entry, plus long-name entries
/// unless the name is already an upper-case 8.3 name.
fn name_slots(name: &str) -> u64 {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let short = !stem.is_empty()
        && stem.len() <= 8
        && ext.len() <= 3
        && name.chars().filter(|&c| c == '.').count() <= 1
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "._-~!#$%&'()@^{}".contains(c));
    if short { 1 } else { 1 + (name.encode_utf16().count() as u64).div_ceil(13) }
}

/// Check a host plan against the card: conflicts, type clashes and space.
/// Space is counted net (replaced files give their clusters back) and
/// includes directory entries; the upload itself is all-or-nothing, so a
/// plan that only fits at the end fails cleanly at its peak.
fn check_plan(index: &mut Index, geo: &Geometry, stats: &Stats, dir: &str, plan: &HostPlan) -> Result<UploadPlan, SdError> {
    if index.folder(dir)?.is_none() {
        return Err(SdError::new("ENOENT", format!("Path does not exist: {dir}")));
    }
    let clusters = |bytes: u64| bytes.div_ceil(geo.cluster_size) as i64;
    let mut needed: i64 = 0;
    // New directory slots per parent folder (folded path).
    let mut slots: HashMap<String, (String, u64)> = HashMap::new();
    let mut add_slots = |path: &str, name: &str| {
        let parent = path[..path.len() - name.len()].trim_end_matches('/');
        let parent = if parent.is_empty() { "/".to_string() } else { parent.to_string() };
        slots.entry(fold(&parent)).or_insert_with(|| (parent, 0)).1 += name_slots(name);
    };
    let mut new_dirs = HashSet::new();
    for folder in &plan.dirs {
        let (_, name) = split(folder)?;
        match index.lookup(folder)? {
            Some(Known::Entry { is_dir: false, .. }) | Some(Known::Created { dir: false }) => {
                return Err(SdError::new("ENOTDIR", format!("{folder} is a file on the SD card; it cannot become a folder.")));
            }
            Some(_) => {}
            None => {
                add_slots(folder, name);
                new_dirs.insert(fold(folder));
            }
        }
    }
    let mut conflicts = Vec::new();
    let mut bytes = 0;
    for (_, path, size) in &plan.files {
        let (_, name) = split(path)?;
        bytes += size;
        needed += clusters(*size);
        match index.lookup(path)? {
            Some(Known::Entry { is_dir: true, .. }) | Some(Known::Created { dir: true }) => {
                return Err(SdError::new("EISDIR", format!("{path} is a folder on the SD card; a file cannot replace it.")));
            }
            Some(Known::Entry { len, .. }) => {
                needed -= clusters(len);
                conflicts.push(path.clone());
            }
            Some(Known::Created { .. }) => {}
            None => add_slots(path, name),
        }
    }
    let slot_bytes = |count: u64| (count * 32).div_ceil(geo.cluster_size) as i64;
    for (key, (parent, count)) in &slots {
        if new_dirs.contains(key) {
            // A new folder holds "." and ".." besides its entries.
            needed += slot_bytes(count + 2).max(1);
        } else if parent == "/" && geo.fixed_root() {
            let used = index.folder("/")?.map_or(0, |root| root.slots);
            if used + count > geo.root_entries {
                return Err(SdError::new(
                    "ENOSPC",
                    format!("The SD card's root folder has room for {} more entries; put the files in a folder.", geo.root_entries.saturating_sub(used)),
                ));
            }
        } else {
            needed += slot_bytes(*count);
        }
    }
    let free = (stats.free / geo.cluster_size) as i64;
    if needed > free {
        return Err(SdError::new(
            "ENOSPC",
            format!("Not enough space: {} to copy, {} free.", format_size(bytes), format_size(stats.free)),
        ));
    }
    Ok(UploadPlan { files: plan.files.len(), bytes, conflicts })
}

fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", b / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GiB", b / 1024.0 / 1024.0 / 1024.0)
    }
}

/// Work out what `upload` would copy. `sources` are host files or folders;
/// folders are copied recursively as a folder of the same name. Hidden
/// entries (name starting with '.') inside folders and symbolic links are
/// skipped; a top-level source that is itself a symlink is skipped too.
/// ENOENT if `dir` does not exist, EINVAL for an invalid name, ENOTSUP above
/// the entry or depth limits, ENOSPC if the bytes exceed free space,
/// EISDIR if a file would replace a folder or ENOTDIR if a folder would
/// replace a file.
pub fn plan_upload(image: &Path, dir: &str, sources: &[PathBuf]) -> Result<UploadPlan, SdError> {
    let plan = host_plan(dir, sources)?;
    with_fs(image, false, no_check, |fs, geo, stats| check_plan(&mut Index::new(fs), geo, stats, dir, &plan))
}

fn copy_in(file: &mut fatfs::File<&mut Disk>, source: &Path) -> Result<(), SdError> {
    let mut input = File::open(source).map_err(|e| host(source, e))?;
    file.truncate().map_err(eio)?;
    let mut buffer = vec![0u8; COPY_BUFFER];
    loop {
        let read = input.read(&mut buffer).map_err(|e| host(source, e))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(eio)?;
    }
    file.flush().map_err(eio)
}

/// Copy `sources` into `dir`, creating folders as needed and replacing
/// existing files (the caller confirmed `plan_upload`'s conflicts). Streams
/// file contents; never loads a whole file or the card into memory. All or
/// nothing: any error leaves the card unchanged.
pub fn upload(image: &Path, dir: &str, sources: &[PathBuf]) -> Result<UploadPlan, SdError> {
    let plan = host_plan(dir, sources)?;
    with_fs(image, true, no_check, |fs, geo, stats| {
        let mut index = Index::new(fs);
        let summary = check_plan(&mut index, geo, stats, dir, &plan)?;
        for folder in &plan.dirs {
            if index.lookup(folder)?.is_none() {
                index.create_dir(folder)?;
            }
        }
        for (source, path, _) in &plan.files {
            let (parents, name) = split(path)?;
            let parent_path = format!("/{}", parents.join("/"));
            let existing = index.lookup(path)?;
            let folder = index.folder(&parent_path)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {parent_path}")))?;
            let mut file = match existing {
                Some(Known::Entry { index, .. }) => folder.entries[index].to_file(),
                Some(Known::Created { .. }) => return Err(SdError::new("EINVAL", format!("Two sources would be copied to {path}."))),
                None => {
                    let file = folder.dir.create_file(name).map_err(eio)?;
                    folder.names.insert(fold(name), Known::Created { dir: false });
                    folder.slots += name_slots(name);
                    file
                }
            };
            copy_in(&mut file, source)?;
        }
        Ok(summary)
    })
}

/// Whether `dest` names the image file itself (or a hard link to it).
fn is_image(image: &Path, dest: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (std::fs::metadata(image), std::fs::metadata(dest)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if a.dev() == b.dev() && a.ino() == b.ino() {
                return true;
            }
        }
        let _ = (a, b);
    }
    let canonical = |path: &Path| -> Option<PathBuf> {
        if let Ok(path) = std::fs::canonicalize(path) {
            return Some(path);
        }
        let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        Some(std::fs::canonicalize(folder).ok()?.join(path.file_name()?))
    };
    canonical(image).is_some_and(|image| canonical(dest) == Some(image))
}

/// A temporary file beside `dest`, created with the process umask applied.
fn temporary_beside(dest: &Path) -> Result<tempfile::NamedTempFile, SdError> {
    let folder = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".eink-qemu-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    builder.tempfile_in(folder).map_err(|e| host(folder, e))
}

/// Copy one file from the card to `dest` on the host (streamed). The card
/// image itself is refused as a destination.
pub fn export(image: &Path, file: &str, dest: &Path) -> Result<(), SdError> {
    let (parents, name) = split(file)?;
    if is_image(image, dest) {
        return Err(SdError::new("EINVAL", "Choose a destination other than the SD card image."));
    }
    with_fs(image, false, no_check, |fs, _, _| {
        let parent = open_dir(fs, &parents)?;
        let entry = find(&parent, name)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {name}")))?;
        if entry.is_dir() {
            return Err(SdError::new("EISDIR", "Cannot read a folder as a file"));
        }
        let mut input = entry.to_file();
        let mut output = temporary_beside(dest)?;
        let mut buffer = vec![0u8; COPY_BUFFER];
        let mut copied = 0u64;
        loop {
            let read = input.read(&mut buffer).map_err(eio)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).map_err(|e| host(dest, e))?;
            copied += read as u64;
        }
        if copied != entry.len() {
            return Err(SdError::new("EINVAL", "File chain is shorter than its size"));
        }
        output.flush().map_err(|e| host(dest, e))?;
        finish(output, dest)
    })
}

/// Move a finished temporary file into place, keeping the permissions of
/// a file it replaces.
fn finish(file: tempfile::NamedTempFile, dest: &Path) -> Result<(), SdError> {
    if let Ok(existing) = std::fs::metadata(dest) {
        if existing.is_file() {
            let _ = file.as_file().set_permissions(existing.permissions());
        }
    }
    file.persist(dest).map_err(|e| host(dest, e.error))?;
    Ok(())
}

pub fn mkdir(image: &Path, path: &str) -> Result<(), SdError> {
    let (parents, name) = split(path)?;
    check_name(name)?;
    with_fs(image, true, no_check, |fs, geo, stats| {
        let parent = open_dir(fs, &parents)?;
        if find(&parent, name)?.is_some() {
            return Err(SdError::new("EEXIST", "Path already exists"));
        }
        if stats.free < geo.cluster_size {
            return Err(SdError::new("ENOSPC", "Not enough free space on the SD card"));
        }
        parent.create_dir(name).map_err(eio)?;
        Ok(())
    })
}

/// Change the last component of `path` to `name` (same folder).
pub fn rename(image: &Path, path: &str, name: &str) -> Result<(), SdError> {
    let (parents, old) = split(path)?;
    check_name(name)?;
    with_fs(image, true, no_check, |fs, _, _| {
        let parent = open_dir(fs, &parents)?;
        let entry = find(&parent, old)?.ok_or_else(|| SdError::new("ENOENT", "Source does not exist"))?;
        let current = entry.file_name();
        if current == name {
            return Ok(());
        }
        if let Some(target) = find(&parent, name)? {
            let same = fold(&target.file_name()) == fold(&current) && target.short_file_name() == entry.short_file_name();
            if !same {
                return Err(SdError::new("EEXIST", "Destination already exists"));
            }
            // A change of case only: fatfs sees the target as taken, so step
            // through a temporary name (one transaction either way).
            let mut temporary = String::new();
            for n in 0.. {
                temporary = format!("rename-{n}.tmp");
                if find(&parent, &temporary)?.is_none() {
                    break;
                }
            }
            parent.rename(&current, &parent, &temporary).map_err(eio)?;
            return parent.rename(&temporary, &parent, name).map_err(eio);
        }
        parent.rename(&current, &parent, name).map_err(eio)
    })
}

fn remove_tree<'a, 'b>(parent: &FsDir<'a, 'b>, entry: FsEntry<'a, 'b>) -> Result<(), SdError> {
    if entry.is_dir() {
        let dir = entry.to_dir();
        for child in children(&dir)? {
            remove_tree(&dir, child)?;
        }
    }
    parent.remove(&entry.file_name()).map_err(eio)
}

/// Remove a file, or a folder and everything in it. A tree whose clusters
/// are shared with each other is refused, as fat.js refuses it.
pub fn remove(image: &Path, path: &str) -> Result<(), SdError> {
    let (parents, name) = split(path)?;
    let names = parts(path)?;
    with_fs(
        image,
        true,
        |journal, geo, fat| check_tree(journal, geo, fat, &names),
        |fs, _, _| {
            let parent = open_dir(fs, &parents)?;
            let entry = find(&parent, name)?.ok_or_else(|| SdError::new("ENOENT", "Path does not exist"))?;
            remove_tree(&parent, entry)
        },
    )
}

/// The embedded blank 64 MiB FAT32 card (mksd.py, 512-byte clusters).
pub const BLANK_CARD_GZ: &[u8] = include_bytes!("../resources/blank-sd.img.gz");

/// Write a blank card to `dest` (via a temporary file and rename). A new
/// file gets the process umask; a replaced one keeps its permissions.
pub fn create_blank(dest: &Path) -> std::io::Result<()> {
    let folder = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let mut output = temporary_beside(dest).map_err(|e| io::Error::other(e.message))?;
    let mut input = flate2::read::GzDecoder::new(BLANK_CARD_GZ);
    io::copy(&mut input, &mut output)?;
    output.as_file().sync_all()?;
    finish(output, dest).map_err(|e| io::Error::other(e.message))
}
