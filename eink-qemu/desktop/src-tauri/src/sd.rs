//! SD card explorer on the image file, with the `fatfs` crate (a vendored
//! 0.3.6 with fixes, see vendor/fatfs/EINK-QEMU.md). Synchronous: callers
//! run these on a blocking thread, with the guest stopped.
//!
//! CONTRACT (the session and commands depend on exactly these items):
//! - Paths on the card are absolute ("/", "/books/a.epub"), case-insensitive,
//!   never containing "." or ".." components.
//! - Names follow fat.js `validName`: 1..=255 chars, none of `\x00-\x1f "*/:<>?\|`
//!   or U+FFFF, no trailing space or dot, not "." / "..", not a DOS device
//!   name (CON PRN AUX NUL COM1-9 LPT1-9, with or without extension). New
//!   names are further limited by fatfs (see the notes below).
//! - The image may be a bare FAT volume or MBR-partitioned (the first
//!   partition of type 0x01 0x04 0x06 0x0b 0x0c 0x0e). fat.js accepts only
//!   cards with exactly one 0x0b/0x0c partition, and FAT32 only.
//! - Every mutating operation (upload, mkdir, rename, remove) is one
//!   transaction: its writes go to a journal and reach the image only after
//!   the whole operation and fatfs's unmount succeeded, the edited FAT still
//!   validates, the edit lost no clusters, and FSInfo was updated.
//! - Committing writes the journal in three stages: (1) data in clusters
//!   that were free in the original FAT, then a sync; (2) the rest of the
//!   data region (folder clusters, data rewritten in place); (3) the
//!   reserved sectors, FATs and fixed root folder, then a sync. Any error
//!   before stage 2 leaves the file system unchanged (`changed` false): the
//!   image is byte-identical, except that a failure in stage 1 may have
//!   written to clusters nothing uses. A failure in stage 2 or 3 sets
//!   `changed`: the card may be inconsistent and needs checking.
//! - Errors carry fat.js-style codes: ENOENT EEXIST EINVAL ENOTDIR EISDIR
//!   ENOSPC ENOTSUP EIO. After `Ok` from a mutating operation, or an error
//!   with `changed`, the device must remount the card (reset).
//! - Damaged cards are refused before fatfs reads them, with ENOTSUP where
//!   fat.js mostly says EINVAL: FAT copies that disagree, FAT32 with
//!   mirroring disabled, broken, cross-linked or cyclic chains, and chains
//!   into bad or reserved clusters (isolated bad or reserved marks are
//!   healthy). Before every edit the whole folder tree is checked too: two
//!   entries sharing clusters, an entry starting inside another chain or at
//!   a free cluster, folders that loop and files longer than their chains.
//! - Upload sources resolve, case-insensitively, against long and short
//!   names, including the short names fatfs will generate for new entries
//!   in the same upload: a source naming an existing entry replaces it, and
//!   two sources naming one entry are refused with EINVAL. `upload` takes
//!   the conflicts the user confirmed and refuses with EEXIST if they
//!   changed since `plan_upload`. The plan follows the upload step by step
//!   (folder slots, folder growth, replacements), so `plan_upload` fails
//!   with ENOSPC exactly when `upload` would. Host files are opened without
//!   following symbolic links.
//!
//! Where this differs from fat.js (the browser page's explorer):
//! - Uploads skip hidden entries inside folders and create empty folders;
//!   the page copies dotfiles and drops empty folders.
//! - New entries always get long-name records, and short names follow
//!   fatfs (NAME~1..~4, then a checksum form) rather than fat.js's.
//! - fatfs writes long names only up to U+FFFF (no U+007F) and 255 UTF-8
//!   bytes; such names are rejected with EINVAL before anything is written.
//! - Listing order compares code points, fat.js UTF-16 units (they differ
//!   only for characters beyond U+FFFF).
//! - Short names decode as code page 437, like fat.js; new ones stay ASCII.
//! - fatfs trusts the FSInfo free count, so free space is counted from the
//!   FAT here and both FSInfo copies are rewritten after every change, as
//!   fat.js does. A FAT32 card without FSInfo (sector 0 or 0xFFFF) is fine.
//! - Temporary disk space: changes over 32 MiB are staged in a temporary
//!   file; running out of it is reported as such, not as a card error.

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
const ROOT_FULL: &str = "The SD card's root folder is full; put the files in a folder.";
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

/// An I/O error from the journal's temporary spill file (host disk space),
/// not from the card.
#[derive(Debug)]
struct SpillError(io::Error);

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for SpillError {}

fn spill_error(error: io::Error) -> io::Error {
    io::Error::other(SpillError(error))
}

fn eio(error: io::Error) -> SdError {
    if let Some(spill) = error.get_ref().and_then(|inner| inner.downcast_ref::<SpillError>()) {
        return SdError::new("EIO", format!("Could not stage the change in temporary storage (is the disk full?): {spill}"));
    }
    if error.kind() == io::ErrorKind::WriteZero {
        // Only a fixed FAT12/16 root folder ends a write early.
        return SdError::new("ENOSPC", ROOT_FULL);
    }
    let message = error.to_string();
    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::AlreadyExists => "EEXIST",
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
        read_exact_at(self.spill.as_ref().unwrap(), &mut data, offset).map_err(spill_error)?;
        Ok(Some(data))
    }

    fn spill_all(&mut self) -> io::Result<()> {
        if self.spill.is_none() {
            self.spill = Some(tempfile::tempfile().map_err(spill_error)?);
        }
        let spill = self.spill.as_ref().unwrap();
        for (index, data) in self.dirty.drain() {
            write_all_at(spill, &data, self.spill_end).map_err(spill_error)?;
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

    /// Which commit stage a chunk belongs to: 0 for data clusters that were
    /// all free in the original FAT (nothing on the card refers to them
    /// yet), 1 for the rest of the data region (folders, data overwritten
    /// in place), 2 for the reserved sectors, FATs and fixed root folder.
    fn stage(&self, index: u64, geo: &Geometry, original: &FatState) -> u8 {
        let start = index * CHUNK;
        let end = start + self.chunk_len(index) as u64;
        if start < geo.data_offset {
            return 2;
        }
        let first = (start - geo.data_offset) / geo.cluster_size + 2;
        let last = (end - 1 - geo.data_offset) / geo.cluster_size + 2;
        let free = (first..=last).all(|c| (c as usize) < original.next.len() && original.next[c as usize] == 0);
        if free { 0 } else { 1 }
    }

    /// Write the overlay to the image and sync it, in three stages (see
    /// `stage`): new data first, then folders and in-place data, then the
    /// FATs. A failure in the first stage touched only clusters the card
    /// does not use, so the file system is unchanged (`changed` false);
    /// a later failure may leave the card inconsistent (`changed` true).
    fn commit(mut self, geo: &Geometry, original: &FatState) -> Result<(), SdError> {
        let failed = |changed: bool| {
            move |error: io::Error| SdError {
                code: "EIO",
                message: if error.get_ref().is_some_and(|inner| inner.is::<SpillError>()) {
                    format!("Could not stage the change in temporary storage (is the disk full?): {error}{}", if changed { "; the card may be inconsistent" } else { "" })
                } else if changed {
                    format!("Writing the SD card image failed partway; the card may be inconsistent: {error}")
                } else {
                    format!("Writing the SD card image failed before anything on it changed: {error}")
                },
                changed,
            }
        };
        let mut stages: [Vec<u64>; 3] = Default::default();
        for index in self.dirty.keys().chain(self.spilled.keys()).copied() {
            stages[self.stage(index, geo, original) as usize].push(index);
        }
        for (stage, mut indices) in stages.into_iter().enumerate() {
            let fail = failed(stage > 0);
            indices.sort_unstable();
            let mut run: Vec<u8> = Vec::with_capacity(1 << 20);
            let mut run_start = 0u64;
            let mut next = None;
            for index in indices {
                let data = match self.dirty.remove(&index) {
                    Some(data) => data,
                    None => self.unspill(index).map_err(fail)?.unwrap(),
                };
                if next != Some(index) || run.len() >= 1 << 20 {
                    if !run.is_empty() {
                        commit_write(&self.file, &run, self.base + run_start).map_err(fail)?;
                        run.clear();
                    }
                    run_start = index * CHUNK;
                }
                run.extend_from_slice(&data);
                next = Some(index + 1);
            }
            if !run.is_empty() {
                commit_write(&self.file, &run, self.base + run_start).map_err(fail)?;
            }
            if stage == 0 {
                self.file.sync_data().map_err(fail)?;
            }
        }
        self.file.sync_data().map_err(failed(true))
    }
}

#[cfg(debug_assertions)]
thread_local! {
    static COMMIT_BUDGET: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Tests: make commits on this thread fail after writing `bytes` more
/// bytes to the image (None: never), to exercise partial commits. Only in
/// debug builds, which is how tests run.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn fail_commit_after(bytes: Option<u64>) {
    COMMIT_BUDGET.with(|budget| budget.set(bytes));
}

fn commit_write(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(debug_assertions)]
    if let Some(budget) = COMMIT_BUDGET.with(|budget| budget.get()) {
        if (buf.len() as u64) > budget {
            write_all_at(file, &buf[..budget as usize], offset)?;
            COMMIT_BUDGET.with(|b| b.set(Some(0)));
            return Err(io::Error::other("injected write failure"));
        }
        COMMIT_BUDGET.with(|b| b.set(Some(budget - buf.len() as u64)));
    }
    write_all_at(file, buf, offset)
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
    /// Whether another cluster links to this one (it is not a chain head).
    has_pred: Vec<bool>,
    free: u64,
    first_free: Option<u64>,
    /// The bad-cluster mark and the first end-of-chain value; values from
    /// `reserved` up to `end` (bad included) mark clusters, not links.
    bad: u32,
    reserved: u32,
    end: u32,
}

/// What a FAT entry's value means.
enum Next {
    Free,
    End,
    Link(u32),
    /// Bad or reserved: the cluster holds no data and no chain may enter it.
    Mark,
    Outside,
}

impl FatState {
    fn classify(&self, value: u32) -> Next {
        if value == 0 {
            Next::Free
        } else if value >= self.end {
            Next::End
        } else if value >= 2 && (value as usize) < self.next.len() && value != self.bad {
            Next::Link(value)
        } else if value >= self.reserved {
            Next::Mark
        } else {
            Next::Outside
        }
    }

    /// Whether cluster `c` holds data (part of a chain).
    fn allocated(&self, c: u32) -> bool {
        (c as usize) < self.next.len() && c >= 2 && matches!(self.classify(self.next[c as usize]), Next::End | Next::Link(_))
    }

    /// The clusters of a chain starting at `first` (none for an empty file).
    /// Never panics: a chain into free, bad, reserved or missing clusters is
    /// an error, as fat.js reports it.
    fn chain(&self, first: u32) -> Result<Vec<u32>, SdError> {
        let mut result = Vec::new();
        if first == 0 {
            return Ok(result);
        }
        let broken = || SdError::new("EINVAL", "Broken FAT cluster chain");
        if !self.allocated(first) {
            return Err(broken());
        }
        let mut cluster = first;
        loop {
            result.push(cluster);
            if result.len() > self.next.len() {
                return Err(SdError::new("EINVAL", "Cyclic FAT cluster chain"));
            }
            match self.classify(self.next[cluster as usize]) {
                Next::End => return Ok(result),
                Next::Link(next) if self.allocated(next) => cluster = next,
                _ => return Err(broken()),
            }
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
    let (bad, reserved, end) = match geo.kind {
        FatKind::Fat12 => (0xff7, 0xff0, 0xff8),
        FatKind::Fat16 => (0xfff7, 0xfff0, 0xfff8),
        FatKind::Fat32 => (0x0fff_fff7, 0x0fff_fff0, 0x0fff_fff8),
    };
    let mut state = FatState { next, has_pred: vec![false; entries], free: 0, first_free: None, bad, reserved, end };
    for cluster in 2..entries {
        match state.classify(state.next[cluster]) {
            Next::Free => {
                state.free += 1;
                state.first_free.get_or_insert(cluster as u64);
            }
            Next::End | Next::Mark => {}
            Next::Outside => return Err(damaged("a cluster chain points outside the card")),
            Next::Link(value) => {
                match state.classify(state.next[value as usize]) {
                    Next::Free => return Err(damaged("a cluster chain runs into free space")),
                    Next::Mark | Next::Outside => return Err(damaged("a cluster chain runs into a bad or reserved cluster")),
                    Next::End | Next::Link(_) => {}
                }
                if state.has_pred[value as usize] {
                    return Err(damaged("cross-linked cluster chains"));
                }
                state.has_pred[value as usize] = true;
            }
        }
    }
    // Each cluster has at most one predecessor, so walking every chain from
    // its head visits each cluster once; an allocated cluster left unvisited
    // sits on a cycle.
    let mut visited = vec![false; entries];
    for head in 2..entries {
        if !state.allocated(head as u32) || state.has_pred[head] {
            continue;
        }
        let mut cluster = head;
        loop {
            visited[cluster] = true;
            match state.classify(state.next[cluster]) {
                Next::Link(value) => cluster = value as usize,
                _ => break,
            }
        }
    }
    if (2..entries).any(|c| !visited[c] && state.allocated(c as u32)) {
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

/* Raw directory reading, for checks and plans fatfs cannot express: start
   clusters and sizes, short names, and where new entries would go. */

struct RawEntry {
    long: Option<String>,
    short: String,
    /// The 11-byte short name as stored.
    raw: [u8; 11],
    attr: u8,
    cluster: u32,
    size: u32,
}

impl RawEntry {
    fn is_dir(&self) -> bool {
        self.attr & 0x10 != 0
    }
    fn matches(&self, key: &str) -> bool {
        self.long.as_deref().is_some_and(|long| fold(long) == key) || fold(&self.short) == key
    }
}

/// Free directory slots, as fatfs's `find_free_entries` uses them: the
/// first run of deleted slots long enough, else the tail from the end
/// marker (or a deleted run just before it), growing the folder by whole
/// clusters (a fixed FAT12/16 root cannot grow).
#[derive(Clone)]
struct Slots {
    runs: Vec<(u64, u64)>,
    tail: u64,
    total: u64,
    fixed: bool,
}

impl Slots {
    /// Take `count` slots; Ok(new clusters the folder needs) or Err when a
    /// fixed root is full.
    fn take(&mut self, count: u64, per_cluster: u64) -> Result<u64, ()> {
        if let Some(run) = self.runs.iter_mut().find(|run| run.1 >= count) {
            run.0 += count;
            run.1 -= count;
            return Ok(0);
        }
        self.tail += count;
        if self.tail <= self.total {
            return Ok(0);
        }
        if self.fixed {
            return Err(());
        }
        let extra = (self.tail - self.total).div_ceil(per_cluster);
        self.total += extra * per_cluster;
        Ok(extra)
    }
}

const LFN_POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// The short name as text: "NAME.EXT", code page 437, as fatfs shows it.
fn short_text(raw: &[u8; 11]) -> String {
    let decode = |bytes: &[u8]| -> String {
        bytes.iter().map(|&b| CP437.decode(b)).collect::<String>().trim_end_matches(' ').to_string()
    };
    let mut base = raw[..8].to_vec();
    if base[0] == 0x05 {
        base[0] = 0xe5;
    }
    let (stem, ext) = (decode(&base), decode(&raw[8..11]));
    if ext.is_empty() { stem } else { format!("{stem}.{ext}") }
}

fn raw_dir(journal: &mut Journal, geo: &Geometry, fat: &FatState, cluster: Option<u32>) -> Result<(Vec<RawEntry>, Slots), SdError> {
    let fixed = cluster.is_none() && geo.fixed_root();
    let bytes = if fixed {
        let mut bytes = vec![0u8; (geo.root_entries * 32) as usize];
        journal.pread(geo.root_offset, &mut bytes).map_err(eio)?;
        bytes
    } else {
        let first = cluster.unwrap_or(geo.root_cluster);
        let mut bytes = Vec::new();
        for c in fat.chain(first)? {
            let start = bytes.len();
            bytes.resize(start + geo.cluster_size as usize, 0);
            journal.pread(geo.data_offset + (c as u64 - 2) * geo.cluster_size, &mut bytes[start..]).map_err(eio)?;
        }
        bytes
    };
    let records = bytes.as_chunks::<32>().0;
    let total = records.len() as u64;
    let mut slots = Slots { runs: Vec::new(), tail: total, total, fixed };
    let (mut run_start, mut run_len) = (0u64, 0u64);
    let mut result = Vec::new();
    let mut lfn: Vec<u16> = Vec::new();
    let mut lfn_checksum = None;
    for (i, record) in records.iter().enumerate() {
        let i = i as u64;
        if record[0] == 0 {
            slots.tail = if run_len > 0 { run_start } else { i };
            run_len = 0;
            break;
        }
        if record[0] == 0xe5 {
            if run_len == 0 {
                run_start = i;
            }
            run_len += 1;
            lfn.clear();
            lfn_checksum = None;
            continue;
        }
        if run_len > 0 {
            slots.runs.push((run_start, run_len));
            run_len = 0;
        }
        let attr = record[11];
        if attr & 0x3f == 0x0f {
            let sequence = (record[0] & 0x1f) as usize;
            if record[0] & 0x40 != 0 {
                lfn = vec![0xffff; sequence * 13];
                lfn_checksum = Some(record[13]);
            }
            if sequence >= 1 && sequence * 13 <= lfn.len() {
                for (k, &at) in LFN_POS.iter().enumerate() {
                    lfn[(sequence - 1) * 13 + k] = u16::from_le_bytes([record[at], record[at + 1]]);
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
        let raw: [u8; 11] = record[..11].try_into().unwrap();
        let short = short_text(&raw);
        if short == "." || short == ".." {
            continue;
        }
        let low = u16_at(record, 26) as u32;
        let high = if geo.kind == FatKind::Fat32 { (u16_at(record, 20) as u32) << 16 } else { 0 };
        result.push(RawEntry { long, short, raw, attr, cluster: high | low, size: u32_at(record, 28) as u32 });
    }
    if run_len > 0 {
        // Deleted slots run to the end of the folder: new entries start there.
        slots.tail = run_start;
    }
    Ok((result, slots))
}

fn raw_folder(journal: &mut Journal, geo: &Geometry, fat: &FatState, cluster: Option<u32>) -> Result<Vec<RawEntry>, SdError> {
    Ok(raw_dir(journal, geo, fat, cluster)?.0)
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

/// Every chain on the card belongs to exactly one file or folder: walk the
/// whole tree once (O(entries + clusters)). Refuses cross-links between any
/// two entries, entries that start inside another chain or point at broken
/// chains, folders that loop, and files longer than their clusters. Returns
/// how many allocated clusters no entry owns (lost clusters, harmless).
fn audit_tree(journal: &mut Journal, geo: &Geometry, fat: &FatState) -> Result<u64, SdError> {
    const MAX_DEPTH: usize = 256;
    let mut owned = vec![false; fat.next.len()];
    let mut claim = |chain: &[u32]| -> Result<(), SdError> {
        for &c in chain {
            if std::mem::replace(&mut owned[c as usize], true) {
                return Err(damaged("cross-linked files or folders"));
            }
        }
        Ok(())
    };
    if geo.kind == FatKind::Fat32 {
        if fat.has_pred[geo.root_cluster as usize] {
            return Err(damaged("the root folder starts inside another cluster chain"));
        }
        claim(&fat.chain(geo.root_cluster).map_err(|_| damaged("the root folder's cluster chain is broken"))?)?;
    }
    let mut pending: Vec<(Option<u32>, usize)> = vec![(None, 0)];
    while let Some((folder, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(damaged("folders nest too deeply"));
        }
        let entries = raw_folder(journal, geo, fat, folder).map_err(|e| if e.code == "EIO" { e } else { damaged("a folder's cluster chain is broken") })?;
        for entry in entries {
            if entry.cluster == 0 {
                if entry.is_dir() {
                    return Err(damaged("a folder has no clusters"));
                }
                if entry.size > 0 {
                    return Err(damaged("a file is longer than its clusters"));
                }
                continue;
            }
            if !fat.allocated(entry.cluster) {
                return Err(damaged("a file or folder starts at a free, bad or missing cluster"));
            }
            if fat.has_pred[entry.cluster as usize] {
                return Err(damaged("a file or folder starts inside another cluster chain"));
            }
            let chain = fat.chain(entry.cluster).map_err(|_| damaged("a file's cluster chain is broken"))?;
            if !entry.is_dir() && (chain.len() as u64) * geo.cluster_size < entry.size as u64 {
                return Err(damaged("a file is longer than its clusters"));
            }
            claim(&chain)?;
            if entry.is_dir() {
                pending.push((Some(entry.cluster), depth + 1));
            }
        }
    }
    Ok((2..fat.next.len() as u32).filter(|&c| fat.allocated(c) && !owned[c as usize]).count() as u64)
}

/// Read-only operations: check what fatfs will read on `names` (the
/// folders on the way, and the target's chain or the listed folder's
/// entries), so a damaged card is an error rather than a fatfs failure.
fn check_path(journal: &mut Journal, geo: &Geometry, fat: &FatState, names: &[&str], listing: bool) -> Result<(), SdError> {
    let broken = |_| damaged("a folder or file's cluster chain is broken");
    let mut folder = None;
    for (i, name) in names.iter().enumerate() {
        let key = fold(name);
        let entries = raw_folder(journal, geo, fat, folder).map_err(broken)?;
        let Some(entry) = entries.into_iter().find(|e| e.matches(&key)) else { return Ok(()) };
        if entry.is_dir() {
            if entry.cluster == 0 || !fat.allocated(entry.cluster) {
                return Err(damaged("a folder has no valid clusters"));
            }
            fat.chain(entry.cluster).map_err(broken)?;
            folder = Some(entry.cluster);
        } else {
            if i + 1 == names.len() && !listing {
                fat.chain(entry.cluster).map_err(broken)?;
            }
            return Ok(());
        }
    }
    if listing {
        for entry in raw_folder(journal, geo, fat, folder).map_err(broken)? {
            let ok = if entry.is_dir() { fat.allocated(entry.cluster) } else { entry.cluster == 0 || fat.allocated(entry.cluster) };
            if !ok {
                return Err(damaged("an entry points at a free, bad or missing cluster"));
            }
        }
    }
    Ok(())
}

type Disk = Journal;

fn inconsistent(error: SdError) -> SdError {
    SdError::new("EIO", format!("The change was not saved because it would leave the card inconsistent: {}", error.message))
}

/// Mount the card through a journal and run `action`. `pre` runs first on
/// the raw card (checks, plans) and hands its result to `action`. For
/// writes, the whole tree is audited before; the journal is committed only
/// if the action and fatfs's unmount succeed, the edited FAT still
/// validates and the edit lost no clusters; otherwise the image is untouched.
fn with_fs<P, R>(
    image: &Path,
    write: bool,
    pre: impl FnOnce(&mut Journal, &Geometry, &FatState) -> Result<P, SdError>,
    action: impl FnOnce(&FileSystem<&mut Disk>, &Geometry, P) -> Result<R, SdError>,
) -> Result<R, SdError> {
    let file = OpenOptions::new().read(true).write(write).open(image).map_err(|e| host(image, e))?;
    let geo = geometry(&file)?;
    let mut journal = Journal::new(file, geo.base, geo.end - geo.base);
    let fat = fat_scan(&mut journal, &geo)?;
    let planned = pre(&mut journal, &geo, &fat)?;
    let lost = if write { audit_tree(&mut journal, &geo, &fat)? } else { 0 };
    journal.pos = 0; // fatfs expects to start at the boot sector
    let value = {
        let fs = FileSystem::new(&mut journal, options())
            .map_err(|e| SdError::new("ENOTSUP", format!("Not a FAT file system the explorer can read: {e}")))?;
        let value = action(&fs, &geo, planned)?;
        fs.unmount().map_err(eio)?;
        value
    };
    if !write || !journal.is_dirty() {
        return Ok(value);
    }
    let after = fat_scan(&mut journal, &geo).map_err(inconsistent)?;
    if audit_tree(&mut journal, &geo, &after).map_err(inconsistent)? > lost {
        return Err(inconsistent(SdError::new("EIO", "clusters would be lost")));
    }
    sync_info(&mut journal, &geo, &after)?;
    journal.commit(&geo, &fat)?;
    Ok(value)
}

fn no_check(_: &mut Journal, _: &Geometry, _: &FatState) -> Result<(), SdError> {
    Ok(())
}

/// Lost (allocated but unreachable) clusters on the card, after checking
/// the whole tree as a write would. For tests and diagnostics.
#[doc(hidden)]
pub fn lost_clusters(image: &Path) -> Result<u64, SdError> {
    let file = OpenOptions::new().read(true).open(image).map_err(|e| host(image, e))?;
    let geo = geometry(&file)?;
    let mut journal = Journal::new(file, geo.base, geo.end - geo.base);
    let fat = fat_scan(&mut journal, &geo)?;
    audit_tree(&mut journal, &geo, &fat)
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
    with_fs(image, false, |journal, geo, fat| {
        check_path(journal, geo, fat, &names, true)?;
        Ok(fat.free * geo.cluster_size)
    }, |fs, geo, free| {
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
        Ok(SdListing { path: dir.to_string(), entries, free, total: geo.total(), fs_type: geo.name().to_string() })
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

/// What an upload knows about a name in a folder, during execution.
#[derive(Clone, Copy)]
enum Known {
    /// An entry that was on the card, by its index in `Folder::entries`.
    Entry { index: usize, is_dir: bool },
    Created,
}

struct Folder<'a, 'b> {
    dir: FsDir<'a, 'b>,
    entries: Vec<FsEntry<'a, 'b>>,
    /// Folded long and short names.
    names: HashMap<String, Known>,
}

impl<'a, 'b> Folder<'a, 'b> {
    fn load(dir: FsDir<'a, 'b>) -> Result<Self, SdError> {
        let entries = children(&dir)?;
        let mut names = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let known = Known::Entry { index, is_dir: entry.is_dir() };
            names.entry(fold(&entry.file_name())).or_insert(known);
            names.entry(fold(&entry.short_file_name())).or_insert(known);
        }
        Ok(Folder { dir, entries, names })
    }
}

/// Every card folder an upload touches, read once.
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
                let parent = self.folders.get(&fold(&current)).and_then(Option::as_ref);
                let loaded = match parent.and_then(|p| p.names.get(&fold(name)).map(|known| (p, *known))) {
                    None => None,
                    Some((parent, Known::Entry { index, is_dir: true })) => Some(Folder::load(parent.entries[index].to_dir())?),
                    Some(_) => return Err(SdError::new("ENOTDIR", format!("A file is in the way of folder {next}."))),
                };
                self.folders.insert(key.clone(), loaded);
            }
            current = next;
        }
        Ok(self.folders.get_mut(&fold(&current)).and_then(Option::as_mut))
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
        folder.names.insert(fold(name), Known::Created);
        self.folders.insert(fold(path), Some(Folder { dir, entries: Vec::new(), names: HashMap::new() }));
        Ok(())
    }
}

/* Upload planning: a simulation, on the raw card, of what fatfs will do in
   the order `upload` does it: the short name each new entry gets, where its
   directory slots go (and whether the folder grows), and the free clusters
   after every step, so the plan fails exactly when the upload would. */

/// The short name fatfs 0.3 gives `name` in a folder whose entries have
/// the `existing` short names: its ShortNameGenerator, reproduced.
fn short_name(name: &str, existing: &HashSet<[u8; 11]>) -> Option<[u8; 11]> {
    fn copy_part(dst: &mut [u8], src: &str) -> (usize, bool, bool) {
        let (mut at, mut lossy) = (0, false);
        for c in src.chars() {
            if at == dst.len() {
                return (at, false, lossy);
            }
            let fixed = match c {
                ' ' | '.' => {
                    lossy = true;
                    continue;
                }
                'A'..='Z' | 'a'..='z' | '0'..='9' => c,
                '!' | '#' | '$' | '%' | '&' | '\'' | '(' | ')' | '-' | '@' | '^' | '_' | '`' | '{' | '}' | '~' => c,
                _ => '_',
            };
            lossy = lossy || fixed != c;
            dst[at] = fixed.to_ascii_uppercase() as u8;
            at += 1;
        }
        (at, true, lossy)
    }
    let mut short = [b' '; 11];
    // As the patched fatfs (vendor/fatfs): leading dots are dropped, and an
    // empty stem becomes "_".
    let base = name.trim_start_matches('.');
    let (mut stem_len, fits, mut lossy) = match base.rfind('.') {
        Some(dot) => {
            let (stem_len, stem_fits, stem_lossy) = copy_part(&mut short[..8], &base[..dot]);
            let (_, ext_fits, ext_lossy) = copy_part(&mut short[8..], &base[dot + 1..]);
            (stem_len, stem_fits && ext_fits, stem_lossy || ext_lossy)
        }
        None => copy_part(&mut short[..8], base),
    };
    lossy = lossy || base.len() != name.len();
    if stem_len == 0 {
        short[0] = b'_';
        stem_len = 1;
        lossy = true;
    }
    let mut checksum = name.chars().fold(0u16, |sum, c| (sum >> 1).wrapping_add(sum << 15).wrapping_add(c as u16));
    let hex = |value: u16| -> [u8; 4] { format!("{value:04X}").as_bytes().try_into().unwrap() };
    let digit = |byte: u8| (byte as char).to_digit(10);
    let mut exact = false;
    for _ in 0..=u16::MAX as u32 {
        let (mut long_bits, mut sum_bits) = (0u16, 0u16);
        let long_len = stem_len.min(6);
        let sum_len = stem_len.min(2);
        for other in existing {
            if *other == short {
                exact = true;
            }
            let ext_matches = other[8..] == short[8..];
            if other[long_len] == b'~' && other[..long_len] == short[..long_len] && ext_matches {
                if let Some(n) = digit(other[long_len + 1]) {
                    long_bits |= 1 << n;
                }
            }
            if other[sum_len + 4] == b'~' && other[..sum_len] == short[..sum_len] && ext_matches {
                let sum = std::str::from_utf8(&other[sum_len..sum_len + 4]).ok().and_then(|s| u16::from_str_radix(s, 16).ok());
                if let (Some(n), Some(sum)) = (digit(other[sum_len + 5]), sum) {
                    if sum == checksum {
                        sum_bits |= 1 << n;
                    }
                }
            }
        }
        if !lossy && fits && !exact {
            return Some(short);
        }
        let build = |n: u32, with_sum: bool| {
            let mut out = [b' '; 11];
            let end = if with_sum {
                out[..sum_len].copy_from_slice(&short[..sum_len]);
                out[sum_len..sum_len + 4].copy_from_slice(&hex(checksum));
                sum_len + 4
            } else {
                out[..long_len].copy_from_slice(&short[..long_len]);
                long_len
            };
            out[end] = b'~';
            out[end + 1] = b'0' + n as u8;
            out[8..].copy_from_slice(&short[8..]);
            out
        };
        if let Some(n) = (1..5).find(|&n| long_bits & (1 << n) == 0) {
            return Some(build(n, false));
        }
        if let Some(n) = (1..10).find(|&n| sum_bits & (1 << n) == 0) {
            return Some(build(n, true));
        }
        checksum = checksum.wrapping_add(1);
    }
    None
}

struct SimEntry {
    /// The name the entry is listed under (its long name if it has one).
    name: String,
    is_dir: bool,
    cluster: u32,
    /// Data clusters a replaced file gives back.
    clusters: u64,
    planned: bool,
}

struct SimFolder {
    /// Folded long and short names -> index in `Sim::entries`.
    names: HashMap<String, usize>,
    shorts: HashSet<[u8; 11]>,
    slots: Slots,
}

struct Sim<'j> {
    journal: &'j mut Journal,
    geo: &'j Geometry,
    fat: &'j FatState,
    folders: HashMap<String, Option<SimFolder>>,
    entries: Vec<SimEntry>,
}

impl Sim<'_> {
    fn load(&mut self, cluster: Option<u32>) -> Result<SimFolder, SdError> {
        let (raw, slots) = raw_dir(self.journal, self.geo, self.fat, cluster)?;
        let mut folder = SimFolder { names: HashMap::new(), shorts: HashSet::new(), slots };
        for entry in raw {
            let clusters = if entry.is_dir() { 0 } else { self.fat.chain(entry.cluster)?.len() as u64 };
            let id = self.entries.len();
            let name = entry.long.clone().unwrap_or_else(|| entry.short.clone());
            self.entries.push(SimEntry { name, is_dir: entry.is_dir(), cluster: entry.cluster, clusters, planned: false });
            if let Some(long) = &entry.long {
                folder.names.entry(fold(long)).or_insert(id);
            }
            folder.names.entry(fold(&entry.short)).or_insert(id);
            folder.shorts.insert(entry.raw);
        }
        Ok(folder)
    }

    fn folder(&mut self, path: &str) -> Result<Option<&mut SimFolder>, SdError> {
        let names = parts(path)?;
        if !self.folders.contains_key("/") {
            let root = self.load(None)?;
            self.folders.insert("/".into(), Some(root));
        }
        let mut current = "/".to_string();
        for name in names {
            let next = join(&current, name);
            let key = fold(&next);
            if !self.folders.contains_key(&key) {
                let found = self.folders.get(&fold(&current)).and_then(Option::as_ref).and_then(|f| f.names.get(&fold(name)).copied());
                let loaded = match found {
                    None => None,
                    Some(id) if !self.entries[id].is_dir => {
                        return Err(SdError::new("ENOTDIR", format!("A file is in the way of folder {next}.")));
                    }
                    // Planned folders are added when they are planned.
                    Some(id) if self.entries[id].planned => return Err(SdError::new("EIO", format!("Upload plan lost folder {next}."))),
                    Some(id) => {
                        let cluster = self.entries[id].cluster;
                        Some(self.load(Some(cluster))?)
                    }
                };
                self.folders.insert(key, loaded);
            }
            current = next;
        }
        Ok(self.folders.get_mut(&fold(&current)).and_then(Option::as_mut))
    }

    fn find(&mut self, parent: &str, name: &str) -> Result<Option<usize>, SdError> {
        let folder = self.folder(parent)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {parent}")))?;
        Ok(folder.names.get(&fold(name)).copied())
    }

    /// Plan a new entry in `parent`: its short name and directory slots.
    /// Returns the clusters the folder grows by.
    fn add(&mut self, parent: &str, name: &str, is_dir: bool) -> Result<u64, SdError> {
        let per_cluster = self.geo.cluster_size / 32;
        self.find(parent, name)?;
        let id = self.entries.len();
        self.entries.push(SimEntry { name: name.to_string(), is_dir, cluster: 0, clusters: 0, planned: true });
        let folder = self.folders.get_mut(&fold(parent)).and_then(Option::as_mut).expect("folder loaded by find");
        let short = short_name(name, &folder.shorts)
            .ok_or_else(|| SdError::new("EEXIST", format!("{parent} has too many names like \"{name}\".")))?;
        let slots = 1 + (name.encode_utf16().count() as u64).div_ceil(13);
        let grown = folder.slots.take(slots, per_cluster).map_err(|_| {
            SdError::new("ENOSPC", ROOT_FULL)
        })?;
        folder.shorts.insert(short);
        folder.names.entry(fold(name)).or_insert(id);
        folder.names.entry(fold(&short_text(&short))).or_insert(id);
        Ok(grown)
    }
}

/// Check a host plan against the card, step by step in upload order:
/// conflicts (by long or short name), type clashes, two sources that name
/// one card entry, and free space at every step (replaced files give their
/// clusters back when they are rewritten).
fn simulate(journal: &mut Journal, geo: &Geometry, fat: &FatState, dir: &str, plan: &HostPlan) -> Result<UploadPlan, SdError> {
    let mut sim = Sim { journal, geo, fat, folders: HashMap::new(), entries: Vec::new() };
    if sim.folder(dir)?.is_none() {
        return Err(SdError::new("ENOENT", format!("Path does not exist: {dir}")));
    }
    let bytes: u64 = plan.files.iter().map(|(_, _, size)| size).sum();
    let mut free = fat.free as i64;
    let no_space = || {
        SdError::new(
            "ENOSPC",
            format!("Not enough space: {} to copy, {} free.", format_size(bytes), format_size(fat.free * geo.cluster_size)),
        )
    };
    let parent_of = |path: &str| -> Result<(String, String), SdError> {
        let (parents, name) = split(path)?;
        Ok((format!("/{}", parents.join("/")), name.to_string()))
    };
    for folder in &plan.dirs {
        let (parent, name) = parent_of(folder)?;
        match sim.find(&parent, &name)? {
            Some(id) if sim.entries[id].is_dir => {}
            Some(_) => return Err(SdError::new("ENOTDIR", format!("{folder} is a file on the SD card; it cannot become a folder."))),
            None => {
                // The new folder's own cluster holds "." and "..".
                free -= sim.add(&parent, &name, true)? as i64 + 1;
                let slots = Slots { runs: Vec::new(), tail: 2, total: geo.cluster_size / 32, fixed: false };
                sim.folders.insert(fold(folder), Some(SimFolder { names: HashMap::new(), shorts: HashSet::new(), slots }));
                if free < 0 {
                    return Err(no_space());
                }
            }
        }
    }
    let mut conflicts = Vec::new();
    let mut targeted = HashSet::new();
    for (_, path, size) in &plan.files {
        let (parent, name) = parent_of(path)?;
        match sim.find(&parent, &name)? {
            Some(id) => {
                let entry = &sim.entries[id];
                if entry.is_dir {
                    return Err(SdError::new("EISDIR", format!("{path} is a folder on the SD card; a file cannot replace it.")));
                }
                if entry.planned || !targeted.insert(id) {
                    return Err(SdError::new("EINVAL", format!("Two sources would be copied to {path}: their names refer to the same file on the card.")));
                }
                free += entry.clusters as i64;
                // Name the file that is replaced, also when the source matched its short alias.
                conflicts.push(if parent == "/" { format!("/{}", entry.name) } else { format!("{parent}/{}", entry.name) });
            }
            None => free -= sim.add(&parent, &name, false)? as i64,
        }
        free -= size.div_ceil(geo.cluster_size) as i64;
        if free < 0 {
            return Err(no_space());
        }
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
/// A source whose name matches an existing entry's long or short name
/// replaces that entry (listed in `conflicts`). ENOENT if `dir` does not
/// exist, EINVAL for an invalid name or two sources naming one card entry,
/// ENOTSUP above the entry or depth limits, ENOSPC if the copy does not fit
/// at any step, EISDIR if a file would replace a folder or ENOTDIR if a
/// folder would replace a file.
pub fn plan_upload(image: &Path, dir: &str, sources: &[PathBuf]) -> Result<UploadPlan, SdError> {
    let plan = host_plan(dir, sources)?;
    with_fs(image, false, |journal, geo, fat| simulate(journal, geo, fat, dir, &plan), |_, _, summary| Ok(summary))
}

/// Open a host source without following a symbolic link swapped in since
/// the walk, and only if it is still a regular file.
fn open_source(source: &Path) -> Result<File, SdError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let input = options.open(source).map_err(|e| host(source, e))?;
    if !input.metadata().map_err(|e| host(source, e))?.is_file() {
        return Err(SdError::new("EINVAL", format!("{} is no longer a regular file.", source.display())));
    }
    Ok(input)
}

fn copy_in(file: &mut fatfs::File<&mut Disk>, source: &Path) -> Result<(), SdError> {
    let mut input = open_source(source)?;
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
/// existing files. `confirmed` lists the card paths `plan_upload` reported
/// as conflicts and the user agreed to replace: if the conflicts are
/// different now (the card or the host files changed since the plan), the
/// upload is refused with EEXIST before anything is written. Streams file
/// contents; never loads a whole file or the card into memory. One
/// transaction (see the module header): nothing reaches the card unless
/// every file was copied.
pub fn upload(image: &Path, dir: &str, sources: &[PathBuf], confirmed: &[String]) -> Result<UploadPlan, SdError> {
    let plan = host_plan(dir, sources)?;
    let agreed: HashSet<String> = confirmed.iter().map(|path| fold(path)).collect();
    let pre = |journal: &mut Journal, geo: &Geometry, fat: &FatState| {
        let summary = simulate(journal, geo, fat, dir, &plan)?;
        if summary.conflicts.iter().map(|path| fold(path)).collect::<HashSet<_>>() != agreed {
            return Err(SdError::new("EEXIST", "The SD card changed since the copy was planned; try again."));
        }
        Ok(summary)
    };
    with_fs(image, true, pre, |fs, _, summary| {
        let mut index = Index::new(fs);
        for folder in &plan.dirs {
            if index.lookup(folder)?.is_none() {
                index.create_dir(folder)?;
            }
        }
        // Folder -> long names created there, to check them afterwards.
        let mut created: HashMap<String, (String, Vec<String>)> = HashMap::new();
        for (source, path, _) in &plan.files {
            let (parents, name) = split(path)?;
            let parent_path = format!("/{}", parents.join("/"));
            let existing = index.lookup(path)?;
            let folder = index.folder(&parent_path)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {parent_path}")))?;
            let mut file = match existing {
                Some(Known::Entry { index, .. }) => folder.entries[index].to_file(),
                Some(Known::Created) => return Err(SdError::new("EINVAL", format!("Two sources would be copied to {path}."))),
                None => {
                    let file = folder.dir.create_file(name).map_err(eio)?;
                    folder.names.insert(fold(name), Known::Created);
                    created.entry(fold(&parent_path)).or_insert_with(|| (parent_path.clone(), Vec::new())).1.push(name.to_string());
                    file
                }
            };
            copy_in(&mut file, source)?;
        }
        // Every new name must have become its own entry: fatfs opens an
        // existing file whose short name matches instead of creating one.
        for (parent, names) in created.values() {
            let folder = index.folder(parent)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {parent}")))?;
            let mut by_name = HashMap::new();
            for (position, entry) in children(&folder.dir)?.iter().enumerate() {
                by_name.entry(fold(&entry.file_name())).or_insert(position);
                by_name.entry(fold(&entry.short_file_name())).or_insert(position);
            }
            let mut seen = HashSet::new();
            for name in names {
                if !by_name.get(&fold(name)).is_some_and(|&position| seen.insert(position)) {
                    return Err(SdError::new("EINVAL", format!("Two sources would be copied to the same file as {name}.")));
                }
            }
        }
        Ok(summary)
    })
}

/// Whether `dest` names the image file itself (or a hard link to it).
fn is_image(image: &Path, dest: &Path) -> bool {
    if let (Some(a), Some(b)) = (crate::host::file_identity(image), crate::host::file_identity(dest)) {
        if a == b {
            return true;
        }
    }
    let canonical = |path: &Path| -> Option<PathBuf> {
        if let Ok(path) = dunce::canonicalize(path) {
            return Some(path);
        }
        let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        Some(dunce::canonicalize(folder).ok()?.join(path.file_name()?))
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
    let names = parts(file)?;
    with_fs(image, false, |journal, geo, fat| check_path(journal, geo, fat, &names, false), |fs, _, ()| {
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
    with_fs(image, true, |_, geo, fat| Ok(fat.free * geo.cluster_size), |fs, geo, free| {
        let parent = open_dir(fs, &parents)?;
        if find(&parent, name)?.is_some() {
            return Err(SdError::new("EEXIST", "Path already exists"));
        }
        if free < geo.cluster_size {
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
    with_fs(image, true, no_check, |fs, _, ()| {
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
        |fs, _, ()| {
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
