//! The helper's protocol: one JSON object per line. Commands arrive on standard input, events leave
//! on standard output. Only frames go in and only decoded text comes out: no picture ever leaves.

use std::io::Write;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub size: usize,
    /// `size * size` characters, row by row: `1` a dark module, `0` a light one.
    pub bits: String,
}

impl Frame {
    pub fn check(&self) -> Result<(), String> {
        if self.size == 0 || self.size > 177 {
            return Err(format!("a frame of size {}", self.size));
        }
        if self.bits.len() != self.size * self.size
            || self.bits.bytes().any(|b| b != b'0' && b != b'1')
        {
            return Err("a frame's bits must be size*size of 0 and 1".into());
        }
        Ok(())
    }

    pub fn dark(&self, x: usize, y: usize) -> bool {
        self.bits.as_bytes()[y * self.size + x] == b'1'
    }
}

fn default_interval() -> u64 {
    200
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Show these frames in turn, each for `interval_ms`.
    Show {
        #[serde(default)]
        title: Option<String>,
        frames: Vec<Frame>,
        #[serde(default = "default_interval")]
        interval_ms: u64,
    },
    /// Open the camera and report the text of every new code.
    Scan {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        hint: Option<String>,
    },
    /// What the caller made of the codes so far, for the window.
    Progress {
        received: u32,
        expected: u32,
    },
    Title {
        title: String,
    },
    Close,
}

#[derive(Debug, Serialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    Ready {
        version: &'static str,
        window: bool,
        fake: bool,
    },
    Camera {
        name: String,
    },
    Text {
        text: String,
    },
    Cancelled,
    Error {
        code: &'static str,
        message: String,
    },
}

/// Standard output, shared by the main thread and the camera's.
pub struct Out(Mutex<std::io::Stdout>);

impl Out {
    pub fn new() -> Out {
        Out(Mutex::new(std::io::stdout()))
    }

    pub fn send(&self, ev: &Event) {
        let line = serde_json::to_string(ev).unwrap_or_default();
        let mut out = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    pub fn error(&self, code: &'static str, message: impl Into<String>) {
        self.send(&Event::Error {
            code,
            message: message.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        let c: Command =
            serde_json::from_str(r#"{"op":"show","frames":[{"size":1,"bits":"1"}]}"#).unwrap();
        match c {
            Command::Show {
                frames,
                interval_ms,
                ..
            } => {
                assert_eq!(frames.len(), 1);
                assert_eq!(interval_ms, 200);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            serde_json::from_str(r#"{"op":"close"}"#).unwrap(),
            Command::Close
        ));
        assert!(serde_json::from_str::<Command>(r#"{"op":"nope"}"#).is_err());
    }

    #[test]
    fn frames_are_checked() {
        assert!(Frame {
            size: 2,
            bits: "1010".into()
        }
        .check()
        .is_ok());
        assert!(Frame {
            size: 2,
            bits: "101".into()
        }
        .check()
        .is_err());
        assert!(Frame {
            size: 2,
            bits: "10x0".into()
        }
        .check()
        .is_err());
        assert!(Frame {
            size: 0,
            bits: String::new()
        }
        .check()
        .is_err());
    }

    #[test]
    fn events_are_tagged() {
        let s = serde_json::to_string(&Event::Text {
            text: "UR:X".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"ev":"text","text":"UR:X"}"#);
    }
}
