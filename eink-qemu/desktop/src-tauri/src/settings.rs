//! Preferences that belong to the app rather than the WebView: emulator
//! options and the chosen firmware and SD card. Theme, zoom and panel widths
//! stay in the WebView's localStorage, as on the page.

use serde::{Deserialize, Serialize};
use eink_core::session::WifiMode;
use eink_bridge::model::{BridgeSelection, VERSION as BRIDGE_VERSION};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub device: String,
    pub panel: String,
    pub wifi: bool,
    pub wifi_mode: String,
    /// Set only after native verification, never through SettingsPatch.
    pub bridge: Option<BridgeSelection>,
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
            wifi_mode: WifiMode::default().as_str().into(),
            bridge: None,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub device: Option<String>,
    pub panel: Option<String>,
    pub wifi: Option<bool>,
    pub wifi_mode: Option<String>,
    pub ghosting: Option<bool>,
    pub turbo: Option<bool>,
}

pub const DEVICES: [&str; 5] = ["auto", "x3", "x4", "x4pro", "picco"];
pub const PANELS: [&str; 2] = ["uc8279-68", "uc8179"];

impl Settings {
    fn decode(bytes: &[u8], problems: &mut Vec<String>) -> Result<Self, serde_json::Error> {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        if let Some(bridge) = value.get("bridge").filter(|bridge| !bridge.is_null()) {
            let valid = serde_json::from_value::<BridgeSelection>(bridge.clone())
                .is_ok_and(|selection| selection.version == BRIDGE_VERSION);
            if !valid {
                value.as_object_mut().unwrap().remove("bridge");
                problems.push("The saved bridge selection needs setup again. Your network mode and other preferences were kept.".into());
            }
        }
        serde_json::from_value(value)
    }
    /// Read the settings. A file that cannot be parsed is renamed to
    /// `settings.json.bad` and replaced by defaults; unknown values fall back
    /// to defaults. Both are reported in `problems`.
    pub fn load(path: &Path, problems: &mut Vec<String>) -> Settings {
        let mut settings = match std::fs::read(path) {
            Err(_) => Settings::default(),
            Ok(bytes) => match Settings::decode(&bytes, problems) {
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
        if settings.wifi_mode.parse::<WifiMode>().is_err() {
            problems.push(format!("Unknown Wi-Fi mode \"{}\" in the settings; using NAT.", settings.wifi_mode));
            settings.wifi_mode = WifiMode::default().as_str().into();
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

    /// Persist first, then publish the new selection/preferences in memory.
    pub fn update_and_save(&mut self, path: &Path,
        change: impl FnOnce(&mut Settings) -> Result<(), eink_core::session::AppError>) -> Result<Settings, eink_core::session::AppError> {
        let mut next = self.clone();
        change(&mut next)?;
        next.save(path)?;
        *self = next;
        Ok(self.clone())
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
        if let Some(mode) = patch.wifi_mode {
            next.wifi_mode = mode.parse::<WifiMode>()?.as_str().into();
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
    fn bridge_selection_migrates_and_is_not_a_webview_setting() {
        let old: Settings = serde_json::from_str(r#"{"wifiMode":"bridge","firmware":"reader.bin"}"#).unwrap();
        assert!(old.bridge.is_none());
        assert_eq!(old.wifi_mode, "bridge");
        let value = serde_json::json!({"wifiMode":"bridge", "bridge": {
            "version":1,"platform":"macos","adapterId":"en0","fingerprint":"en0:original",
            "resourceId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","displayName":"Ethernet"
        }});
        let settings: Settings = serde_json::from_value(value.clone()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        settings.save(&path).unwrap();
        let mut problems = Vec::new();
        let loaded = Settings::load(&path, &mut problems);
        assert_eq!(serde_json::to_value(loaded).unwrap()["bridge"], value["bridge"]);
        assert!(problems.is_empty());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"bridge":null}"#).is_err());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"bridge":{"adapterId":"forged"}}"#).is_err());
    }

    #[test]
    fn invalid_bridge_selection_preserves_mode_and_other_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for bridge in [serde_json::json!({"version":99}), serde_json::json!({
            "version":99,"platform":"macos","adapterId":"en0","fingerprint":"original",
            "resourceId":null,"displayName":"Ethernet"
        })] {
            std::fs::write(&path, serde_json::to_vec(&serde_json::json!({
                "wifiMode":"bridge","wifi":false,"firmware":"reader.bin","bridge":bridge
            })).unwrap()).unwrap();
            let mut problems = Vec::new();
            let loaded = Settings::load(&path, &mut problems);
            assert!(loaded.bridge.is_none());
            assert_eq!(loaded.wifi_mode, "bridge");
            assert!(!loaded.wifi);
            assert_eq!(loaded.firmware, Some(PathBuf::from("reader.bin")));
            assert_eq!(problems.len(), 1);
            assert!(!path.with_extension("json.bad").exists());
        }
    }

    #[test]
    fn wifi_mode_migrates_persists_and_recovers_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"wifi":false,"firmware":"reader.bin"}"#).unwrap();
        let mut problems = Vec::new();
        let mut settings = Settings::load(&path, &mut problems);
        assert_eq!(settings.wifi_mode, "nat");
        assert!(!settings.wifi);
        assert!(problems.is_empty());
        settings.apply(serde_json::from_str(r#"{"wifiMode":"bridge"}"#).unwrap()).unwrap();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path, &mut problems).wifi_mode, "bridge");
        assert!(settings.apply(serde_json::from_str(r#"{"wifi":true,"wifiMode":"typo"}"#).unwrap()).is_err());
        assert!(!settings.wifi); // Invalid patches must not partially apply.
        settings.wifi_mode = "typo".into();
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path, &mut problems);
        assert_eq!(loaded.wifi_mode, "nat");
        assert_eq!(loaded.firmware, Some(PathBuf::from("reader.bin")));
        assert!(!loaded.wifi);
        assert_eq!(problems.len(), 1);
        assert!(!path.with_extension("json.bad").exists());
    }

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
    #[test]
    fn bridge_save_failure_keeps_the_previous_selection_and_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        let mut settings = Settings::default();
        settings.wifi_mode = "bridge".into();
        let selected = serde_json::from_value(serde_json::json!({
            "version":1,"platform":"macos","adapterId":"en0","fingerprint":"original",
            "resourceId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","displayName":"Ethernet"
        })).unwrap();
        let result = settings.update_and_save(&path, |next| { next.bridge = Some(selected); Ok(()) });
        assert!(result.is_err(), "a directory cannot be replaced with the settings file");
        assert!(settings.bridge.is_none());
        assert_eq!(settings.wifi_mode, "bridge");
    }
}
