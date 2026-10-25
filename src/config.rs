use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{AppError, Result};
use crate::service::SortKey;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub db_path: Option<String>,
    pub ascii: bool,
    /// auto | always | never
    pub color: String,
    pub default_sort: String,
    /// One of `GUI_THEMES`; only the GUI reads it.
    pub gui_theme: String,
    /// Window opacity in percent (`OPACITY_RANGE`); only the translucent theme uses it.
    pub gui_opacity: u8,
    pub update: UpdateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdateConfig {
    pub check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { check: true }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: None,
            ascii: false,
            color: "auto".into(),
            default_sort: "priority".into(),
            gui_theme: GUI_THEMES[0].into(),
            gui_opacity: 80,
            update: UpdateConfig::default(),
        }
    }
}

pub const KEYS: &[&str] = &[
    "db_path",
    "ascii",
    "color",
    "default_sort",
    "gui_theme",
    "gui_opacity",
    "update.check",
];

/// Below ~20% text becomes unreadable over a busy wallpaper.
pub const OPACITY_RANGE: std::ops::RangeInclusive<u8> = 20..=100;

/// Theme names the GUI knows; the first is the default. `gui::palette` has one arm per name (a test checks).
pub const GUI_THEMES: &[&str] = &["frosted", "slate", "paper", "dusk", "contrast"];

fn storage(path: &Path, e: impl std::fmt::Display) -> AppError {
    AppError::Storage(format!("{}: {e}", path.display()))
}

/// `PROJECTGUI_CONFIG` overrides the location (used by tests and portable setups).
pub fn config_path() -> PathBuf {
    std::env::var_os("PROJECTGUI_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("projectgui")
                .join("config.toml")
        })
}

/// Resolution order: `PROJECTGUI_DB`, then `db_path` from the config, then the platform data dir.
pub fn db_path(cfg: &Config) -> PathBuf {
    if let Some(p) = std::env::var_os("PROJECTGUI_DB") {
        return p.into();
    }
    if let Some(p) = &cfg.db_path {
        return p.into();
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("projectgui")
        .join("projectgui.db")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut c: Self = toml::from_str(&s).map_err(|e| storage(path, e))?;
                // Clamped, not rejected: an error here would also block the CLI, and 0 would hide the window.
                c.gui_opacity = c
                    .gui_opacity
                    .clamp(*OPACITY_RANGE.start(), *OPACITY_RANGE.end());
                Ok(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(storage(path, e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| storage(dir, e))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| storage(path, e))?;
        std::fs::write(path, text).map_err(|e| storage(path, e))
    }

    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "db_path" => self.db_path.clone().unwrap_or_default(),
            "ascii" => self.ascii.to_string(),
            "color" => self.color.clone(),
            "default_sort" => self.default_sort.clone(),
            "gui_theme" => self.gui_theme.clone(),
            "gui_opacity" => self.gui_opacity.to_string(),
            "update.check" => self.update.check.to_string(),
            _ => return Err(unknown_key(key)),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let boolean = |v: &str| {
            v.parse::<bool>()
                .map_err(|_| AppError::invalid("value", "expected true or false"))
        };
        match key {
            "db_path" => {
                self.db_path = (!value.is_empty()).then(|| value.to_string());
                if let Some(p) = &self.db_path
                    && !Path::new(p).is_absolute()
                {
                    return Err(AppError::invalid(
                        "db_path",
                        "must be an absolute path (or empty to reset)",
                    ));
                }
            }
            "ascii" => self.ascii = boolean(value)?,
            "color" => {
                if !["auto", "always", "never"].contains(&value) {
                    return Err(AppError::invalid("color", "expected auto, always or never"));
                }
                self.color = value.into();
            }
            "default_sort" => {
                value.parse::<SortKey>()?;
                self.default_sort = value.into();
            }
            "gui_theme" => {
                if !GUI_THEMES.contains(&value) {
                    return Err(AppError::invalid(
                        "gui_theme",
                        format!("expected one of: {}", GUI_THEMES.join(", ")),
                    ));
                }
                self.gui_theme = value.into();
            }
            "gui_opacity" => {
                self.gui_opacity = value
                    .parse::<u8>()
                    .ok()
                    .filter(|v| OPACITY_RANGE.contains(v))
                    .ok_or_else(|| {
                        AppError::invalid(
                            "gui_opacity",
                            format!(
                                "expected a whole percent from {} to {}",
                                OPACITY_RANGE.start(),
                                OPACITY_RANGE.end()
                            ),
                        )
                    })?;
            }
            "update.check" => self.update.check = boolean(value)?,
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }
}

fn unknown_key(key: &str) -> AppError {
    AppError::invalid("key", format!("'{key}' (known keys: {})", KEYS.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let mut c = Config::load(&path).unwrap();
        assert_eq!(c, Config::default());
        c.set("ascii", "true").unwrap();
        c.set("update.check", "false").unwrap();
        c.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert!(back.ascii && !back.update.check);
    }

    #[test]
    fn partial_files_keep_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "ascii = true\n").unwrap();
        let c = Config::load(&path).unwrap();
        assert!(c.ascii && c.update.check && c.color == "auto");
    }

    #[test]
    fn set_validates() {
        let mut c = Config::default();
        assert!(c.set("color", "rainbow").is_err());
        assert!(c.set("default_sort", "nope").is_err());
        assert!(c.set("db_path", "relative.db").is_err());
        assert!(c.set("ascii", "yes").is_err());
        assert!(c.set("bogus", "1").is_err());
        assert_eq!(c.get("color").unwrap(), "auto");
    }

    #[test]
    fn gui_theme_is_validated_and_defaults_to_frosted() {
        let mut c = Config::default();
        assert_eq!(c.get("gui_theme").unwrap(), "frosted");
        c.set("gui_theme", "paper").unwrap();
        assert_eq!(c.get("gui_theme").unwrap(), "paper");
        let e = c.set("gui_theme", "neon").unwrap_err().to_string();
        assert!(e.contains("frosted"), "{e}");
    }

    #[test]
    fn a_hand_edited_opacity_is_clamped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "gui_opacity = 0\n").unwrap();
        assert_eq!(Config::load(&path).unwrap().gui_opacity, 20);
        std::fs::write(&path, "gui_opacity = 255\n").unwrap();
        assert_eq!(Config::load(&path).unwrap().gui_opacity, 100);
    }

    #[test]
    fn gui_opacity_is_a_validated_percentage() {
        let mut c = Config::default();
        assert_eq!(c.get("gui_opacity").unwrap(), "80");
        c.set("gui_opacity", "45").unwrap();
        assert_eq!(c.gui_opacity, 45);
        for bad in ["19", "101", "abc", "-5"] {
            assert!(c.set("gui_opacity", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn corrupt_file_is_an_error_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "ascii = [").unwrap();
        assert!(Config::load(&path).is_err());
    }
}
