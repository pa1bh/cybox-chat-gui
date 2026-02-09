use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_DIR: &str = ".config/cybox-chat-gui";
const LEGACY_SETTINGS_FILE: &str = ".cybox-chat-gui-settings.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub server_url: String,
    pub username: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            server_url: "ws://127.0.0.1:3001".to_string(),
            username: String::new(),
        }
    }
}

fn settings_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(SETTINGS_DIR).join(SETTINGS_FILE);
    }

    PathBuf::from(".cybox-chat-gui-settings.json")
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => match fs::read_to_string(LEGACY_SETTINGS_FILE) {
            Ok(legacy_raw) => legacy_raw,
            Err(_) => return AppSettings::default(),
        },
    };

    serde_json::from_str::<AppSettings>(&raw).unwrap_or_default()
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create settings directory: {}", err))?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("Failed to serialize settings: {}", err))?;

    fs::write(path, json).map_err(|err| format!("Failed to write settings file: {}", err))
}
