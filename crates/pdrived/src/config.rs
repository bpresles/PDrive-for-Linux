// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Settings, from `$XDG_CONFIG_HOME/pdrive/config.toml`:
//!
//! ```toml
//! # Where "My files" is mounted ("~" is the home folder).
//! mountpoint = "~/ProtonDrive"
//! # Maximum size of the cache of downloaded files, in GiB.
//! cache_size_gib = 10
//! # Whether launching pdrive-app opens its status window (read by pdrive-app only).
//! show_window_at_startup = true
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const APP_DIR: &str = "pdrive";
/// Directory name used before the project got its name.
const LEGACY_APP_DIR: &str = "nautilus-drive-poc";

/// Moves the settings and the cache (including pending uploads) from the legacy directories.
pub fn migrate_legacy_dirs() -> Result<()> {
    for base in [xdg_dir("XDG_CONFIG_HOME", ".config")?, xdg_dir("XDG_CACHE_HOME", ".cache")?] {
        let (legacy, current) = (base.join(LEGACY_APP_DIR), base.join(APP_DIR));
        if legacy.is_dir() && !current.exists() {
            std::fs::rename(&legacy, &current).with_context(|| format!("cannot move {} to {}", legacy.display(), current.display()))?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mountpoint: String,
    pub cache_size_gib: f64,
    /// Setting of pdrive-app, accepted here as the file is shared.
    #[allow(dead_code)]
    pub show_window_at_startup: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { mountpoint: "~/ProtonDrive".into(), cache_size_gib: 10.0, show_window_at_startup: true }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.mountpoint, "~/ProtonDrive");
        assert_eq!(settings.cache_size_gib, 10.0);
        assert!(settings.show_window_at_startup);
    }

    #[test]
    fn documented_example_parses() {
        let text = "# comment\nmountpoint = \"/mnt/drive\"\ncache_size_gib = 2.5\nshow_window_at_startup = false\n";
        let settings: Settings = toml::from_str(text).unwrap();
        assert_eq!(settings.mountpoint, "/mnt/drive");
        assert_eq!(settings.cache_size_gib, 2.5);
        assert!(!settings.show_window_at_startup);
        // An integer is accepted for the size, as written by the application.
        assert_eq!(toml::from_str::<Settings>("cache_size_gib = 10").unwrap().cache_size_gib, 10.0);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Settings>("cache = 1").is_err());
    }

    #[test]
    fn cache_size_in_bytes() {
        let size = |gib| Settings { cache_size_gib: gib, ..Settings::default() }.cache_size_bytes();
        assert_eq!(size(1.0), 1 << 30);
        assert_eq!(size(0.5), 1 << 29);
        assert_eq!(size(-3.0), 0);
    }

    #[test]
    fn home_expansion_leaves_other_paths_alone() {
        assert_eq!(expand_home("/abs/path").unwrap(), PathBuf::from("/abs/path"));
        assert_eq!(expand_home("rel/path").unwrap(), PathBuf::from("rel/path"));
    }
}
