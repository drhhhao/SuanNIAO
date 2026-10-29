//! 电脑上复制的图片转成发给手机的文件：剪贴板里现成的 PNG 不太大就原样发；位图（DIB）先编成 PNG；
//! 太大（超过上限）改成 JPEG，还太大就缩小，直到放得下。蓝牙传得慢，图片越小手机上越快能粘贴。

use std::io::Cursor;

use image::codecs::bmp::BmpDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage};

use crate::clipboard::Shared;

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// 编成不超过 max 字节的 PNG 或 JPEG
pub fn encode(shared: Shared, max: usize) -> Result<Encoded, String> {
    let image = match shared {
        Shared::Text(_) => return Err("不是图片".into()),
        Shared::Png(bytes) => {
            let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png).map_err(|e| format!("PNG 读不出来：{e}"))?;
            if bytes.len() <= max {
                let (width, height) = image.dimensions();
                return Ok(Encoded { bytes, mime: "image/png", width, height });
            }
            image
        }
        Shared::Dib(bytes) => decode_dib(&bytes)?,
    };
    fit(image, max)
}

/// 剪贴板的位图没有文件头，按 BMP 的信息头往下读。32 位的透明度常常全是 0（不用透明度的程序就这么放），当成不透明
fn decode_dib(bytes: &[u8]) -> Result<DynamicImage, String> {
    let decoder = BmpDecoder::new_without_file_header(Cursor::new(bytes)).map_err(|e| format!("位图读不出来：{e}"))?;
    let image = DynamicImage::from_decoder(decoder).map_err(|e| format!("位图读不出来：{e}"))?;
    Ok(match image {
        DynamicImage::ImageRgba8(mut rgba) => {
            if rgba.pixels().all(|p| p[3] == 0) {
                rgba.pixels_mut().for_each(|p| p[3] = 255);
            }
            if rgba.pixels().all(|p| p[3] == 255) {
                DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).to_rgb8())
            } else {
                DynamicImage::ImageRgba8(rgba)
            }
        }
        other => other,
    })
}

fn fit(image: DynamicImage, max: usize) -> Result<Encoded, String> {
    let (width, height) = image.dimensions();
    let mut png = Vec::new();
    image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).map_err(|e| format!("编不成 PNG：{e}"))?;
    if png.len() <= max {
        return Ok(Encoded { bytes: png, mime: "image/png", width, height });
    }
    // 照片这类：JPEG 小得多。透明的地方垫白底
    let mut rgb = flatten(&image);
    for _ in 0..6 {
        let jpeg = jpeg(&rgb)?;
        if jpeg.len() <= max {
            return Ok(Encoded { bytes: jpeg, mime: "image/jpeg", width: rgb.width(), height: rgb.height() });
        }
        // 按面积缩：大小大致和像素数成正比，多缩一点免得再来一轮
        let scale = ((max as f64 / jpeg.len() as f64).sqrt() * 0.9).min(0.9);
        let (w, h) = (((rgb.width() as f64 * scale) as u32).max(1), ((rgb.height() as f64 * scale) as u32).max(1));
        rgb = image::imageops::resize(&rgb, w, h, FilterType::Triangle);
    }
    Err("图片太大，缩小了也放不下".into())
}

fn flatten(image: &DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y);
        let a = p[3] as u32;
        let mix = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

fn jpeg(rgb: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85).encode_image(rgb).map_err(|e| format!("编不成 JPEG：{e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 32 位 BI_RGB 的剪贴板位图（自下而上），透明度字节都是 0
    fn dib(width: u32, height: u32, mut pixel: impl FnMut(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        out.extend_from_slice(&(height as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 24]); // BI_RGB、大小、分辨率、调色板都填 0
        for y in (0..height).rev() {
            for x in 0..width {
                let [r, g, b] = pixel(x, y);
                out.extend_from_slice(&[b, g, r, 0]);
            }
        }
        out
    }

    #[test]
    fn dib_with_zero_alpha_is_opaque_png() {
        let bytes = dib(3, 2, |x, y| if (x, y) == (0, 0) { [255, 0, 0] } else { [0, 0, 255] });
        let encoded = encode(Shared::Dib(bytes), 1 << 20).unwrap();
        assert_eq!((encoded.mime, encoded.width, encoded.height), ("image/png", 3, 2));
        let back = image::load_from_memory(&encoded.bytes).unwrap();
        assert!(!back.color().has_alpha());
        assert_eq!(back.to_rgb8().get_pixel(0, 0).0, [255, 0, 0]); // 左上角：自下而上的行序对了
        assert_eq!(back.to_rgb8().get_pixel(2, 1).0, [0, 0, 255]);
    }

    #[test]
    fn big_noisy_image_becomes_small_jpeg() {
        let mut seed = 1u32;
        let bytes = dib(1600, 1200, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let v = seed.to_le_bytes();
            [v[1], v[2], v[3]]
        });
        let max = 300 << 10;
        let encoded = encode(Shared::Dib(bytes), max).unwrap();
        assert_eq!(encoded.mime, "image/jpeg");
        assert!(encoded.bytes.len() <= max);
        assert!(encoded.width < 1600, "噪点图压成 JPEG 也放不下，应该缩小了");
    }
}

