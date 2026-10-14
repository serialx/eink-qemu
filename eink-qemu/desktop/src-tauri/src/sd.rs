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
//! - Errors carry fat.js-style codes: ENOENT EEXIST EINVAL ENOTDIR EISDIR
//!   ENOTEMPTY ENOSPC ENOTSUP EIO. `SdError::precheck()` is true for the codes
//!   raised before any sector is written (sd-ops.js `sdPrecheckErrors`).
//!
//! Notes on fatfs 0.3: it trusts the FSInfo free count, so free space is
//! counted from the FAT here and FSInfo is rewritten after every change, as
//! fat.js does. Its long-name encoder only accepts characters up to U+FFFF
//! (no U+007F) and at most 255 UTF-8 bytes; such names are rejected with
//! EINVAL before anything is written.

use fatfs::{Dir, DirEntry, FileSystem, FsOptions};
use fscommon::{BufStream, StreamSlice};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SdError {
    pub code: &'static str,
    pub message: String,
}

impl SdError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
    /// Raised before anything was written: the card is unchanged.
    pub fn precheck(&self) -> bool {
        matches!(self.code, "EEXIST" | "ENOENT" | "EINVAL" | "ENOTSUP" | "EISDIR" | "ENOTDIR" | "ENOTEMPTY")
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
    /// The listed folder, normalised to the card's stored capitalisation is
    /// not required: echo the requested path.
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

/* Volume geometry, read from the boot sector like fat.js. */

#[derive(Debug, Clone, Copy, PartialEq)]
enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone)]
struct Geometry {
    base: u64,
    end: u64,
    bytes_per_sector: u64,
    reserved: u64,
    fat_sectors: u64,
    clusters: u64,
    cluster_size: u64,
    kind: FatKind,
    fs_info: Option<u64>,
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
}

fn u16_at(buf: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([buf[at], buf[at + 1]]) as u64
}
fn u32_at(buf: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as u64
}

fn read_sector(file: &mut File, offset: u64) -> Result<[u8; 512], SdError> {
    let mut sector = [0u8; 512];
    file.seek(SeekFrom::Start(offset)).map_err(eio)?;
    file.read_exact(&mut sector)
        .map_err(|_| SdError::new("ENOTSUP", "Not a FAT disk image (truncated boot sector)"))?;
    Ok(sector)
}

fn plausible_bpb(sector: &[u8; 512]) -> bool {
    matches!(u16_at(sector, 11), 512 | 1024 | 2048 | 4096) && sector[13] != 0 && u16_at(sector, 14) != 0
}

fn geometry(file: &mut File) -> Result<Geometry, SdError> {
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
    let fs_info = match kind {
        FatKind::Fat32 => match u16_at(&boot, 48) {
            0 | 0xffff => None,
            sector if sector < reserved => Some(sector * bps),
            _ => None,
        },
        _ => None,
    };
    Ok(Geometry {
        base,
        end,
        bytes_per_sector: bps,
        reserved,
        fat_sectors,
        clusters,
        cluster_size: bps * spc,
        kind,
        fs_info,
    })
}

type Disk = BufStream<StreamSlice<File>>;

/// Free clusters and the first free cluster, counted from the first FAT.
fn count_free(disk: &mut Disk, geo: &Geometry) -> Result<(u64, Option<u64>), SdError> {
    let fat_start = geo.reserved * geo.bytes_per_sector;
    let entries = geo.clusters + 2;
    let mut free = 0;
    let mut first = None;
    disk.seek(SeekFrom::Start(fat_start)).map_err(eio)?;
    match geo.kind {
        FatKind::Fat12 => {
            let mut fat = vec![0u8; (entries * 3).div_ceil(2) as usize + 1];
            let length = fat.len().min((geo.fat_sectors * geo.bytes_per_sector) as usize);
            disk.read_exact(&mut fat[..length]).map_err(eio)?;
            for cluster in 2..entries {
                let at = (cluster * 3 / 2) as usize;
                let pair = u16::from_le_bytes([fat[at], fat[at + 1]]);
                let value = if cluster & 1 == 1 { pair >> 4 } else { pair & 0xfff };
                if value == 0 {
                    free += 1;
                    first.get_or_insert(cluster);
                }
            }
        }
        FatKind::Fat16 | FatKind::Fat32 => {
            let width: u64 = if geo.kind == FatKind::Fat16 { 2 } else { 4 };
            let mut chunk = vec![0u8; 1 << 20];
            let mut cluster = 0u64;
            while cluster < entries {
                let count = ((entries - cluster) * width).min(chunk.len() as u64) as usize;
                disk.read_exact(&mut chunk[..count]).map_err(eio)?;
                for raw in chunk[..count].chunks_exact(width as usize) {
                    let value = if width == 2 {
                        u16::from_le_bytes([raw[0], raw[1]]) as u32
                    } else {
                        u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) & 0x0fff_ffff
                    };
                    if cluster >= 2 && value == 0 {
                        free += 1;
                        first.get_or_insert(cluster);
                    }
                    cluster += 1;
                }
            }
        }
    }
    Ok((free, first))
}

/// Rewrite the FAT32 FSInfo hints from the FAT, as fat.js `syncInfo` does.
fn sync_info(disk: &mut Disk, geo: &Geometry) -> Result<(), SdError> {
    let Some(offset) = geo.fs_info else { return Ok(()) };
    let mut sector = [0u8; 512];
    disk.seek(SeekFrom::Start(offset)).map_err(eio)?;
    disk.read_exact(&mut sector).map_err(eio)?;
    if u32_at(&sector, 0) != 0x4161_5252 || u32_at(&sector, 484) != 0x6141_7272 || u32_at(&sector, 508) != 0xaa55_0000 {
        return Ok(());
    }
    let (free, first) = count_free(disk, geo)?;
    sector[488..492].copy_from_slice(&(free as u32).to_le_bytes());
    sector[492..496].copy_from_slice(&first.map_or(0xffff_ffff, |c| c as u32).to_le_bytes());
    disk.seek(SeekFrom::Start(offset)).map_err(eio)?;
    disk.write_all(&sector).map_err(eio)?;
    Ok(())
}

struct Stats {
    free: u64,
}

/// Mount the card, run `action`, unmount and (for writes) fix FSInfo and flush.
fn with_fs<R>(
    image: &Path,
    write: bool,
    action: impl FnOnce(&FileSystem<&mut Disk>, &Geometry, &Stats) -> Result<R, SdError>,
) -> Result<R, SdError> {
    let mut file = OpenOptions::new().read(true).write(write).open(image).map_err(|e| host(image, e))?;
    let geo = geometry(&mut file)?;
    let slice = StreamSlice::new(file, geo.base, geo.end).map_err(eio)?;
    let mut disk: Disk = BufStream::new(slice);
    let (free, _) = count_free(&mut disk, &geo)?;
    disk.seek(SeekFrom::Start(0)).map_err(eio)?;
    let stats = Stats { free: free * geo.cluster_size };
    let result = {
        let fs = FileSystem::new(&mut disk, FsOptions::new()).map_err(|e| {
            let mut error = eio(e);
            error.code = "ENOTSUP";
            error
        })?;
        let result = action(&fs, &geo, &stats);
        let unmounted = fs.unmount().map_err(eio);
        match (result, unmounted) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    };
    if write {
        // Even a failed write may have changed sectors; keep FSInfo honest.
        let synced = sync_info(&mut disk, &geo).and_then(|()| disk.flush().map_err(eio));
        if let (Ok(_), Err(error)) = (&result, synced) {
            return Err(error);
        }
    }
    result
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
fn split<'p>(path: &'p str) -> Result<(Vec<&'p str>, &'p str), SdError> {
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
    with_fs(image, false, |fs, geo, stats| {
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
        if !plan.dirs.iter().any(|d| fold(d) == fold(&target)) {
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
    let mut plan = HostPlan { dirs: Vec::new(), files: Vec::new() };
    let mut seen = HashMap::new();
    let mut count = 0;
    for source in sources {
        walk(source, dir, 1, true, &mut plan, &mut seen, &mut count)?;
    }
    Ok(plan)
}

/// What exists on the card along a path: Ok(None) when the path (or a
/// parent) does not exist yet.
fn probe<'a, 'b>(fs: &'a FileSystem<&'b mut Disk>, path: &str) -> Result<Option<FsEntry<'a, 'b>>, SdError> {
    let (parents, name) = split(path)?;
    let mut dir = fs.root_dir();
    for parent in parents {
        match find(&dir, parent)? {
            None => return Ok(None),
            Some(entry) if !entry.is_dir() => return Err(SdError::new("ENOTDIR", format!("A file is in the way of folder {parent}."))),
            Some(entry) => dir = entry.to_dir(),
        }
    }
    find(&dir, name)
}

/// Check a host plan against the card: conflicts, type clashes and space.
fn check_plan(fs: &FileSystem<&mut Disk>, geo: &Geometry, stats: &Stats, dir: &str, plan: &HostPlan) -> Result<UploadPlan, SdError> {
    let names = parts(dir)?;
    open_dir(fs, &names)?;
    let clusters = |bytes: u64| bytes.div_ceil(geo.cluster_size);
    let mut needed = 0u64;
    for folder in &plan.dirs {
        match probe(fs, folder)? {
            Some(entry) if !entry.is_dir() => {
                return Err(SdError::new("ENOTDIR", format!("{folder} is a file on the SD card; it cannot become a folder.")));
            }
            Some(_) => {}
            None => needed += 1,
        }
    }
    let mut conflicts = Vec::new();
    let mut bytes = 0;
    for (_, path, size) in &plan.files {
        bytes += size;
        needed += clusters(*size);
        match probe(fs, path)? {
            Some(entry) if entry.is_dir() => {
                return Err(SdError::new("EISDIR", format!("{path} is a folder on the SD card; a file cannot replace it.")));
            }
            Some(entry) => {
                needed = needed.saturating_sub(clusters(entry.len()));
                conflicts.push(path.clone());
            }
            None => {}
        }
    }
    let free = stats.free / geo.cluster_size;
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
    with_fs(image, false, |fs, geo, stats| check_plan(fs, geo, stats, dir, &plan))
}

fn copy_in<'a, 'b>(dir: &FsDir<'a, 'b>, existing: Option<FsEntry<'a, 'b>>, name: &str, source: &Path) -> Result<(), SdError> {
    let mut input = File::open(source).map_err(|e| host(source, e))?;
    let mut file = match existing {
        Some(entry) => entry.to_file(),
        None => dir.create_file(name).map_err(eio)?,
    };
    let result = (|| {
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
    })();
    drop(file);
    if result.is_err() {
        // Do not leave a truncated copy behind.
        let _ = dir.remove(name);
    }
    result
}

/// Copy `sources` into `dir`, creating folders as needed and replacing
/// existing files (the caller confirmed `plan_upload`'s conflicts). Streams
/// file contents; never loads a whole file or the card into memory.
pub fn upload(image: &Path, dir: &str, sources: &[PathBuf]) -> Result<UploadPlan, SdError> {
    let plan = host_plan(dir, sources)?;
    with_fs(image, true, |fs, geo, stats| {
        let summary = check_plan(fs, geo, stats, dir, &plan)?;
        for folder in &plan.dirs {
            let (parents, name) = split(folder)?;
            let parent = open_dir(fs, &parents)?;
            if find(&parent, name)?.is_none() {
                parent.create_dir(name).map_err(eio)?;
            }
        }
        for (source, path, _) in &plan.files {
            let (parents, name) = split(path)?;
            let parent = open_dir(fs, &parents)?;
            let existing = find(&parent, name)?;
            let name = existing.as_ref().map_or_else(|| name.to_string(), |e| e.file_name());
            copy_in(&parent, existing, &name, source)?;
        }
        Ok(summary)
    })
}

/// Copy one file from the card to `dest` on the host (streamed).
pub fn export(image: &Path, file: &str, dest: &Path) -> Result<(), SdError> {
    let (parents, name) = split(file)?;
    let folder = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    with_fs(image, false, |fs, _, _| {
        let parent = open_dir(fs, &parents)?;
        let entry = find(&parent, name)?.ok_or_else(|| SdError::new("ENOENT", format!("Path does not exist: {name}")))?;
        if entry.is_dir() {
            return Err(SdError::new("EISDIR", "Cannot read a folder as a file"));
        }
        let mut input = entry.to_file();
        let mut output = tempfile::NamedTempFile::new_in(folder).map_err(|e| host(folder, e))?;
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

/// Give a finished temporary file ordinary permissions and move it into place.
fn finish(file: tempfile::NamedTempFile, dest: &Path) -> Result<(), SdError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.as_file().set_permissions(std::fs::Permissions::from_mode(0o644));
    }
    file.persist(dest).map_err(|e| host(dest, e.error))?;
    Ok(())
}

pub fn mkdir(image: &Path, path: &str) -> Result<(), SdError> {
    let (parents, name) = split(path)?;
    check_name(name)?;
    with_fs(image, true, |fs, geo, stats| {
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
    with_fs(image, true, |fs, _, _| {
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
            // through a temporary name.
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

/// Remove a file, or a folder and everything in it.
pub fn remove(image: &Path, path: &str) -> Result<(), SdError> {
    let (parents, name) = split(path)?;
    with_fs(image, true, |fs, _, _| {
        let parent = open_dir(fs, &parents)?;
        let entry = find(&parent, name)?.ok_or_else(|| SdError::new("ENOENT", "Path does not exist"))?;
        remove_tree(&parent, entry)
    })
}

/// The embedded blank 64 MiB FAT32 card (mksd.py, 512-byte clusters).
pub const BLANK_CARD_GZ: &[u8] = include_bytes!("../resources/blank-sd.img.gz");

/// Write a blank card to `dest` (via a temporary file and rename).
pub fn create_blank(dest: &Path) -> std::io::Result<()> {
    let folder = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let mut output = tempfile::NamedTempFile::new_in(folder)?;
    let mut input = flate2::read::GzDecoder::new(BLANK_CARD_GZ);
    io::copy(&mut input, &mut output)?;
    output.as_file().sync_all()?;
    finish(output, dest).map_err(|e| io::Error::other(e.message))
}
