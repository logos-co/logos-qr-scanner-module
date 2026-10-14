//! QR frames from text: each part becomes one code, as `{size, bits}`, the shape the helper draws
//! and the terminal presenter prints. Uppercase text such as UR parts fits alphanumeric mode.

use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub size: usize,
    pub bits: String,
}

impl Frame {
    pub fn dark(&self, x: usize, y: usize) -> bool {
        self.bits.as_bytes()[y * self.size + x] == b'1'
    }
}

/// The largest version a reader takes: the Keycard Shell reads up to 25.
pub const MAX_VERSION: u8 = 25;

/// One frame per part, error correction L (raised when it costs nothing), at most `max_version`.
pub fn encode(parts: &[String], max_version: u8) -> Result<Vec<Frame>, String> {
    let max = Version::new(max_version.clamp(1, 40));
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let segs = QrSegment::make_segments(part);
            let qr = QrCode::encode_segments_advanced(
                &segs,
                QrCodeEcc::Low,
                Version::MIN,
                max,
                None,
                true,
            )
            .map_err(|_| {
                format!(
                    "part {} ({} characters) doesn't fit a version {max_version} code",
                    i + 1,
                    part.len()
                )
            })?;
            let n = qr.size() as usize;
            let mut bits = String::with_capacity(n * n);
            for y in 0..n as i32 {
                for x in 0..n as i32 {
                    bits.push(if qr.get_module(x, y) { '1' } else { '0' });
                }
            }
            Ok(Frame { size: n, bits })
        })
        .collect()
}

/// The size in modules of a version's code.
pub fn size_of(version: u8) -> usize {
    17 + 4 * version as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ur_part_is_a_small_code() {
        let part = "UR:CRYPTO-PSBT/12-3/LPCSBTAXCFAXHLCYYNMDLYHNHDGTJOJKIDJYZMADAEJSAOAEAEAEAOHTJLEMKBTSKIASRKZT".to_string();
        let f = encode(&[part], MAX_VERSION).unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].size <= size_of(5), "size {}", f[0].size);
        assert_eq!(f[0].bits.len(), f[0].size * f[0].size);
    }

    #[test]
    fn too_long_for_the_cap_is_refused() {
        let long = "A".repeat(3000);
        let e = encode(&[long], 10).unwrap_err();
        assert!(e.contains("version 10"), "{e}");
    }

    #[test]
    fn sizes() {
        assert_eq!(size_of(1), 21);
        assert_eq!(size_of(25), 117);
    }
}
