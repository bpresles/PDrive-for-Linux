//! Fills the freedesktop.org thumbnail cache with the thumbnails stored by Proton Drive.
//!
//! File managers look up `$XDG_CACHE_HOME/thumbnails/<size>/<md5(uri)>.png` and only generate a
//! thumbnail themselves (reading the whole file) when there is no valid one, valid meaning that
//! its `Thumb::URI` and `Thumb::MTime` match the file. Writing them from the small thumbnails
//! Proton already has avoids downloading every picture of a folder.
//! Spec: https://specifications.freedesktop.org/thumbnail-spec/latest/

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use image::DynamicImage;
use md5::Digest;

/// Directory names and maximum sizes of the spec. Proton thumbnails are at most 512 px and the
/// spec forbids upscaling, so the larger flavours hold the original size.
const FLAVOURS: [(&str, u32); 4] = [("normal", 128), ("large", 256), ("x-large", 512), ("xx-large", 1024)];

pub struct ThumbnailCache {
    dir: PathBuf,
}

impl ThumbnailCache {
    pub fn new(cache_home: &Path) -> Self {
        Self { dir: cache_home.join("thumbnails") }
    }

    /// Whether a valid "large" thumbnail exists for this file.
    pub fn has_valid(&self, file: &Path, mtime: SystemTime) -> bool {
        let Some(uri) = uri_of(file) else { return false };
        let path = self.dir.join("large").join(name_for(&uri));
        let Ok(reader) = File::open(&path) else { return false };
        let Ok(png) = png::Decoder::new(std::io::BufReader::new(reader)).read_info() else { return false };
        let text = &png.info().uncompressed_latin1_text;
        let value = |key: &str| text.iter().find(|t| t.keyword == key).map(|t| t.text.as_str());
        value("Thumb::URI") == Some(uri.as_str()) && value("Thumb::MTime") == Some(mtime_value(mtime).as_str())
    }

    /// Writes all the flavours of a thumbnail from image data (JPEG from Proton).
    pub fn write(&self, file: &Path, mtime: SystemTime, data: &[u8]) -> Result<()> {
        let uri = uri_of(file).context("cannot build the file URI")?;
        let image = image::load_from_memory(data).context("cannot decode the thumbnail")?;
        let name = name_for(&uri);
        for (flavour, size) in FLAVOURS {
            let scaled = if image.width() > size || image.height() > size { image.thumbnail(size, size) } else { image.clone() };
            let dir = self.dir.join(flavour);
            create_private_dir(&dir)?;
            write_png(&dir.join(&name), &scaled, &uri, mtime)?;
        }
        Ok(())
    }
}

/// The URI exactly as GIO builds it, since the cache key is its hash.
fn uri_of(path: &Path) -> Option<String> {
    glib::filename_to_uri(path, None).ok().map(|uri| uri.to_string())
}

fn name_for(uri: &str) -> String {
    let digest = md5::Md5::digest(uri.as_bytes());
    format!("{}.png", digest.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn mtime_value(mtime: SystemTime) -> String {
    mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string()
}

fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("cannot create {}", dir.display()))
}

/// Writes the PNG atomically (temporary file then rename), readable by the owner only.
fn write_png(path: &Path, image: &DynamicImage, uri: &str, mtime: SystemTime) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let rgba = image.to_rgba8();
    let temporary = path.with_extension(format!("png.{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), rgba.width(), rgba.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.add_text_chunk("Thumb::URI".into(), uri.into())?;
    encoder.add_text_chunk("Thumb::MTime".into(), mtime_value(mtime))?;
    encoder.add_text_chunk("Software".into(), "nautilus-drive-poc".into())?;
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba.as_raw())?;
    writer.finish()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}
