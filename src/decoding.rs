use crate::{
    budget::{ByteBudget, ByteLease},
    models::ImageRecord,
    performance::{self, MIB},
};
use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType, metadata::Orientation};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    sync::Arc,
    time::Instant,
};

#[derive(Clone)]
pub struct DecodedImage {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub source_width: u32,
    pub source_height: u32,
}
pub struct DecodeOutput {
    pub image: DecodedImage,
    pub _lease: ByteLease,
}

#[derive(Debug)]
pub struct ResourceLimit;
impl std::fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("图片处理所需内存超出预算")
    }
}
impl std::error::Error for ResourceLimit {}
#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("任务已取消")
    }
}
impl std::error::Error for Cancelled {}

fn reserve(
    budget: &Arc<ByteBudget>,
    bytes: usize,
    preview: bool,
    cancel: &impl Fn() -> bool,
) -> Result<ByteLease> {
    if bytes > budget.limit(preview) {
        return Err(ResourceLimit.into());
    }
    budget
        .acquire(bytes, preview, cancel)
        .ok_or_else(|| Cancelled.into())
}
pub fn decode(
    record: &ImageRecord,
    max_side: u32,
    preview: bool,
    budget: &Arc<ByteBudget>,
    cancel: impl Fn() -> bool,
) -> Result<DecodeOutput> {
    let started = Instant::now();
    if cancel() {
        return Err(Cancelled.into());
    }
    let meta = std::fs::metadata(&record.path)?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64;
    if meta.len() != record.size || modified != record.modified_ns {
        return Err(Cancelled.into());
    }
    // Read only the JPEG header before admission, not the full compressed input.
    let mut file = BufReader::new(File::open(&record.path)?);
    let header = jpeg_header(&mut file).ok().flatten();
    if let Some((width, height, orientation, progressive)) = header {
        let factor = turbojpeg::Decompressor::supported_scaling_factors()
            .into_iter()
            .filter(|f| {
                f.num() <= f.denom()
                    && f.scale(width.max(height) as usize)
                        >= (max_side.min(width.max(height))) as usize
            })
            .min_by_key(|f| f.scale(width.max(height) as usize))
            .unwrap_or(turbojpeg::ScalingFactor::ONE);
        let sw = factor.scale(width as usize);
        let sh = factor.scale(height as usize);
        let estimate = usize::try_from(record.size)
            .unwrap_or(usize::MAX)
            .saturating_add(sw.saturating_mul(sh).saturating_mul(20))
            .saturating_add(if progressive {
                width as usize * height as usize * 6
            } else {
                8 * MIB
            })
            .saturating_add(4 * max_side as usize * max_side as usize);
        let lease = reserve(budget, estimate, preview, &cancel)?;
        file.seek(SeekFrom::Start(0))?;
        let mut input = Vec::with_capacity(record.size as usize);
        // A growing source may not bypass the admission estimate.
        file.take(record.size.saturating_add(1))
            .read_to_end(&mut input)?;
        if input.len() as u64 != record.size {
            return Err(Cancelled.into());
        }
        if cancel() {
            return Err(Cancelled.into());
        }
        let fast = (|| -> Result<DynamicImage> {
            let mut decoder = turbojpeg::Decompressor::new()?;
            decoder.set_scan_limit(100)?;
            let header = decoder.read_header(&input)?;
            if header.width != width as usize || header.height != height as usize {
                bail!("JPEG 尺寸已改变");
            }
            decoder.set_scaling_factor(factor)?;
            let mut pixels = vec![0; sw * sh * 4];
            decoder.decompress(
                &input,
                turbojpeg::Image {
                    pixels: &mut pixels[..],
                    width: sw,
                    pitch: sw * 4,
                    height: sh,
                    format: turbojpeg::PixelFormat::RGBA,
                },
            )?;
            Ok(DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(sw as u32, sh as u32, pixels)
                    .context("JPEG 缓冲尺寸不匹配")?,
            ))
        })();
        if let Ok(mut decoded) = fast {
            decoded.apply_orientation(orientation);
            let (source_width, source_height) = oriented_dimensions(width, height, orientation);
            let image = finish(decoded, max_side, source_width, source_height);
            if cancel() || !same_version(record) {
                return Err(Cancelled.into());
            }
            performance::elapsed("jpeg_scaled_decode_ms", started);
            performance::sample("decode_admitted_bytes", estimate as f64);
            return Ok(DecodeOutput {
                image,
                _lease: lease,
            });
        }
        drop(input);
        drop(lease);
        performance::sample("jpeg_fallback", 1.0);
    }
    if cancel() {
        return Err(Cancelled.into());
    }
    // Some codecs retain compressed input while inspecting headers. Admit that
    // separately, then release the decoder before waiting for the larger lease.
    let header_bytes = usize::try_from(record.size)
        .unwrap_or(usize::MAX)
        .saturating_add(8 * MIB);
    let header_lease = reserve(budget, header_bytes, preview, &cancel)?;
    let decoder = ImageReader::open(&record.path)?
        .with_guessed_format()?
        .into_decoder()?;
    let (width, height) = decoder.dimensions();
    let raw_bytes = usize::try_from(decoder.total_bytes()).unwrap_or(usize::MAX);
    drop(decoder);
    drop(header_lease);
    // Area downsampling avoids a source-width × target-height float buffer.
    // Reserve simultaneous decoder/source/rotation and conversion buffers.
    let longest = width.max(height).max(1) as usize;
    let target = (max_side as usize).min(longest);
    let output_pixels = (width as usize * target / longest)
        .max(1)
        .saturating_mul((height as usize * target / longest).max(1));
    let pixel_bytes = raw_bytes
        .checked_div((width as usize).saturating_mul(height as usize))
        .unwrap_or(16)
        .max(4);
    let estimate = raw_bytes
        .saturating_mul(2)
        .max(raw_bytes.saturating_add(output_pixels.saturating_mul(pixel_bytes * 2)))
        .max(output_pixels.saturating_mul(12))
        .saturating_add(header_bytes);
    let lease = reserve(budget, estimate, preview, &cancel)?;
    let mut reader = ImageReader::open(&record.path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(budget.limit(preview) as u64);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut decoded = DynamicImage::from_decoder(decoder)?;
    if cancel() {
        return Err(Cancelled.into());
    }
    decoded.apply_orientation(orientation);
    let source_width = decoded.width();
    let source_height = decoded.height();
    let decoded = if decoded.width().max(decoded.height()) > max_side {
        decoded.thumbnail(max_side, max_side)
    } else {
        decoded
    };
    let image = finish(decoded, max_side, source_width, source_height);
    if cancel() || !same_version(record) {
        return Err(Cancelled.into());
    }
    performance::elapsed("generic_decode_ms", started);
    performance::sample("decode_admitted_bytes", estimate as f64);
    Ok(DecodeOutput {
        image,
        _lease: lease,
    })
}
fn same_version(record: &ImageRecord) -> bool {
    std::fs::metadata(&record.path).ok().is_some_and(|m| {
        m.len() == record.size
            && m.modified().ok().is_some_and(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .min(i64::MAX as u128) as i64
                    == record.modified_ns
            })
    })
}
fn finish(
    image: DynamicImage,
    max_side: u32,
    source_width: u32,
    source_height: u32,
) -> DecodedImage {
    let resized = if image.width().max(image.height()) > max_side {
        image
            .resize(max_side, max_side, FilterType::Triangle)
            .into_rgba8()
    } else {
        image.into_rgba8()
    };
    DecodedImage {
        width: resized.width() as usize,
        height: resized.height() as usize,
        pixels: resized.into_raw(),
        source_width,
        source_height,
    }
}
pub fn oriented_dimensions(w: u32, h: u32, o: Orientation) -> (u32, u32) {
    match o {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (h, w),
        _ => (w, h),
    }
}
/// Bounded JPEG marker parser. EXIF is metadata only; entropy data is left unread.
fn jpeg_header(reader: &mut (impl Read + Seek)) -> Result<Option<(u32, u32, Orientation, bool)>> {
    let mut magic = [0; 2];
    reader.read_exact(&mut magic)?;
    if magic != [0xff, 0xd8] {
        return Ok(None);
    }
    let mut orientation = Orientation::NoTransforms;
    let mut dims = None;
    for _ in 0..2048 {
        let mut b = [0];
        reader.read_exact(&mut b)?;
        if b[0] != 0xff {
            return Ok(None);
        }
        while b[0] == 0xff {
            reader.read_exact(&mut b)?;
        }
        let marker = b[0];
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        reader.read_exact(&mut magic)?;
        let len = u16::from_be_bytes(magic) as usize;
        if len < 2 {
            return Ok(None);
        }
        if marker == 0xe1 || matches!(marker, 0xc0 | 0xc1 | 0xc2) {
            let mut bytes = vec![0; len - 2];
            reader.read_exact(&mut bytes)?;
            if marker == 0xe1 && bytes.starts_with(b"Exif\0\0") {
                orientation = Orientation::from_exif_chunk(&bytes[6..]).unwrap_or(orientation);
            }
            if matches!(marker, 0xc0 | 0xc1 | 0xc2) && bytes.len() >= 5 && bytes[0] == 8 {
                dims = Some((
                    u16::from_be_bytes([bytes[3], bytes[4]]) as u32,
                    u16::from_be_bytes([bytes[1], bytes[2]]) as u32,
                    marker == 0xc2,
                ));
            }
        } else {
            reader.seek(SeekFrom::Current((len - 2) as i64))?;
        }
        if reader.stream_position()? > 4 * MIB as u64 {
            return Ok(None);
        }
    }
    Ok(dims
        .filter(|(w, h, _)| *w > 0 && *h > 0)
        .map(|(w, h, p)| (w, h, orientation, p)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_does_not_upscale_and_preserves_alpha() {
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([1, 2, 3, 4]));
        let decoded = finish(DynamicImage::ImageRgba8(img), 256, 2, 1);
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.pixels[3], 4);
    }
    #[test]
    fn every_exif_orientation_has_correct_dimensions() {
        for v in 1..=8 {
            let o = Orientation::from_exif(v).unwrap();
            let expected = if v >= 5 { (2, 3) } else { (3, 2) };
            assert_eq!(oriented_dimensions(3, 2, o), expected);
        }
    }
    #[test]
    fn malformed_jpeg_header_is_bounded() {
        assert!(
            jpeg_header(&mut std::io::Cursor::new([0xff, 0xd8, 0xff, 0xe1, 0, 1]))
                .unwrap()
                .is_none()
        );
    }
}
