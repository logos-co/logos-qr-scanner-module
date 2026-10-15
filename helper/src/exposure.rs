//! Exposure for a screen, on Linux webcams. A webcam's own auto exposure meters the whole room, so a
//! wallet's backlit screen held up to it comes out white: on a Framework laptop webcam nothing read
//! at 30 cm under auto exposure, and 34 parts in 10 s at 6 ms. While it scans, the helper sets the
//! exposure itself through V4L2, in whole multiples of 5 ms: the Keycard Shell dims its backlight
//! with 200 Hz PWM, and other exposures band the code. It searches on the middle of the picture,
//! then meters on the code it reads and holds while parts arrive. The camera's own settings are
//! written down first, and given back on close or, after a crash, by the next helper.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::decode::{Found, Gray, Region};

const V4L2_CID_EXPOSURE_AUTO: u32 = 0x009a_0901;
const V4L2_CID_EXPOSURE_ABSOLUTE: u32 = 0x009a_0902;
const V4L2_EXPOSURE_MANUAL: i32 = 1;
// _IOWR('V', 27/28/36, …) for struct v4l2_control (8 bytes) and struct v4l2_queryctrl (68 bytes).
const VIDIOC_G_CTRL: u64 = 0xc008_561b;
const VIDIOC_S_CTRL: u64 = 0xc008_561c;
const VIDIOC_QUERYCTRL: u64 = 0xc044_5624;

/// Manual exposures, in V4L2's 100 µs units: whole multiples of 5 ms, up to 30 ms, past which a
/// hand-held code smears.
const LADDER: [i32; 5] = [50, 100, 150, 200, 300];
/// How long without a part before searching again, and before trying the next rung.
const LOST: Duration = Duration::from_millis(1500);
const STUCK: Duration = Duration::from_secs(3);

#[repr(C)]
struct Control {
    id: u32,
    value: i32,
}

#[repr(C)]
struct QueryCtrl {
    id: u32,
    kind: u32,
    name: [u8; 32],
    minimum: i32,
    maximum: i32,
    step: i32,
    default_value: i32,
    flags: u32,
    reserved: [u32; 2],
}

pub struct Exposure {
    dev: File,
    /// Where the camera's own settings are written down while they are changed.
    state: PathBuf,
    /// The camera's own mode and time, given back on close.
    own: (i32, i32),
    manual: bool,
    pinned: bool,
    value: i32,
    range: (i32, i32),
    checked: Instant,
    last_read: Option<Instant>,
    moved: Instant,
}

fn get(dev: &File, id: u32) -> Option<i32> {
    let mut c = Control { id, value: 0 };
    // SAFETY: VIDIOC_G_CTRL fills a struct v4l2_control, which `Control` lays out.
    let r = unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_G_CTRL as _, &mut c) };
    (r == 0).then_some(c.value)
}

fn set(dev: &File, id: u32, value: i32) -> bool {
    let mut c = Control { id, value };
    // SAFETY: as in `get`; the driver reads the struct.
    unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_S_CTRL as _, &mut c) == 0 }
}

fn state_file(node: &std::ffi::OsStr) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    dir.join(format!("logos-qr-exposure-{}", node.to_string_lossy()))
}

impl Exposure {
    /// The V4L2 device SDL named `camera`, when it lets its exposure be set.
    pub fn for_camera(camera: &str) -> Option<Exposure> {
        let found = Self::find(camera);
        match &found {
            Some(e) => eprintln!(
                "logos-qr-helper: metering the exposure of {camera:?} (auto mode {}, {}..{})",
                e.own.0, e.range.0, e.range.1
            ),
            None => eprintln!("logos-qr-helper: no V4L2 exposure control for {camera:?}"),
        }
        found
    }

    fn find(camera: &str) -> Option<Exposure> {
        let mut nodes: Vec<_> = std::fs::read_dir("/sys/class/video4linux")
            .ok()?
            .flatten()
            .filter(|e| {
                std::fs::read_to_string(e.path().join("name"))
                    .is_ok_and(|n| n.trim() == camera.trim())
            })
            .map(|e| e.file_name())
            .collect();
        nodes.sort();
        nodes.into_iter().find_map(|n| {
            let dev = File::options()
                .read(true)
                .write(true)
                .open(std::path::Path::new("/dev").join(&n))
                .ok()?;
            let mut q = QueryCtrl {
                id: V4L2_CID_EXPOSURE_ABSOLUTE,
                kind: 0,
                name: [0; 32],
                minimum: 0,
                maximum: 0,
                step: 0,
                default_value: 0,
                flags: 0,
                reserved: [0; 2],
            };
            // SAFETY: VIDIOC_QUERYCTRL fills a struct v4l2_queryctrl, which `QueryCtrl` lays out.
            let r = unsafe { libc::ioctl(dev.as_raw_fd(), VIDIOC_QUERYCTRL as _, &mut q) };
            if r != 0 || q.maximum <= q.minimum {
                return None;
            }
            let state = state_file(&n);
            restore_left_behind(&dev, &state);
            let own = (
                get(&dev, V4L2_CID_EXPOSURE_AUTO)?,
                get(&dev, V4L2_CID_EXPOSURE_ABSOLUTE).unwrap_or(q.default_value),
            );
            let mut e = Exposure {
                dev,
                state,
                own,
                manual: false,
                pinned: false,
                value: own.1,
                range: (q.minimum.max(1), q.maximum),
                checked: Instant::now(),
                last_read: None,
                moved: Instant::now(),
            };
            // For trying one exposure by hand: LOGOS_QR_EXPOSURE=50 is 5 ms.
            if let Some(v) = std::env::var("LOGOS_QR_EXPOSURE")
                .ok()
                .and_then(|v| v.parse::<i32>().ok())
            {
                e.pinned = true;
                e.go(v);
            }
            Some(e)
        })
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
                b if b < 150 && rung + 1 < LADDER.len() => LADDER[rung + 1],
                _ => return,
            }
        } else {
            let b = percentile(img, middle(img), 98);
            if !self.manual {
                if b < 240 {
                    return;
                }
                LADDER[1]
            } else if b >= 240 && rung > 0 {
                LADDER[rung - 1]
            } else if b < 160 && rung + 1 < LADDER.len() {
                LADDER[rung + 1]
            } else if now.duration_since(self.moved) >= STUCK {
                // Nothing read at this rung for a while: try the next, round the first three.
                LADDER[(rung + 1) % 3]
            } else {
                return;
            }
        };
        self.go(next);
    }

    fn go(&mut self, value: i32) {
        let value = value.clamp(self.range.0, self.range.1);
        if !self.manual {
            // Written down before anything changes, so a crash can't lose it.
            let _ = std::fs::write(&self.state, format!("{} {}", self.own.0, self.own.1));
            self.manual = set(&self.dev, V4L2_CID_EXPOSURE_AUTO, V4L2_EXPOSURE_MANUAL);
            if !self.manual {
                let _ = std::fs::remove_file(&self.state);
                return;
            }
        }
        if set(&self.dev, V4L2_CID_EXPOSURE_ABSOLUTE, value) {
            eprintln!("logos-qr-helper: exposure {} -> {value}", self.value);
            self.value = value;
            self.moved = Instant::now();
        }
    }
}

impl Drop for Exposure {
    fn drop(&mut self) {
        if self.manual {
            set(&self.dev, V4L2_CID_EXPOSURE_AUTO, self.own.0);
            set(&self.dev, V4L2_CID_EXPOSURE_ABSOLUTE, self.own.1);
            let _ = std::fs::remove_file(&self.state);
        }
    }
}

/// A helper that died with the camera set by hand left its settings behind: give them back.
fn restore_left_behind(dev: &File, state: &PathBuf) {
    let Ok(text) = std::fs::read_to_string(state) else {
        return;
    };
    let mut it = text
        .split_whitespace()
        .filter_map(|v| v.parse::<i32>().ok());
    if let (Some(mode), Some(time)) = (it.next(), it.next()) {
        set(dev, V4L2_CID_EXPOSURE_AUTO, mode);
        set(dev, V4L2_CID_EXPOSURE_ABSOLUTE, time);
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
