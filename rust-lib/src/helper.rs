//! The link to the Logos QR helper: a child process spoken to in JSON lines over its standard input
//! and output. The module never touches the camera itself.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use serde::Deserialize;

/// What the helper reports.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum HelperEvent {
    Ready {
        #[serde(default)]
        window: bool,
    },
    Camera {
        name: String,
    },
    Text {
        text: String,
    },
    Cancelled,
    Error {
        code: String,
        message: String,
    },
    /// Not from the helper: its output ended.
    #[serde(skip)]
    Exited,
}

pub type Sink = Box<dyn Fn(HelperEvent) + Send>;

pub trait Helper: Send {
    fn send(&mut self, line: &str) -> Result<(), String>;
    fn close(&mut self);
}

pub trait Launcher: Send + Sync {
    /// Start a helper, windowless when `headless`, its events going to `sink` from another thread.
    fn launch(&self, headless: bool, sink: Sink) -> Result<Box<dyn Helper>, String>;
}

/// Where the helper is installed beside the plugin, per system.
pub fn installed_at(module_dir: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        module_dir.join("LogosQR.app/Contents/MacOS/logos-qr-helper")
    } else if cfg!(windows) {
        module_dir.join("logos-qr-helper.exe")
    } else {
        module_dir.join("logos-qr-helper")
    }
}

pub struct ProcessLauncher {
    pub path: PathBuf,
}

impl ProcessLauncher {
    /// `LOGOS_QR_HELPER` names another helper, for development and tests.
    pub fn new(module_dir: &Path) -> ProcessLauncher {
        let path = std::env::var_os("LOGOS_QR_HELPER")
            .map(PathBuf::from)
            .unwrap_or_else(|| installed_at(module_dir));
        ProcessLauncher { path }
    }
}

struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Helper for Process {
    fn send(&mut self, line: &str) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("the helper is closed")?;
        writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("the helper: {e}"))
    }

    fn close(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = writeln!(stdin, r#"{{"op":"close"}}"#);
        }
        // Closing stdin ends it too; a helper that hangs on its camera is not waited for long.
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.close();
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&self, headless: bool, sink: Sink) -> Result<Box<dyn Helper>, String> {
        let mut cmd = Command::new(&self.path);
        if headless {
            cmd.arg("--no-window");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("starting the QR helper at {}: {e}", self.path.display()))?;
        let stdout = child.stdout.take().ok_or("the helper has no output")?;
        let stdin = child.stdin.take();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(ev) = serde_json::from_str::<HelperEvent>(&line) {
                    sink(ev);
                }
            }
            sink(HelperEvent::Exited);
        });
        Ok(Box::new(Process { child, stdin }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_parse() {
        let e: HelperEvent = serde_json::from_str(r#"{"ev":"text","text":"UR:X"}"#).unwrap();
        assert_eq!(
            e,
            HelperEvent::Text {
                text: "UR:X".into()
            }
        );
        let e: HelperEvent =
            serde_json::from_str(r#"{"ev":"ready","version":"0.1.0","window":true,"fake":false}"#)
                .unwrap();
        assert_eq!(e, HelperEvent::Ready { window: true });
        let e: HelperEvent =
            serde_json::from_str(r#"{"ev":"error","code":"camera_denied","message":"m"}"#).unwrap();
        assert!(matches!(e, HelperEvent::Error { .. }));
    }

    #[test]
    fn installed_beside_the_plugin() {
        let p = installed_at(Path::new("/m"));
        assert!(p.starts_with("/m"));
    }
}
