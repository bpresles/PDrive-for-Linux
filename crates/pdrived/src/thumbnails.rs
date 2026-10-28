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
    encoder.add_text_chunk("Software".into(), "pdrive".into())?;
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba.as_raw())?;
    writer.finish()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A small JPEG of the given size, as Proton would deliver.
    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30])));
        let mut data = std::io::Cursor::new(Vec::new());
        image.write_to(&mut data, image::ImageFormat::Jpeg).unwrap();
        data.into_inner()
    }

    fn read_png(path: &Path) -> (u32, u32, Vec<(String, String)>) {
        let reader = png::Decoder::new(std::io::BufReader::new(File::open(path).unwrap())).read_info().unwrap();
        let info = reader.info();
        let text = info.uncompressed_latin1_text.iter().map(|t| (t.keyword.clone(), t.text.clone())).collect();
        (info.width, info.height, text)
    }

    #[test]
    fn name_is_the_lowercase_md5_of_the_uri() {
        // Example of the freedesktop.org thumbnail specification, "Thumbnail Saving".
        assert_eq!(name_for("file:///home/jens/photos/me.png"), "c6ee772d9e49320e97ec29a7eb5b1697.png");
    }

    #[test]
    fn uri_is_built_like_gio_does() {
        assert_eq!(uri_of(Path::new("/home/jens/photos/me.png")).unwrap(), "file:///home/jens/photos/me.png");
        // Escaped as GIO does, since the file manager hashes that string.
        assert_eq!(uri_of(Path::new("/home/jens/My Photos/é#1.png")).unwrap(), "file:///home/jens/My%20Photos/%C3%A9%231.png");
        assert!(uri_of(Path::new("relative/path.png")).is_none());
    }

    #[test]
    fn mtime_is_whole_seconds_since_the_epoch() {
        assert_eq!(mtime_value(UNIX_EPOCH + Duration::new(1_700_000_000, 999_999_999)), "1700000000");
        assert_eq!(mtime_value(UNIX_EPOCH), "0");
        // Before the epoch: no negative value.
        assert_eq!(mtime_value(UNIX_EPOCH - Duration::from_secs(1)), "0");
    }

    #[test]
    fn write_creates_every_flavour_with_spec_metadata() {
        let cache_home = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(cache_home.path());
        let file = Path::new("/home/user/ProtonDrive/My Photos/a.jpg");
        let mtime = UNIX_EPOCH + Duration::from_secs(1_700_000_123);
        cache.write(file, mtime, &jpeg(512, 256)).unwrap();

        let uri = "file:///home/user/ProtonDrive/My%20Photos/a.jpg";
        let name = name_for(uri);
        for (flavour, max) in [("normal", 128), ("large", 256), ("x-large", 512), ("xx-large", 1024)] {
            let path = cache_home.path().join("thumbnails").join(flavour).join(&name);
            let (width, height, text) = read_png(&path);
            assert!(width <= max && height <= max, "{flavour}: {width}x{height}");
            // Never upscaled, aspect ratio kept.
            if max >= 512 {
                assert_eq!((width, height), (512, 256));
            } else {
                assert_eq!(width, max);
                assert_eq!(height * 2, width);
            }
            let value = |key: &str| text.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
            assert_eq!(value("Thumb::URI"), Some(uri));
            assert_eq!(value("Thumb::MTime"), Some("1700000123"));
        }
    }

    #[test]
    fn written_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let cache_home = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(cache_home.path());
        cache.write(Path::new("/x/a.jpg"), UNIX_EPOCH, &jpeg(10, 10)).unwrap();
        let dir = cache_home.path().join("thumbnails/large");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let file = dir.join(name_for("file:///x/a.jpg"));
        assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn has_valid_checks_uri_and_mtime() {
        let cache_home = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(cache_home.path());
        let file = Path::new("/x/a.jpg");
        let mtime = UNIX_EPOCH + Duration::from_secs(100);
        assert!(!cache.has_valid(file, mtime));
        cache.write(file, mtime, &jpeg(300, 300)).unwrap();
        assert!(cache.has_valid(file, mtime));
        // Sub-second differences do not matter, whole seconds do.
        assert!(cache.has_valid(file, mtime + Duration::from_millis(500)));
        assert!(!cache.has_valid(file, mtime + Duration::from_secs(1)));
        assert!(!cache.has_valid(Path::new("/x/b.jpg"), mtime));
    }

    #[test]
    fn has_valid_rejects_a_png_named_after_another_uri() {
        let cache_home = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(cache_home.path());
        let mtime = UNIX_EPOCH + Duration::from_secs(5);
        cache.write(Path::new("/x/a.jpg"), mtime, &jpeg(20, 20)).unwrap();
        let large = cache_home.path().join("thumbnails/large");
        // A thumbnail copied under the name of a.jpg's neighbour: its Thumb::URI does not match.
        std::fs::copy(large.join(name_for("file:///x/a.jpg")), large.join(name_for("file:///x/b.jpg"))).unwrap();
        assert!(!cache.has_valid(Path::new("/x/b.jpg"), mtime));
    }

    #[test]
    fn write_fails_cleanly_on_bad_data() {
        let cache_home = tempfile::tempdir().unwrap();
        let cache = ThumbnailCache::new(cache_home.path());
        assert!(cache.write(Path::new("/x/a.jpg"), UNIX_EPOCH, b"not an image").is_err());
        assert!(cache.write(Path::new("relative.jpg"), UNIX_EPOCH, &jpeg(4, 4)).is_err());
        assert!(!cache.has_valid(Path::new("/x/a.jpg"), UNIX_EPOCH));
    }
}
