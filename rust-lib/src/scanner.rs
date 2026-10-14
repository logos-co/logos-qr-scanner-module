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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct Poll {
    pub state: State,
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
    texts: Vec<String>,
    error: Option<Problem>,
    camera: Option<String>,
    progress: Option<(u32, u32)>,
    helper: Option<Box<dyn Helper>>,
    polled: Instant,
}

struct Presenter {
    handle: String,
    seen: Instant,
    cols: usize,
    rows: usize,
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
            let headless = inner
                .presenter
                .as_ref()
                .is_some_and(|p| p.seen.elapsed() < PRESENTER_FRESH);
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
                texts: Vec::new(),
                error: None,
                camera: None,
                progress: None,
                helper: None,
                polled: Instant::now(),
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
        if scan {
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

    pub fn presenter_attach(&self) -> String {
        let handle = format!("prs_{}.{}", self.lock().seq, token());
        self.lock().presenter = Some(Presenter {
            handle: handle.clone(),
            seen: Instant::now(),
            cols: 80,
            rows: 24,
        });
        handle
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
        let mut status = s.title.clone();
        if let Some(h) = &s.hint {
            status = format!("{status}: {h}");
        }
        if let Some((got, of)) = s.progress {
            status = format!("{status}  -  read {got} of {of}");
        }
        if s.frames.is_empty() || !s.headless {
            let place = if s.headless {
                "the camera"
            } else {
                "the Logos QR window"
            };
            return Ok(format!("{status}\nContinue in {place}."));
        }
        let i = (s.since.elapsed().as_millis() / s.interval.as_millis().max(1)) as usize
            % s.frames.len();
        Ok(render::frame(&s.frames[i], cols, rows, &status))
    }

    fn presenter_max_version(&self) -> Option<u8> {
        let inner = self.lock();
        let p = inner
            .presenter
            .as_ref()
            .filter(|p| p.seen.elapsed() < PRESENTER_FRESH)?;
        (1..=frames::MAX_VERSION)
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
        let before = s.state;
        match ev {
            HelperEvent::Ready { .. } => {
                if s.state == State::Starting {
                    s.state = State::Running;
                }
            }
            HelperEvent::Camera { name } => s.camera = Some(name),
            HelperEvent::Text { text } => s.texts.push(text),
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
        (s.state != before || !s.texts.is_empty()).then_some(s.state)
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
        assert!(sent[2].contains(r#""op":"scan""#));

        fake.emit(HelperEvent::Ready { window: true });
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
        let h = sc.presenter_attach();
        assert!(sc
            .presenter_render(&h, 100, 50)
            .unwrap()
            .starts_with("Waiting"));
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
        assert!(text.contains('█'), "{text}");
        assert!(sc.presenter_render("prs_0.nope", 100, 50).is_err());
        let p = sc.poll(&id, &token).unwrap();
        assert!(p.presenter);
        assert_eq!(p.presenter_max_version, Some(19));
        sc.presenter_detach(&h).unwrap();
        assert!(sc.presenter_render(&h, 100, 50).is_err());
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
