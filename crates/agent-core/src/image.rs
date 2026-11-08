//! Image pipeline (spec §7): original PNG -> downscaled JPEG + base64,
//! sized to what providers actually tokenize.

use base64::Engine as _;
use image::ImageEncoder as _;

use crate::capture::CapturedImage;
use crate::config::ImageSection;
use crate::context::PreparedImage;
use crate::error::{Error, Result};

pub fn prepare(cap: &CapturedImage, cfg: &ImageSection) -> Result<PreparedImage> {
    let dynimg = image::load_from_memory(&cap.png)
        .map_err(|e| Error::Image(format!("decode: {e}")))?;

    let long_edge = dynimg.width().max(dynimg.height());
    let processed = if long_edge > cfg.max_edge {
        let scale = cfg.max_edge as f64 / long_edge as f64;
        let (w, h) = ((dynimg.width() as f64 * scale).round() as u32, (dynimg.height() as f64 * scale).round() as u32);
        dynimg.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
    } else {
        dynimg
    };

    let has_alpha = processed.color().has_alpha();
    let mut out = std::io::Cursor::new(Vec::new());
    let mime = if has_alpha {
        processed
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| Error::Image(format!("png encode: {e}")))?;
        "image/png"
    } else {
        let rgb = processed.to_rgb8();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut out,
            cfg.jpeg_quality.min(100) as u8,
        );
        enc.write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
            .map_err(|e| Error::Image(format!("jpeg encode: {e}")))?;
        "image/jpeg"
    };
    let bytes = out.into_inner();
    if bytes.len() > cfg.max_bytes {
        return Err(Error::Image(format!(
            "processed image {} bytes exceeds cap {}",
            bytes.len(),
            cfg.max_bytes
        )));
    }

    let (w, h) = (processed.width(), processed.height());
    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(PreparedImage { mime: mime.into(), base64, width: w, height: h, bytes: bytes.len() })
}

/// Save the *original* capture content-addressed under <data>/images/.
pub fn store_original(data_dir: &std::path::Path, png: &[u8]) -> Result<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    png.hash(&mut h);
    let dir = data_dir.join("images");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{:016x}.png", h.finish()));
    if !path.exists() {
        // atomic-ish: write temp then rename
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, png)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    fn synthetic_png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let enc = image::codecs::png::PngEncoder::new(&mut buf);
        if alpha {
            let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 30, 30, 255]));
            enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8).unwrap();
        } else {
            let img = image::RgbImage::from_pixel(w, h, image::Rgb([10, 120, 220]));
            enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8).unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn downscales_to_max_edge() {
        let png = synthetic_png(3200, 1800, false);
        let cap = CapturedImage { png, width: 3200, height: 1800 };
        let out = prepare(&cap, &ImageSection::default()).unwrap();
        assert!(out.width.max(out.height) <= 1568);
        assert_eq!(out.width * 9, out.height * 16); // aspect kept (approx: 3200x1800 -> 1568x882)
        assert_eq!(out.mime, "image/jpeg");
    }

    #[test]
    fn small_images_pass_through_as_jpeg() {
        let png = synthetic_png(800, 600, false);
        let cap = CapturedImage { png, width: 800, height: 600 };
        let out = prepare(&cap, &ImageSection::default()).unwrap();
        assert_eq!((out.width, out.height), (800, 600));
        assert_eq!(out.mime, "image/jpeg");
    }

    #[test]
    fn alpha_keeps_png() {
        let png = synthetic_png(400, 300, true);
        let cap = CapturedImage { png, width: 400, height: 300 };
        let out = prepare(&cap, &ImageSection::default()).unwrap();
        assert_eq!(out.mime, "image/png");
    }

    #[test]
    fn original_store_is_content_addressed() {
        let dir = std::env::temp_dir().join(format!("aisle-img-{}", std::process::id()));
        let png = synthetic_png(10, 10, false);
        let a = store_original(&dir, &png).unwrap();
        let b = store_original(&dir, &png).unwrap();
        assert_eq!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), png);
        std::fs::remove_dir_all(dir).ok();
    }
}
