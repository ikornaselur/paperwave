use std::path::Path;
use std::fs;

use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, ImageBuffer, RgbImage};

use super::error::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    pub fn apply(self, image: RgbImage) -> RgbImage {
        match self {
            Rotation::Deg0 => image,
            Rotation::Deg90 => imageops::rotate90(&image),
            Rotation::Deg180 => imageops::rotate180(&image),
            Rotation::Deg270 => imageops::rotate270(&image),
        }
    }

    pub fn target_dimensions(self, width: u16, height: u16) -> (u16, u16) {
        match self {
            Rotation::Deg0 | Rotation::Deg180 => (width, height),
            Rotation::Deg90 | Rotation::Deg270 => (height, width),
        }
    }
}

pub fn clamp_aspect_resize(image: &DynamicImage, target_w: u32, target_h: u32) -> RgbImage {
    let (src_w, src_h) = image.dimensions();
    if src_w == target_w && src_h == target_h {
        return image.to_rgb8();
    }

    let src_ratio = src_w as f32 / src_h as f32;
    let target_ratio = target_w as f32 / target_h as f32;

    let crop_image: DynamicImage = if (src_ratio - target_ratio).abs() < 1e-6 {
        image.clone()
    } else if src_ratio > target_ratio {
        let desired_width = ((target_ratio * src_h as f32).round() as u32).clamp(1, src_w);
        let x = (src_w - desired_width) / 2;
        image.crop_imm(x, 0, desired_width, src_h)
    } else {
        let desired_height = ((src_w as f32 / target_ratio).round() as u32).clamp(1, src_h);
        let y = (src_h - desired_height) / 2;
        image.crop_imm(0, y, src_w, desired_height)
    };

    crop_image
        .resize_exact(target_w, target_h, FilterType::Triangle)
        .to_rgb8()
}

pub fn lighten_image_in_place(image: &mut RgbImage, lighten: f32) {
    let l = lighten.clamp(0.0, 1.0);
    if l <= 0.0 {
        return;
    }
    // Gamma curve: lower gamma (<1.0) lightens the tones.
    let gamma = 1.0 - 0.5 * l; // l=1.0 -> gamma=0.5; l=0.0 -> gamma=1.0
    for p in image.pixels_mut() {
        for c in 0..3 {
            let v = (p[c] as f32) / 255.0;
            let nv = v.powf(gamma);
            let out = (nv * 255.0).round().clamp(0.0, 255.0) as u8;
            p[c] = out;
        }
    }
}

pub fn pack_luma_nibbles(
    image: &ImageBuffer<image::Luma<u8>, Vec<u8>>,
    start: usize,
    end: usize,
) -> Vec<u8> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut packed = Vec::with_capacity(height * (end - start) / 2);

    for y in 0..height {
        let row = &image.as_raw()[y * width..(y + 1) * width];
        let slice = &row[start..end];
        for chunk in slice.chunks(2) {
            let high = chunk[0] & 0x0F;
            let low = chunk.get(1).copied().unwrap_or(0) & 0x0F;
            packed.push((high << 4) | low);
        }
    }
    packed
}

pub fn pack_buffer_nibbles(buffer: &[u8]) -> Vec<u8> {
    let mut packed = Vec::with_capacity((buffer.len() + 1) / 2);
    let mut iter = buffer.iter();
    while let Some(&high) = iter.next() {
        let low = iter.next().copied().unwrap_or(0);
        let byte = ((high & 0x0F) << 4) | (low & 0x0F);
        packed.push(byte);
    }
    packed
}

pub fn nearest_colour(palette: &[[f32; 3]], colour: [f32; 3]) -> (usize, [f32; 3]) {
    let mut best_index = 0usize;
    let mut best_distance = f32::MAX;
    for (idx, candidate) in palette.iter().enumerate() {
        let dr = colour[0] - candidate[0];
        let dg = colour[1] - candidate[1];
        let db = colour[2] - candidate[2];
        let distance = dr * dr + dg * dg + db * db;
        if distance < best_distance {
            best_distance = distance;
            best_index = idx;
        }
    }

    (best_index, palette[best_index])
}

pub fn distribute_error(
    working: &mut [[f32; 3]],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    error: [f32; 3],
) {
    let apply = |working: &mut [[f32; 3]], nx: isize, ny: isize, factor: f32| {
        if nx < 0 || ny < 0 {
            return;
        }
        let nx = nx as usize;
        let ny = ny as usize;
        if nx >= width || ny >= height {
            return;
        }
        let idx = ny * width + nx;
        for channel in 0..3 {
            let value = working[idx][channel] + error[channel] * factor;
            working[idx][channel] = value.clamp(0.0, 255.0);
        }
    };

    apply(working, (x as isize) + 1, y as isize, 7.0 / 16.0);
    apply(working, (x as isize) - 1, (y as isize) + 1, 3.0 / 16.0);
    apply(working, x as isize, (y as isize) + 1, 5.0 / 16.0);
    apply(working, (x as isize) + 1, (y as isize) + 1, 1.0 / 16.0);
}

pub trait InkyDisplay {
    fn width(&self) -> u16;
    fn height(&self) -> u16;
    fn set_rotation(&mut self, rotation: Rotation);
    fn input_dimensions(&self) -> (u16, u16);
    fn clear(&mut self, colour: u8);
    fn set_pixel(&mut self, x: usize, y: usize, colour: u8);
    fn set_image_from_path(&mut self, path: &Path, saturation: f32, lighten: f32) -> Result<()>;
    fn set_image(&mut self, image: &DynamicImage, saturation: f32, lighten: f32) -> Result<()>;
    fn show(&mut self) -> Result<()>;
}

// ---- EXIF helpers ----

pub fn load_image_respecting_exif(path: &Path) -> Result<DynamicImage> {
    let bytes = fs::read(path)?;
    let img = image::load_from_memory(&bytes)?;
    Ok(apply_exif_orientation_bytes(&bytes, img))
}

pub fn apply_exif_orientation_bytes(bytes: &[u8], img: DynamicImage) -> DynamicImage {
    match exif_orientation_from_jpeg(bytes) {
        Some(90) => DynamicImage::ImageRgba8(imageops::rotate90(&img)),
        Some(180) => DynamicImage::ImageRgba8(imageops::rotate180(&img)),
        Some(270) => DynamicImage::ImageRgba8(imageops::rotate270(&img)),
        _ => img,
    }
}

fn exif_orientation_from_jpeg(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 { return None; }
    let mut i = 2usize;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF { i += 1; continue; }
        let marker = bytes[i+1];
        i += 2;
        if marker == 0xD9 || marker == 0xDA { break; }
        if i + 2 > bytes.len() { break; }
        let seg_len = u16::from_be_bytes([bytes[i], bytes[i+1]]) as usize;
        i += 2;
        if seg_len < 2 || i + seg_len - 2 > bytes.len() { break; }
        if marker == 0xE1 {
            let data = &bytes[i..i + seg_len - 2];
            if data.len() >= 6 && &data[0..6] == b"Exif\0\0" {
                return parse_tiff_orientation(&data[6..]);
            }
        }
        i += seg_len - 2;
    }
    None
}

fn parse_tiff_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 8 { return None; }
    let be = if &tiff[0..2] == b"MM" { true } else if &tiff[0..2] == b"II" { false } else { return None; };
    let u16_at = |off: usize, be: bool| -> Option<u16> {
        if off + 2 > tiff.len() { return None; }
        Some(if be { u16::from_be_bytes([tiff[off], tiff[off+1]]) } else { u16::from_le_bytes([tiff[off], tiff[off+1]]) })
    };
    let u32_at = |off: usize, be: bool| -> Option<u32> {
        if off + 4 > tiff.len() { return None; }
        Some(if be { u32::from_be_bytes([tiff[off], tiff[off+1], tiff[off+2], tiff[off+3]]) } else { u32::from_le_bytes([tiff[off], tiff[off+1], tiff[off+2], tiff[off+3]]) })
    };
    if u16_at(2, be)? != 0x002A { return None; }
    let ifd0_off = u32_at(4, be)? as usize;
    if ifd0_off + 2 > tiff.len() { return None; }
    let count = u16_at(ifd0_off, be)? as usize;
    let mut p = ifd0_off + 2;
    for _ in 0..count {
        if p + 12 > tiff.len() { return None; }
        let tag = u16_at(p, be)?;
        let typ = u16_at(p+2, be)?;
        let cnt = u32_at(p+4, be)?;
        let val_off = u32_at(p+8, be)? as usize;
        if tag == 0x0112 {
            if typ == 3 && cnt == 1 {
                let value = if be { (val_off >> 16) as u16 } else { (val_off & 0xFFFF) as u16 };
                return match value { 3=>Some(180), 6=>Some(90), 8=>Some(270), _=>None };
            } else if typ == 3 && cnt >= 1 {
                let off = val_off;
                if off + 2 <= tiff.len() {
                    let v = if be { u16::from_be_bytes([tiff[off], tiff[off+1]]) } else { u16::from_le_bytes([tiff[off], tiff[off+1]]) };
                    return match v { 3=>Some(180), 6=>Some(90), 8=>Some(270), _=>None };
                }
            }
            return None;
        }
        p += 12;
    }
    None
}
