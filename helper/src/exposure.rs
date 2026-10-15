//! Exposure for a screen, on Linux and Windows webcams. A webcam's own auto exposure meters the
//! whole room, so a wallet's backlit screen held up to it comes out white: on a Framework laptop
//! webcam nothing read at 30 cm under auto exposure, on Linux and on Windows alike. While it scans,
//! the helper sets the exposure itself, along a short ladder: it searches on the middle of the
//! picture, then meters on the code it reads and holds while parts arrive. The camera's own
//! settings are written down first, and given back on close or, after a crash, by the next helper.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::decode::{Found, Gray, Region};

#[cfg(windows)]
mod dshow;
#[cfg(target_os = "linux")]
mod v4l2;
#[cfg(windows)]
use dshow::{Device, DIM, LADDER, START};
#[cfg(target_os = "linux")]
use v4l2::{Device, DIM, LADDER, START};

/// How long without a part before searching again, and before trying the next rung.
const LOST: Duration = Duration::from_millis(1500);
const STUCK: Duration = Duration::from_secs(3);

pub struct Exposure {
    dev: Device,
    /// Where the camera's own settings are written down while they are changed.
    state: PathBuf,
    /// The camera's own mode and value, given back on close.
    own: (i32, i32),
    manual: bool,
    pinned: bool,
    value: i32,
    checked: Instant,
    last_read: Option<Instant>,
    moved: Instant,
}

fn state_file(key: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    dir.join(format!("logos-qr-exposure-{key}"))
}

impl Exposure {
    /// The camera SDL named `camera`, when it lets its exposure be set.
    pub fn for_camera(camera: &str) -> Option<Exposure> {
        let found = Self::find(camera);
        match &found {
            Some(e) => eprintln!(
                "logos-qr-helper: metering the exposure of {camera:?} (mode {}, {}..{})",
                e.own.0,
                e.dev.range().0,
                e.dev.range().1
            ),
            None => eprintln!("logos-qr-helper: no exposure control for {camera:?}"),
        }
        found
    }

    fn find(camera: &str) -> Option<Exposure> {
        let dev = Device::find(camera)?;
        let state = state_file(dev.key());
        restore_left_behind(&dev, &state);
        let own = dev.own()?;
        let mut e = Exposure {
            dev,
            state,
            own,
            manual: false,
            pinned: false,
            value: own.1,
            checked: Instant::now(),
            last_read: None,
            moved: Instant::now(),
        };
        // For trying one exposure by hand, in the camera's units: on Linux 50 is 5 ms, on Windows -6
        // is 2^-6 s.
        if let Some(v) = std::env::var("LOGOS_QR_EXPOSURE")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
        {
            e.pinned = true;
            e.go(v);
        }
        Some(e)
    }

    /// After each picture: where the code was, if one read. A few times a second it moves along
    /// the ladder when the picture is clipped or too dark, and holds while parts arrive.
    pub fn tune(&mut self, img: &Gray, found: &Found) {
        let now = Instant::now();
        if found.any {
            self.last_read = Some(now);
        }
        if self.pinned || now.duration_since(self.checked) < Duration::from_millis(300) {
            return;
        }
        self.checked = now;
        let reading = self.last_read.is_some_and(|t| now.duration_since(t) < LOST);
        let rung = LADDER
            .iter()
            .position(|v| *v >= self.value)
            .unwrap_or(LADDER.len() - 1);
        let next = if reading {
            // On the code itself: down for clipped whites, up for a dim code, else hold.
            let Some(r) = found.region else { return };
            match percentile(img, r, 95) {
                b if b >= 245 && rung > 0 => LADDER[rung - 1],
                b if b < DIM && rung + 1 < LADDER.len() => LADDER[rung + 1],
                _ => return,
            }
        } else {
            let b = percentile(img, middle(img), 98);
            if !self.manual {
                if b < 240 {
                    return;
                }
                LADDER[START]
            } else if b >= 240 && rung > 0 {
                LADDER[rung - 1]
            } else if b < DIM + 10 && rung + 1 < LADDER.len() {
                LADDER[rung + 1]
            } else if now.duration_since(self.moved) >= STUCK {
                // Nothing read at this rung for a while: try the next, round the first three.
                LADDER[(rung + 1) % LADDER.len().min(3)]
            } else {
                return;
            }
        };
        self.go(next);
    }

    fn go(&mut self, value: i32) {
        let (min, max) = self.dev.range();
        let value = value.clamp(min, max);
        if !self.manual {
            // Written down before anything changes, so a crash can't lose it.
            let _ = std::fs::write(&self.state, format!("{} {}", self.own.0, self.own.1));
            self.manual = self.dev.manual();
            if !self.manual {
                let _ = std::fs::remove_file(&self.state);
                return;
            }
        }
        if self.dev.set(value) {
            eprintln!("logos-qr-helper: exposure {} -> {value}", self.value);
            self.value = value;
            self.moved = Instant::now();
        }
    }
}

impl Drop for Exposure {
    fn drop(&mut self) {
        if self.manual {
            self.dev.give_back(self.own);
            let _ = std::fs::remove_file(&self.state);
        }
    }
}

/// A helper that died with the camera set by hand left its settings behind: give them back.
fn restore_left_behind(dev: &Device, state: &PathBuf) {
    let Ok(text) = std::fs::read_to_string(state) else {
        return;
    };
    let mut it = text
        .split_whitespace()
        .filter_map(|v| v.parse::<i32>().ok());
    if let (Some(mode), Some(value)) = (it.next(), it.next()) {
        dev.give_back((mode, value));
        eprintln!("logos-qr-helper: gave the camera back the exposure an earlier helper left set");
    }
    let _ = std::fs::remove_file(state);
}

/// The middle half of the picture, where a code held up usually is.
fn middle(img: &Gray) -> Region {
    Region {
        x0: img.w / 4,
        y0: img.h / 4,
        x1: img.w * 3 / 4,
        y1: img.h * 3 / 4,
    }
}

/// The `p`th percentile of a region's brightness, every other pixel each way.
pub fn percentile(img: &Gray, r: Region, p: u32) -> u8 {
    let mut hist = [0u32; 256];
    let mut n = 0u32;
    for y in (r.y0..r.y1.min(img.h)).step_by(2) {
        for x in (r.x0..r.x1.min(img.w)).step_by(2) {
            hist[img.px[y * img.w + x] as usize] += 1;
            n += 1;
        }
    }
    let mut above = 0;
    for v in (0..256).rev() {
        above += hist[v];
        if above * 100 >= n * (100 - p) {
            return v as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::{middle, percentile};
    use crate::decode::Gray;

    #[test]
    fn a_white_screen_in_the_middle_reads_as_clipped() {
        let (w, h) = (200, 100);
        let px = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if (70..130).contains(&x) && (30..70).contains(&y) {
                    255
                } else {
                    90
                }
            })
            .collect();
        let img = Gray { w, h, px };
        assert_eq!(percentile(&img, middle(&img), 98), 255);
        let dim = Gray {
            w,
            h,
            px: vec![120; w * h],
        };
        assert_eq!(percentile(&dim, middle(&dim), 98), 120);
    }
}
