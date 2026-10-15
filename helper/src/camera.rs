//! Where pictures come from: a camera through SDL3, or, in fake mode, a TCP peer that plays the
//! device. Either way the rest of the helper sees grayscale pictures and nothing else.

use std::ffi::CStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sdl3_sys::everything::*;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::decode::{Found, Gray};
use crate::protocol::Frame;

pub trait Source: Send {
    fn name(&self) -> String;
    /// The next picture, `None` while there is none yet, an error once the source is gone.
    fn next(&mut self) -> Result<Option<Gray>, CameraError>;
    /// Move to the next camera, where there is more than one.
    fn switch(&mut self) -> Result<(), CameraError> {
        Ok(())
    }
    /// What the decoders made of a picture, for a camera that sets its exposure for the code.
    fn tune(&mut self, _img: &Gray, _found: &Found) {}
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
    #[cfg(target_os = "linux")]
    exposure: Option<crate::exposure::Exposure>,
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
/// rate. Raw formats are asked for in NV12, whose first plane is the grayscale picture, and SDL
/// converts. MJPEG, which SDL can't convert but USB webcams often keep their widest modes for,
/// comes as it is and is decoded here; a raw mode wins a tie.
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
        if std::env::var_os("LOGOS_QR_DUMP").is_some() {
            for s in &specs {
                eprintln!(
                    "logos-qr-helper: camera offers {}x{} {} at {:.0} fps",
                    s.width,
                    s.height,
                    CStr::from_ptr(SDL_GetPixelFormatName(s.format)).to_string_lossy(),
                    fps(s)
                );
            }
        }
        let raw = |s: &SDL_CameraSpec| s.format != SDL_PIXELFORMAT_MJPG;
        // A slow mode beats none: a few frames a second still read a code.
        let pick = |min: f32| {
            specs
                .iter()
                .filter(|s| s.width <= 1920 && fps(s) >= min)
                .max_by_key(|s| (s.width, s.height, raw(s), fps(s) as i32))
                .copied()
        };
        let spec = pick(10.0).or_else(|| pick(0.0))?;
        eprintln!(
            "logos-qr-helper: camera mode {}x{} {} at {:.0} fps",
            spec.width,
            spec.height,
            CStr::from_ptr(SDL_GetPixelFormatName(spec.format)).to_string_lossy(),
            fps(&spec)
        );
        if !raw(&spec) {
            return Some(spec);
        }
        Some(SDL_CameraSpec {
            format: SDL_PIXELFORMAT_NV12,
            ..spec
        })
    }
}

/// An MJPEG frame's grayscale picture: its luma alone, decoded without the colour.
fn mjpeg_luma(jpeg: &[u8]) -> Option<Gray> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::Luma);
    let mut d = JpegDecoder::new_with_options(jpeg, opts);
    let px = d.decode().ok()?;
    let (w, h) = d.dimensions()?;
    (px.len() == w * h).then_some(Gray { w, h, px })
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
            #[cfg(target_os = "linux")]
            exposure: None,
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
        #[cfg(target_os = "linux")]
        {
            self.exposure = crate::exposure::Exposure::for_camera(&self.name);
        }
        Ok(())
    }

    fn close(&mut self) {
        // Its own auto exposure back, before the camera goes.
        #[cfg(target_os = "linux")]
        {
            self.exposure = None;
        }
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

/// The grayscale picture of a camera frame: NV12's first plane as it is, MJPEG decoded here,
/// anything else through SDL's conversion to RGBA.
///
/// # Safety
/// `f` must be a valid surface SDL handed out and not yet released.
unsafe fn luma(f: *mut SDL_Surface) -> Option<Gray> {
    let s = &*f;
    let (w, h) = (s.w.max(0) as usize, s.h.max(0) as usize);
    if w == 0 || h == 0 || s.pixels.is_null() {
        return None;
    }
    if s.format == SDL_PIXELFORMAT_MJPG {
        // SDL hands a compressed frame over as it came, its length in `pitch`.
        return mjpeg_luma(std::slice::from_raw_parts(
            s.pixels as *const u8,
            s.pitch.max(0) as usize,
        ));
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

    #[cfg(target_os = "linux")]
    fn tune(&mut self, img: &Gray, found: &Found) {
        if let Some(e) = self.exposure.as_mut() {
            e.tune(img, found);
        }
    }
}

/// Fake mode: a TCP peer plays the device. It is told what the window would show and when the
/// camera is on, sends the pictures the camera would see as `raster <w> <h>\n` followed by `w*h`
/// grayscale bytes, and `press\n` where a person would press the window's button.
#[derive(Clone)]
pub struct FakeLink {
    writer: Arc<Mutex<TcpStream>>,
    scanning: Arc<AtomicBool>,
}

/// The pictures the peer sends while the camera is on; one reader thread takes them off the link.
#[derive(Clone)]
pub struct FakeCamera {
    pictures: Arc<Mutex<Receiver<Gray>>>,
}

/// Connect to the peer. `on_press` runs, on the reader thread, for each press it sends.
pub fn fake(
    addr: &str,
    on_press: impl Fn() + Send + 'static,
) -> std::io::Result<(FakeLink, FakeCamera)> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    let link = FakeLink {
        writer: Arc::new(Mutex::new(stream.try_clone()?)),
        scanning: Arc::new(AtomicBool::new(false)),
    };
    let (tx, rx) = mpsc::channel();
    let scanning = link.scanning.clone();
    let mut reader = BufReader::new(stream);
    std::thread::spawn(move || loop {
        let mut line = String::new();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["press"] => on_press(),
            ["raster", w, h] => {
                let (w, h) = (
                    w.parse::<usize>().unwrap_or(0),
                    h.parse::<usize>().unwrap_or(0),
                );
                if w == 0 || h == 0 || w * h > 4096 * 4096 {
                    return;
                }
                let mut px = vec![0u8; w * h];
                if reader.read_exact(&mut px).is_err() {
                    return;
                }
                // A picture taken while the camera is off is never seen.
                if scanning.load(Ordering::Relaxed) && tx.send(Gray { w, h, px }).is_err() {
                    return;
                }
            }
            _ => return,
        }
    });
    Ok((
        link,
        FakeCamera {
            pictures: Arc::new(Mutex::new(rx)),
        },
    ))
}

impl FakeLink {
    pub fn shown(&self, frames: &[Frame], interval_ms: u64) {
        self.say(serde_json::json!({ "show": { "frames": frames, "interval_ms": interval_ms } }));
    }

    /// The camera turned on or off.
    pub fn scanning(&self, on: bool) {
        self.scanning.store(on, Ordering::Relaxed);
        self.say(serde_json::json!({ "scan": on }));
    }

    fn say(&self, line: serde_json::Value) {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{line}");
        let _ = w.flush();
    }
}

impl Source for FakeCamera {
    fn name(&self) -> String {
        "fake camera".into()
    }

    fn next(&mut self) -> Result<Option<Gray>, CameraError> {
        let rx = self.pictures.lock().unwrap_or_else(|e| e.into_inner());
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(g) => Ok(Some(g)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(CameraError::Failed("the fake device went away".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mjpeg_luma;
    use crate::decode::Decoder;
    use qrcodegen::{QrCode, QrCodeEcc};

    /// A code as an MJPEG webcam sends it, a JPEG, reads once decoded.
    #[test]
    fn an_mjpeg_frame_reads() {
        let text = "UR:CRYPTO-PSBT/1-9/LPADASCFADAXCYBDINHGDHHDLTAEAEAEAEAEAE";
        let qr = QrCode::encode_text(text, QrCodeEcc::Low).unwrap();
        let (n, scale) = (qr.size() as usize, 6);
        let side = (n + 8) * scale;
        let px: Vec<u8> = (0..side * side)
            .map(|i| {
                let (x, y) = ((i % side) / scale, (i / side) / scale);
                let dark = (4..n + 4).contains(&x)
                    && (4..n + 4).contains(&y)
                    && qr.get_module(x as i32 - 4, y as i32 - 4);
                if dark {
                    30
                } else {
                    225
                }
            })
            .collect();
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 85)
            .encode(&px, side as u16, side as u16, jpeg_encoder::ColorType::Luma)
            .unwrap();
        let img = mjpeg_luma(&jpeg).expect("the JPEG decodes");
        assert_eq!((img.w, img.h), (side, side));
        assert_eq!(Decoder::new().decode(&img).new, vec![text.to_string()]);
    }
}
