//! A Linux webcam's exposure, through V4L2.

use std::fs::File;
use std::os::fd::AsRawFd;

const V4L2_CID_EXPOSURE_AUTO: u32 = 0x009a_0901;
const V4L2_CID_EXPOSURE_ABSOLUTE: u32 = 0x009a_0902;
const V4L2_EXPOSURE_MANUAL: i32 = 1;
// _IOWR('V', 27/28/36, …) for struct v4l2_control (8 bytes) and struct v4l2_queryctrl (68 bytes).
const VIDIOC_G_CTRL: u64 = 0xc008_561b;
const VIDIOC_S_CTRL: u64 = 0xc008_561c;
const VIDIOC_QUERYCTRL: u64 = 0xc044_5624;

/// In 100 µs: whole multiples of 5 ms, since the Keycard Shell dims its backlight with 200 Hz PWM and
/// other exposures band the code; up to 30 ms, past which a hand-held code smears.
pub const LADDER: &[i32] = &[50, 100, 150, 200, 300];
pub const START: usize = 1;
/// Below this, a code or the middle of the picture is too dim.
pub const DIM: u8 = 150;

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

pub struct Device {
    dev: File,
    node: String,
    range: (i32, i32),
    default: i32,
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

impl Device {
    /// The V4L2 device SDL named `camera`, when it lets its exposure be set.
    pub fn find(camera: &str) -> Option<Device> {
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
            Some(Device {
                dev,
                node: n.to_string_lossy().into_owned(),
                range: (q.minimum.max(1), q.maximum),
                default: q.default_value,
            })
        })
    }

    /// What names this camera in the state file's name.
    pub fn key(&self) -> &str {
        &self.node
    }

    pub fn range(&self) -> (i32, i32) {
        self.range
    }

    /// The camera's own mode and time.
    pub fn own(&self) -> Option<(i32, i32)> {
        let time = get(&self.dev, V4L2_CID_EXPOSURE_ABSOLUTE).unwrap_or(self.default);
        Some((get(&self.dev, V4L2_CID_EXPOSURE_AUTO)?, time))
    }

    pub fn manual(&self) -> bool {
        set(&self.dev, V4L2_CID_EXPOSURE_AUTO, V4L2_EXPOSURE_MANUAL)
    }

    pub fn set(&self, value: i32) -> bool {
        set(&self.dev, V4L2_CID_EXPOSURE_ABSOLUTE, value)
    }

    pub fn give_back(&self, (mode, time): (i32, i32)) {
        set(&self.dev, V4L2_CID_EXPOSURE_AUTO, mode);
        set(&self.dev, V4L2_CID_EXPOSURE_ABSOLUTE, time);
    }
}
