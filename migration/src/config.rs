use anyhow::{anyhow, Context, Result};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tiers {
    #[serde(default = "default_hot_days", rename = "hotDays")]
    pub hot_days: i64,
    #[serde(default = "default_hot_min_access", rename = "hotMinAccess")]
    pub hot_min_access: i64,
    #[serde(default = "default_warm_days", rename = "warmDays")]
    pub warm_days: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "dataPath")]
    pub data_path: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    pub token: String,
    #[serde(default = "default_tiers")]
    pub tiers: Tiers,
}

fn default_port() -> u16 {
    7878
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_hot_days() -> i64 {
    7
}

fn default_hot_min_access() -> i64 {
    10
}

fn default_warm_days() -> i64 {
    30
}

fn default_tiers() -> Tiers {
    Tiers {
        hot_days: default_hot_days(),
        hot_min_access: default_hot_min_access(),
        warm_days: default_warm_days(),
    }
}

pub(crate) fn default_data_path() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    Ok(home.join(".dbrain"))
}

pub(crate) fn resolve_data_path(path_arg: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = path_arg {
        return Ok(to_absolute(expand_tilde(path)));
    }
    default_data_path().map(to_absolute)
}

pub(crate) fn expand_tilde(input: &str) -> PathBuf {
    if input.starts_with("~/") {
        if let Some(home) = home_dir() {
            return home.join(input.trim_start_matches("~/"));
        }
    }
    PathBuf::from(input)
}

pub(crate) fn to_absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }

    if let Ok(current_dir) = std::env::current_dir() {
        return current_dir.join(path);
    }

    path
}

pub(crate) fn config_path(data_path: &Path) -> PathBuf {
    data_path.join("config.json")
}

pub(crate) fn load_config(data_path: &Path) -> Result<Config> {
    let path = config_path(data_path);
    if !path.exists() {
        return Err(anyhow!(
            "Config not found at {}. Run 'dbrain init' first.",
            path.display()
        ));
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file at {}", path.display()))?;

    let mut config: Config = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid JSON in {}", path.display()))?;

    if config.data_path.is_empty() {
        config.data_path = data_path.display().to_string();
    }

    Ok(config)
}

pub(crate) fn save_config(config: &Config) -> Result<()> {
    let data_path = PathBuf::from(&config.data_path);
    fs::create_dir_all(&data_path)
        .with_context(|| format!("Failed to create data directory {}", data_path.display()))?;

    let path = config_path(&data_path);
    let raw = serde_json::to_string_pretty(config)? + "\n";
    fs::write(&path, raw).with_context(|| format!("Failed to write config {}", path.display()))?;
    Ok(())
}
