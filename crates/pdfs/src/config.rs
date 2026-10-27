//! Settings, from `$XDG_CONFIG_HOME/nautilus-drive-poc/config.toml`:
//!
//! ```toml
//! # Where "My files" is mounted ("~" is the home folder).
//! mountpoint = "~/ProtonDrive"
//! # Maximum size of the cache of downloaded files, in GiB.
//! cache_size_gib = 10
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const APP_DIR: &str = "nautilus-drive-poc";

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mountpoint: String,
    pub cache_size_gib: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { mountpoint: "~/ProtonDrive".into(), cache_size_gib: 10.0 }
    }
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = xdg_dir("XDG_CONFIG_HOME", ".config")?.join(APP_DIR).join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid settings in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn cache_size_bytes(&self) -> u64 {
        (self.cache_size_gib.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64
    }
}

/// Expands a leading "~" to the home folder.
pub fn expand_home(path: &str) -> Result<PathBuf> {
    Ok(match path.strip_prefix('~') {
        Some(rest) => home()?.join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    })
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?))
}

/// An XDG base directory, e.g. `XDG_CACHE_HOME` with `~/.cache` as default.
pub fn xdg_dir(variable: &str, default: &str) -> Result<PathBuf> {
    Ok(match std::env::var_os(variable) {
        Some(dir) if !dir.is_empty() && Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => home()?.join(default),
    })
}
