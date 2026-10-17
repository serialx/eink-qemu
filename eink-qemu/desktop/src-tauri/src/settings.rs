//! Preferences that belong to the app rather than the WebView: emulator
//! options and the chosen firmware and SD card. Theme, zoom and panel widths
//! stay in the WebView's localStorage, as on the page.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub device: String,
    pub panel: String,
    pub wifi: bool,
    pub ghosting: bool,
    pub turbo: bool,
    pub firmware: Option<PathBuf>,
    pub card: Option<PathBuf>,
    pub recent_cards: Vec<PathBuf>,
    /// The firmware's ELF, for the heap view.
    pub elf: Option<PathBuf>,
    /// Whether the ELF was found beside the firmware rather than chosen.
    pub elf_auto: bool,
    /// Firmware chosen before, newest first, each with its device and ELF.
    pub recent: Vec<Recent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Recent {
    pub firmware: PathBuf,
    pub device: String,
    pub elf: Option<PathBuf>,
    pub elf_auto: bool,
}

pub const MAX_RECENT: usize = 6;

impl Default for Settings {
    fn default() -> Self {
        Self {
            device: "auto".into(),
            panel: "uc8279-68".into(),
            wifi: true,
            ghosting: true,
            turbo: true,
            firmware: None,
            card: None,
            recent_cards: Vec::new(),
            elf: None,
            elf_auto: false,
            recent: Vec::new(),
        }
    }
}

/// The options the WebView may change directly.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub device: Option<String>,
    pub panel: Option<String>,
    pub wifi: Option<bool>,
    pub ghosting: Option<bool>,
    pub turbo: Option<bool>,
}

pub const DEVICES: [&str; 5] = ["auto", "x3", "x4", "x4pro", "picco"];
pub const PANELS: [&str; 2] = ["uc8279-68", "uc8179"];

impl Settings {
    /// Read the settings. A file that cannot be parsed is renamed to
    /// `settings.json.bad` and replaced by defaults; unknown values fall back
    /// to defaults. Both are reported in `problems`.
    pub fn load(path: &Path, problems: &mut Vec<String>) -> Settings {
        let mut settings = match std::fs::read(path) {
            Err(_) => Settings::default(),
            Ok(bytes) => match serde_json::from_slice::<Settings>(&bytes) {
                Ok(settings) => settings,
                Err(error) => {
                    let bad = path.with_extension("json.bad");
                    let _ = std::fs::rename(path, &bad);
                    problems.push(format!(
                        "The settings file could not be read ({error}); it was kept as {} and defaults are used.",
                        bad.display()
                    ));
                    Settings::default()
                }
            },
        };
        if !DEVICES.contains(&settings.device.as_str()) {
            problems.push(format!("Unknown device \"{}\" in the settings; using Auto.", settings.device));
            settings.device = "auto".into();
        }
        if !PANELS.contains(&settings.panel.as_str()) {
            problems.push(format!("Unknown X4 Pro panel \"{}\" in the settings; using UC8279 (0x68).", settings.panel));
            settings.panel = "uc8279-68".into();
        }
        settings
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&temp, serde_json::to_vec_pretty(self).unwrap())?;
        std::fs::rename(temp, path)
    }

    /// Apply a whole patch, or nothing when any field is invalid.
    pub fn apply(&mut self, patch: SettingsPatch) -> Result<(), String> {
        let mut next = self.clone();
        if let Some(device) = patch.device {
            if !DEVICES.contains(&device.as_str()) {
                return Err(format!("Unknown device: {device}."));
            }
            next.device = device;
        }
        if let Some(panel) = patch.panel {
            if !PANELS.contains(&panel.as_str()) {
                return Err(format!("Unknown X4 Pro panel: {panel}."));
            }
            next.panel = panel;
        }
        if let Some(wifi) = patch.wifi {
            next.wifi = wifi;
        }
        if let Some(ghosting) = patch.ghosting {
            next.ghosting = ghosting;
        }
        if let Some(turbo) = patch.turbo {
            next.turbo = turbo;
        }
        next.remember();
        *self = next;
        Ok(())
    }

    pub fn remember_card(&mut self, path: PathBuf) {
        self.recent_cards.retain(|p| *p != path);
        self.recent_cards.insert(0, path.clone());
        self.recent_cards.truncate(MAX_RECENT);
        self.card = Some(path);
    }

    /// Record the chosen firmware's device and ELF in its recent entry.
    pub fn remember(&mut self) {
        let Some(firmware) = self.firmware.clone() else { return };
        let entry = Recent { firmware, device: self.device.clone(), elf: self.elf.clone(), elf_auto: self.elf_auto };
        self.recent.retain(|r| r.firmware != entry.firmware);
        self.recent.insert(0, entry);
        self.recent.truncate(MAX_RECENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_cards_persist_newest_first_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        for n in 0..8 { settings.remember_card(PathBuf::from(format!("card-{n}.img"))); }
        settings.remember_card(PathBuf::from("card-4.img"));
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path, &mut Vec::new());
        assert_eq!(loaded.recent_cards, [4, 7, 6, 5, 3, 2].map(|n| PathBuf::from(format!("card-{n}.img"))));
        assert_eq!(loaded.card, Some(PathBuf::from("card-4.img")));
        let old: Settings = serde_json::from_str(r#"{"card":"old.img"}"#).unwrap();
        assert!(old.recent_cards.is_empty());
    }
}
