//! Overlay images (SPEC §5.9 [M2]; SR R-MEDIA-SEC-3): untrusted PNG and JPEG decoded once,
//! through pure-Rust decoders only (`png`, `zune-jpeg`; never ImageIO), within limits —
//! 8192 px a side, 256 MB of decoded pixels — and re-encoded as an RGBA PNG ([`encode_png`]),
//! which is what the app stores, the renderer reads and the webview may see as a thumbnail
//! (never the user's original bytes). WebP and GIF are refused for now (named, so the person
//! knows to convert); DNG, HEIC and TIFF are refused by the SR. No gmclip code decodes or
//! encodes the pixels itself (SPEC §15.0): both directions are the `png` crate's.
//!
//! [`fit`] resamples to an overlay's place on the canvas with exact area weights in
//! integer arithmetic, so a frame draws the same bytes on every machine.

/// Largest side of a decoded image.
pub const MAX_SIDE: u32 = 8192;
/// Most pixels of a decoded image (256 MB of RGBA8).
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;
/// Largest input file.
pub const MAX_BYTES: usize = 64 << 20;

/// Straight (not premultiplied) RGBA8, row-major.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba8 {
    pub width: u32,
    pub height: u32,
    pub px: Vec<u8>,
}

impl std::fmt::Debug for Rgba8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba8({}x{})", self.width, self.height)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0} images are not supported for overlays (save it as PNG or JPEG)")]
    Unsupported(&'static str),
    #[error("the image is too large ({w}×{h}; at most 8192 a side)")]
    TooLarge { w: u32, h: u32 },
}

/// The kind of an image file, by its magic bytes.
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        Some("tiff")
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" {
        Some("heic")
    } else {
        None
    }
}

fn check_size(w: u32, h: u32) -> Result<(), ImageError> {
    if w == 0 || h == 0 {
        return Err(ImageError::Invalid("an empty image".into()));
    }
    if w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(ImageError::TooLarge { w, h });
    }
    Ok(())
}

fn decode_png(b: &[u8]) -> Result<Rgba8, ImageError> {
    let bad = |e: png::DecodingError| ImageError::Invalid(format!("png: {e}"));
    let mut d = png::Decoder::new_with_limits(
        std::io::Cursor::new(b),
        png::Limits {
            bytes: (MAX_PIXELS * 4) as usize,
        },
    );
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().map_err(bad)?;
    let (w, h) = (r.info().width, r.info().height);
    check_size(w, h)?;
    let size = r
        .output_buffer_size()
        .ok_or_else(|| ImageError::Invalid("png: too large".into()))?;
    let mut buf = vec![0; size];
    let info = r.next_frame(&mut buf).map_err(bad)?;
    buf.truncate(info.buffer_size());
    let n = w as usize * h as usize;
    let px = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => {
            return Err(ImageError::Invalid("png: palette not expanded".into()));
        }
    };
    if px.len() != n * 4 {
        return Err(ImageError::Invalid("png: short image data".into()));
    }
    Ok(Rgba8 {
        width: w,
        height: h,
        px,
    })
}

fn decode_jpeg(b: &[u8]) -> Result<Rgba8, ImageError> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_SIDE as usize)
        .set_max_height(MAX_SIDE as usize)
        .set_strict_mode(false);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(b), opts);
    d.decode_headers()
        .map_err(|e| ImageError::Invalid(format!("jpeg: {e:?}")))?;
    let (w, h) = d
        .dimensions()
        .ok_or_else(|| ImageError::Invalid("jpeg: no size".into()))?;
    check_size(w as u32, h as u32)?;
    let px = d
        .decode()
        .map_err(|e| ImageError::Invalid(format!("jpeg: {e:?}")))?;
    if px.len() != w * h * 4 {
        return Err(ImageError::Invalid("jpeg: short image data".into()));
    }
    Ok(Rgba8 {
        width: w as u32,
        height: h as u32,
        px,
    })
}

/// Encodes straight RGBA8 as a PNG (what gmclip stores for an overlay image).
pub fn encode_png(img: &Rgba8) -> Result<Vec<u8>, ImageError> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, img.width, img.height);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e
            .write_header()
            .map_err(|e| ImageError::Invalid(format!("png: {e}")))?;
        w.write_image_data(&img.px)
            .map_err(|e| ImageError::Invalid(format!("png: {e}")))?;
    }
    Ok(out)
}

/// Decodes an untrusted image file (PNG, JPEG).
pub fn decode_untrusted(b: &[u8]) -> Result<Rgba8, ImageError> {
    if b.len() > MAX_BYTES {
        return Err(ImageError::Invalid("the file is over 64 MB".into()));
    }
    match sniff(b) {
        Some("png") => decode_png(b),
        Some("jpeg") => decode_jpeg(b),
        Some("webp") => Err(ImageError::Unsupported("WebP")),
        Some("gif") => Err(ImageError::Unsupported("GIF")),
        Some("tiff") => Err(ImageError::Unsupported("TIFF")),
        Some("heic") => Err(ImageError::Unsupported("HEIC")),
        _ => Err(ImageError::Invalid("not an image gmclip reads".into())),
    }
}

/// `img` resampled to `w × h` by exact area averaging of premultiplied colour, returned
/// premultiplied (the compositor's tile format). Integer weights in 1/`img.width·w` units,
/// so the bytes are the same on every machine. Upscaling repeats source pixels
/// (nearest) and downscaling averages them.
pub fn fit(img: &Rgba8, w: u32, h: u32) -> Vec<u8> {
    let (sw, sh) = (u64::from(img.width), u64::from(img.height));
    let (dw, dh) = (u64::from(w.max(1)), u64::from(h.max(1)));
    // premultiply once
    let pm: Vec<u64> = img
        .px
        .chunks_exact(4)
        .flat_map(|p| {
            let a = u64::from(p[3]);
            [
                u64::from(p[0]) * a,
                u64::from(p[1]) * a,
                u64::from(p[2]) * a,
                a * 255,
            ]
        })
        .collect();
    // per destination column: source spans with weights (in units of 1/dw of a source px)
    let spans = |s: u64, d: u64| -> Vec<Vec<(usize, u64)>> {
        (0..d)
            .map(|k| {
                // destination pixel k covers source [k·s/d, (k+1)·s/d): in units of 1/d
                let (a, b) = (k * s, (k + 1) * s);
                let mut v = Vec::new();
                let mut i = a / d;
                while i * d < b {
                    let lo = (i * d).max(a);
                    let hi = ((i + 1) * d).min(b);
                    if hi > lo {
                        v.push((i.min(s - 1) as usize, hi - lo));
                    }
                    i += 1;
                }
                v
            })
            .collect()
    };
    let (xs, ys) = (spans(sw, dw), spans(sh, dh));
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for (y, sy) in ys.iter().enumerate() {
        for (x, sx) in xs.iter().enumerate() {
            let mut acc = [0u64; 4];
            for &(j, wy) in sy {
                for &(i, wx) in sx {
                    let o = (j * sw as usize + i) * 4;
                    let wgt = wx * wy;
                    for (k, a) in acc.iter_mut().enumerate() {
                        *a += pm[o + k] * wgt;
                    }
                }
            }
            // total weight = sw·sh; values carry ×255 (premultiplied)
            let den = sw * sh * 255;
            let o = (y * dw as usize + x) * 4;
            for k in 0..4 {
                out[o + k] = ((acc[k] + den / 2) / den).min(255) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_and_refuses_what_is_not_read() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxx"), Some("png"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert!(matches!(
            decode_untrusted(b"GIF89a.."),
            Err(ImageError::Unsupported("GIF"))
        ));
        assert!(decode_untrusted(b"hello").is_err());
    }

    #[test]
    fn png_round_trips_and_bombs_are_refused() {
        let img = Rgba8 {
            width: 3,
            height: 2,
            px: (0..24).map(|i| (i * 10) as u8).collect(),
        };
        let png = encode_png(&img).unwrap();
        assert_eq!(decode_untrusted(&png).unwrap(), img);
        // a header claiming 65535² (R-MEDIA-SEC-3's bomb sample) is refused before decoding
        let mut big = encode_png(&Rgba8 {
            width: 1,
            height: 1,
            px: vec![0, 0, 0, 255],
        })
        .unwrap();
        big[16..20].copy_from_slice(&65_535u32.to_be_bytes());
        big[20..24].copy_from_slice(&65_535u32.to_be_bytes());
        // fix the IHDR CRC so the size is what is refused
        let crc = crc(&big[12..29]);
        big[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(decode_untrusted(&big).is_err());
        assert!(
            decode_untrusted(&png[..png.len() / 2]).is_err(),
            "truncated"
        );
    }

    fn crc(b: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &x in b {
            c ^= u32::from(x);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    #[test]
    fn fit_averages_exactly() {
        // 2×2: red, transparent, red, transparent → 1×1 is half-alpha red, premultiplied
        let img = Rgba8 {
            width: 2,
            height: 2,
            px: vec![255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255, 0, 0, 0, 0],
        };
        assert_eq!(fit(&img, 1, 1), vec![128, 0, 0, 128]);
        // upscale repeats
        let one = Rgba8 {
            width: 1,
            height: 1,
            px: vec![10, 20, 30, 255],
        };
        assert_eq!(fit(&one, 2, 1), vec![10, 20, 30, 255, 10, 20, 30, 255]);
        // 3 → 2 columns: middle pixel split half/half
        let row = Rgba8 {
            width: 3,
            height: 1,
            px: vec![0, 0, 0, 255, 90, 90, 90, 255, 180, 180, 180, 255],
        };
        assert_eq!(fit(&row, 2, 1), vec![30, 30, 30, 255, 150, 150, 150, 255]);
    }
}
