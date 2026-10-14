//! logos-qr-helper: shows the QR frames a module hands it, reads QR codes through the camera, and
//! returns only their text. It knows nothing of UR or wallets. See `docs/helper.md`.
//!
//! `--no-window` reads the camera without a window, for a terminal presenter. `--probe` starts
//! scanning at once, for trying a camera by hand. `LOGOS_QR_HELPER_FAKE=host:port` replaces the
//! camera and the window with a TCP peer, for tests. `LOGOS_QR_CAMERA` picks a camera by name.

mod camera;
mod decode;
mod platform;
mod protocol;
mod window;

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sdl3_sys::everything::*;

use camera::{CameraError, FakeCamera, FakeLink, SdlCamera, Source};
use decode::{Decoder, Gray};
use protocol::{Command, Event, Frame, Out};
use window::{View, Window};

const VERSION: &str = env!("CARGO_PKG_VERSION");

enum Msg {
    Cmd(Command),
    Bad(String),
    Eof,
    Seen { preview: Option<Gray>, any: bool },
    Camera(String),
    CameraFailed(CameraError),
}

struct Scan {
    stop: Arc<AtomicBool>,
    switch: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

/// Reads pictures until told to stop, reporting each new text straight to the module.
fn scan(out: Arc<Out>, tx: Sender<Msg>, fake: Option<FakeCamera>, preview: bool) -> Scan {
    let stop = Arc::new(AtomicBool::new(false));
    let switch = Arc::new(AtomicBool::new(false));
    let (s, sw) = (stop.clone(), switch.clone());
    let worker = thread::spawn(move || {
        let mut src: Box<dyn Source> = match fake {
            Some(f) => Box::new(f),
            None => {
                match platform::camera_access() {
                    platform::Access::Granted => {}
                    _ => {
                        let _ = tx.send(Msg::CameraFailed(CameraError::Denied));
                        return;
                    }
                }
                match SdlCamera::open(std::env::var("LOGOS_QR_CAMERA").ok().as_deref()) {
                    Ok(c) => Box::new(c),
                    Err(e) => {
                        let _ = tx.send(Msg::CameraFailed(e));
                        return;
                    }
                }
            }
        };
        let _ = tx.send(Msg::Camera(src.name()));
        let mut decoder = Decoder::new();
        let mut last_preview = Instant::now() - Duration::from_secs(1);
        while !s.load(Ordering::Relaxed) {
            if sw.swap(false, Ordering::Relaxed) {
                if let Err(e) = src.switch() {
                    let _ = tx.send(Msg::CameraFailed(e));
                    return;
                }
                decoder = Decoder::new();
                let _ = tx.send(Msg::Camera(src.name()));
            }
            match src.next() {
                Ok(Some(img)) => {
                    let found = decoder.decode(&img);
                    for text in found.new {
                        out.send(&Event::Text { text });
                    }
                    let shot = (preview && last_preview.elapsed() >= Duration::from_millis(66))
                        .then(|| {
                            last_preview = Instant::now();
                            img.shrunk(480)
                        });
                    if shot.is_some() || found.any {
                        let _ = tx.send(Msg::Seen {
                            preview: shot,
                            any: found.any,
                        });
                    }
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(e) => {
                    let _ = tx.send(Msg::CameraFailed(e));
                    return;
                }
            }
        }
    });
    Scan {
        stop,
        switch,
        worker,
    }
}

fn read_commands(tx: Sender<Msg>) {
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let msg = match serde_json::from_str::<Command>(&line) {
                Ok(c) => Msg::Cmd(c),
                Err(e) => Msg::Bad(e.to_string()),
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
        let _ = tx.send(Msg::Eof);
    });
}

#[derive(Default)]
struct State {
    title: String,
    frames: Vec<Frame>,
    interval: Duration,
    since: Option<Instant>,
    hint: Option<String>,
    progress: Option<(u32, u32)>,
    preview: Option<Gray>,
    seen_at: Option<Instant>,
    camera: Option<String>,
    problem: Option<String>,
}

impl State {
    fn frame(&self) -> Option<&Frame> {
        if self.frames.is_empty() {
            return None;
        }
        let ms = self.since.map_or(0, |t| t.elapsed().as_millis()) as u64;
        let i = (ms / self.interval.as_millis().max(1) as u64) as usize % self.frames.len();
        self.frames.get(i)
    }

    fn lines(&self, scanning: bool) -> Vec<String> {
        let mut first = self
            .problem
            .clone()
            .or_else(|| self.hint.clone())
            .unwrap_or_default();
        if first.is_empty() && !self.frames.is_empty() {
            first = "Scan these codes with your device".into();
        }
        let mut second = String::new();
        if scanning {
            second = self
                .camera
                .clone()
                .map_or("Opening the camera...".into(), |c| {
                    format!("Camera: {c} (Tab to switch)")
                });
            if let Some((got, of)) = self.progress {
                second = format!("{second}  -  read {got} of {of}");
            }
        }
        vec![first, second]
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("logos-qr-helper {VERSION}");
        return;
    }
    platform::disclaim();
    let probe = args.iter().any(|a| a == "--probe");
    let out = Arc::new(Out::new());
    let (tx, rx) = mpsc::channel::<Msg>();
    read_commands(tx.clone());

    let mut fake: Option<(FakeLink, Option<FakeCamera>)> = None;
    if let Ok(addr) = std::env::var("LOGOS_QR_HELPER_FAKE") {
        match camera::fake(&addr) {
            Ok((link, cam)) => fake = Some((link, Some(cam))),
            Err(e) => {
                out.error("fake_failed", format!("{addr}: {e}"));
                return;
            }
        }
    }
    let want_window = fake.is_none() && !args.iter().any(|a| a == "--no-window");
    let camera_flag = if fake.is_some() {
        SDL_InitFlags(0)
    } else {
        SDL_INIT_CAMERA
    };
    // SAFETY: SDL is initialised once, on the main thread, before any other SDL call.
    let video = unsafe {
        want_window && SDL_Init(camera_flag | SDL_INIT_VIDEO) || {
            SDL_Init(camera_flag);
            false
        }
    };
    let mut win = if video {
        Window::open("Logos QR").ok()
    } else {
        None
    };
    out.send(&Event::Ready {
        version: VERSION,
        window: win.is_some(),
        fake: fake.is_some(),
    });

    let mut st = State {
        title: "Logos QR".into(),
        interval: Duration::from_millis(200),
        ..State::default()
    };
    let mut scanning: Option<Scan> = None;
    let start_scan = |scanning: &mut Option<Scan>,
                      fake: &mut Option<(FakeLink, Option<FakeCamera>)>,
                      preview: bool| {
        if scanning.is_some() {
            return;
        }
        let cam = fake.as_mut().and_then(|(link, cam)| {
            link.scanning();
            cam.take()
        });
        *scanning = Some(scan(out.clone(), tx.clone(), cam, preview));
    };
    if probe {
        start_scan(&mut scanning, &mut fake, win.is_some());
    }

    'main: loop {
        if let Some(w) = win.as_mut() {
            // SAFETY: an all-zero event is valid; SDL fills it, and only the variant its type names is read.
            unsafe {
                let mut ev: SDL_Event = std::mem::zeroed();
                if SDL_WaitEventTimeout(&mut ev, 16) {
                    let t = SDL_EventType(ev.r#type);
                    let cancel = t == SDL_EVENT_QUIT
                        || t == SDL_EVENT_WINDOW_CLOSE_REQUESTED
                        || (t == SDL_EVENT_KEY_DOWN && ev.key.key == SDLK_ESCAPE)
                        || (t == SDL_EVENT_MOUSE_BUTTON_DOWN
                            && w.hits_cancel(ev.button.x, ev.button.y));
                    if cancel {
                        out.send(&Event::Cancelled);
                        break 'main;
                    }
                    if t == SDL_EVENT_KEY_DOWN && ev.key.key == SDLK_TAB {
                        if let Some(s) = &scanning {
                            s.switch.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        let wait = if win.is_some() {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };
        let mut first = true;
        loop {
            let msg = if first && !wait.is_zero() {
                match rx.recv_timeout(wait) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => break 'main,
                }
            } else {
                match rx.try_recv() {
                    Ok(m) => m,
                    Err(_) => break,
                }
            };
            first = false;
            match msg {
                Msg::Cmd(Command::Show {
                    title,
                    frames,
                    interval_ms,
                }) => {
                    if let Some(e) = frames.iter().find_map(|f| f.check().err()) {
                        out.error("bad_command", e);
                        continue;
                    }
                    if let Some((link, _)) = &fake {
                        link.shown(&frames, interval_ms);
                    } else if win.is_none() && !frames.is_empty() {
                        out.error(
                            "no_window",
                            "there is no window to show codes in: use a presenter",
                        );
                    }
                    if let Some(t) = title {
                        st.title = t;
                    }
                    st.frames = frames;
                    st.interval = Duration::from_millis(interval_ms.clamp(50, 5000));
                    st.since = Some(Instant::now());
                }
                Msg::Cmd(Command::Scan { title, hint }) => {
                    if let Some(t) = title {
                        st.title = t;
                    }
                    st.hint = hint;
                    let preview = win.is_some();
                    start_scan(&mut scanning, &mut fake, preview);
                }
                Msg::Cmd(Command::Progress { received, expected }) => {
                    st.progress = Some((received, expected))
                }
                Msg::Cmd(Command::Title { title }) => st.title = title,
                // A probe started by hand may have no module on its standard input.
                Msg::Eof if probe => {}
                Msg::Cmd(Command::Close) | Msg::Eof => break 'main,
                Msg::Bad(e) => out.error("bad_command", e),
                Msg::Seen { preview, any } => {
                    if preview.is_some() {
                        st.preview = preview;
                    }
                    if any {
                        st.seen_at = Some(Instant::now());
                    }
                }
                Msg::Camera(name) => {
                    st.problem = None;
                    out.send(&Event::Camera { name: name.clone() });
                    st.camera = Some(name);
                }
                Msg::CameraFailed(e) => {
                    out.error(e.code(), e.message());
                    st.problem = Some(e.message());
                }
            }
            if let Some(w) = &win {
                w.set_title(&format!("Logos QR - {}", st.title));
            }
        }
        if let Some(w) = win.as_mut() {
            let lines = st.lines(scanning.is_some());
            let seeing = st
                .seen_at
                .is_some_and(|t| t.elapsed() < Duration::from_millis(400));
            w.draw(&View {
                frame: st.frame(),
                preview: st.preview.as_ref(),
                scanning: scanning.is_some(),
                seeing,
                lines: &lines,
            });
        }
    }

    if let Some(s) = scanning.take() {
        s.stop.store(true, Ordering::Relaxed);
        let _ = s.worker.join();
    }
    drop(win);
    // SAFETY: every SDL object is gone; SDL_Quit runs once, on the main thread.
    unsafe { SDL_Quit() };
}
