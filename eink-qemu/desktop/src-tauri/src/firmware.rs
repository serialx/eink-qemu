//! What the firmware card shows about a firmware file: its chip, the devices
//! that take it, its version, and the ELF built with it.

use crate::heap::app_descs;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// ESP image chip IDs (esp_app_format.h).
const CHIP_C3: u16 = 5;
const CHIP_S3: u16 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Chip {
    #[serde(rename = "ESP32-C3")]
    C3,
    #[serde(rename = "ESP32-S3")]
    S3,
}

impl Chip {
    /// The devices that run this chip's firmware, the default first.
    pub fn devices(self) -> &'static [&'static str] {
        match self {
            Chip::C3 => &["x4", "x3"],
            Chip::S3 => &["x4pro", "picco"],
        }
    }
}

/// The chip a firmware file is for: an ESP image's header names it; an
/// .xota update is always the X4 Pro's.
pub fn chip(path: &Path, image: &[u8]) -> Option<Chip> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xota")) {
        return Some(Chip::S3);
    }
    if image.first() != Some(&0xE9) {
        return None;
    }
    match u16::from_le_bytes([*image.get(12)?, *image.get(13)?]) {
        CHIP_C3 => Some(Chip::C3),
        CHIP_S3 => Some(Chip::S3),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// The folder holding it, the home folder written as ~.
    pub folder: String,
    /// That folder's own name, which tells builds apart (`.pio/build/x4pro`).
    pub parent: String,
    /// Modification time, milliseconds since the epoch.
    pub modified: Option<u64>,
    pub chip: Option<Chip>,
    /// The app descriptor's version and project name.
    pub version: Option<String>,
    pub project: Option<String>,
}

fn text(field: &[u8]) -> Option<String> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let text = String::from_utf8_lossy(&field[..end]).trim().to_string();
    (!text.is_empty()).then_some(text)
}

pub fn home_relative(path: &Path) -> String {
    if let Some(rest) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf))
    {
        return Path::new("~").join(rest).to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
}

pub fn parent_name(path: &Path) -> String {
    path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn describe(path: &Path, image: &[u8]) -> FirmwareInfo {
    let meta = std::fs::metadata(path).ok();
    let desc = app_descs(image).into_iter().next();
    FirmwareInfo {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        size: meta.as_ref().map_or(image.len() as u64, |m| m.len()),
        folder: path.parent().map(home_relative).unwrap_or_default(),
        parent: parent_name(path),
        modified: meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64),
        chip: chip(path, image),
        version: desc.and_then(|d| text(&d[16..48])),
        project: desc.and_then(|d| text(&d[48..80])),
    }
}

/// ELFs that may have been built with `firmware`, best first: the same
/// name with .elf, then PlatformIO's and ESP-IDF's firmware.elf beside it.
pub fn elf_candidates(firmware: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Some(dir) = firmware.parent() else { return found };
    let mut names = Vec::new();
    if let Some(stem) = firmware.file_stem() {
        let mut name = stem.to_os_string();
        name.push(".elf");
        names.push(name);
    }
    names.push("firmware.elf".into());
    for name in names {
        let path = dir.join(name);
        if path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(chip: u16, version: &str) -> Vec<u8> {
        let mut image = vec![0u8; 32 + 256];
        image[0] = 0xE9;
        image[12..14].copy_from_slice(&chip.to_le_bytes());
        image[32..36].copy_from_slice(&0xABCD_5432u32.to_le_bytes());
        image[48..48 + version.len()].copy_from_slice(version.as_bytes());
        image
    }

    #[test]
    fn reads_chip_and_version() {
        let image = app(CHIP_S3, "1.6.5-37-g7394e40f");
        let info = describe(Path::new("/nowhere/x4pro/firmware.bin"), &image);
        assert_eq!(info.chip, Some(Chip::S3));
        assert_eq!(info.version.as_deref(), Some("1.6.5-37-g7394e40f"));
        assert_eq!(info.parent, "x4pro");
        assert_eq!(chip(Path::new("a.bin"), &app(CHIP_C3, "")), Some(Chip::C3));
        assert_eq!(chip(Path::new("a.xota"), b"XOTA"), Some(Chip::S3));
        assert_eq!(chip(Path::new("a.bin"), b"nope"), None);
    }

    #[test]
    fn finds_elfs_beside_the_firmware() {
        let dir = std::env::temp_dir().join(format!("eink-qemu-elf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("firmware.elf"), b"").unwrap();
        std::fs::write(dir.join("app.elf"), b"").unwrap();
        assert_eq!(elf_candidates(&dir.join("app.bin")), [dir.join("app.elf"), dir.join("firmware.elf")]);
        assert_eq!(elf_candidates(&dir.join("firmware.bin")), [dir.join("firmware.elf")]);
        assert!(elf_candidates(&dir.join("other/x.bin")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
