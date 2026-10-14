//! Where pictures come from: a camera through SDL3, or, in fake mode, a TCP peer that plays the
//! device. Either way the rest of the helper sees grayscale pictures and nothing else.

use std::ffi::CStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sdl3_sys::everything::*;

use crate::decode::Gray;
use crate::protocol::Frame;

pub trait Source: Send {
    fn name(&self) -> String;
    /// The next picture, `None` while there is none yet, an error once the source is gone.
    fn next(&mut self) -> Result<Option<Gray>, CameraError>;
    /// Move to the next camera, where there is more than one.
    fn switch(&mut self) -> Result<(), CameraError> {
        Ok(())
    }
}

#[derive(Debug)]
pub enum CameraError {
    NoCamera,
    Denied,
    Failed(String),
}

impl CameraError {
    pub fn code(&self) -> &'static str {
        match self {
            CameraError::NoCamera => "no_camera",
            CameraError::Denied => "camera_denied",
            CameraError::Failed(_) => "camera_failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            CameraError::NoCamera => "no camera found".into(),
            CameraError::Denied => crate::platform::DENIED_HINT.into(),
            CameraError::Failed(m) => m.clone(),
        }
    }
}

pub fn sdl_error() -> String {
    // SAFETY: SDL_GetError returns a valid, NUL-terminated string owned by SDL.
    unsafe {
        CStr::from_ptr(SDL_GetError())
            .to_string_lossy()
            .into_owned()
    }
}

pub struct SdlCamera {
    cam: *mut SDL_Camera,
    ids: Vec<SDL_CameraID>,
    at: usize,
    name: String,
}

// SAFETY: SDL's camera functions are documented as safe to call from any thread, and the camera is
// only ever used by the one thread that owns this value.
unsafe impl Send for SdlCamera {}

fn camera_ids() -> Vec<SDL_CameraID> {
    let mut n = 0;
    // SAFETY: SDL allocates the array, which is freed below after copying it out.
    unsafe {
        let p = SDL_GetCameras(&mut n);
        if p.is_null() {
            return Vec::new();
        }
        let ids = std::slice::from_raw_parts(p, n.max(0) as usize).to_vec();
        SDL_free(p.cast());
        ids
    }
}

fn camera_name(id: SDL_CameraID) -> String {
    // SAFETY: the name, when present, is a NUL-terminated string SDL owns.
    unsafe {
        let p = SDL_GetCameraName(id);
        if p.is_null() {
            String::from("camera")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// The widest format up to 1920 pixels: a Shell's codes are small, so resolution matters more than
/// rate. Asked for in NV12, whose first plane is the grayscale picture; SDL converts if it must.
fn best_spec(id: SDL_CameraID) -> Option<SDL_CameraSpec> {
    let mut n = 0;
    // SAFETY: as in `camera_ids`; the specs are copied before the array is freed.
    unsafe {
        let p = SDL_GetCameraSupportedFormats(id, &mut n);
        if p.is_null() {
            return None;
        }
        let specs: Vec<SDL_CameraSpec> = std::slice::from_raw_parts(p, n.max(0) as usize)
            .iter()
            .filter(|s| !s.is_null())
            .map(|s| **s)
            .collect();
        SDL_free(p.cast());
        let fps = |s: &SDL_CameraSpec| {
            s.framerate_numerator as f32 / s.framerate_denominator.max(1) as f32
        };
        specs
            .into_iter()
            .filter(|s| s.width <= 1920 && fps(s) >= 10.0)
            .max_by_key(|s| (s.width, s.height, fps(s) as i32))
            .map(|mut s| {
                s.format = SDL_PIXELFORMAT_NV12;
                s
            })
    }
}

impl SdlCamera {
    /// Open the camera whose name contains `prefer`, else the first one.
    pub fn open(prefer: Option<&str>) -> Result<SdlCamera, CameraError> {
        let ids = camera_ids();
        if ids.is_empty() {
            return Err(CameraError::NoCamera);
        }
        let at = prefer
            .and_then(|want| {
                let want = want.to_lowercase();
                ids.iter()
                    .position(|id| camera_name(*id).to_lowercase().contains(&want))
            })
            .unwrap_or(0);
        let mut cam = SdlCamera {
            cam: null_mut(),
            ids,
            at,
            name: String::new(),
        };
        cam.open_at()?;
        Ok(cam)
    }

    fn open_at(&mut self) -> Result<(), CameraError> {
        let id = self.ids[self.at];
        let spec = best_spec(id);
        // SAFETY: `id` came from SDL_GetCameras; a null spec lets SDL choose.
        let cam = unsafe { SDL_OpenCamera(id, spec.as_ref().map_or(null(), |s| s as *const _)) };
        if cam.is_null() {
            return Err(CameraError::Failed(format!(
                "opening {}: {}",
                camera_name(id),
                sdl_error()
            )));
        }
        self.cam = cam;
        self.name = camera_name(id);
        Ok(())
    }

    fn close(&mut self) {
        if !self.cam.is_null() {
            // SAFETY: the camera was opened by this value and is closed once.
            unsafe { SDL_CloseCamera(self.cam) };
            self.cam = null_mut();
        }
    }
}

impl Drop for SdlCamera {
    fn drop(&mut self) {
        self.close();
    }
}

/// The grayscale picture of a camera frame: NV12's first plane as it is, anything else through
/// SDL's conversion to RGBA.
///
/// # Safety
/// `f` must be a valid surface SDL handed out and not yet released.
unsafe fn luma(f: *mut SDL_Surface) -> Option<Gray> {
    let s = &*f;
    let (w, h) = (s.w.max(0) as usize, s.h.max(0) as usize);
    if w == 0 || h == 0 || s.pixels.is_null() {
        return None;
    }
    let mut px = Vec::with_capacity(w * h);
    if s.format == SDL_PIXELFORMAT_NV12 {
        let base = s.pixels as *const u8;
        for y in 0..h {
            px.extend_from_slice(std::slice::from_raw_parts(
                base.add(y * s.pitch as usize),
                w,
            ));
        }
        return Some(Gray { w, h, px });
    }
    let rgba = SDL_ConvertSurface(f, SDL_PIXELFORMAT_RGBA32);
    if rgba.is_null() {
        return None;
    }
    let r = &*rgba;
    let base = r.pixels as *const u8;
    for y in 0..h {
        let row = std::slice::from_raw_parts(base.add(y * r.pitch as usize), w * 4);
        px.extend(
            row.chunks_exact(4)
                .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8),
        );
    }
    SDL_DestroySurface(rgba);
    Some(Gray { w, h, px })
}

impl Source for SdlCamera {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn next(&mut self) -> Result<Option<Gray>, CameraError> {
        // SAFETY: `self.cam` is an open camera; each acquired frame is released before returning.
        unsafe {
            let state = SDL_GetCameraPermissionState(self.cam);
            if state == SDL_CAMERA_PERMISSION_STATE_DENIED {
                return Err(CameraError::Denied);
            }
            if state != SDL_CAMERA_PERMISSION_STATE_APPROVED {
                return Ok(None);
            }
            let f = SDL_AcquireCameraFrame(self.cam, null_mut());
            if f.is_null() {
                return Ok(None);
            }
            let img = luma(f);
            SDL_ReleaseCameraFrame(self.cam, f);
            Ok(img)
        }
    }

    fn switch(&mut self) -> Result<(), CameraError> {
        self.ids = camera_ids();
        if self.ids.len() < 2 {
            return Ok(());
        }
        self.close();
        self.at = (self.at + 1) % self.ids.len();
        self.open_at()
    }
}

/// Fake mode: a TCP peer plays the device. It is told what the window would show, and sends the
/// pictures the camera would see as `raster <w> <h>\n` followed by `w*h` grayscale bytes.
#[derive(Clone)]
pub struct FakeLink {
    writer: Arc<Mutex<TcpStream>>,
}

pub struct FakeCamera {
    reader: BufReader<TcpStream>,
}

pub fn fake(addr: &str) -> std::io::Result<(FakeLink, FakeCamera)> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    Ok((
        FakeLink { writer },
        FakeCamera {
            reader: BufReader::new(stream),
        },
    ))
}

impl FakeLink {
    pub fn shown(&self, frames: &[Frame], interval_ms: u64) {
        let line = serde_json::json!({ "show": { "frames": frames, "interval_ms": interval_ms } });
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{line}");
        let _ = w.flush();
    }

    pub fn scanning(&self) {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{}", serde_json::json!({ "scan": true }));
        let _ = w.flush();
    }
}

fn timed_out(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

impl Source for FakeCamera {
    fn name(&self) -> String {
        "fake camera".into()
    }

    fn next(&mut self) -> Result<Option<Gray>, CameraError> {
        let mut header = String::new();
        match self.reader.read_line(&mut header) {
            Ok(0) => return Err(CameraError::Failed("the fake device went away".into())),
            Ok(_) => {}
            Err(e) if timed_out(&e) => return Ok(None),
            Err(e) => return Err(CameraError::Failed(e.to_string())),
        }
        let parts: Vec<&str> = header.split_whitespace().collect();
        let (w, h) = match parts.as_slice() {
            ["raster", w, h] => (
                w.parse::<usize>().unwrap_or(0),
                h.parse::<usize>().unwrap_or(0),
            ),
            _ => return Err(CameraError::Failed(format!("fake device sent {header:?}"))),
        };
        if w == 0 || h == 0 || w * h > 4096 * 4096 {
            return Err(CameraError::Failed(format!("fake raster {w}x{h}")));
        }
        let mut px = vec![0u8; w * h];
        let mut got = 0;
        while got < px.len() {
            match self.reader.read(&mut px[got..]) {
                Ok(0) => return Err(CameraError::Failed("the fake device went away".into())),
                Ok(n) => got += n,
                Err(e) if timed_out(&e) => continue,
                Err(e) => return Err(CameraError::Failed(e.to_string())),
            }
        }
        Ok(Some(Gray { w, h, px }))
    }
}
