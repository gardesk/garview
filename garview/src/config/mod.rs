use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub images: ImageConfig,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_zoom")]
    pub default_zoom: String,
    #[serde(default)]
    pub remember_position: bool,
    #[serde(default)]
    pub remember_zoom: bool,
    #[serde(default = "default_bg")]
    pub background_color: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            default_zoom: default_zoom(),
            remember_position: false,
            remember_zoom: false,
            background_color: default_bg(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ImageConfig {
    #[serde(default = "default_true")]
    pub auto_rotate_exif: bool,
    #[serde(default = "default_interpolation")]
    pub interpolation: String,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            auto_rotate_exif: true,
            interpolation: default_interpolation(),
        }
    }
}

fn default_zoom() -> String {
    "fit".to_string()
}

fn default_bg() -> String {
    "#1a1b26".to_string()
}

fn default_true() -> bool {
    true
}

fn default_interpolation() -> String {
    "lanczos".to_string()
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();

        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("garview/config.toml")
    }
}
