//! Sessions: one caller at a time shows codes and reads codes through the helper. What a session
//! read is handed only to the holder of its token; events say no more than "this session changed".

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::frames::{self, Frame};
use crate::helper::{Helper, HelperEvent, Launcher};
use crate::render;

/// A session nobody polls for this long has lost its caller, and ends.
pub const IDLE: Duration = Duration::from_secs(30);
/// A terminal presenter that hasn't rendered for this long is gone.
pub const PRESENTER_FRESH: Duration = Duration::from_secs(10);
/// The densest code a terminal shows, whatever its size: a Keycard Shell read version 4 from a
/// terminal well and version 6 with trouble, and a large text console allows far denser ones.
pub const PRESENTER_MAX_VERSION: u8 = 5;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartParams {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub show: Option<ShowParams>,
    #[serde(default)]
    pub scan: bool,
    #[serde(default)]
    pub hint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShowParams {
    pub parts: Vec<String>,
    #[serde(default)]
    pub interval_ms: Option<u64>,
    #[serde(default)]
    pub max_version: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Cancelled,
    Failed,
    Stopped,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Cancelled => "cancelled",
            State::Failed => "failed",
            State::Stopped => "stopped",
        }
    }

    fn active(self) -> bool {
        matches!(self, State::Starting | State::Running)
    }
}

/// A session that shows a request and reads its answer does one, then the other: the camera stays
/// off while the codes are shown, and comes on when the person says the wallet shows its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Show,
    Scan,
}

const SCAN_LABEL: &str = "Scan the answer";
const BACK_LABEL: &str = "Show the request again";
const SHOW_HINT: &str =
    "Scan these codes with your wallet and confirm on it, then press Scan the answer";
const SCAN_HINT: &str = "Hold your wallet's answer up to the camera";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct Poll {
    pub state: State,
    /// `show` while the request is shown and the camera is off, `scan` once it reads.
    pub step: Step,
    pub texts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Problem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// The codes are drawn in a terminal, not a window.
    pub presenter: bool,
    /// The largest code version that terminal holds, so the caller can use smaller parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presenter_max_version: Option<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScanError {
    Busy,
    Invalid(String),
    NotAuthorized,
    Helper(String),
}

impl ScanError {
    pub fn code(&self) -> &'static str {
        match self {
            ScanError::Busy => "busy",
            ScanError::Invalid(_) => "invalid",
            ScanError::NotAuthorized => "not_authorized",
            ScanError::Helper(_) => "helper",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ScanError::Busy => "another module is using the QR scanner".into(),
            ScanError::Invalid(m) | ScanError::Helper(m) => m.clone(),
            // The same for an unknown session and someone else's: no oracle.
            ScanError::NotAuthorized => "not authorized".into(),
        }
    }
}

pub trait Notify: Send + Sync {
    fn changed(&self, session: &str, state: &str);
}

struct Session {
    id: String,
    token: String,
    title: String,
    hint: Option<String>,
    frames: Vec<Frame>,
    interval: Duration,
    since: Instant,
    scan: bool,
    headless: bool,
    state: State,
    step: Step,
    texts: Vec<String>,
    error: Option<Problem>,
    camera: Option<String>,
    progress: Option<(u32, u32)>,
    helper: Option<Box<dyn Helper>>,
    polled: Instant,
    /// An app draws it (`presenter_view`), not a terminal.
    view: bool,
    /// A code is in the camera's view.
    seeing: bool,
}

/// Who draws sessions instead of the Logos QR window, and which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawer {
    /// A terminal: any session, its codes as text sized to it.
    Terminal,
    /// The signer app, which shows what is signed: any session, a request's codes included.
    Signer,
    /// An app that reads a wallet's code for this module: only its scans, which show no codes.
    Scans(&'static str),
}

impl Drawer {
    /// An app, which draws a session itself through `presenter_view`.
    fn app(self) -> bool {
        self != Drawer::Terminal
    }

    /// A drawer's claim can't be taken by a lesser one while it is fresh.
    fn rank(self) -> u8 {
        match self {
            Drawer::Terminal => 0,
            Drawer::Scans(_) => 1,
            Drawer::Signer => 2,
        }
    }

    fn draws(self, frames: &[Frame], caller: Option<&str>) -> bool {
        match self {
            Drawer::Terminal | Drawer::Signer => true,
            Drawer::Scans(module) => frames.is_empty() && caller == Some(module),
        }
    }
}

struct Presenter {
    handle: String,
    seen: Instant,
    cols: usize,
    rows: usize,
    drawer: Drawer,
}

/// A session as an app draws it: no picture, only the codes to show and what the camera makes of
/// the answer.
#[derive(Debug, Serialize)]
pub struct View {
    /// A session is showing or reading codes, for this app to draw.
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<Step>,
    /// It is in the Logos QR window instead: it started before this app was drawing.
    pub in_window: bool,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The button's label, when the person has a step to take.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<u32>,
    /// A code is in the camera's view.
    pub seeing: bool,
    /// Names the codes below; they are sent only when the app doesn't have them yet.
    pub frames_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<Frame>>,
    pub interval_ms: u64,
}

#[derive(Default)]
struct Inner {
    seq: u64,
    session: Option<Session>,
    presenter: Option<Presenter>,
}

#[derive(Clone)]
pub struct Scanner {
    inner: Arc<Mutex<Inner>>,
    launcher: Arc<dyn Launcher>,
    notify: Arc<dyn Notify>,
}

fn token() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("the system's random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Codes whose helper can't go on.
fn fatal(code: &str) -> bool {
    code != "bad_command"
}

fn ask(label: &str, hint: &str) -> String {
    serde_json::json!({ "op": "ask", "label": label, "hint": hint }).to_string()
}

impl Session {
    /// Shows a request and reads its answer, one step at a time.
    fn two_step(&self) -> bool {
        self.scan && !self.frames.is_empty()
    }

    /// The person's word: from the request to its answer, the camera on, or back.
    fn press(&mut self) -> bool {
        if !self.two_step() || !self.state.active() {
            return false;
        }
        let Some(h) = self.helper.as_mut() else {
            return false;
        };
        let windowed = !self.headless;
        let mut lines = Vec::new();
        match self.step {
            Step::Show => {
                self.step = Step::Scan;
                lines.push(serde_json::json!({ "op": "scan", "hint": SCAN_HINT }).to_string());
                if windowed {
                    lines.push(serde_json::json!({ "op": "show", "frames": [] }).to_string());
                }
                lines.push(ask(BACK_LABEL, SCAN_HINT));
            }
            Step::Scan => {
                self.step = Step::Show;
                self.since = Instant::now();
                lines.push(serde_json::json!({ "op": "stop_scan" }).to_string());
                if windowed {
                    let show = serde_json::json!({ "op": "show", "frames": self.frames, "interval_ms": self.interval.as_millis() as u64 });
                    lines.push(show.to_string());
                }
                lines.push(ask(SCAN_LABEL, SHOW_HINT));
            }
        }
        for l in lines {
            let _ = h.send(&l);
        }
        true
    }
}

impl Scanner {
    pub fn new(launcher: Arc<dyn Launcher>, notify: Arc<dyn Notify>) -> Scanner {
        Scanner {
            inner: Arc::new(Mutex::new(Inner::default())),
            launcher,
            notify,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `caller` is the asking module's name, for the window's title, when the core names it.
    pub fn start(
        &self,
        p: StartParams,
        caller: Option<&str>,
    ) -> Result<(String, String), ScanError> {
        let show = p.show.unwrap_or(ShowParams {
            parts: Vec::new(),
            interval_ms: None,
            max_version: None,
        });
        if show.parts.is_empty() && !p.scan {
            return Err(ScanError::Invalid(
                "nothing to do: give parts to show, scan, or both".into(),
            ));
        }
        let max = show.max_version.unwrap_or(frames::MAX_VERSION);
        let frames = frames::encode(&show.parts, max).map_err(ScanError::Invalid)?;
        let mut stale = None;
        let (id, token, headless) = {
            let mut inner = self.lock();
            if let Some(s) = inner.session.as_mut() {
                if s.state.active() && s.polled.elapsed() > IDLE {
                    s.state = State::Stopped;
                    stale = s.helper.take();
                } else if s.state.active() {
                    return Err(ScanError::Busy);
                }
            }
            inner.seq += 1;
            let id = format!("qrs_{}", inner.seq);
            let token = token();
            let drawer = inner
                .presenter
                .as_ref()
                .filter(|p| p.seen.elapsed() < PRESENTER_FRESH)
                .map(|p| p.drawer)
                .filter(|d| d.draws(&frames, caller));
            let (headless, view) = (drawer.is_some(), drawer.is_some_and(Drawer::app));
            let title = match (p.title, caller) {
                (Some(t), Some(c)) => format!("{t} ({c})"),
                (Some(t), None) => t,
                (None, c) => c.unwrap_or("QR codes").to_string(),
            };
            inner.session = Some(Session {
                id: id.clone(),
                token: token.clone(),
                title,
                hint: p.hint,
                frames,
                interval: Duration::from_millis(show.interval_ms.unwrap_or(200).clamp(50, 5000)),
                since: Instant::now(),
                scan: p.scan,
                headless,
                state: State::Starting,
                step: if show.parts.is_empty() {
                    Step::Scan
                } else {
                    Step::Show
                },
                texts: Vec::new(),
                error: None,
                camera: None,
                progress: None,
                helper: None,
                polled: Instant::now(),
                view,
                seeing: false,
            });
            (id, token, headless)
        };
        if let Some(mut h) = stale {
            h.close();
        }
        let sink = {
            let weak: Weak<Mutex<Inner>> = Arc::downgrade(&self.inner);
            let notify = self.notify.clone();
            let id = id.clone();
            Box::new(move |ev| on_event(&weak, &*notify, &id, ev))
        };
        let mut helper = match self.launcher.launch(headless, sink) {
            Ok(h) => h,
            Err(e) => {
                if let Some(s) = self.lock().session.as_mut().filter(|s| s.id == id) {
                    s.state = State::Failed;
                    s.error = Some(Problem {
                        code: "helper".into(),
                        message: e.clone(),
                    });
                }
                return Err(ScanError::Helper(e));
            }
        };
        let (title, hint, frames, interval, scan) = {
            let inner = self.lock();
            let s = inner
                .session
                .as_ref()
                .filter(|s| s.id == id)
                .ok_or_else(|| ScanError::Helper("the session ended".into()))?;
            (
                s.title.clone(),
                s.hint.clone(),
                s.frames.clone(),
                s.interval,
                s.scan,
            )
        };
        let mut sent =
            helper.send(&serde_json::json!({ "op": "title", "title": title }).to_string());
        if !frames.is_empty() && !headless {
            let show = serde_json::json!({ "op": "show", "frames": frames, "interval_ms": interval.as_millis() as u64 });
            sent = sent.and_then(|_| helper.send(&show.to_string()));
        }
        if scan && !frames.is_empty() {
            // The request first, the camera off, until the person says the answer is up.
            sent = sent.and_then(|_| helper.send(&ask(SCAN_LABEL, SHOW_HINT)));
        } else if scan {
            sent = sent.and_then(|_| {
                helper.send(&serde_json::json!({ "op": "scan", "hint": hint }).to_string())
            });
        }
        if let Err(e) = sent {
            helper.close();
            return Err(ScanError::Helper(e));
        }
        if let Some(s) = self.lock().session.as_mut().filter(|s| s.id == id) {
            s.helper = Some(helper);
        }
        Ok((id, token))
    }

    fn with_session<T>(
        &self,
        id: &str,
        token: &str,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, ScanError> {
        let mut inner = self.lock();
        match inner.session.as_mut() {
            Some(s) if s.id == id && s.token == token => Ok(f(s)),
            _ => Err(ScanError::NotAuthorized),
        }
    }

    pub fn poll(&self, id: &str, token: &str) -> Result<Poll, ScanError> {
        let max_version = self.presenter_max_version();
        self.with_session(id, token, |s| {
            s.polled = Instant::now();
            Poll {
                state: s.state,
                step: s.step,
                texts: std::mem::take(&mut s.texts),
                error: s.error.clone(),
                camera: s.camera.clone(),
                presenter: s.headless,
                presenter_max_version: if s.headless { max_version } else { None },
            }
        })
    }

    pub fn progress(
        &self,
        id: &str,
        token: &str,
        received: u32,
        expected: u32,
    ) -> Result<(), ScanError> {
        self.with_session(id, token, |s| {
            s.polled = Instant::now();
            s.progress = Some((received, expected));
            if let Some(h) = s.helper.as_mut() {
                let _ = h.send(&serde_json::json!({ "op": "progress", "received": received, "expected": expected }).to_string());
            }
        })
    }

    pub fn stop(&self, id: &str, token: &str) -> Result<(), ScanError> {
        let helper = self.with_session(id, token, |s| {
            if s.state.active() {
                s.state = State::Stopped;
            }
            s.helper.take()
        })?;
        if let Some(mut h) = helper {
            h.close();
        }
        self.notify.changed(id, State::Stopped.as_str());
        Ok(())
    }

    /// Claim the drawing of sessions. A lesser drawer can't take a fresh claim: a terminal not an
    /// app's, an app reading wallets' codes not the signer app's.
    pub fn presenter_attach(&self, drawer: Drawer) -> Result<String, ScanError> {
        let mut inner = self.lock();
        if inner
            .presenter
            .as_ref()
            .is_some_and(|p| p.drawer.rank() > drawer.rank() && p.seen.elapsed() < PRESENTER_FRESH)
        {
            return Err(ScanError::Busy);
        }
        let handle = format!("prs_{}.{}", inner.seq, token());
        inner.presenter = Some(Presenter {
            handle: handle.clone(),
            seen: Instant::now(),
            cols: 80,
            rows: 24,
            drawer,
        });
        Ok(handle)
    }

    /// The session as an app draws it, renewing its claim. `frames_id` names the codes the
    /// app already has, so they are sent once a session.
    pub fn presenter_view(&self, handle: &str, frames_id: &str) -> Result<View, ScanError> {
        let mut inner = self.lock();
        match inner.presenter.as_mut() {
            Some(p) if p.handle == handle && p.drawer.app() => p.seen = Instant::now(),
            _ => return Err(ScanError::NotAuthorized),
        }
        let Some(s) = inner
            .session
            .as_ref()
            .filter(|s| s.state.active() || s.view)
        else {
            return Ok(View {
                active: false,
                state: None,
                step: None,
                in_window: false,
                title: String::new(),
                hint: None,
                label: None,
                received: None,
                expected: None,
                seeing: false,
                frames_id: String::new(),
                frames: None,
                interval_ms: 0,
            });
        };
        let mine = s.view && s.state.active();
        let (hint, label) = match (s.two_step(), s.step) {
            (true, Step::Show) => (Some(SHOW_HINT.to_string()), Some(SCAN_LABEL)),
            (true, Step::Scan) => (Some(SCAN_HINT.to_string()), Some(BACK_LABEL)),
            (false, _) => (s.hint.clone(), None),
        };
        Ok(View {
            active: mine,
            state: Some(s.state),
            step: Some(s.step),
            in_window: s.state.active() && !s.headless,
            title: s.title.clone(),
            hint,
            label: label.filter(|_| mine),
            received: s.progress.map(|p| p.0),
            expected: s.progress.map(|p| p.1),
            seeing: s.seeing,
            frames_id: s.id.clone(),
            frames: (mine && s.id != frames_id && !s.frames.is_empty()).then(|| s.frames.clone()),
            interval_ms: s.interval.as_millis() as u64,
        })
    }

    /// An app's Cancel: ends the session it draws. Whether one was running.
    pub fn presenter_cancel(&self, handle: &str) -> Result<bool, ScanError> {
        let (helper, id) = {
            let mut inner = self.lock();
            match inner.presenter.as_mut() {
                Some(p) if p.handle == handle && p.drawer.app() => p.seen = Instant::now(),
                _ => return Err(ScanError::NotAuthorized),
            }
            match inner
                .session
                .as_mut()
                .filter(|s| s.view && s.state.active())
            {
                Some(s) => {
                    s.state = State::Cancelled;
                    (s.helper.take(), s.id.clone())
                }
                None => return Ok(false),
            }
        };
        if let Some(mut h) = helper {
            h.close();
        }
        self.notify.changed(&id, State::Cancelled.as_str());
        Ok(true)
    }

    pub fn presenter_detach(&self, handle: &str) -> Result<(), ScanError> {
        let mut inner = self.lock();
        match &inner.presenter {
            Some(p) if p.handle == handle => {
                inner.presenter = None;
                Ok(())
            }
            _ => Err(ScanError::NotAuthorized),
        }
    }

    /// A key pressed in the presenter's terminal: from the request to its answer, or back. Whether
    /// a session took it.
    pub fn presenter_press(&self, handle: &str) -> Result<bool, ScanError> {
        let (pressed, id) = {
            let mut inner = self.lock();
            match inner.presenter.as_mut() {
                Some(p) if p.handle == handle => p.seen = Instant::now(),
                _ => return Err(ScanError::NotAuthorized),
            }
            match inner.session.as_mut() {
                Some(s) => (s.press(), s.id.clone()),
                None => (false, String::new()),
            }
        };
        if pressed {
            self.notify.changed(&id, State::Running.as_str());
        }
        Ok(pressed)
    }

    pub fn presenter_render(
        &self,
        handle: &str,
        cols: usize,
        rows: usize,
    ) -> Result<String, ScanError> {
        let mut inner = self.lock();
        match inner.presenter.as_mut() {
            Some(p) if p.handle == handle => {
                p.seen = Instant::now();
                p.cols = cols;
                p.rows = rows;
            }
            _ => return Err(ScanError::NotAuthorized),
        }
        let Some(s) = inner.session.as_ref().filter(|s| s.state.active()) else {
            return Ok("Waiting for a QR session... (Ctrl-C to stop)".into());
        };
        let hint = match (s.two_step(), s.step) {
            (true, Step::Show) => Some(
                "Scan these codes with your wallet and confirm on it, then press Enter to scan its answer",
            ),
            (true, Step::Scan) => Some(
                "Hold your wallet's answer up to the camera. Enter shows the request again",
            ),
            (false, _) => s.hint.as_deref(),
        };
        let mut status = s.title.clone();
        if let Some(h) = hint {
            status = format!("{status}: {h}");
        }
        if let Some((got, of)) = s.progress {
            status = format!("{status}  -  read {got} of {of}");
        }
        if !s.headless {
            return Ok(format!("{status}\nContinue in the Logos QR window."));
        }
        if s.frames.is_empty() || s.step == Step::Scan {
            return Ok(format!("{status}\nThe camera is on."));
        }
        let i = (s.since.elapsed().as_millis() / s.interval.as_millis().max(1)) as usize
            % s.frames.len();
        Ok(render::frame(&s.frames[i], cols, rows, &status))
    }

    fn presenter_max_version(&self) -> Option<u8> {
        let inner = self.lock();
        // A terminal's size limits its codes; the signing app draws them as large as they come.
        let p = inner
            .presenter
            .as_ref()
            .filter(|p| p.seen.elapsed() < PRESENTER_FRESH && p.drawer == Drawer::Terminal)?;
        (1..=PRESENTER_MAX_VERSION)
            .rev()
            .find(|v| render::fits(frames::size_of(*v) + 4, p.cols, p.rows))
    }
}

fn on_event(inner: &Weak<Mutex<Inner>>, notify: &dyn Notify, id: &str, ev: HelperEvent) {
    let Some(inner) = inner.upgrade() else { return };
    let changed = {
        let mut inner = inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = inner.session.as_mut().filter(|s| s.id == id) else {
            return;
        };
        let before = (s.state, s.step);
        match ev {
            HelperEvent::Pressed => {
                s.press();
            }
            HelperEvent::Ready { .. } => {
                if s.state == State::Starting {
                    s.state = State::Running;
                }
            }
            HelperEvent::Camera { name } => s.camera = Some(name),
            HelperEvent::Text { text } => s.texts.push(text),
            HelperEvent::Seeing { code } => s.seeing = code,
            HelperEvent::Cancelled => {
                if s.state.active() {
                    s.state = State::Cancelled;
                }
            }
            HelperEvent::Error { code, message } => {
                if fatal(&code) && s.state.active() {
                    s.state = State::Failed;
                }
                s.error = Some(Problem { code, message });
            }
            HelperEvent::Exited => {
                if s.state.active() {
                    s.state = State::Failed;
                    s.error.get_or_insert(Problem {
                        code: "helper".into(),
                        message: "the QR helper exited".into(),
                    });
                }
            }
        }
        ((s.state, s.step) != before || !s.texts.is_empty()).then_some(s.state)
    };
    if let Some(state) = changed {
        notify.changed(id, state.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Sink;

    /// A helper that records what it was sent and hands its sink to the test.
    #[derive(Default, Clone)]
    struct Fake {
        sent: Arc<Mutex<Vec<String>>>,
        sinks: Arc<Mutex<Vec<(bool, Sink)>>>,
        closed: Arc<Mutex<u32>>,
    }

    struct FakeHelper(Fake);

    impl Helper for FakeHelper {
        fn send(&mut self, line: &str) -> Result<(), String> {
            self.0.sent.lock().unwrap().push(line.to_string());
            Ok(())
        }
        fn close(&mut self) {
            *self.0.closed.lock().unwrap() += 1;
        }
    }

    impl Launcher for Fake {
        fn launch(&self, headless: bool, sink: Sink) -> Result<Box<dyn Helper>, String> {
            self.sinks.lock().unwrap().push((headless, sink));
            Ok(Box::new(FakeHelper(self.clone())))
        }
    }

    impl Fake {
        fn emit(&self, ev: HelperEvent) {
            let sinks = self.sinks.lock().unwrap();
            (sinks.last().unwrap().1)(ev);
        }
    }

    #[derive(Default)]
    struct Heard(Mutex<Vec<(String, String)>>);

    impl Notify for Heard {
        fn changed(&self, session: &str, state: &str) {
            self.0.lock().unwrap().push((session.into(), state.into()));
        }
    }

    fn scanner() -> (Scanner, Fake, Arc<Heard>) {
        let fake = Fake::default();
        let heard = Arc::new(Heard::default());
        (
            Scanner::new(Arc::new(fake.clone()), heard.clone()),
            fake,
            heard,
        )
    }

    fn params(json: &str) -> StartParams {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_session_shows_scans_and_hands_texts_to_its_holder_only() {
        let (sc, fake, heard) = scanner();
        let (id, token) = sc
            .start(
                params(
                    r#"{"title":"Sign","show":{"parts":["UR:BYTES/AA"]},"scan":true,"hint":"h"}"#,
                ),
                Some("qr_signer_module"),
            )
            .unwrap();
        let sent = fake.sent.lock().unwrap().clone();
        assert!(sent[0].contains(r#""op":"title""#) && sent[0].contains("Sign (qr_signer_module)"));
        assert!(sent[1].contains(r#""op":"show""#));
        // The request first, the camera off, and a button for when the answer is up.
        assert!(sent[2].contains(r#""op":"ask""#) && sent[2].contains("Scan the answer"));
        assert_eq!(sent.len(), 3);
        assert_eq!(sc.poll(&id, &token).unwrap().step, Step::Show);

        fake.emit(HelperEvent::Ready { window: true });
        fake.emit(HelperEvent::Pressed);
        let sent = fake.sent.lock().unwrap()[3..].to_vec();
        assert!(sent[0].contains(r#""op":"scan""#));
        assert!(
            sent[1].contains(r#""frames":[]"#),
            "the codes go: {}",
            sent[1]
        );
        assert!(sent[2].contains("Show the request again"));
        assert_eq!(sc.poll(&id, &token).unwrap().step, Step::Scan);
        // And back: the camera off, the codes again.
        fake.emit(HelperEvent::Pressed);
        let sent = fake.sent.lock().unwrap()[6..].to_vec();
        assert!(sent[0].contains(r#""op":"stop_scan""#));
        assert!(sent[1].contains(r#""bits""#));
        assert_eq!(sc.poll(&id, &token).unwrap().step, Step::Show);
        fake.emit(HelperEvent::Pressed);

        fake.emit(HelperEvent::Text {
            text: "UR:CRYPTO-PSBT/1-2/X".into(),
        });
        assert_eq!(sc.poll(&id, "wrong").unwrap_err(), ScanError::NotAuthorized);
        let p = sc.poll(&id, &token).unwrap();
        assert_eq!(p.state, State::Running);
        assert_eq!(p.texts, vec!["UR:CRYPTO-PSBT/1-2/X".to_string()]);
        assert!(sc.poll(&id, &token).unwrap().texts.is_empty());
        // Events carry the session and its state, never what it read.
        assert!(heard
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|(s, st)| s == &id && !st.contains("UR:")));

        sc.stop(&id, &token).unwrap();
        assert_eq!(*fake.closed.lock().unwrap(), 1);
        assert_eq!(sc.poll(&id, &token).unwrap().state, State::Stopped);
    }

    #[test]
    fn one_session_at_a_time() {
        let (sc, _, _) = scanner();
        let (id, token) = sc.start(params(r#"{"scan":true}"#), Some("a")).unwrap();
        assert_eq!(
            sc.start(params(r#"{"scan":true}"#), Some("b")).unwrap_err(),
            ScanError::Busy
        );
        sc.stop(&id, &token).unwrap();
        assert!(sc.start(params(r#"{"scan":true}"#), Some("b")).is_ok());
    }

    #[test]
    fn cancel_and_failures_end_the_session() {
        let (sc, fake, _) = scanner();
        let (id, token) = sc.start(params(r#"{"scan":true}"#), Some("a")).unwrap();
        fake.emit(HelperEvent::Cancelled);
        assert_eq!(sc.poll(&id, &token).unwrap().state, State::Cancelled);

        let (id, token) = sc.start(params(r#"{"scan":true}"#), Some("a")).unwrap();
        fake.emit(HelperEvent::Error {
            code: "camera_denied".into(),
            message: "off".into(),
        });
        let p = sc.poll(&id, &token).unwrap();
        assert_eq!(p.state, State::Failed);
        assert_eq!(p.error.unwrap().code, "camera_denied");

        let (id, token) = sc.start(params(r#"{"scan":true}"#), Some("a")).unwrap();
        fake.emit(HelperEvent::Exited);
        assert_eq!(sc.poll(&id, &token).unwrap().state, State::Failed);
    }

    #[test]
    fn nothing_to_do_and_oversized_parts_are_refused() {
        let (sc, _, _) = scanner();
        assert!(matches!(
            sc.start(params("{}"), Some("a")),
            Err(ScanError::Invalid(_))
        ));
        let long = format!(
            r#"{{"show":{{"parts":["{}"],"max_version":5}}}}"#,
            "A".repeat(500)
        );
        assert!(matches!(
            sc.start(params(&long), Some("a")),
            Err(ScanError::Invalid(_))
        ));
        assert!(serde_json::from_str::<StartParams>(r#"{"nope":1}"#).is_err());
    }

    #[test]
    fn a_presenter_takes_the_frames_and_the_helper_reads_without_a_window() {
        let (sc, fake, _) = scanner();
        let h = sc.presenter_attach(Drawer::Terminal).unwrap();
        assert!(sc
            .presenter_render(&h, 100, 50)
            .unwrap()
            .starts_with("Waiting"));
        assert_eq!(
            sc.presenter_view(&h, "").unwrap_err(),
            ScanError::NotAuthorized,
            "a terminal gets text, not the session"
        );
        let (id, token) = sc
            .start(
                params(r#"{"show":{"parts":["UR:BYTES/AA"]},"scan":true}"#),
                Some("a"),
            )
            .unwrap();
        let (headless, _) = &fake.sinks.lock().unwrap()[0];
        assert!(*headless);
        assert!(!fake
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(r#""op":"show""#)));
        let text = sc.presenter_render(&h, 100, 50).unwrap();
        assert!(text.contains("\x1b[48;5;16m"), "{text}");
        assert!(text.contains("press Enter to scan its answer"), "{text}");
        assert!(!fake
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(r#""op":"scan""#)));
        // Enter in the terminal: the camera on, the code gone.
        assert_eq!(sc.presenter_press(&h), Ok(true));
        assert!(fake
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(r#""op":"scan""#)));
        let text = sc.presenter_render(&h, 100, 50).unwrap();
        assert!(
            text.contains("The camera is on.") && !text.contains("\x1b[48;5;16m"),
            "{text}"
        );
        assert_eq!(sc.presenter_press(&h), Ok(true));
        assert!(sc
            .presenter_render(&h, 100, 50)
            .unwrap()
            .contains("\x1b[48;5;16m"));
        assert_eq!(
            sc.presenter_press("prs_0.nope"),
            Err(ScanError::NotAuthorized)
        );
        assert!(sc.presenter_render("prs_0.nope", 100, 50).is_err());
        let p = sc.poll(&id, &token).unwrap();
        assert!(p.presenter);
        // Two columns and one row a module: 100 by 50 would hold a version 6 code and its margin,
        // but a terminal shows no denser than version 5.
        assert_eq!(p.presenter_max_version, Some(5));
        // A small terminal still limits it: 70 by 35 holds 33 modules (version 3 and its margin) and
        // the status lines.
        sc.presenter_render(&h, 70, 35).unwrap();
        assert_eq!(sc.poll(&id, &token).unwrap().presenter_max_version, Some(3));
        sc.presenter_detach(&h).unwrap();
        assert!(sc.presenter_render(&h, 100, 50).is_err());
    }

    #[test]
    fn the_signing_app_draws_the_session_itself() {
        let (sc, fake, heard) = scanner();
        let h = sc.presenter_attach(Drawer::Signer).unwrap();
        assert!(!sc.presenter_view(&h, "").unwrap().active);
        assert_eq!(
            sc.presenter_attach(Drawer::Terminal),
            Err(ScanError::Busy),
            "a terminal can't take the claim from it"
        );
        let (id, token) = sc
            .start(
                params(r#"{"show":{"parts":["UR:BYTES/AA"],"interval_ms":250},"scan":true}"#),
                Some("qr_signer_module"),
            )
            .unwrap();
        assert!(fake.sinks.lock().unwrap()[0].0, "no window");

        let v = sc.presenter_view(&h, "").unwrap();
        assert!(v.active && !v.in_window);
        assert_eq!((v.step, v.label), (Some(Step::Show), Some(SCAN_LABEL)));
        assert_eq!(v.frames.as_ref().map(Vec::len), Some(1));
        assert_eq!(v.interval_ms, 250);
        // The codes once a session: an app that has them gets the rest only.
        assert!(sc
            .presenter_view(&h, &v.frames_id)
            .unwrap()
            .frames
            .is_none());
        // Its own size doesn't shrink the codes, as a terminal's does.
        assert_eq!(sc.poll(&id, &token).unwrap().presenter_max_version, None);

        assert_eq!(sc.presenter_press(&h), Ok(true));
        fake.emit(HelperEvent::Seeing { code: true });
        sc.progress(&id, &token, 3, 10).unwrap();
        let v = sc.presenter_view(&h, &v.frames_id).unwrap();
        assert_eq!((v.step, v.label), (Some(Step::Scan), Some(BACK_LABEL)));
        assert!(v.seeing);
        assert_eq!((v.received, v.expected), (Some(3), Some(10)));

        assert_eq!(sc.presenter_cancel(&h), Ok(true));
        assert_eq!(sc.poll(&id, &token).unwrap().state, State::Cancelled);
        assert_eq!(*fake.closed.lock().unwrap(), 1);
        assert!(heard.0.lock().unwrap().contains(&(id, "cancelled".into())));
        let v = sc.presenter_view(&h, "").unwrap();
        assert_eq!((v.active, v.state), (false, Some(State::Cancelled)));
        assert_eq!(sc.presenter_cancel(&h), Ok(false), "nothing left to cancel");
        assert_eq!(
            sc.presenter_cancel("prs_0.nope"),
            Err(ScanError::NotAuthorized)
        );
    }

    #[test]
    fn a_session_the_window_shows_is_left_to_it() {
        let (sc, _, _) = scanner();
        sc.start(
            params(r#"{"show":{"parts":["UR:BYTES/AA"]},"scan":true}"#),
            Some("a"),
        )
        .unwrap();
        let h = sc.presenter_attach(Drawer::Signer).unwrap();
        let v = sc.presenter_view(&h, "").unwrap();
        assert!(v.in_window && !v.active && v.frames.is_none() && v.label.is_none());
        assert_eq!(
            sc.presenter_cancel(&h),
            Ok(false),
            "not the app's to cancel"
        );
    }

    #[test]
    fn the_wallets_app_draws_its_modules_scans_only() {
        let wallets = Drawer::Scans("qr_signer_module");
        let (sc, fake, _) = scanner();
        let h = sc.presenter_attach(wallets).unwrap();
        assert_eq!(
            sc.presenter_attach(Drawer::Terminal),
            Err(ScanError::Busy),
            "a terminal can't take its claim"
        );

        // Its module's account scan: no window, drawn by the app.
        let (id, token) = sc
            .start(
                params(r#"{"scan":true,"hint":"h"}"#),
                Some("qr_signer_module"),
            )
            .unwrap();
        assert!(fake.sinks.lock().unwrap()[0].0, "no window");
        fake.emit(HelperEvent::Seeing { code: true });
        let v = sc.presenter_view(&h, "").unwrap();
        assert!(v.active && v.seeing && v.frames.is_none());
        assert_eq!((v.step, v.hint.as_deref()), (Some(Step::Scan), Some("h")));
        assert_eq!(sc.presenter_cancel(&h), Ok(true));
        assert_eq!(sc.poll(&id, &token).unwrap().state, State::Cancelled);

        // A request's codes are never its to draw: they go to the window.
        sc.presenter_view(&h, "").unwrap();
        sc.start(
            params(r#"{"show":{"parts":["UR:BYTES/AA"]},"scan":true}"#),
            Some("qr_signer_module"),
        )
        .unwrap();
        assert!(!fake.sinks.lock().unwrap()[1].0, "in the window");
        let v = sc.presenter_view(&h, "").unwrap();
        assert!(!v.active && v.in_window && v.frames.is_none());

        // The signer app takes the claim; the wallets app can't take it back while it draws.
        let s = sc.presenter_attach(Drawer::Signer).unwrap();
        assert_eq!(sc.presenter_attach(wallets), Err(ScanError::Busy));
        assert_eq!(
            sc.presenter_view(&h, "").unwrap_err(),
            ScanError::NotAuthorized
        );
        sc.presenter_detach(&s).unwrap();
    }

    #[test]
    fn another_modules_scan_is_not_the_wallets_apps() {
        let (sc, fake, _) = scanner();
        sc.presenter_attach(Drawer::Scans("qr_signer_module"))
            .unwrap();
        sc.start(params(r#"{"scan":true}"#), Some("some_wallet"))
            .unwrap();
        assert!(!fake.sinks.lock().unwrap()[0].0, "in the window");
    }

    #[test]
    fn a_session_nobody_polls_gives_way() {
        let (sc, _, _) = scanner();
        let (id, _) = sc.start(params(r#"{"scan":true}"#), Some("a")).unwrap();
        sc.lock().session.as_mut().unwrap().polled = Instant::now() - IDLE - Duration::from_secs(1);
        let (id2, _) = sc.start(params(r#"{"scan":true}"#), Some("b")).unwrap();
        assert_ne!(id, id2);
    }
}
