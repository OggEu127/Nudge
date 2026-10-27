//! Bounded, off-thread decoding for optional instance cover artwork.
//!
//! Covers are presentation-only. Custom artwork is re-encoded to a bounded PNG
//! so planted metadata, decompression bombs and symlinks never reach the UI or
//! the instance directory unchecked.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use launcher_core::instances::check_no_links;
use tokio_util::sync::CancellationToken;

use crate::errors::FrontendError;

const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 4096;
const MAX_THUMBNAIL_DIMENSION: u32 = 512;
const MAX_DECODE_ALLOC: u64 = 128 * 1024 * 1024;
const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;
const COVER_PATH_TTL: Duration = Duration::from_secs(3);
const THUMBNAIL_NAME: &str = "cover.png";
const COVER_NAMES: [&str; 4] = ["cover.png", "cover.jpg", "cover.jpeg", "cover.webp"];

struct Request {
    key: String,
    path: PathBuf,
}

struct Loaded {
    key: String,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct Failed {
    key: String,
}

/// Locates an existing regular, non-link cover in one instance directory.
pub(crate) fn find_cover(instance_directory: &Path) -> Option<PathBuf> {
    for name in COVER_NAMES {
        let path = instance_directory.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
            return Some(path);
        }
    }
    None
}

/// Reads and bounds an image from an untrusted path.
fn read_bounded_image(path: &Path) -> Result<DynamicImage> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        bail!("封面文件必须是不超过 16 MiB 的 PNG、JPEG 或 WebP 图片");
    }
    let reader = ImageReader::new(BufReader::new(File::open(path)?))
        .with_guessed_format()
        .map_err(|error| anyhow::anyhow!("无法识别封面格式：{error}"))?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        bail!("封面仅支持 PNG、JPEG 或 WebP 图片");
    }
    let mut reader = reader;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| anyhow::anyhow!("无法解码封面图片：{error}"))?;
    Ok(bounded_thumbnail(image))
}

/// Creates the bounded thumbnail used by the UI.
///
/// # Errors
/// Rejects oversized files and dimensions, unsupported data, and I/O errors.
pub(crate) fn decode_cover(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let rgba = read_bounded_image(path)?.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

fn bounded_thumbnail(image: DynamicImage) -> DynamicImage {
    let width = image.width();
    let height = image.height();
    if width <= MAX_THUMBNAIL_DIMENSION && height <= MAX_THUMBNAIL_DIMENSION {
        return image;
    }
    let scale = (f64::from(MAX_THUMBNAIL_DIMENSION) / f64::from(width.max(height))).min(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let target_width = ((f64::from(width) * scale).round() as u32).max(1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let target_height = ((f64::from(height) * scale).round() as u32).max(1);
    image.thumbnail(target_width, target_height)
}

/// Re-encodes dropped custom artwork into the instance's managed data area.
///
/// # Errors
/// Rejects invalid IDs and symlinked instance roots, and image/IO failures.
pub(crate) async fn install_custom_cover(
    data_dir: &Path,
    instance_id: &str,
    source: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    check_cancel(cancel)?;
    launcher_core::instances::validate_id(instance_id)?;
    let directory = data_dir.join("instances").join(instance_id);
    let target = directory.join(THUMBNAIL_NAME);
    check_cover_destination(&directory, &target).await?;
    check_no_links(source).await?;
    let source = source.to_path_buf();
    let worker_directory = directory.clone();
    let worker_cancel = cancel.clone();
    // Decoding, resampling, encoding and sync are blocking work. Await this
    // worker rather than dropping it on cancellation: it owns the temporary.
    let temporary = tokio::task::spawn_blocking(move || -> Result<_> {
        check_cancel(&worker_cancel)?;
        let image = read_bounded_image(&source)?;
        check_cancel(&worker_cancel)?;
        let mut temporary = tempfile::NamedTempFile::new_in(worker_directory)?;
        image.write_to(temporary.as_file_mut(), ImageFormat::Png)?;
        temporary.as_file().sync_all()?;
        check_cancel(&worker_cancel)?;
        Ok(temporary)
    })
    .await
    .map_err(|_| anyhow::anyhow!("封面处理任务意外结束"))??;
    check_cancel(cancel)?;
    check_cover_destination(&directory, &target).await?;
    let commit_target = target.clone();
    let commit_cancel = cancel.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        check_cancel(&commit_cancel)?;
        // Replace the directory entry, never truncate an existing inode. This
        // preserves any external file hard-linked to the previous cover.
        launcher_net::persistence::persist(temporary, &commit_target).map_err(|error| {
            anyhow::anyhow!("无法原子提交封面 {}：{}", commit_target.display(), error)
        })?;
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("封面保存任务意外结束"))??;
    Ok(target)
}

async fn check_cover_destination(directory: &Path, target: &Path) -> Result<()> {
    check_no_links(target).await?;
    if !tokio::fs::symlink_metadata(directory).await?.is_dir() {
        bail!("实例目录不可用于保存封面");
    }
    match tokio::fs::symlink_metadata(target).await {
        Ok(metadata) if !metadata.is_file() => bail!("封面目标必须是普通文件"),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        bail!(FrontendError::Cancelled);
    }
    Ok(())
}

/// Background decoder plus the textures currently available to the UI.
pub(crate) struct CoverLoader {
    sender: Option<Sender<Request>>,
    receiver: Receiver<Loaded>,
    failed_receiver: Receiver<Failed>,
    requested: HashSet<String>,
    textures: HashMap<String, TextureHandle>,
    texture_bytes: HashMap<String, usize>,
    last_used: HashMap<String, u64>,
    usage_tick: u64,
    cached_paths: HashMap<String, (Option<PathBuf>, Instant)>,
    failed: HashSet<String>,
}

impl CoverLoader {
    pub(crate) fn new() -> Self {
        let (request_sender, request_receiver) = mpsc::channel::<Request>();
        let (loaded_sender, loaded_receiver) = mpsc::channel::<Loaded>();
        let (failed_sender, failed_receiver) = mpsc::channel::<Failed>();
        std::thread::Builder::new()
            .name("nudge-covers".to_owned())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    match decode_cover(&request.path) {
                        Ok((width, height, rgba)) => {
                            if loaded_sender
                                .send(Loaded {
                                    key: request.key,
                                    width,
                                    height,
                                    rgba,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => {
                            if failed_sender.send(Failed { key: request.key }).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("the cover worker thread can be created");
        Self {
            sender: Some(request_sender),
            receiver: loaded_receiver,
            failed_receiver,
            requested: HashSet::new(),
            textures: HashMap::new(),
            texture_bytes: HashMap::new(),
            last_used: HashMap::new(),
            usage_tick: 0,
            cached_paths: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    pub(crate) fn cover_path(&mut self, instance_id: &str, directory: &Path) -> Option<PathBuf> {
        if let Some((path, checked)) = self.cached_paths.get(instance_id)
            && checked.elapsed() < COVER_PATH_TTL
        {
            return path.clone();
        }
        let path = find_cover(directory);
        if self
            .cached_paths
            .get(instance_id)
            .is_some_and(|(previous, _)| previous != &path)
        {
            self.reload(instance_id);
        }
        self.cached_paths
            .insert(instance_id.to_owned(), (path.clone(), Instant::now()));
        path
    }

    fn evict_oldest(&mut self) {
        if let Some(key) = self
            .last_used
            .iter()
            .min_by_key(|(_, tick)| *tick)
            .map(|(key, _)| key.clone())
        {
            self.textures.remove(&key);
            self.texture_bytes.remove(&key);
            self.last_used.remove(&key);
        }
    }

    /// Requests a texture once; repeated card redraws remain cheap.
    pub(crate) fn ensure(&mut self, instance_id: &str, path: &Path) {
        if self.textures.contains_key(instance_id)
            || self.failed.contains(instance_id)
            || self.requested.contains(instance_id)
        {
            return;
        }
        let Some(sender) = &self.sender else {
            return;
        };
        if sender
            .send(Request {
                key: instance_id.to_owned(),
                path: path.to_path_buf(),
            })
            .is_ok()
        {
            self.requested.insert(instance_id.to_owned());
        }
    }

    /// Uploads any decoded covers. Called once per frame from the UI.
    pub(crate) fn poll(&mut self, context: &egui::Context) {
        while let Ok(loaded) = self.receiver.try_recv() {
            self.requested.remove(&loaded.key);
            let bytes = loaded.rgba.len();
            let size = [
                usize::try_from(loaded.width).unwrap_or(0),
                usize::try_from(loaded.height).unwrap_or(0),
            ];
            if size[0] == 0 || size[1] == 0 {
                self.failed.insert(loaded.key);
                continue;
            }
            if bytes > MAX_TEXTURE_BYTES {
                self.failed.insert(loaded.key);
                continue;
            }
            while self.texture_bytes.values().sum::<usize>() + bytes > MAX_TEXTURE_BYTES {
                self.evict_oldest();
            }
            let image = ColorImage::from_rgba_unmultiplied(size, &loaded.rgba);
            let texture = context.load_texture(
                format!("instance-cover-{}", loaded.key),
                image,
                TextureOptions::LINEAR,
            );
            self.usage_tick = self.usage_tick.wrapping_add(1);
            self.last_used.insert(loaded.key.clone(), self.usage_tick);
            self.texture_bytes.insert(loaded.key.clone(), bytes);
            self.textures.insert(loaded.key, texture);
        }
        while let Ok(failed) = self.failed_receiver.try_recv() {
            self.requested.remove(&failed.key);
            self.failed.insert(failed.key);
        }
    }

    pub(crate) fn texture(&mut self, instance_id: &str) -> Option<&TextureHandle> {
        self.usage_tick = self.usage_tick.wrapping_add(1);
        if let Some(last_used) = self.last_used.get_mut(instance_id) {
            *last_used = self.usage_tick;
        }
        self.textures.get(instance_id)
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.requested.is_empty()
    }

    /// Forget one cover after the user supplies new artwork.
    pub(crate) fn reload(&mut self, instance_id: &str) {
        self.textures.remove(instance_id);
        self.texture_bytes.remove(instance_id);
        self.last_used.remove(instance_id);
        self.cached_paths.remove(instance_id);
        self.failed.remove(instance_id);
        self.requested.remove(instance_id);
    }

    /// Drops textures for instances that no longer exist.
    pub(crate) fn retain_instances(&mut self, active: &HashSet<String>) {
        self.textures.retain(|key, _| active.contains(key));
        self.texture_bytes.retain(|key, _| active.contains(key));
        self.last_used.retain(|key, _| active.contains(key));
        self.cached_paths.retain(|key, _| active.contains(key));
        self.failed.retain(|key| active.contains(key));
        self.requested.retain(|key| active.contains(key));
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_cover, install_custom_cover};
    use image::{ImageFormat, RgbaImage};
    use std::fs;

    fn write_png(path: &std::path::Path) {
        RgbaImage::from_pixel(8, 4, image::Rgba([32, 64, 96, 255]))
            .save_with_format(path, ImageFormat::Png)
            .expect("write fixture image");
    }

    #[tokio::test]
    async fn covers_are_bounded_and_custom_art_is_reencoded() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("source.png");
        write_png(&source);
        let (width, height, rgba) = decode_cover(&source)?;
        assert_eq!((width, height), (8, 4));
        assert_eq!(rgba.len(), 8 * 4 * 4);

        let data = directory.path().join("data");
        let instance = data.join("instances").join("demo");
        fs::create_dir_all(&instance)?;
        let target = install_custom_cover(
            &data,
            "demo",
            &source,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await?;
        assert_eq!(target, instance.join("cover.png"));
        assert!(target.is_file());
        Ok(())
    }

    #[test]
    fn invalid_cover_data_is_rejected() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("bad.png");
        fs::write(&source, b"not an image")?;
        assert!(decode_cover(&source).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn failed_or_cancelled_cover_import_preserves_existing_bytes() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let instance = root.path().join("instances/demo");
        fs::create_dir_all(&instance)?;
        let target = instance.join("cover.png");
        fs::write(&target, b"old cover must survive")?;
        let bad_source = root.path().join("invalid.png");
        fs::write(&bad_source, b"not an image")?;
        let cancel = tokio_util::sync::CancellationToken::new();
        assert!(
            install_custom_cover(root.path(), "demo", &bad_source, &cancel)
                .await
                .is_err()
        );
        assert_eq!(fs::read(&target)?, b"old cover must survive");
        let source = root.path().join("source.png");
        write_png(&source);
        cancel.cancel();
        assert!(
            install_custom_cover(root.path(), "demo", &source, &cancel)
                .await
                .is_err()
        );
        assert_eq!(fs::read(&target)?, b"old cover must survive");
        assert_eq!(fs::read_dir(&instance)?.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn cover_replacement_does_not_truncate_a_hardlinked_external_file() -> anyhow::Result<()>
    {
        let root = tempfile::tempdir()?;
        let instance = root.path().join("instances/demo");
        fs::create_dir_all(&instance)?;
        let external = root.path().join("external.dat");
        fs::write(&external, b"irreplaceable bytes")?;
        fs::hard_link(&external, instance.join("cover.png"))?;
        let source = root.path().join("source.png");
        write_png(&source);
        let target = install_custom_cover(
            root.path(),
            "demo",
            &source,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await?;
        assert_eq!(fs::read(external)?, b"irreplaceable bytes");
        assert_eq!(&fs::read(target)?[..8], b"\x89PNG\r\n\x1a\n");
        Ok(())
    }
}
