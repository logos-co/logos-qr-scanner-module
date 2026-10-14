//! qr-fake-device: the other end of the helper's fake mode, for doctests. It prints the address it
//! listens on, logs every line the helper sends (what its window would show), and once the helper
//! scans, shows it each `--say` text as a picture of a QR code, over and over.
//!
//!   qr-fake-device [--listen 127.0.0.1:0] [--say TEXT]... [--log FILE]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
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
        let mut writer = stream.try_clone().expect("clone");
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
                if line.contains(r#""scan":true"#) && !say.is_empty() {
                    let mut w = writer.try_clone().expect("clone");
                    let say = say.clone();
                    std::thread::spawn(move || loop {
                        for text in &say {
                            let (cw, ch, px) = raster(text);
                            if writeln!(w, "raster {cw} {ch}").is_err() || w.write_all(&px).is_err()
                            {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    });
                }
            }
            let _ = writer.flush();
        });
    }
}
