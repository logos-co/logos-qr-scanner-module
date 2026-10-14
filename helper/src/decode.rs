//! Finding QR codes in a grayscale picture: zxing-cpp over the whole frame, then rqrr over its
//! centre when zxing found nothing, as Sparrow chains its decoders. Each text is reported once.

use std::collections::HashSet;

use zxingcpp::{BarcodeFormat, BarcodeReader, ImageFormat, ImageView};

/// A grayscale picture, one byte a pixel, row by row.
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    /// A copy at most `max_w` wide, for the window's preview.
    pub fn shrunk(&self, max_w: usize) -> Gray {
        if self.w <= max_w || self.w == 0 {
            return Gray {
                w: self.w,
                h: self.h,
                px: self.px.clone(),
            };
        }
        let w = max_w;
        let h = (self.h * max_w / self.w).max(1);
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            let sy = y * self.h / h;
            for x in 0..w {
                px.push(self.px[sy * self.w + x * self.w / w]);
            }
        }
        Gray { w, h, px }
    }
}

pub struct Decoder {
    reader: BarcodeReader,
    seen: HashSet<String>,
}

pub struct Found {
    /// A code was in view, new or not: the window says so.
    pub any: bool,
    /// Texts not reported before.
    pub new: Vec<String>,
}

impl Decoder {
    pub fn new() -> Decoder {
        let reader = BarcodeReader::new()
            .formats([BarcodeFormat::QRCode])
            .try_harder(true)
            .try_rotate(true)
            .try_invert(false)
            .try_downscale(true);
        Decoder {
            reader,
            seen: HashSet::new(),
        }
    }

    pub fn decode(&mut self, img: &Gray) -> Found {
        let mut texts = self.zxing(img);
        if texts.is_empty() {
            texts = centre(img);
        }
        let any = !texts.is_empty();
        let new = texts
            .into_iter()
            .filter(|t| self.seen.insert(t.clone()))
            .collect();
        Found { any, new }
    }

    fn zxing(&self, img: &Gray) -> Vec<String> {
        let Ok(view) = ImageView::from_slice(&img.px, img.w as i32, img.h as i32, ImageFormat::Lum)
        else {
            return Vec::new();
        };
        match self.reader.from(view) {
            Ok(codes) => codes
                .into_iter()
                .map(|c| c.text())
                .filter(|t| !t.is_empty())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// rqrr over the middle half of the picture, where a code held up to the camera usually is.
fn centre(img: &Gray) -> Vec<String> {
    let (cw, ch) = (img.w / 2, img.h / 2);
    if cw < 21 || ch < 21 {
        return Vec::new();
    }
    let (x0, y0) = (img.w / 4, img.h / 4);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(cw, ch, |x, y| {
        img.px[(y0 + y) * img.w + x0 + x]
    });
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|g| g.decode().ok().map(|(_, text)| text))
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrcodegen::{QrCode, QrCodeEcc};

    /// A code drawn as a camera might see it: `scale` pixels a module, a quiet zone, placed off
    /// centre on a mid-gray background, with a little noise.
    fn picture(text: &str, scale: usize, w: usize, h: usize, at: (usize, usize)) -> Gray {
        let qr = QrCode::encode_text(text, QrCodeEcc::Low).unwrap();
        let n = qr.size() as usize;
        let mut px = vec![140u8; w * h];
        let side = (n + 8) * scale;
        for y in 0..side {
            for x in 0..side {
                let (mx, my) = (x / scale, y / scale);
                let dark = (4..n + 4).contains(&mx)
                    && (4..n + 4).contains(&my)
                    && qr.get_module(mx as i32 - 4, my as i32 - 4);
                let noise = ((x * 7 + y * 13) % 11) as u8;
                px[(at.1 + y) * w + at.0 + x] = if dark { 30 + noise } else { 220 - noise };
            }
        }
        Gray { w, h, px }
    }

    #[test]
    fn reads_a_ur_part() {
        let text = "UR:CRYPTO-PSBT/1-3/LPADAXCFAXHLCYYNMDLYHNHDGTJOJKIDJYZMADAEJSAOAEAEAEAOHTJLEMKBTSKIASRKZT";
        let img = picture(text, 4, 640, 480, (180, 60));
        let mut d = Decoder::new();
        let found = d.decode(&img);
        assert!(found.any);
        assert_eq!(found.new, vec![text.to_string()]);
        // The same code again is seen, not reported again.
        let again = d.decode(&img);
        assert!(again.any);
        assert!(again.new.is_empty());
    }

    #[test]
    fn nothing_in_a_blank_picture() {
        let img = Gray {
            w: 320,
            h: 240,
            px: vec![128; 320 * 240],
        };
        let found = Decoder::new().decode(&img);
        assert!(!found.any);
        assert!(found.new.is_empty());
    }

    #[test]
    fn rqrr_reads_the_centre() {
        let text = "UR:BYTES/HDCXDWINVEZM";
        let img = picture(text, 3, 400, 400, (140, 140));
        assert_eq!(centre(&img), vec![text.to_string()]);
    }

    #[test]
    fn shrinks_for_the_preview() {
        let img = Gray {
            w: 1920,
            h: 1080,
            px: vec![7; 1920 * 1080],
        };
        let s = img.shrunk(480);
        assert_eq!((s.w, s.h), (480, 270));
        assert_eq!(s.px.len(), 480 * 270);
    }
}
