//! First-frame check: is a `screencap -p` PNG blank or uniform?

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Result, bail};

pub struct FrameStats {
    /// Share of pixels with the most common colour (5 bits per channel).
    pub dominant: f64,
    /// The most common colour, as 8-bit RGB.
    pub dominant_rgb: [u8; 3],
    /// Distinct colours (5 bits per channel).
    pub colours: usize,
}

impl FrameStats {
    /// Nothing drawn: one colour covers (nearly) the whole app area.
    pub fn blank(&self) -> bool {
        self.dominant > 0.995 || self.colours <= 2
    }

    pub fn describe(&self) -> String {
        let [r, g, b] = self.dominant_rgb;
        format!(
            "{:.1}% #{r:02x}{g:02x}{b:02x}, {} colours",
            self.dominant * 100.0,
            self.colours
        )
    }
}

/// Rows at the top that belong to the status bar, not the app.
const STATUS_BAR: u32 = 40;

pub fn analyze(path: &Path) -> Result<FrameStats> {
    let mut dec = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    let ch = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        t => bail!("unexpected PNG colour type {t:?}"),
    };
    let (w, h) = (info.width as usize, info.height as usize);
    let mut hist: HashMap<u16, usize> = HashMap::new();
    let top = (STATUS_BAR as usize).min(h / 4);
    for y in top..h {
        let row = &buf[y * info.line_size..y * info.line_size + w * ch];
        for px in row.chunks_exact(ch) {
            let k = (u16::from(px[0] >> 3) << 10)
                | (u16::from(px[1] >> 3) << 5)
                | u16::from(px[2] >> 3);
            *hist.entry(k).or_default() += 1;
        }
    }
    let total = (w * (h - top)).max(1);
    let (&k, &n) = hist.iter().max_by_key(|(_, n)| **n).unwrap_or((&0, &0));
    let c = |s: u16| (((k >> s) & 31) << 3) as u8;
    Ok(FrameStats {
        dominant: n as f64 / total as f64,
        dominant_rgb: [c(10), c(5), c(0)],
        colours: hist.len(),
    })
}

fn decode(path: &Path) -> Result<(u32, u32, usize, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    let ch = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        t => bail!("unexpected PNG colour type {t:?}"),
    };
    buf.truncate(info.line_size * info.height as usize);
    Ok((info.width, info.height, ch, buf))
}

/// Share of pixels below the status bar that differ between two frames.
pub fn diff(a: &Path, b: &Path) -> Result<f64> {
    let (wa, ha, ca, pa) = decode(a)?;
    let (wb, hb, cb, pb) = decode(b)?;
    if (wa, ha, ca) != (wb, hb, cb) {
        return Ok(1.0);
    }
    let top = (STATUS_BAR as usize).min(ha as usize / 4) * wa as usize * ca;
    let (pa, pb) = (&pa[top..], &pb[top..]);
    let n = pa.len() / ca;
    let changed = pa
        .chunks_exact(ca)
        .zip(pb.chunks_exact(ca))
        .filter(|(x, y)| x[..3] != y[..3])
        .count();
    Ok(changed as f64 / n.max(1) as f64)
}
