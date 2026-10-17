//! Panel images as 8-bit gray PNGs, and pixel comparisons between them.

use super::steps::Rect;
use serde::Serialize;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// An 8-bit gray image, row by row.
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(pixels).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Read a PNG as 8-bit gray (color images by luminance; alpha ignored).
pub fn read_png(path: &Path) -> Result<Gray, String> {
    let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("{} is not a PNG: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("cannot decode {}: {e}", path.display()))?;
    let (w, h) = (info.width, info.height);
    let data = &buf[..info.buffer_size()];
    let channels = info.color_type.samples();
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for px in data.chunks(channels) {
        pixels.push(match channels {
            1 | 2 => px[0],
            _ => ((u32::from(px[0]) * 299 + u32::from(px[1]) * 587 + u32::from(px[2]) * 114) / 1000) as u8,
        });
    }
    Ok(Gray { width: w, height: h, pixels })
}

#[derive(Debug, Serialize)]
pub struct Diff {
    pub equal: bool,
    pub size_matches: bool,
    pub differing_pixels: u64,
    pub differing_rows: u32,
    /// x, y, w, h of the differing area.
    pub bbox: Option<[u32; 4]>,
}

fn masked(masks: &[Rect], x: u32, y: u32) -> bool {
    masks.iter().any(|m| x >= m.x && x < m.x + m.w && y >= m.y && y < m.y + m.h)
}

/// Compare two images outside `masks`; with `out`, write a picture of the
/// difference: differing pixels black, equal ones light, masks mid-gray.
pub fn diff(a: &Gray, b: &Gray, masks: &[Rect], out: Option<&Path>) -> Result<Diff, String> {
    if a.width != b.width || a.height != b.height {
        return Ok(Diff { equal: false, size_matches: false, differing_pixels: 0, differing_rows: 0, bbox: None });
    }
    let (w, h) = (a.width, a.height);
    let mut picture = out.map(|_| vec![0u8; (w * h) as usize]);
    let (mut count, mut rows) = (0u64, 0u32);
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        let mut row_differs = false;
        for x in 0..w {
            let i = (y * w + x) as usize;
            let m = masked(masks, x, y);
            let differs = !m && a.pixels[i] != b.pixels[i];
            if let Some(p) = picture.as_mut() {
                p[i] = if m { 160 } else if differs { 0 } else { 215 + (a.pixels[i] / 7) };
            }
            if differs {
                count += 1;
                row_differs = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        rows += u32::from(row_differs);
    }
    if let (Some(path), Some(p)) = (out, picture.as_ref()) {
        if count > 0 {
            write_png(path, w, h, p)?;
        }
    }
    Ok(Diff {
        equal: count == 0,
        size_matches: true,
        differing_pixels: count,
        differing_rows: rows,
        bbox: (count > 0).then(|| [x0, y0, x1 - x0 + 1, y1 - y0 + 1]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_diff() {
        let dir = tempfile::tempdir().unwrap();
        let a = Gray { width: 4, height: 2, pixels: vec![255, 255, 0, 0, 255, 255, 0, 0] };
        let path = dir.path().join("a.png");
        write_png(&path, a.width, a.height, &a.pixels).unwrap();
        let back = read_png(&path).unwrap();
        assert_eq!(back.pixels, a.pixels);
        let mut b = Gray { width: 4, height: 2, pixels: a.pixels.clone() };
        b.pixels[5] = 0;
        let d = diff(&a, &b, &[], Some(&dir.path().join("d.png"))).unwrap();
        assert_eq!((d.equal, d.differing_pixels, d.differing_rows, d.bbox), (false, 1, 1, Some([1, 1, 1, 1])));
        assert!(dir.path().join("d.png").exists());
        let masked = diff(&a, &b, &[Rect { x: 1, y: 1, w: 1, h: 1 }], None).unwrap();
        assert!(masked.equal);
    }
}
