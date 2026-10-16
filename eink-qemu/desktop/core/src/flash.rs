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
    /// The board it runs on: the requested one, else the chip's default.
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
    match chip {
        5 => &["x3", "x4"],
        9 => &["x4pro", "picco"],
        _ => &[],
    }
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

/// Encode a partition CSV in ESP-IDF's binary format, as gen_esp32part.py
/// reads it: named or numeric types and subtypes, sizes and offsets with
/// K/M suffixes, offsets left empty placed after the previous partition
/// (apps on 64 KiB boundaries), and the encrypted/readonly flags.
pub fn partition_table(csv: &str) -> Result<Vec<u8>, String> {
    let number = |text: &str| -> Result<u32, String> {
        let bad = || format!("bad number {text:?} in partition CSV");
        let (digits, scale) = match text.chars().last() {
            Some('K' | 'k') => (&text[..text.len() - 1], 1024),
            Some('M' | 'm') => (&text[..text.len() - 1], 1024 * 1024),
            _ => (text, 1),
        };
        let value = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => digits.parse(),
        }
        .map_err(|_| bad())?;
        value.checked_mul(scale).ok_or_else(bad)
    };
    let mut table = Vec::new();
    let mut next = (TABLE_OFFSET + 0x1000) as u32;
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() == 5 {
            fields.push("");
        }
        let [name, kind, subtype, offset, size, flags] = fields[..] else {
            return Err(format!("bad partition CSV line: {line}"));
        };
        let kind: u8 = match kind {
            "app" => 0,
            "data" => 1,
            other => u8::try_from(number(other).map_err(|_| format!("unknown partition type {other}"))?)
                .map_err(|_| format!("partition type {other} out of range"))?,
        };
        let subtype: u8 = match (kind, subtype) {
            (0, "factory") => 0,
            (0, "test") => 0x20,
            (0, ota) if ota.starts_with("ota_") => match ota[4..].parse::<u8>() {
                Ok(n) if n < 16 => 0x10 + n,
                _ => return Err(format!("unknown partition subtype {ota}")),
            },
            (1, "ota") => 0,
            (1, "phy") => 1,
            (1, "nvs") => 2,
            (1, "coredump") => 3,
            (1, "nvs_keys") => 4,
            (1, "efuse") => 5,
            (1, "undefined") => 6,
            (1, "esphttpd") => 0x80,
            (1, "fat") => 0x81,
            (1, "spiffs") => 0x82,
            (1, "littlefs") => 0x83,
            (_, other) => u8::try_from(number(other).map_err(|_| format!("unknown partition subtype {other}"))?)
                .map_err(|_| format!("partition subtype {other} out of range"))?,
        };
        let size = number(size)?;
        let offset = if offset.is_empty() {
            let align = if kind == 0 { 0x10000 } else { 0x1000 };
            next.div_ceil(align) * align
        } else {
            number(offset)?
        };
        next = offset.checked_add(size).ok_or("partition beyond 4 GiB")?;
        let mut bits = 0u32;
        for flag in flags.split(':').map(str::trim).filter(|f| !f.is_empty()) {
            bits |= match flag {
                "encrypted" => 1,
                "readonly" => 2,
                other => return Err(format!("unknown partition flag {other}")),
            };
        }
        if name.len() > 16 {
            return Err("partition label exceeds 16 bytes".into());
        }
        table.extend_from_slice(&[0xAA, 0x50, kind, subtype]);
        table.extend_from_slice(&offset.to_le_bytes());
        table.extend_from_slice(&size.to_le_bytes());
        let mut label = [0u8; 16];
        label[..name.len()].copy_from_slice(name.as_bytes());
        table.extend_from_slice(&label);
        table.extend_from_slice(&bits.to_le_bytes());
    }
    let digest = md5::Md5::digest(&table);
    table.extend_from_slice(&[0xEB, 0xEB]);
    table.extend_from_slice(&[0xFF; 14]);
    table.extend_from_slice(&digest);
    if table.len() > TABLE_SIZE {
        return Err("partition table exceeds 0xC00 bytes".into());
    }
    table.resize(TABLE_SIZE, 0xFF);
    read_partitions(&table)?;
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

/// Decode and check a binary partition table.
fn read_partitions(table: &[u8]) -> Result<Vec<Entry>, String> {
    if table.is_empty() || table.len() > 0x1000 || !table.len().is_multiple_of(ENTRY_SIZE) {
        return Err("invalid partition table size".into());
    }
    let mut entries: Vec<Entry> = Vec::new();
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
        let word = |at: usize| u32::from_le_bytes(record[at..at + 4].try_into().unwrap());
        let (kind, subtype, offset, size) = (record[2], record[3], word(4), word(8));
        if record[..2] != [0xAA, 0x50] || size == 0 || offset < 0x9000 || offset as u64 + size as u64 > FLASH_SIZE as u64 {
            return Err("invalid partition entry or partition outside 16 MiB flash".into());
        }
        if entries.iter().any(|&(_, _, o, s)| offset < o + s && o < offset + size) {
            return Err("overlapping partitions".into());
        }
        entries.push((kind, subtype, offset, size));
    }
    if entries.is_empty() {
        return Err("empty partition table".into());
    }
    Ok(entries)
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

/// The partition an app goes to, as `idf.py flash` picks it: the factory
/// app, else ota_0, else the first app. The Picco's stock layout keeps its
/// factory slot for recovery, so its app goes to ota_0.
fn app_partition(entries: &[Entry], picco: bool) -> Option<Entry> {
    let find = |subtype: u8| entries.iter().copied().find(|e| e.0 == 0 && e.1 == subtype);
    if picco {
        return find(0x10);
    }
    find(0x00).or_else(|| find(0x10)).or_else(|| entries.iter().copied().find(|e| e.0 == 0))
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
        return Err(format!("the partition table has no {} partition", if picco { "ota_0 app" } else { "app" }));
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
/// build directory, or a 16 MiB image. `machine` picks the board (needed
/// for the X4 and the Picco, whose chips the X3 and X4 Pro share).
pub fn make_flash(source: &Path, machine: Option<&str>) -> Result<Flash, String> {
    make_flash_with(source, machine, None)
}

/// `make_flash` with a partition table (CSV or binary) for an app or build
/// directory instead of the bundled or the build's own.
pub fn make_flash_with(source: &Path, machine: Option<&str>, partitions: Option<&Path>) -> Result<Flash, String> {
    let pick = |chip: u16| machine.map(str::to_string).unwrap_or_else(|| machines(chip)[0].to_string());
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
        let (image, chip, nvs_seed) = compose(&app, &boot, &table, machine)?;
        return Ok(Flash { image, chip, machine: pick(chip), app: Some(app_path), kind: "build", partitions, nvs_seed });
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
        return Ok(Flash { image: data, chip, machine: pick(chip), app: None, kind: "dump", partitions: "the dump's own".into(), nvs_seed: None });
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
        return Ok(Flash { image, chip, machine: pick(chip), app: None, kind: "merged", partitions: "the image's own".into(), nvs_seed: None });
    }
    validate_app(&data)?;
    let boot = if chip == 5 { BOOT_C3 } else { BOOT_S3 };
    let (table, partitions) = match given {
        Some(table) => (table, given_name()),
        None if machine == Some("picco") => (partition_table(PARTITIONS_PICCO)?, "the Picco's stock layout (bundled)".into()),
        None => (partition_table(PARTITIONS)?, "Arduino-ESP32's 16 MB default (bundled)".into()),
    };
    let (image, chip, nvs_seed) = compose(&data, boot, &table, machine)?;
    Ok(Flash { image, chip, machine: pick(chip), app: Some(source.to_path_buf()), kind: "app", partitions, nvs_seed })
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
        assert!(partition_table("x, data, nvs, 0x9000, 4K, sticky\n").is_err());
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
