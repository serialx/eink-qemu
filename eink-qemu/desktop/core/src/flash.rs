//! 16 MiB flash images for the emulator, as eink-qemu/mkflash.py composes
//! them: a release app gets the bundled bootloader and the board's partition
//! table (and, on the X3, CrossPoint's cached device type in NVS); a
//! PlatformIO or ESP-IDF build directory is merged from its own bootloader,
//! table and app; a merged image keeps its own layout; a 16 MiB dump passes
//! through. App bytes are never modified. X4 Pro .xota updates are composed
//! by the desktop app and the browser page only.

use sha2::Digest;
use std::path::{Path, PathBuf};

pub const FLASH_SIZE: usize = 0x100_0000;
const TABLE_OFFSET: usize = 0x8000;
const TABLE_SIZE: usize = 0xC00;
const APP_OFFSET: u32 = 0x10000;
const PICCO_APP_OFFSET: u32 = 0xE0000;
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

/// Encode an explicit-offset partition CSV in ESP-IDF's binary format.
pub fn partition_table(csv: &str) -> Result<Vec<u8>, String> {
    let mut table = Vec::new();
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let [name, kind, subtype, offset, size, flags] = fields[..] else {
            return Err(format!("bad partition CSV line: {line}"));
        };
        if !flags.is_empty() {
            return Err("partition CSV flags are not supported".into());
        }
        let kind: u8 = match kind { "app" => 0, "data" => 1, _ => return Err(format!("unknown partition type {kind}")) };
        let subtype: u8 = match subtype {
            "factory" => 0, "ota_0" => 0x10, "ota_1" => 0x11, "nvs" => 2, "ota" => 0, "spiffs" => 0x82, "coredump" => 3,
            _ => return Err(format!("unknown partition subtype {subtype}")),
        };
        let number = |text: &str| -> Result<u32, String> {
            let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => text.parse(),
            };
            parsed.map_err(|_| format!("bad number {text} in partition CSV"))
        };
        if name.len() > 16 {
            return Err("partition label exceeds 16 bytes".into());
        }
        table.extend_from_slice(&[0xAA, 0x50, kind, subtype]);
        table.extend_from_slice(&number(offset)?.to_le_bytes());
        table.extend_from_slice(&number(size)?.to_le_bytes());
        let mut label = [0u8; 16];
        label[..name.len()].copy_from_slice(name.as_bytes());
        table.extend_from_slice(&label);
        table.extend_from_slice(&0u32.to_le_bytes());
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

/// Merge a bootloader, a partition table and an app into 16 MiB.
fn compose(app: &[u8], boot: &[u8], table: &[u8], machine: Option<&str>) -> Result<(Vec<u8>, u16), String> {
    let chip = detect_chip(app, machine)?;
    if detect_chip(boot, None)? != chip {
        return Err("the bootloader and the app are for different chips".into());
    }
    if boot.len() > TABLE_OFFSET {
        return Err("the bootloader overlaps the partition table".into());
    }
    let entries = read_partitions(table)?;
    let picco = machine == Some("picco");
    let app_offset = if picco { PICCO_APP_OFFSET } else { APP_OFFSET };
    let apps: Vec<&Entry> = entries.iter().filter(|e| e.0 == 0 && e.2 == app_offset).collect();
    let [app_part] = apps[..] else {
        return Err(format!("the partition table must have one app partition at {app_offset:#x}"));
    };
    if app.len() > app_part.3 as usize {
        return Err(format!("the app ({} bytes) exceeds its partition ({:#x} bytes)", app.len(), app_part.3));
    }
    let mut flash = vec![0xFFu8; FLASH_SIZE];
    flash[..boot.len()].copy_from_slice(boot);
    flash[TABLE_OFFSET..TABLE_OFFSET + table.len()].copy_from_slice(table);
    flash[app_offset as usize..app_offset as usize + app.len()].copy_from_slice(app);
    if picco {
        if app_part.1 != 0x10 || !entries.contains(&(1, 0, 0x15000, 0x2000)) {
            return Err("the Picco needs an ota_0 app and OTA data at 0x15000".into());
        }
        // Select ota_0: sequence 1, erased label, VALID state, sequence CRC.
        flash[0x15000..0x15004].copy_from_slice(&1u32.to_le_bytes());
        flash[0x15018..0x1501C].copy_from_slice(&2u32.to_le_bytes());
        flash[0x1501C..0x15020].copy_from_slice(&0x4743_989Au32.to_le_bytes());
    }
    if chip == 5 && machine != Some("x4") {
        // CrossPoint's cached device detection, so an X3 does not probe.
        if !entries.contains(&(1, 2, 0x9000, 0x5000)) {
            return Err("an ESP32-C3 image needs NVS at 0x9000 with size 0x5000".into());
        }
        flash[0x9000..0xE000].copy_from_slice(&nvs_image(0x5000, "cphw", &[("dev_det", 2)]));
    }
    Ok((flash, chip))
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
    let pick = |chip: u16| machine.map(str::to_string).unwrap_or_else(|| machines(chip)[0].to_string());
    if source.is_dir() {
        let (app_path, boot, table) = build_files(source)?;
        let read = |p: &Path| std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
        let (app, boot, table) = (read(&app_path)?, read(&boot)?, read(&table)?);
        let (image, chip) = compose(&app, &boot, &table, machine)?;
        return Ok(Flash { image, chip, machine: pick(chip), app: Some(app_path), kind: "build" });
    }
    let data = std::fs::read(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if data.starts_with(b"XOTA") {
        return Err("X4 Pro .xota updates are not read here; use the release's .bin, or open the .xota in the desktop app".into());
    }
    if data.len() == FLASH_SIZE {
        // Dumps may be encrypted or lack a boot header: every byte is kept.
        let chip = detect_chip(&data, machine).unwrap_or(9);
        return Ok(Flash { image: data, chip, machine: pick(chip), app: None, kind: "dump" });
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
        return Ok(Flash { image, chip, machine: pick(chip), app: None, kind: "merged" });
    }
    let boot = if chip == 5 { BOOT_C3 } else { BOOT_S3 };
    let table = partition_table(if machine == Some("picco") { PARTITIONS_PICCO } else { PARTITIONS })?;
    let (image, chip) = compose(&data, boot, &table, machine)?;
    Ok(Flash { image, chip, machine: pick(chip), app: Some(source.to_path_buf()), kind: "app" })
}

/// SHA-256 of bytes, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // A minimal C3 app: header with chip 5.
        let mut app = vec![0u8; 4096];
        app[0] = 0xE9;
        app[12] = 5;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        std::fs::write(&path, &app).unwrap();
        let flash = make_flash(&path, None).unwrap();
        assert_eq!(flash.machine, "x3");
        assert_eq!(flash.image.len(), FLASH_SIZE);
        assert_eq!(&flash.image[..BOOT_C3.len()], BOOT_C3);
        assert_eq!(&flash.image[0x10000..0x10000 + app.len()], &app[..]);
        assert_eq!(&flash.image[0x9000..0x9004], &0xFFFF_FFFEu32.to_le_bytes());
        assert!(make_flash(&path, Some("x4pro")).is_err());
        let x4 = make_flash(&path, Some("x4")).unwrap();
        assert!(x4.image[0x9000..0xE000].iter().all(|&b| b == 0xFF));
    }
}
