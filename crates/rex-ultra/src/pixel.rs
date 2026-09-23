//! Deterministic pixel gate.
//!
//! Visual claims are checked by the daemon decoding the actual screenshot
//! bytes out of the content-addressed artifact store, never by trusting a
//! host's description of its own render. The artifact hash IS the binding:
//! the store re-hashes bytes on read, so metrics computed here belong to
//! exactly the digest the host declared.
//!
//! All math is integer-only so results are identical on every platform.

use serde::{Deserialize, Serialize};

/// Minimum edge length for a screenshot to count as a real render.
pub const MIN_DIMENSION: u32 = 64;
/// A qualifying render must use at least this many distinct colors;
/// near-blank images fail the floor.
pub const MIN_DISTINCT_COLORS: u32 = 16;
/// Minimum Hamming distance (of 64) between two candidates' 8x8 average
/// hashes for their renders to count as machine-distinct theses.
pub const MIN_AHASH_DISTANCE: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelMetrics {
    /// sha256 of the decoded bytes; equals the artifact store address.
    pub artifact_hash: String,
    pub width: u32,
    pub height: u32,
    /// Distinct RGB colors, counted up to a cap of 1_000_000.
    pub distinct_colors: u32,
    /// Mean luma (0-255), integer average over all pixels.
    pub mean_luma: u32,
    /// Population variance of luma, integer. Zero means a flat image.
    pub luma_variance: u64,
    /// 8x8 average hash for cross-candidate distinctness.
    pub ahash: u64,
}

impl PixelMetrics {
    /// The deterministic pixel floor: decodable, large enough, and not a
    /// flat or near-blank render.
    pub fn passes_floor(&self) -> Result<(), String> {
        if self.width < MIN_DIMENSION || self.height < MIN_DIMENSION {
            return Err(format!(
                "render is {}x{}, below the {}px floor",
                self.width, self.height, MIN_DIMENSION
            ));
        }
        if self.distinct_colors < MIN_DISTINCT_COLORS {
            return Err(format!(
                "render uses {} distinct colors, below the {} floor",
                self.distinct_colors, MIN_DISTINCT_COLORS
            ));
        }
        if self.luma_variance == 0 {
            return Err("render is a flat single-luma image".into());
        }
        Ok(())
    }
}

/// Decode PNG bytes and compute deterministic metrics. Any decode failure
/// is an error, never a silent pass.
pub fn decode_metrics(bytes: &[u8], artifact_hash: &str) -> Result<PixelMetrics, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png header: {e}"))?;
    let (width, height, color_type, bit_depth) = {
        let info = reader.info();
        (info.width, info.height, info.color_type, info.bit_depth)
    };
    if width == 0 || height == 0 {
        return Err("png has zero dimensions".into());
    }
    // Bound decompression work: 16k x 16k RGBA is the hard cap.
    if width > 16_384 || height > 16_384 {
        return Err(format!(
            "png dimensions {width}x{height} exceed the decode cap"
        ));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "png buffer size unknown".to_string())?;
    let mut buf = vec![0u8; size];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png decode: {e}"))?;
    let raw = &buf[..out.buffer_size()];
    let rgb = to_rgb(color_type, bit_depth, raw)?;
    Ok(metrics_from_rgb(&rgb, width, height, artifact_hash))
}

/// Expand any supported PNG pixel format to packed RGB8.
fn to_rgb(color: png::ColorType, depth: png::BitDepth, raw: &[u8]) -> Result<Vec<u8>, String> {
    use png::{BitDepth, ColorType};
    if depth != BitDepth::Eight {
        return Err(format!("unsupported png bit depth {depth:?}"));
    }
    let channels = match color {
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Indexed => {
            return Err("indexed png is rejected; encode truecolor screenshots".into())
        }
    };
    if raw.len() % channels != 0 {
        return Err("png payload is not a whole number of pixels".into());
    }
    let mut rgb = Vec::with_capacity(raw.len() / channels * 3);
    match color {
        ColorType::Rgb => rgb.extend_from_slice(raw),
        ColorType::Rgba => {
            for px in raw.chunks_exact(4) {
                rgb.extend_from_slice(&px[..3]);
            }
        }
        ColorType::Grayscale => {
            for &g in raw {
                rgb.extend_from_slice(&[g, g, g]);
            }
        }
        ColorType::GrayscaleAlpha => {
            for px in raw.chunks_exact(2) {
                rgb.extend_from_slice(&[px[0], px[0], px[0]]);
            }
        }
        ColorType::Indexed => unreachable!(),
    }
    Ok(rgb)
}

fn metrics_from_rgb(rgb: &[u8], width: u32, height: u32, artifact_hash: &str) -> PixelMetrics {
    use std::collections::HashSet;
    let pixels = rgb.len() / 3;
    let mut colors: HashSet<u32> = HashSet::new();
    let mut luma_sum: u64 = 0;
    let mut luma_sq_sum: u128 = 0;
    // 8x8 block sums for the average hash.
    let mut block_sum = [0u64; 64];
    let mut block_count = [0u64; 64];
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
        if colors.len() < 1_000_000 {
            colors.insert((r << 16) | (g << 8) | b);
        }
        // Rec. 601 integer luma.
        let luma = (77 * r + 150 * g + 29 * b) >> 8;
        luma_sum += luma as u64;
        luma_sq_sum += (luma as u128) * (luma as u128);
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        let bx = (x as u64 * 8 / width as u64) as usize;
        let by = (y as u64 * 8 / height as u64) as usize;
        let block = by * 8 + bx;
        block_sum[block] += luma as u64;
        block_count[block] += 1;
    }
    let n = pixels as u64;
    let mean_luma = if n > 0 { (luma_sum / n) as u32 } else { 0 };
    // population variance = E[x^2] - E[x]^2, kept in integers
    let luma_variance = if n > 0 {
        let n128 = n as u128;
        let ex2 = luma_sq_sum / n128;
        let ex_sq = (luma_sum as u128) * (luma_sum as u128) / (n128 * n128);
        (ex2.saturating_sub(ex_sq)) as u64
    } else {
        0
    };
    let mut block_means = [0u64; 64];
    let mut total: u64 = 0;
    for b in 0..64 {
        block_means[b] = if block_count[b] > 0 {
            block_sum[b] / block_count[b]
        } else {
            0
        };
        total += block_means[b];
    }
    let overall = total / 64;
    let mut ahash: u64 = 0;
    for (b, m) in block_means.iter().enumerate() {
        if m > &overall {
            ahash |= 1 << b;
        }
    }
    PixelMetrics {
        artifact_hash: artifact_hash.to_string(),
        width,
        height,
        distinct_colors: colors.len() as u32,
        mean_luma,
        luma_variance,
        ahash,
    }
}

/// Hamming distance between two 64-bit average hashes.
pub fn ahash_distance(left: u64, right: u64) -> u32 {
    (left ^ right).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real PNG in-memory with the given pixel function.
    fn make_png(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&f(x, y));
            }
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgb).unwrap();
        }
        out
    }

    #[test]
    fn a_real_render_passes_the_floor() {
        let png = make_png(128, 128, |x, y| {
            [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]
        });
        let m = decode_metrics(&png, "hash").unwrap();
        assert_eq!((m.width, m.height), (128, 128));
        assert!(m.distinct_colors >= MIN_DISTINCT_COLORS);
        assert!(m.luma_variance > 0);
        m.passes_floor().unwrap();
    }

    #[test]
    fn a_flat_render_fails_the_floor() {
        let png = make_png(128, 128, |_, _| [200, 200, 200]);
        let m = decode_metrics(&png, "hash").unwrap();
        assert_eq!(m.distinct_colors, 1);
        assert_eq!(m.luma_variance, 0);
        assert!(m.passes_floor().is_err());
    }

    #[test]
    fn a_tiny_render_fails_the_floor() {
        let png = make_png(16, 16, |x, y| [(x * 16) as u8, (y * 16) as u8, 0]);
        let m = decode_metrics(&png, "hash").unwrap();
        assert!(m.passes_floor().is_err());
    }

    #[test]
    fn garbage_bytes_are_an_error_not_a_pass() {
        assert!(decode_metrics(b"not a png at all", "hash").is_err());
        assert!(decode_metrics(&[], "hash").is_err());
    }

    #[test]
    fn identical_renders_have_zero_distance_and_distinct_renders_do_not() {
        let a = decode_metrics(&make_png(128, 128, |x, _| [x as u8; 3]), "a").unwrap();
        let b = decode_metrics(&make_png(128, 128, |x, _| [x as u8; 3]), "b").unwrap();
        let c = decode_metrics(&make_png(128, 128, |_, y| [y as u8; 3]), "c").unwrap();
        assert_eq!(ahash_distance(a.ahash, b.ahash), 0);
        assert!(ahash_distance(a.ahash, c.ahash) >= MIN_AHASH_DISTANCE);
    }

    #[test]
    fn metrics_are_deterministic() {
        let png = make_png(96, 80, |x, y| [(x ^ y) as u8, (x * 3) as u8, (y * 5) as u8]);
        let first = decode_metrics(&png, "h").unwrap();
        let second = decode_metrics(&png, "h").unwrap();
        assert_eq!(first, second);
    }
}
