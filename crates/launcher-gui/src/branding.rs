//! Embedded Nudge artwork shared by the window chrome and native window icon.

use anyhow::{Context as _, Result};
use eframe::egui::{
    self, ColorImage, TextureFilter, TextureHandle, TextureOptions, TextureWrapMode,
};
use image::{Rgba, RgbaImage};

/// Source artwork handed to the desktop shell as the window icon.
const WINDOW_ICON_PNG: &[u8] =
    include_bytes!("../../../assets/branding/nudge-icon-blue-violet.png");
/// Runtime icon edge; keep the image bounded before sending it to the window manager.
/// 512 × 512 RGBA is too large for some servers; 128 keeps the request in bounds.
const WINDOW_ICON_SIZE: u32 = 128;
/// Logo sheet holding both the square icon and the lowercase wordmark.
const LOGO_PNG: &[u8] = include_bytes!("../../../assets/branding/nudge-logo-blue-violet.png");
/// The same ICO handed to the Windows resource compiler, parsed by tests.
#[cfg(test)]
const WINDOW_ICON_ICO: &[u8] =
    include_bytes!("../../../assets/branding/nudge-icon-blue-violet.ico");
/// Downscaled atlas edge. The title bar draws it below 150 pt wide even at 150%
/// scaling, so 512 px keeps memory and texture-size demands modest.
const LOGO_TEXTURE_WIDTH: u32 = 512;

/// Crop of the icon inside [`LOGO_PNG`], measured from its alpha bounds.
pub(crate) const ICON_UV: egui::Rect = egui::Rect::from_min_max(
    egui::pos2(106.0 / 1508.0, 45.0 / 488.0),
    egui::pos2(473.0 / 1508.0, 431.0 / 488.0),
);
/// Crop of the wordmark inside [`LOGO_PNG`], excluding the tagline below it.
pub(crate) const WORDMARK_UV: egui::Rect = egui::Rect::from_min_max(
    egui::pos2(556.0 / 1508.0, 124.0 / 488.0),
    egui::pos2(1310.0 / 1508.0, 306.0 / 488.0),
);
/// Aspect ratio of [`ICON_UV`].
pub(crate) const ICON_ASPECT_RATIO: f32 = 367.0 / 386.0;
/// Aspect ratio of [`WORDMARK_UV`].
pub(crate) const WORDMARK_ASPECT_RATIO: f32 = 754.0 / 182.0;

/// Resizes RGBA artwork without letting fully transparent black pixels bleed
/// into the edges of the opaque shapes.
///
/// `image::imageops::resize` filters each channel independently, so straight
/// alpha would mix the hidden RGB of transparent pixels into the result and
/// leave dark fringes. Resampling premultiplied channels keeps the source
/// colour, and the result is converted back because [`egui::IconData`] and
/// [`ColorImage::from_rgba_unmultiplied`] expect straight alpha.
fn resize_premultiplied(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let mut premultiplied = RgbaImage::new(image.width(), image.height());
    for (destination, source) in premultiplied.pixels_mut().zip(image.pixels()) {
        let alpha = u32::from(source[3]);
        let channel =
            |value: u8| u8::try_from((u32::from(value) * alpha + 127) / 255).unwrap_or(u8::MAX);
        *destination = Rgba([
            channel(source[0]),
            channel(source[1]),
            channel(source[2]),
            source[3],
        ]);
    }
    let mut resized = image::imageops::resize(
        &premultiplied,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    );
    for pixel in resized.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel.0 = [0, 0, 0, 0];
            continue;
        }
        let channel = |value: u8| {
            u8::try_from((u32::from(value) * 255 + alpha / 2) / alpha).unwrap_or(u8::MAX)
        };
        pixel.0 = [
            channel(pixel[0]),
            channel(pixel[1]),
            channel(pixel[2]),
            pixel[3],
        ];
    }
    resized
}

/// Decodes the embedded square artwork for the native window icon.
///
/// # Errors
/// Returns an error only if the checked-in PNG cannot be decoded.
pub(crate) fn window_icon() -> Result<egui::IconData> {
    let source = image::load_from_memory(WINDOW_ICON_PNG)
        .context("无法解码内置的 Nudge 窗口图标")?
        .into_rgba8();
    let rgba = resize_premultiplied(&source, WINDOW_ICON_SIZE, WINDOW_ICON_SIZE);
    Ok(egui::IconData {
        rgba: rgba.into_raw(),
        width: WINDOW_ICON_SIZE,
        height: WINDOW_ICON_SIZE,
    })
}

/// Uploads the embedded logo sheet for use by the custom title bar.
///
/// # Errors
/// Returns an error only if the checked-in PNG cannot be decoded.
pub(crate) fn logo_texture(context: &egui::Context) -> Result<TextureHandle> {
    let image = image::load_from_memory(LOGO_PNG)
        .context("无法解码内置的 Nudge 标志")?
        .into_rgba8();
    let height = (LOGO_TEXTURE_WIDTH * image.height() / image.width()).max(1);
    let image = resize_premultiplied(&image, LOGO_TEXTURE_WIDTH, height);
    let size = [
        usize::try_from(image.width()).unwrap_or(0),
        usize::try_from(image.height()).unwrap_or(0),
    ];
    let color_image = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    let options = TextureOptions {
        magnification: TextureFilter::Linear,
        minification: TextureFilter::Linear,
        wrap_mode: TextureWrapMode::ClampToEdge,
        mipmap_mode: Some(TextureFilter::Linear),
    };
    Ok(context.load_texture("nudge-brand-logo", color_image, options))
}

#[cfg(test)]
mod tests {
    use super::{
        ICON_UV, LOGO_PNG, WINDOW_ICON_ICO, WORDMARK_UV, resize_premultiplied, window_icon,
    };
    use image::{Rgba, RgbaImage};

    #[test]
    fn embedded_brand_artwork_is_valid() {
        let icon = window_icon().expect("the window icon must decode");
        assert_eq!((icon.width, icon.height), (128, 128));
        assert_eq!(icon.rgba.len(), 128 * 128 * 4);
        assert!(
            icon.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0)
        );

        let logo = image::load_from_memory(LOGO_PNG).expect("the logo must decode");
        let logo = logo.into_rgba8();
        assert_eq!(logo.dimensions(), (1508, 488));
        assert!(logo.pixels().any(|pixel| pixel[3] > 0));
        for uv in [ICON_UV, WORDMARK_UV] {
            assert!((0.0..=1.0).contains(&uv.min.x));
            assert!((0.0..=1.0).contains(&uv.min.y));
            assert!((0.0..=1.0).contains(&uv.max.x));
            assert!((0.0..=1.0).contains(&uv.max.y));
            assert!(uv.min.x < uv.max.x && uv.min.y < uv.max.y);
        }

        // Seven PNG-compressed frames feed rc.exe; parse the directory so a
        // truncated or regenerated ICO cannot slip through unnoticed.
        assert_eq!(&WINDOW_ICON_ICO[..4], [0, 0, 1, 0]);
        let count = usize::from(u16::from_le_bytes([WINDOW_ICON_ICO[4], WINDOW_ICON_ICO[5]]));
        assert_eq!(count, 7);
        let mut sizes = Vec::new();
        for index in 0..count {
            let entry = &WINDOW_ICON_ICO[6 + index * 16..6 + (index + 1) * 16];
            let width = if entry[0] == 0 {
                256
            } else {
                u16::from(entry[0])
            };
            let height = if entry[1] == 0 {
                256
            } else {
                u16::from(entry[1])
            };
            assert_eq!(width, height);
            // PNG-compressed frames leave the legacy planes field at 0.
            let planes = u16::from_le_bytes([entry[4], entry[5]]);
            assert!(matches!(planes, 0 | 1));
            assert_eq!(u16::from_le_bytes([entry[6], entry[7]]), 32);
            let length = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
            let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);
            let start = usize::try_from(offset).expect("ICO offset fits usize");
            let end = start + usize::try_from(length).expect("ICO length fits usize");
            assert!(end <= WINDOW_ICON_ICO.len());
            assert_eq!(&WINDOW_ICON_ICO[start..start + 8], b"\x89PNG\r\n\x1a\n");
            sizes.push(width);
        }
        assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 256]);
    }

    #[test]
    fn premultiplied_resize_keeps_colour_at_soft_edges() {
        let mut source = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
        for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            source.put_pixel(x, y, Rgba([255, 0, 0, 255]));
        }
        let resized = resize_premultiplied(&source, 2, 2);
        for pixel in resized.pixels() {
            if pixel[3] > 0 {
                assert_eq!(&pixel.0[..3], &[255, 0, 0][..]);
            }
        }
    }
}
