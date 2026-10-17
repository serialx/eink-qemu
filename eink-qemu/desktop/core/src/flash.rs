//! 16 MiB flash images for the emulator, as eink-qemu/mkflash.py composes
//! them: a release app gets the bundled bootloader and a partition table
//! (Arduino-ESP32's 16 MB default, the Picco's stock layout, or the one
//! given with --partitions); a PlatformIO or ESP-IDF build directory is
//! merged from its own bootloader, table and app; a merged image keeps its
//! own layout; a 16 MiB dump passes through. App bytes are never modified.
//! The only firmware-specific step is CrossPoint's: its X3 build expects a
//! cached device type in NVS, written only into images that are
//! CrossPoint's (eink-qemu/profiles/crosspoint.json). X4 Pro .xota updates
//! are composed by the desktop app and the browser page only.

use crate::firmware::Chip;
use sha2::Digest;
use std::path::{Path, PathBuf};

pub const FLASH_SIZE: usize = 0x100_0000;
const TABLE_OFFSET: usize = 0x8000;
const TABLE_SIZE: usize = 0xC00;
const ENTRY_SIZE: usize = 32;

const BOOT_C3: &[u8] = include_bytes!("../../../boot/bootloader-esp32c3.bin");
const BOOT_S3: &[u8] = include_bytes!("../../../boot/bootloader-esp32s3.bin");
const PARTITIONS: &str = include_str!("../../../boot/partitions.csv");
const PARTITIONS_PICCO: &str = include_str!("../../../boot/partitions-picco.csv");

/// A composed image and what it is for.
pub struct Flash {
    pub image: Vec<u8>,
    /// The ESP chip in the image's header: 5 (ESP32-C3) or 9 (ESP32-S3).
    pub chip: u16,
    /// The board it runs on: requested, hinted by the filename, or the chip's default.
    pub machine: String,
    /// The app image the flash was composed from, for matching an ELF.
    pub app: Option<PathBuf>,
    /// How the input was read: "app", "build", "merged" or "dump".
    pub kind: &'static str,
    /// Which partition table the image has.
    pub partitions: String,
    /// The NVS entries written for the firmware, when any.
    pub nvs_seed: Option<String>,
}

/// The boards that take a chip, default first.
pub fn machines(chip: u16) -> &'static [&'static str] {
    Chip::from_id(chip).map_or(&[], Chip::devices)
}

fn chip_name(chip: u16) -> &'static str {
    if chip == 5 { "ESP32-C3" } else { "ESP32-S3" }
}

/// The chip of an ESP image header, checked against `machine` when given.
pub fn detect_chip(data: &[u8], machine: Option<&str>) -> Result<u16, String> {
    if data.len() < 24 || data[0] != 0xE9 {
        return Err("expected an ESP image (0xE9 magic and a 24-byte header)".into());
    }
    let chip = u16::from_le_bytes([data[12], data[13]]);
    if chip != 5 && chip != 9 {
        return Err(format!("unsupported ESP chip ID {chip}; expected 5 (ESP32-C3) or 9 (ESP32-S3)"));
    }
    if let Some(machine) = machine {
        if !machines(chip).contains(&machine) {
            return Err(format!(
                "--machine {machine} does not take this {} image; use {}",
                chip_name(chip),
                machines(chip).join(" or ")
            ));
        }
    }
    Ok(chip)
}

/// One partition-table entry: type, subtype, offset, size.
type Entry = (u8, u8, u32, u32);

/// Python's `int(text, 0)`: an optional sign, then decimal (no leading
/// zeros), 0x hex, 0o octal or 0b binary, with single underscores between
/// digits.
fn python_int(text: &str) -> Option<i64> {
    let (negative, body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let lower = body.to_ascii_lowercase();
    let (radix, digits) = match lower.get(..2) {
        Some("0x") => (16, &lower[2..]),
        Some("0o") => (8, &lower[2..]),
        Some("0b") => (2, &lower[2..]),
        _ => (10, lower.as_str()),
    };
    // Python allows one underscore after the prefix and between digits.
    let digits = if radix != 10 { digits.strip_prefix('_').unwrap_or(digits) } else { digits };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|&c| c != '_').collect();
    if radix == 10 && clean.len() > 1 && clean.starts_with('0') && clean.chars().any(|c| c != '0') {
        return None;
    }
    let value = i64::from_str_radix(&clean, radix).ok()?;
    Some(if negative { -value } else { value })
}

/// gen_esp32part.py's `parse_int`: K/M suffixes (any case), then
/// `int(x, 0)`, then a keyword (any case).
fn parse_int(text: &str, keywords: &[(&str, u8)]) -> Result<i64, String> {
    let lower = text.to_ascii_lowercase();
    for (letter, scale) in [('k', 1024), ('m', 1024 * 1024)] {
        if lower.ends_with(letter) {
            return parse_int(&text[..text.len() - 1], keywords).map(|v| v * scale);
        }
    }
    if let Some(v) = python_int(text) {
        return Ok(v);
    }
    if keywords.is_empty() {
        return Err(format!("invalid field value {text}"));
    }
    keywords
        .iter()
        .find(|(k, _)| *k == lower)
        .map(|&(_, v)| i64::from(v))
        .ok_or_else(|| format!("value '{text}' is not valid; known keywords: {}", keywords.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")))
}

const APP_SUBTYPES: &[(&str, u8)] = &[
    ("factory", 0x00), ("test", 0x20), ("ota_0", 0x10), ("ota_1", 0x11), ("ota_2", 0x12), ("ota_3", 0x13),
    ("ota_4", 0x14), ("ota_5", 0x15), ("ota_6", 0x16), ("ota_7", 0x17), ("ota_8", 0x18), ("ota_9", 0x19),
    ("ota_10", 0x1a), ("ota_11", 0x1b), ("ota_12", 0x1c), ("ota_13", 0x1d), ("ota_14", 0x1e), ("ota_15", 0x1f),
    ("tee_0", 0x30), ("tee_1", 0x31),
];
const DATA_SUBTYPES: &[(&str, u8)] = &[
    ("ota", 0x00), ("phy", 0x01), ("nvs", 0x02), ("coredump", 0x03), ("nvs_keys", 0x04), ("efuse", 0x05),
    ("undefined", 0x06), ("esphttpd", 0x80), ("fat", 0x81), ("spiffs", 0x82), ("littlefs", 0x83), ("tee_ota", 0x90),
];
const FLAG_ENCRYPTED: u32 = 1;
const FLAG_READONLY: u32 = 2;

/// One partition-table entry with its label and flags, for checking.
struct Part {
    name: Vec<u8>,
    kind: u8,
    subtype: u8,
    offset: u32,
    size: u32,
    flags: u32,
}

/// ESP-IDF's table rules (gen_esp32part.py's verify): offsets aligned (apps
/// to 64 KiB, the rest to 4 KiB), app sizes to 4 KiB, unique labels, no
/// overlap or entry below the table's sector, at most one OTA data
/// partition and of 0x2000 bytes, OTA data and core dumps writable, a
/// writable NVS of at least 0x3000 bytes; and here, everything inside 16 MiB.
fn verify(parts: &[Part]) -> Result<(), String> {
    if parts.is_empty() {
        return Err("empty partition table".into());
    }
    let label = |p: &Part| String::from_utf8_lossy(&p.name).into_owned();
    for p in parts {
        let align = if p.kind == 0 { 0x10000 } else { 0x1000 };
        if p.size == 0 || p.offset as u64 + p.size as u64 > FLASH_SIZE as u64 {
            return Err(format!("partition '{}' ({:#x} bytes at {:#x}) lies outside 16 MiB flash", label(p), p.size, p.offset));
        }
        if p.offset % align != 0 {
            return Err(format!("partition '{}' offset {:#x} is not aligned to {align:#x}", label(p), p.offset));
        }
        if p.kind == 0 && p.size % 0x1000 != 0 {
            return Err(format!("app partition '{}' size {:#x} is not aligned to 0x1000", label(p), p.size));
        }
        if p.kind == 1 && (p.subtype == 0 || p.subtype == 3) && p.flags & FLAG_READONLY != 0 {
            return Err(format!("partition '{}' (OTA data or core dump) is always read-write and cannot be read-only", label(p)));
        }
        if p.kind == 1 && p.subtype == 2 && p.size < 0x3000 && p.flags & FLAG_READONLY == 0 {
            return Err(format!("NVS partition '{}' of {:#x} bytes must be flagged readonly (read/write NVS needs at least 0x3000)", label(p), p.size));
        }
    }
    for (i, p) in parts.iter().enumerate() {
        if parts[..i].iter().any(|q| q.name == p.name) {
            return Err(format!("partition names must be unique: '{}' appears twice", label(p)));
        }
    }
    let mut sorted: Vec<&Part> = parts.iter().collect();
    sorted.sort_by_key(|p| p.offset);
    for (i, p) in sorted.iter().enumerate() {
        if (p.offset as usize) < TABLE_OFFSET + 0x1000 {
            return Err(format!("partition '{}' offset {:#x} is below {:#x}", label(p), p.offset, TABLE_OFFSET + 0x1000));
        }
        if i > 0 && p.offset < sorted[i - 1].offset + sorted[i - 1].size {
            return Err(format!("partition '{}' at {:#x} overlaps '{}'", label(p), p.offset, label(sorted[i - 1])));
        }
    }
    let ota: Vec<&Part> = parts.iter().filter(|p| p.kind == 1 && p.subtype == 0).collect();
    if ota.len() > 1 {
        return Err("found more than one OTA data partition (type data, subtype ota)".into());
    }
    if ota.first().is_some_and(|p| p.size != 0x2000) {
        return Err("the OTA data partition must have size 0x2000".into());
    }
    Ok(())
}

/// Encode a partition CSV in ESP-IDF's binary format, reading it as
/// gen_esp32part.py does: fields split at every comma (no quoting), named
/// or numeric types and subtypes in any case, `int(x, 0)` numbers with K/M
/// suffixes, an empty data subtype meaning `undefined`, empty offsets placed
/// after the previous partition (apps on 64 KiB), a negative size meaning
/// "up to that offset", and the encrypted/readonly flags.
pub fn partition_table(csv: &str) -> Result<Vec<u8>, String> {
    let mut parts: Vec<Part> = Vec::new();
    let mut last_end = (TABLE_OFFSET + 0x1000) as i64;
    for (number, line) in csv.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |e: String| format!("partition CSV line {}: {e}", number + 1);
        let padded = format!("{line},,,,");
        let fields: Vec<&str> = padded.split(',').map(str::trim).collect();
        let (name, kind, subtype, offset, size, flags) = (fields[0], fields[1], fields[2], fields[3], fields[4], fields[5]);
        if kind.is_empty() {
            return Err(at("the type field can't be empty".into()));
        }
        let kind = parse_int(kind, &[("app", 0), ("data", 1), ("bootloader", 2), ("partition_table", 3)]).map_err(at)?;
        if kind == 2 || kind == 3 {
            return Err(at("bootloader and partition_table entries are not supported here".into()));
        }
        let kind = u8::try_from(kind).map_err(|_| at(format!("type {kind} out of range")))?;
        let subtype = match (kind, subtype) {
            (0, "") => return Err(at("an app partition cannot have an empty subtype".into())),
            (_, "") => 0x06,
            (0, text) => parse_int(text, APP_SUBTYPES).map_err(at)?,
            (1, text) => parse_int(text, DATA_SUBTYPES).map_err(at)?,
            (_, text) => parse_int(text, &[]).map_err(at)?,
        };
        let subtype = u8::try_from(subtype).map_err(|_| at(format!("subtype {subtype} out of range")))?;
        if size.is_empty() {
            return Err(at("the size field can't be empty".into()));
        }
        let mut size = parse_int(size, &[]).map_err(at)?;
        let offset = if offset.is_empty() {
            let align = if kind == 0 { 0x10000 } else { 0x1000 };
            (last_end + align - 1) / align * align
        } else {
            let offset = parse_int(offset, &[]).map_err(at)?;
            if offset < last_end {
                return Err(at(format!("partitions overlap: this one starts at {offset:#x}, the previous one ends at {last_end:#x}")));
            }
            offset
        };
        if size < 0 {
            size = -size - offset;
        }
        last_end = offset + size;
        let mut bits = 0;
        for flag in flags.split(':') {
            bits |= match flag {
                "encrypted" => FLAG_ENCRYPTED,
                "readonly" => FLAG_READONLY,
                "" => 0,
                other => return Err(at(format!("unknown flag '{other}'"))),
            };
        }
        if name.len() > 16 {
            return Err(at(format!("label '{name}' is longer than 16 bytes")));
        }
        let fits = |v: i64| u32::try_from(v).map_err(|_| at(format!("{v:#x} does not fit the table")));
        parts.push(Part { name: name.as_bytes().to_vec(), kind, subtype, offset: fits(offset)?, size: fits(size)?, flags: bits });
    }
    verify(&parts)?;
    let mut table = Vec::new();
    for p in &parts {
        table.extend_from_slice(&[0xAA, 0x50, p.kind, p.subtype]);
        table.extend_from_slice(&p.offset.to_le_bytes());
        table.extend_from_slice(&p.size.to_le_bytes());
        let mut label = [0u8; 16];
        label[..p.name.len()].copy_from_slice(&p.name);
        table.extend_from_slice(&label);
        table.extend_from_slice(&p.flags.to_le_bytes());
    }
    let digest = md5::Md5::digest(&table);
    table.extend_from_slice(&[0xEB, 0xEB]);
    table.extend_from_slice(&[0xFF; 14]);
    table.extend_from_slice(&digest);
    if table.len() > TABLE_SIZE {
        return Err("partition table exceeds 0xC00 bytes".into());
    }
    table.resize(TABLE_SIZE, 0xFF);
    Ok(table)
}

/// A partition table file: a CSV, or ESP-IDF's binary table (0xC00 bytes,
/// or a whole sector as some builds write it).
pub fn read_partition_file(path: &Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| format!("--partitions {}: {e}", path.display()))?;
    let table = if data.starts_with(&[0xAA, 0x50]) {
        let mut table = data;
        table.truncate(TABLE_SIZE);
        table.resize(TABLE_SIZE, 0xFF);
        table
    } else {
        let text = String::from_utf8(data).map_err(|_| format!("--partitions {}: neither a partition CSV nor a binary table", path.display()))?;
        partition_table(&text)?
    };
    read_partitions(&table).map_err(|e| format!("--partitions {}: {e}", path.display()))?;
    Ok(table)
}

/// Decode and check a binary partition table with ESP-IDF's rules.
fn read_partitions(table: &[u8]) -> Result<Vec<Entry>, String> {
    if table.is_empty() || table.len() > 0x1000 || !table.len().is_multiple_of(ENTRY_SIZE) {
        return Err("invalid partition table size".into());
    }
    let mut parts: Vec<Part> = Vec::new();
    for pos in (0..table.len()).step_by(ENTRY_SIZE) {
        let record = &table[pos..pos + ENTRY_SIZE];
        if record.iter().all(|&b| b == 0xFF) {
            break;
        }
        if record[..2] == [0xEB, 0xEB] {
            if record[16..] != md5::Md5::digest(&table[..pos])[..] {
                return Err("partition table MD5 mismatch".into());
            }
            break;
        }
        if record[..2] != [0xAA, 0x50] {
            return Err("invalid partition entry (bad magic)".into());
        }
        let word = |at: usize| u32::from_le_bytes(record[at..at + 4].try_into().unwrap());
        let name = record[12..28].iter().copied().take_while(|&b| b != 0).collect();
        parts.push(Part { name, kind: record[2], subtype: record[3], offset: word(4), size: word(8), flags: word(28) });
    }
    verify(&parts)?;
    Ok(parts.iter().map(|p| (p.kind, p.subtype, p.offset, p.size)).collect())
}

/// zlib's crc32 continued from `seed`, as the ESP-IDF NVS generator uses it.
fn crc32(seed: u32, data: &[u8]) -> u32 {
    let mut crc = !seed;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A minimal NVS partition (version 2, one page) holding u8 keys in one
/// namespace, as eink-qemu/mknvs.py writes it.
pub fn nvs_image(size: usize, namespace: &str, values: &[(&str, u8)]) -> Vec<u8> {
    let entry = |ns: u8, key: &str, value: u8| -> [u8; 32] {
        let mut e = [0xFFu8; 32];
        e[0] = ns;
        e[1] = 0x01; // u8
        e[2] = 1; // span
        e[3] = 0xFF; // not a blob chunk
        let mut name = [0u8; 16];
        name[..key.len()].copy_from_slice(key.as_bytes());
        e[8..24].copy_from_slice(&name);
        e[24] = value;
        let mut covered = e[0..4].to_vec();
        covered.extend_from_slice(&e[8..32]);
        e[4..8].copy_from_slice(&crc32(0xFFFF_FFFF, &covered).to_le_bytes());
        e
    };
    let mut entries = vec![entry(0, namespace, 1)];
    entries.extend(values.iter().map(|&(key, value)| entry(1, key, value)));
    let mut page = vec![0xFFu8; 4096];
    let mut header = [0xFFu8; 32];
    header[0..4].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes()); // ACTIVE
    header[4..8].copy_from_slice(&0u32.to_le_bytes()); // sequence
    header[8] = 0xFE; // version 2
    let crc = crc32(0xFFFF_FFFF, &header[4..28]);
    header[28..32].copy_from_slice(&crc.to_le_bytes());
    page[0..32].copy_from_slice(&header);
    let mut bitmap = [0xFFu8; 32];
    for (i, e) in entries.iter().enumerate() {
        bitmap[i / 4] &= !(1 << ((i % 4) * 2));
        page[64 + i * 32..64 + (i + 1) * 32].copy_from_slice(e);
    }
    page[32..64].copy_from_slice(&bitmap);
    page.resize(size, 0xFF);
    page
}

/// The partition an app goes to, as ESP-IDF's boot default picks it
/// (parttool's PARTITION_BOOT_DEFAULT): the factory app, else the lowest
/// OTA slot. The Picco's stock layout keeps its factory slot for recovery,
/// so its app goes to ota_0.
fn app_partition(entries: &[Entry], picco: bool) -> Option<Entry> {
    let find = |subtype: u8| entries.iter().copied().find(|e| e.0 == 0 && e.1 == subtype);
    if picco {
        return find(0x10);
    }
    find(0x00).or_else(|| (0x10..0x20).find_map(find))
}

/// Merge a bootloader, a partition table and an app into 16 MiB, with the
/// NVS entries a CrossPoint X3 build expects when the app is one.
fn compose(app: &[u8], boot: &[u8], table: &[u8], machine: Option<&str>) -> Result<(Vec<u8>, u16, Option<String>), String> {
    let chip = detect_chip(app, machine)?;
    if detect_chip(boot, None)? != chip {
        return Err("the bootloader and the app are for different chips".into());
    }
    if boot.len() > TABLE_OFFSET {
        return Err("the bootloader overlaps the partition table".into());
    }
    let entries = read_partitions(table)?;
    let picco = machine == Some("picco");
    let Some((_, subtype, app_offset, app_size)) = app_partition(&entries, picco) else {
        return Err(format!("the partition table has no {} partition", if picco { "ota_0 app" } else { "factory or OTA app" }));
    };
    if app.len() > app_size as usize {
        return Err(format!(
            "the app ({} bytes) exceeds its partition ({app_size:#x} bytes at {app_offset:#x}); give the firmware's own table with --partitions",
            app.len()
        ));
    }
    let mut flash = vec![0xFFu8; FLASH_SIZE];
    flash[..boot.len()].copy_from_slice(boot);
    flash[TABLE_OFFSET..TABLE_OFFSET + table.len()].copy_from_slice(table);
    flash[app_offset as usize..app_offset as usize + app.len()].copy_from_slice(app);
    let factory = entries.iter().any(|e| e.0 == 0 && e.1 == 0);
    if (0x10..0x20).contains(&subtype) && factory {
        // Boot the OTA slot rather than the factory app: OTA data selecting
        // it (sequence n+1, erased label, VALID state, sequence CRC).
        let Some(&(_, _, ota_data, _)) = entries.iter().find(|e| e.0 == 1 && e.1 == 0) else {
            return Err("the app goes to an OTA slot beside a factory app, but the table has no OTA data partition".into());
        };
        let at = ota_data as usize;
        let sequence = u32::from(subtype - 0x10) + 1;
        flash[at..at + 4].copy_from_slice(&sequence.to_le_bytes());
        flash[at + 24..at + 28].copy_from_slice(&2u32.to_le_bytes());
        let crc = crc32(0xFFFF_FFFF, &sequence.to_le_bytes());
        flash[at + 28..at + 32].copy_from_slice(&crc.to_le_bytes());
    }
    let profile = &*crate::profiles::CROSSPOINT;
    let board = machine.unwrap_or(machines(chip)[0]);
    let mut seeded = None;
    if profile.nvs_seed.machines.iter().any(|m| m == board) && profile.is_image(app) {
        if let Some(&(_, _, offset, size)) = entries.iter().find(|e| e.0 == 1 && e.1 == 2) {
            let values: Vec<(&str, u8)> = profile.nvs_seed.values.iter().map(|(k, v)| (k.as_str(), *v)).collect();
            let nvs = nvs_image(size as usize, &profile.nvs_seed.namespace, &values);
            flash[offset as usize..(offset + size) as usize].copy_from_slice(&nvs);
            let entries: Vec<String> = values.iter().map(|(k, v)| format!("{}/{k}={v}", profile.nvs_seed.namespace)).collect();
            seeded = Some(format!("CrossPoint's {}", entries.join(", ")));
        }
    }
    Ok((flash, chip, seeded))
}

/// A build directory's bootloader, partition table and app (firmware.bin,
/// else its only other .bin), from the directory or one level below.
fn build_files(dir: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let list = |dir: &Path| -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        files
    };
    let files = list(dir);
    let mut nested: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.is_dir()).flat_map(|d| list(&d)).collect();
    nested.sort();
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let unique = |found: Vec<PathBuf>, what: &str| -> Result<PathBuf, String> {
        match found.len() {
            1 => Ok(found.into_iter().next().unwrap()),
            n => Err(format!("need exactly one {what} in {}; found {n}", dir.display())),
        }
    };
    let all: Vec<PathBuf> = files.iter().chain(nested.iter()).cloned().collect();
    let boot = unique(all.iter().filter(|p| name(p) == "bootloader.bin").cloned().collect(), "bootloader.bin")?;
    let is_table = |p: &PathBuf| name(p).starts_with("partition") && name(p).ends_with(".bin");
    let table = unique(all.iter().filter(|p| is_table(p)).cloned().collect(), "partition*.bin")?;
    let app = dir.join("firmware.bin");
    let app = if app.is_file() {
        app
    } else {
        unique(
            files.iter().filter(|p| name(p).ends_with(".bin") && **p != boot && !is_table(p)).cloned().collect(),
            "app .bin",
        )?
    };
    Ok((app, boot, table))
}

/// Compose the emulator's flash from `source`: an app or merged .bin, a
/// build directory, or a 16 MiB image. `machine` overrides the filename
/// hint and the chip's default board.
pub fn make_flash(source: &Path, machine: Option<&str>) -> Result<Flash, String> {
    make_flash_with(source, machine, None)
}

/// `make_flash` with a partition table (CSV or binary) for an app or build
/// directory instead of the bundled or the build's own.
pub fn make_flash_with(source: &Path, machine: Option<&str>, partitions: Option<&Path>) -> Result<Flash, String> {
    let pick = |chip: u16, path: &Path| machine.unwrap_or_else(|| Chip::from_id(chip).unwrap().device(path, None)).to_string();
    let given = partitions.map(read_partition_file).transpose()?;
    let given_name = || partitions.map(|p| format!("from {}", p.display())).unwrap_or_default();
    if source.is_dir() {
        let (app_path, boot, table_path) = build_files(source)?;
        let read = |p: &Path| std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
        let (app, boot) = (read(&app_path)?, read(&boot)?);
        let (table, partitions) = match given {
            Some(table) => (table, given_name()),
            None => (read(&table_path)?, format!("the build's {}", table_path.file_name().unwrap_or_default().to_string_lossy())),
        };
        let board = pick(detect_chip(&app, machine)?, &app_path);
        let (image, chip, nvs_seed) = compose(&app, &boot, &table, Some(&board))?;
        return Ok(Flash { image, chip, machine: board, app: Some(app_path), kind: "build", partitions, nvs_seed });
    }
    let data = std::fs::read(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if data.starts_with(b"XOTA") {
        return Err("X4 Pro .xota updates are not read here; use the release's .bin, or open the .xota in the desktop app".into());
    }
    let whole = data.len() == FLASH_SIZE || (data.len() > TABLE_OFFSET + 2 && data[TABLE_OFFSET..TABLE_OFFSET + 2] == [0xAA, 0x50]);
    if whole && given.is_some() {
        return Err("--partitions is for an app .bin or a build directory; a merged image or flash dump has its own table".into());
    }
    if data.len() == FLASH_SIZE {
        // Dumps may be encrypted or lack a boot header: every byte is kept.
        // A named board must take the dump's chip, as mkflash.py checks.
        let chip = match machine {
            Some(_) => detect_chip(&data, machine)?,
            None => detect_chip(&data, None).unwrap_or(9),
        };
        return Ok(Flash { image: data, chip, machine: pick(chip, source), app: None, kind: "dump", partitions: "the dump's own".into(), nvs_seed: None });
    }
    let chip = detect_chip(&data, machine)?;
    if data.len() > TABLE_OFFSET + 2 && data[TABLE_OFFSET..TABLE_OFFSET + 2] == [0xAA, 0x50] {
        if data.len() > FLASH_SIZE {
            return Err("the merged firmware exceeds 16 MiB".into());
        }
        if data.len() < TABLE_OFFSET + TABLE_SIZE {
            return Err("the merged firmware's partition table is truncated".into());
        }
        let entries = read_partitions(&data[TABLE_OFFSET..TABLE_OFFSET + TABLE_SIZE])?;
        let has_app = entries.iter().any(|&(kind, _, offset, _)| {
            let o = offset as usize;
            kind == 0 && o + 24 <= data.len() && data[o] == 0xE9 && u16::from_le_bytes([data[o + 12], data[o + 13]]) == chip
        });
        if !has_app {
            return Err("the merged firmware has no application for its chip".into());
        }
        let mut image = data;
        image.resize(FLASH_SIZE, 0xFF);
        return Ok(Flash { image, chip, machine: pick(chip, source), app: None, kind: "merged", partitions: "the image's own".into(), nvs_seed: None });
    }
    validate_app(&data)?;
    let board = pick(chip, source);
    let machine = Some(board.as_str());
    let boot = if chip == 5 { BOOT_C3 } else { BOOT_S3 };
    let (table, partitions) = match given {
        Some(table) => (table, given_name()),
        None if machine == Some("picco") => (partition_table(PARTITIONS_PICCO)?, "the Picco's stock layout (bundled)".into()),
        None => (partition_table(PARTITIONS)?, "Arduino-ESP32's 16 MB default (bundled)".into()),
    };
    let (image, chip, nvs_seed) = compose(&data, boot, &table, machine)?;
    Ok(Flash { image, chip, machine: board, app: Some(source.to_path_buf()), kind: "app", partitions, nvs_seed })
}

/// Check an ESP application image the way the ROM loader would: every
/// segment inside the file, the checksum, and the appended SHA-256 when the
/// header says there is one (the desktop page's validateApp).
pub fn validate_app(bytes: &[u8]) -> Result<(), String> {
    let invalid = |reason: &str| Err(format!("not a complete ESP application image: {reason} (a truncated or damaged .bin?)"));
    if bytes.len() < 24 || bytes[0] != 0xE9 || !(1..=16).contains(&bytes[1]) {
        return invalid("bad header or segment count");
    }
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let (mut offset, mut checksum) = (24usize, 0xEFu8);
    for _ in 0..bytes[1] {
        if offset + 8 > bytes.len() {
            return invalid("truncated segment header");
        }
        let (address, length) = (word(offset), word(offset + 4) as usize);
        offset += 8;
        if length > FLASH_SIZE || offset + length > bytes.len() || u64::from(address) + length as u64 > 1 << 32 {
            return invalid("truncated or invalid segment length");
        }
        checksum = bytes[offset..offset + length].iter().fold(checksum, |c, b| c ^ b);
        offset += length;
    }
    let checksum_at = offset / 16 * 16 + 15;
    if checksum_at >= bytes.len() {
        return invalid("truncated checksum");
    }
    if bytes[checksum_at] != checksum {
        return invalid("checksum mismatch");
    }
    match bytes[23] {
        0 => Ok(()),
        1 => {
            let end = checksum_at + 1;
            if end + 32 > bytes.len() {
                return invalid("truncated SHA-256 digest");
            }
            if sha2::Sha256::digest(&bytes[..end])[..] != bytes[end..end + 32] {
                return invalid("SHA-256 mismatch");
            }
            Ok(())
        }
        _ => invalid("invalid appended-hash flag"),
    }
}

/// SHA-256 of bytes, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) fn test_app(chip: u16) -> Vec<u8> {
    // One 16-byte segment, the checksum at the 16-byte boundary, no hash.
    let mut app = vec![0u8; 24];
    app[0] = 0xE9;
    app[1] = 1;
    app[12..14].copy_from_slice(&chip.to_le_bytes());
    app.extend_from_slice(&0x3FC8_0000u32.to_le_bytes());
    app.extend_from_slice(&16u32.to_le_bytes());
    app.extend((1..=16u8).collect::<Vec<_>>());
    let checksum = (1..=16u8).fold(0xEFu8, |c, b| c ^ b);
    app.resize(63, 0);
    app.push(checksum);
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_apps_and_mismatched_dumps() {
        let app = test_app(5);
        assert!(validate_app(&app).is_ok());
        assert!(validate_app(&app[..40]).unwrap_err().contains("truncated"));
        let mut bad = app.clone();
        bad[33] ^= 1;
        assert!(validate_app(&bad).unwrap_err().contains("checksum"));
        let dir = tempfile::tempdir().unwrap();
        let mut dump = vec![0xFFu8; FLASH_SIZE];
        dump[..24].copy_from_slice(&app[..24]);
        let path = dir.path().join("dump.bin");
        std::fs::write(&path, &dump).unwrap();
        assert!(make_flash(&path, Some("x4pro")).is_err());
        assert_eq!(make_flash(&path, None).unwrap().machine, "x3");
    }

    #[test]
    fn filename_hint_applies_before_composition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crosspoint-x4.bin");
        std::fs::write(&path, crosspoint_app()).unwrap();
        let x4 = make_flash(&path, None).unwrap();
        assert_eq!(x4.machine, "x4");
        assert!(x4.nvs_seed.is_none());
        let x3 = make_flash(&path, Some("x3")).unwrap();
        assert_eq!(x3.machine, "x3");
        assert!(x3.nvs_seed.is_some());

        let path = dir.path().join("firmware-picco.bin");
        let app = test_app(9);
        std::fs::write(&path, &app).unwrap();
        let picco = make_flash(&path, None).unwrap();
        assert_eq!(picco.machine, "picco");
        assert_eq!(&picco.image[0xe0000..0xe0000 + app.len()], app);
    }

    #[test]
    fn crc_matches_zlib() {
        // zlib.crc32(b"123456789") == 0xCBF43926; seeded with 0xffffffff
        // it continues from that value.
        assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(0xFFFF_FFFF, b""), 0xFFFF_FFFF);
    }

    #[test]
    fn bundled_tables_parse() {
        for csv in [PARTITIONS, PARTITIONS_PICCO] {
            let table = partition_table(csv).unwrap();
            assert_eq!(table.len(), TABLE_SIZE);
            assert!(!read_partitions(&table).unwrap().is_empty());
        }
    }

    #[test]
    fn composes_a_release_app_like_mkflash() {
        let app = test_app(5);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        std::fs::write(&path, &app).unwrap();
        let flash = make_flash(&path, None).unwrap();
        assert_eq!(flash.machine, "x3");
        assert_eq!(flash.image.len(), FLASH_SIZE);
        assert_eq!(&flash.image[..BOOT_C3.len()], BOOT_C3);
        assert_eq!(&flash.image[0x10000..0x10000 + app.len()], &app[..]);
        assert!(flash.partitions.contains("Arduino"));
        // Not CrossPoint: its NVS stays erased.
        assert_eq!(flash.nvs_seed, None);
        assert!(flash.image[0x9000..0xE000].iter().all(|&b| b == 0xFF));
        assert!(make_flash(&path, Some("x4pro")).is_err());
    }

    /// An app image whose data holds CrossPoint's NVS strings.
    fn crosspoint_app() -> Vec<u8> {
        let mut app = vec![0u8; 24];
        app[0] = 0xE9;
        app[1] = 1;
        app[12..14].copy_from_slice(&5u16.to_le_bytes());
        let data = b"cphw\0dev_det\0\0\0\0";
        app.extend_from_slice(&0x3FC8_0000u32.to_le_bytes());
        app.extend_from_slice(&(data.len() as u32).to_le_bytes());
        app.extend_from_slice(data);
        let checksum = data.iter().fold(0xEFu8, |c, b| c ^ b);
        app.resize(63, 0);
        app.push(checksum);
        app
    }

    #[test]
    fn seeds_nvs_only_for_crosspoint_on_the_x3() {
        let app = crosspoint_app();
        assert!(validate_app(&app).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crosspoint.bin");
        std::fs::write(&path, &app).unwrap();
        let x3 = make_flash(&path, None).unwrap();
        assert_eq!(&x3.image[0x9000..0x9004], &0xFFFF_FFFEu32.to_le_bytes());
        assert!(x3.nvs_seed.unwrap().contains("cphw/dev_det=2"));
        let x4 = make_flash(&path, Some("x4")).unwrap();
        assert!(x4.image[0x9000..0xE000].iter().all(|&b| b == 0xFF));
        assert_eq!(x4.nvs_seed, None);
    }

    #[test]
    fn reads_partition_csvs_like_gen_esp32part() {
        let csv = "# Name, Type, SubType, Offset, Size, Flags\n\
                   nvs, data, nvs, , 24K,\n\
                   phy_init, data, phy, , 4K\n\
                   factory, app, factory, , 1M\n\
                   storage, data, littlefs, , 0x100000, readonly\n\
                   custom, 0x40, 0x7, , 4K, encrypted:readonly\n";
        let table = partition_table(csv).unwrap();
        assert_eq!(
            read_partitions(&table).unwrap(),
            [(1, 2, 0x9000, 0x6000), (1, 1, 0xF000, 0x1000), (0, 0, 0x10000, 0x100000), (1, 0x83, 0x110000, 0x100000), (0x40, 7, 0x210000, 0x1000)]
        );
        assert_eq!(u32::from_le_bytes(table[3 * 32 + 28..3 * 32 + 32].try_into().unwrap()), 2);
        assert!(partition_table("x, app, ota_16, , 1M\n").is_err());
        assert!(partition_table("factory, app, factory, 0x11000, 1M\n").unwrap_err().contains("not aligned to 0x10000"));
        assert!(partition_table("x, data, nvs, 0x9000, 4K, sticky\n").is_err());
    }

    /// Cases where gen_esp32part.py's reading is subtle (checked against it).
    #[test]
    fn matches_gen_esp32part_on_corner_cases() {
        let parts = |csv: &str| partition_table(csv).and_then(|t| read_partitions(&t));
        // No CSV quoting: quotes stay in the label; keywords in any case.
        let quoted = partition_table("\"factory\",app,factory,,1M\n").unwrap();
        assert_eq!(&quoted[12..21], b"\"factory\"");
        assert!(parts("factory,\"app\",factory,,1M\n").is_err());
        assert_eq!(parts("factory,APP,FACTORY,,1M\n").unwrap(), [(0, 0, 0x10000, 0x100000)]);
        // An empty data subtype is `undefined`; numbers are int(x, 0).
        assert_eq!(parts("custom,data,,,12K\nfactory,app,factory,,1M\n").unwrap()[0], (1, 6, 0x9000, 0x3000));
        assert_eq!(parts("factory,0b0,0b0,,0b100000000000000000000\n").unwrap(), [(0, 0, 0x10000, 0x100000)]);
        assert!(parts("factory,00,00,,01048576\n").is_err());
        // A negative size runs up to that offset.
        assert_eq!(parts("factory,app,factory,0x10000,-0x110000\n").unwrap(), [(0, 0, 0x10000, 0x100000)]);
        // ESP-IDF's table rules.
        for bad in [
            "factory,app,factory,0x10000,12345\n",
            "x,data,nvs,0x9001,0x5000\nfactory,app,factory,,1M\n",
            "a,data,nvs,,0x5000\na,app,factory,,1M\n",
            "otadata,data,ota,,0x1000\nfactory,app,factory,,1M\n",
            "otadata,data,ota,,0x2000,readonly\nfactory,app,factory,,1M\n",
            "nvs,data,nvs,,0x2000\nfactory,app,factory,,1M\n",
            "factory,app,factory,0x10000,1M\nnvs,data,nvs,0x9000,0x5000\n",
            "factory,app,factory,0x10000,1M\nota_0,app,ota_0,0x110000,1M\notadata,data,ota,0xffffff,1\n",
        ] {
            assert!(partition_table(bad).is_err(), "{bad}");
        }
        // The app goes to factory, else the lowest OTA slot; a test-only table has nowhere.
        let entries = parts("otadata,data,ota,,0x2000\ntest,app,test,0x10000,1M\nota_1,app,ota_1,,1M\n").unwrap();
        assert_eq!(app_partition(&entries, false).unwrap().2, 0x110000);
        let entries = parts("ota_2,app,ota_2,0x10000,1M\nota_1,app,ota_1,,1M\n").unwrap();
        assert_eq!(app_partition(&entries, false).unwrap().1, 0x11);
        assert_eq!(app_partition(&parts("test,app,test,0x10000,1M\n").unwrap(), false), None);
    }

    #[test]
    fn places_the_app_where_idf_flashes_it() {
        let app = test_app(9);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        std::fs::write(&path, &app).unwrap();
        // A factory app beside OTA slots: the app goes to factory, OTA data stays erased.
        let csv = dir.path().join("factory.csv");
        std::fs::write(&csv, "nvs,data,nvs,0x9000,0x4000\notadata,data,ota,0xd000,0x2000\nphy_init,data,phy,0xf000,0x1000\nfactory,app,factory,0x10000,1M\nota_0,app,ota_0,0x110000,1M\n").unwrap();
        let flash = make_flash_with(&path, None, Some(&csv)).unwrap();
        assert_eq!(&flash.image[0x10000..0x10000 + app.len()], &app[..]);
        assert!(flash.image[0xD000..0xF000].iter().all(|&b| b == 0xFF));
        assert!(flash.partitions.contains("factory.csv"));
        // The Picco keeps its factory slot for recovery: ota_0, selected in OTA data.
        let picco = make_flash(&path, Some("picco")).unwrap();
        assert_eq!(&picco.image[0xE0000..0xE0000 + app.len()], &app[..]);
        assert_eq!(&picco.image[0x15000..0x15004], &1u32.to_le_bytes());
        assert_eq!(&picco.image[0x1501C..0x15020], &0x4743_989Au32.to_le_bytes());
        // An app bigger than the bundled table's slot fits a table that has room.
        let mut big = vec![0u8; 0x700000];
        big[..app.len()].copy_from_slice(&app);
        let big_path = dir.path().join("big.bin");
        std::fs::write(&big_path, &big).unwrap();
        let roomy = dir.path().join("roomy.bin");
        std::fs::write(&roomy, partition_table("nvs,data,nvs,0x9000,0x5000\napp0,app,ota_0,0x10000,0x800000\n").unwrap()).unwrap();
        let err = compose(&big, BOOT_S3, &partition_table(PARTITIONS).unwrap(), None).unwrap_err();
        assert!(err.contains("--partitions"), "{err}");
        let table = read_partition_file(&roomy).unwrap();
        assert!(compose(&big, BOOT_S3, &table, None).is_ok());
        // A dump has its own table.
        let dump = dir.path().join("dump.bin");
        std::fs::write(&dump, vec![0xFFu8; FLASH_SIZE]).unwrap();
        assert!(make_flash_with(&dump, None, Some(&roomy)).err().unwrap().contains("own table"));
    }
}
