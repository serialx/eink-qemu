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
}

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
    pub fn load(path: &Path) -> Settings {
        let mut settings: Settings = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if !DEVICES.contains(&settings.device.as_str()) {
            settings.device = "auto".into();
        }
        if !PANELS.contains(&settings.panel.as_str()) {
            settings.panel = "uc8279-68".into();
        }
        settings
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self).unwrap())?;
        std::fs::rename(temp, path)
    }

    pub fn apply(&mut self, patch: SettingsPatch) -> Result<(), String> {
        if let Some(device) = patch.device {
            if !DEVICES.contains(&device.as_str()) {
                return Err(format!("Unknown device: {device}."));
            }
            self.device = device;
        }
        if let Some(panel) = patch.panel {
            if !PANELS.contains(&panel.as_str()) {
                return Err(format!("Unknown X4 Pro panel: {panel}."));
            }
            self.panel = panel;
        }
        if let Some(wifi) = patch.wifi {
            self.wifi = wifi;
        }
        if let Some(ghosting) = patch.ghosting {
            self.ghosting = ghosting;
        }
        if let Some(turbo) = patch.turbo {
            self.turbo = turbo;
        }
        Ok(())
    }
}
