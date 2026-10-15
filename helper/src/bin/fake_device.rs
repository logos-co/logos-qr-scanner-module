//! qr-fake-device: the other end of the helper's fake mode, for doctests. It prints the address it
//! listens on, logs every line the helper sends (what its window would show), presses the window's
//! button after each show, and while the helper scans, shows it each `--say` text as a picture of a
//! QR code, over and over.
//!
//!   qr-fake-device [--listen 127.0.0.1:0] [--say TEXT]... [--log FILE]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qrcodegen::{QrCode, QrCodeEcc};

fn raster(text: &str) -> (usize, usize, Vec<u8>) {
    let qr = QrCode::encode_text(text, QrCodeEcc::Low).expect("text fits a QR code");
    let n = qr.size() as usize;
    let scale = 4;
    let side = (n + 8) * scale;
    let (w, h) = (side + 80, side + 60);
    let mut px = vec![110u8; w * h];
    for y in 0..side {
        for x in 0..side {
            let (mx, my) = (x / scale, y / scale);
            let dark = (4..n + 4).contains(&mx)
                && (4..n + 4).contains(&my)
                && qr.get_module(mx as i32 - 4, my as i32 - 4);
            px[(30 + y) * w + 40 + x] = if dark { 20 } else { 235 };
        }
    }
    (w, h, px)
}

fn main() {
    let mut listen = "127.0.0.1:0".to_string();
    let mut say = Vec::new();
    let mut log = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().expect("--listen ADDR"),
            "--say" => say.push(args.next().expect("--say TEXT")),
            "--log" => log = Some(args.next().expect("--log FILE")),
            other => panic!("unknown argument {other}"),
        }
    }
    let listener = TcpListener::bind(&listen).expect("listen");
    println!("{}", listener.local_addr().expect("address"));
    let _ = std::io::stdout().flush();
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        // One writer for presses and pictures, so a press never lands inside a picture.
        let writer = Arc::new(Mutex::new(stream.try_clone().expect("clone")));
        let on = Arc::new(AtomicBool::new(false));
        let say = say.clone();
        let mut log = log.as_ref().map(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .expect("log")
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if let Some(f) = log.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
                // A person scans the codes shown, then presses the window's button.
                if line.contains(r#""show""#) && !line.contains(r#""frames":[]"#) {
                    let mut w = writer.lock().unwrap();
                    let _ = writeln!(w, "press");
                }
                if line.contains(r#""scan":false"#) {
                    on.store(false, Ordering::Relaxed);
                }
                if line.contains(r#""scan":true"#)
                    && !say.is_empty()
                    && !on.swap(true, Ordering::Relaxed)
                {
                    let (writer, on, say) = (writer.clone(), on.clone(), say.clone());
                    std::thread::spawn(move || {
                        while on.load(Ordering::Relaxed) {
                            for text in &say {
                                let (cw, ch, px) = raster(text);
                                let mut w = writer.lock().unwrap();
                                if writeln!(w, "raster {cw} {ch}").is_err()
                                    || w.write_all(&px).is_err()
                                {
                                    return;
                                }
                                drop(w);
                                std::thread::sleep(Duration::from_millis(200));
                            }
                        }
                    });
                }
            }
        });
    }
}
