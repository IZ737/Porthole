//! `app.icon`: a PNG, decoded for the window and taskbar icon.

use anyhow::{Context, Result, bail};

/// Largest icon side accepted, in pixels.
const MAX_SIDE: u32 = 1024;

/// An RGBA8 image.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decode a PNG into 8-bit RGBA.
pub fn decode_png(bytes: &[u8]) -> Result<Rgba> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::ALPHA | png::Transformations::STRIP_16,
    );
    let mut reader = decoder.read_info().context("not a PNG image")?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width > MAX_SIDE || height > MAX_SIDE {
        bail!("the icon is {width}x{height}; use at most {MAX_SIDE}x{MAX_SIDE}");
    }
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .context("the icon is too large")?
    ];
    let frame = reader.next_frame(&mut buf).context("the PNG is damaged")?;
    buf.truncate(frame.buffer_size());
    let pixels = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[v, a]| [v, v, v, a])
            .collect(),
        other => bail!("unexpected PNG colour type {other:?}"),
    };
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

/// Width and height from a PNG's header, without decoding it.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap());
    Some((be(16), be(20)))
}

/// The window icon for `bytes` (PNG), or a warning saying why there is none.
pub fn window_icon(bytes: &[u8]) -> Result<tao::window::Icon> {
    let img = decode_png(bytes)?;
    tao::window::Icon::from_rgba(img.pixels, img.width, img.height)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A `w`x`h` RGB PNG, for tests.
    pub fn sample_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer
                .write_image_data(&vec![200; (w * h * 3) as usize])
                .unwrap();
        }
        out
    }

    #[test]
    fn decodes_to_rgba() {
        let png = sample_png(4, 3);
        assert_eq!(png_size(&png), Some((4, 3)));
        let img = decode_png(&png).unwrap();
        assert_eq!((img.width, img.height), (4, 3));
        assert_eq!(img.pixels.len(), 4 * 3 * 4);
        assert_eq!(&img.pixels[..4], &[200, 200, 200, 255]);
        assert!(window_icon(&png).is_ok());
        assert!(decode_png(b"not a png").is_err());
        assert_eq!(png_size(b"GIF89a"), None);
    }
}
