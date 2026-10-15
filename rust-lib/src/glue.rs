//! Logos module glue for `qr_scanner_module`. The builder derives the `.lidl` from the
//! `QrScannerModule` trait and injects the generated scaffold at the `include!` point.
//!
//! The session methods answer JSON: `{ "ok": true, ... }` or `{ "ok": false, "code", "error" }`.
//! The presenter's answer plain text, so a shell script can print them as they are.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde_json::json;

use crate::helper::{Helper, Launcher, ProcessLauncher, Sink};
use crate::scanner::{Notify, ScanError, Scanner, StartParams};

pub trait QrScannerModule: Send + Sync + 'static {
    /// `{ ok, version, helper }`: the module's version and the helper it runs.
    fn scanner_info(&self) -> String;
    /// Start a session: `{ title?, show?: { parts, interval_ms?, max_version? }, scan, hint? }` →
    /// `{ ok, session, token }`.
    fn start(&self, params_json: String) -> String;
    /// `{ ok, state, step, texts, error?, camera?, presenter, presenter_max_version? }`; texts once
    /// each. `step` is `show` while a request is shown and the camera is off, then `scan`.
    fn poll(&self, session: String, token: String) -> String;
    /// What the caller made of the codes so far: `{ received, expected }`.
    fn progress(&self, session: String, token: String, params_json: String) -> String;
    fn stop(&self, session: String, token: String) -> String;

    // ── a terminal presenter, for tools/qr-present ──────────────────────
    /// A presenter's handle, as plain text.
    fn presenter_attach(&self) -> String;
    /// The frame due now, as terminal text; each call renews the presenter's claim.
    fn presenter_render(&self, handle: String, cols: i64, rows: i64) -> String;
    /// A key pressed in the terminal: from the request shown to reading its answer, or back.
    /// `pressed`, or `nothing to press` when no session shows a request and reads its answer.
    fn presenter_press(&self, handle: String) -> String;
    fn presenter_detach(&self, handle: String) -> String;

    fn on_context_ready(&self, _ctx: &RustModuleContext) {}
}

pub trait QrScannerModuleEvents {
    fn scanner_changed(&self, session: String, state: String);
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/generated/provider_gen.rs"
));

struct Emit;

impl Notify for Emit {
    fn changed(&self, session: &str, state: &str) {
        emit_scanner_changed(session, state);
    }
}

/// Finds the helper beside the plugin, which the host names only once the context is ready.
struct Beside(OnceLock<ProcessLauncher>);

impl Launcher for Beside {
    fn launch(&self, headless: bool, sink: Sink) -> Result<Box<dyn Helper>, String> {
        let launcher = self.0.get_or_init(|| {
            let dir = context()
                .map(|c| PathBuf::from(c.module_path))
                .unwrap_or_default();
            ProcessLauncher::new(&dir)
        });
        launcher.launch(headless, sink)
    }
}

struct QrScannerModuleImpl {
    scanner: Scanner,
    launcher: Arc<Beside>,
}

impl Default for QrScannerModuleImpl {
    fn default() -> Self {
        let launcher = Arc::new(Beside(OnceLock::new()));
        QrScannerModuleImpl {
            scanner: Scanner::new(launcher.clone(), Arc::new(Emit)),
            launcher,
        }
    }
}

fn fail(e: ScanError) -> String {
    json!({ "ok": false, "code": e.code(), "error": e.message() }).to_string()
}

fn caller() -> Option<String> {
    match logos_rust_sdk::current_caller() {
        logos_rust_sdk::LogosCaller::Module { name, .. } => Some(name),
        logos_rust_sdk::LogosCaller::Derived { parent, leaf } => Some(format!("{parent}/{leaf}")),
        logos_rust_sdk::LogosCaller::Operator { name } => Some(name),
        _ => None,
    }
}

impl QrScannerModule for QrScannerModuleImpl {
    fn scanner_info(&self) -> String {
        let helper = self.launcher.0.get().map(|l| l.path.display().to_string());
        json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "helper": helper }).to_string()
    }

    fn start(&self, params_json: String) -> String {
        let params: StartParams = match serde_json::from_str(&params_json) {
            Ok(p) => p,
            Err(e) => return fail(ScanError::Invalid(format!("params: {e}"))),
        };
        match self.scanner.start(params, caller().as_deref()) {
            Ok((session, token)) => {
                json!({ "ok": true, "session": session, "token": token }).to_string()
            }
            Err(e) => fail(e),
        }
    }

    fn poll(&self, session: String, token: String) -> String {
        match self.scanner.poll(&session, &token) {
            Ok(p) => {
                let mut v = json!({ "ok": true });
                if let (Some(out), Ok(serde_json::Value::Object(fields))) =
                    (v.as_object_mut(), serde_json::to_value(p))
                {
                    out.extend(fields);
                }
                v.to_string()
            }
            Err(e) => fail(e),
        }
    }

    fn progress(&self, session: String, token: String, params_json: String) -> String {
        let v: serde_json::Value = serde_json::from_str(&params_json).unwrap_or_default();
        let num = |k: &str| v.get(k).and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        match self
            .scanner
            .progress(&session, &token, num("received"), num("expected"))
        {
            Ok(()) => json!({ "ok": true }).to_string(),
            Err(e) => fail(e),
        }
    }

    fn stop(&self, session: String, token: String) -> String {
        match self.scanner.stop(&session, &token) {
            Ok(()) => json!({ "ok": true }).to_string(),
            Err(e) => fail(e),
        }
    }

    fn presenter_attach(&self) -> String {
        self.scanner.presenter_attach()
    }

    fn presenter_render(&self, handle: String, cols: i64, rows: i64) -> String {
        let (cols, rows) = (cols.clamp(10, 1000) as usize, rows.clamp(5, 1000) as usize);
        match self.scanner.presenter_render(&handle, cols, rows) {
            Ok(text) => text,
            Err(e) => format!("error: {}", e.message()),
        }
    }

    fn presenter_press(&self, handle: String) -> String {
        match self.scanner.presenter_press(&handle) {
            Ok(true) => "pressed".into(),
            Ok(false) => "nothing to press".into(),
            Err(e) => format!("error: {}", e.message()),
        }
    }

    fn presenter_detach(&self, handle: String) -> String {
        match self.scanner.presenter_detach(&handle) {
            Ok(()) => "detached".into(),
            Err(e) => format!("error: {}", e.message()),
        }
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<QrScannerModuleImpl>();
}
