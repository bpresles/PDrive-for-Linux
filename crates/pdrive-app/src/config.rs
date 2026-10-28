//! Settings file shared with the daemon (`$XDG_CONFIG_HOME/pdrive/config.toml`, see
//! `crates/pdrived/src/config.rs`), edited while preserving comments and layout, and the XDG
//! autostart entry.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Value};

pub const APP_ID: &str = "io.github.pdrive.PDrive.App";

pub struct Settings {
    pub mountpoint: String,
    pub cache_size_gib: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { mountpoint: "~/ProtonDrive".into(), cache_size_gib: 10.0 }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() && Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => home().join(".config"),
    }
}

fn settings_path() -> PathBuf {
    config_home().join("pdrive").join("config.toml")
}

fn read_document() -> DocumentMut {
    std::fs::read_to_string(settings_path()).ok().and_then(|text| text.parse().ok()).unwrap_or_default()
}

pub fn load() -> Settings {
    let document = read_document();
    let defaults = Settings::default();
    Settings {
        mountpoint: document.get("mountpoint").and_then(|v| v.as_str()).map_or(defaults.mountpoint, str::to_owned),
        cache_size_gib: document
            .get("cache_size_gib")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(defaults.cache_size_gib),
    }
}

fn write_document(document: &DocumentMut) -> Result<()> {
    let path = settings_path();
    std::fs::create_dir_all(path.parent().context("no parent directory")?)?;
    std::fs::write(&path, document.to_string()).with_context(|| format!("cannot write {}", path.display()))
}

/// Replaces a value keeping the comment that follows it.
fn set(document: &mut DocumentMut, key: &str, mut value: Value) {
    if let Some(old) = document.get(key).and_then(|item| item.as_value()) {
        *value.decor_mut() = old.decor().clone();
    }
    document[key] = toml_edit::Item::Value(value);
}

pub fn save(settings: &Settings) -> Result<()> {
    let mut document = read_document();
    set(&mut document, "mountpoint", Value::from(settings.mountpoint.as_str()));
    let size = if settings.cache_size_gib.fract() == 0.0 { Value::from(settings.cache_size_gib as i64) } else { Value::from(settings.cache_size_gib) };
    set(&mut document, "cache_size_gib", size);
    write_document(&document)
}

/// Whether a launch (other than `--background`) opens the status window; on by default.
pub fn show_window_at_startup() -> bool {
    read_document().get("show_window_at_startup").and_then(|v| v.as_bool()).unwrap_or(true)
}

pub fn set_show_window_at_startup(show: bool) -> Result<()> {
    let mut document = read_document();
    set(&mut document, "show_window_at_startup", Value::from(show));
    write_document(&document)
}

/// Expands a leading "~" to the home folder.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) => home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

fn autostart_path() -> PathBuf {
    config_home().join("autostart").join(format!("{APP_ID}.desktop"))
}

pub fn autostart_enabled() -> bool {
    autostart_path().exists()
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = autostart_path();
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().context("cannot find the executable")?;
    // Quoted as the Desktop Entry specification requires for paths with spaces.
    let exec = format!("\"{}\" --background", exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=PDrive\nComment=Proton Drive mount (unofficial third-party client)\nExec={exec}\nIcon={APP_ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );
    std::fs::create_dir_all(path.parent().context("no parent directory")?)?;
    std::fs::write(&path, entry).with_context(|| format!("cannot write {}", path.display()))
}
