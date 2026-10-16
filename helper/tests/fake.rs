//! The helper end to end in fake mode: a TCP peer plays the device, sees what the helper shows, and
//! sends pictures of a code for the helper's decoders to read.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

use qrcodegen::{QrCode, QrCodeEcc};

fn raster(text: &str, scale: usize) -> (usize, usize, Vec<u8>) {
    let qr = QrCode::encode_text(text, QrCodeEcc::Low).unwrap();
    let n = qr.size() as usize;
    let side = (n + 8) * scale;
    let (w, h) = (side + 120, side + 80);
    let mut px = vec![120u8; w * h];
    for y in 0..side {
        for x in 0..side {
            let (mx, my) = (x / scale, y / scale);
            let dark = (4..n + 4).contains(&mx)
                && (4..n + 4).contains(&my)
                && qr.get_module(mx as i32 - 4, my as i32 - 4);
            px[(40 + y) * w + 60 + x] = if dark { 25 } else { 230 };
        }
    }
    (w, h, px)
}

#[test]
fn shows_frames_and_reads_codes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mut helper = Command::new(env!("CARGO_BIN_EXE_logos-qr-helper"))
        .env("LOGOS_QR_HELPER_FAKE", &addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = helper.stdin.take().unwrap();
    let mut events = BufReader::new(helper.stdout.take().unwrap()).lines();

    let (device, _) = listener.accept().unwrap();
    device
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut seen = BufReader::new(device.try_clone().unwrap()).lines();
    let ready = events.next().unwrap().unwrap();
    assert!(
        ready.contains(r#""ev":"ready""#) && ready.contains(r#""fake":true"#),
        "{ready}"
    );

    writeln!(
        stdin,
        r#"{{"op":"show","title":"t","frames":[{{"size":2,"bits":"1001"}}],"interval_ms":150}}"#
    )
    .unwrap();
    writeln!(stdin, r#"{{"op":"scan","hint":"show the answer"}}"#).unwrap();
    let shown = seen.next().unwrap().unwrap();
    assert!(
        shown.contains(r#""bits":"1001""#) && shown.contains(r#""interval_ms":150"#),
        "{shown}"
    );
    assert_eq!(seen.next().unwrap().unwrap(), r#"{"scan":true}"#);

    // The same code twice, then another: each text is reported once.
    let mut device = device;
    for text in [
        "UR:BYTES/HDCXDWINVEZM",
        "UR:BYTES/HDCXDWINVEZM",
        "UR:BYTES/AEADAOLYTOMWGTKP",
    ] {
        let (w, h, px) = raster(text, 4);
        writeln!(device, "raster {w} {h}").unwrap();
        device.write_all(&px).unwrap();
    }
    let (mut texts, mut seeing) = (Vec::new(), false);
    while texts.len() < 2 {
        let line = events.next().unwrap().unwrap();
        if line.contains(r#""ev":"text""#) {
            texts.push(line);
        } else if line.contains(r#""ev":"seeing""#) {
            // Whether a code is in view, never the picture.
            seeing |= line == r#"{"ev":"seeing","code":true}"#;
        } else {
            assert!(line.contains(r#""ev":"camera""#), "{line}");
        }
    }
    assert!(seeing, "a code in view is said");
    assert_eq!(texts[0], r#"{"ev":"text","text":"UR:BYTES/HDCXDWINVEZM"}"#);
    assert_eq!(
        texts[1],
        r#"{"ev":"text","text":"UR:BYTES/AEADAOLYTOMWGTKP"}"#
    );

    writeln!(
        stdin,
        r#"{{"op":"show","frames":[{{"size":2,"bits":"10x1"}}]}}"#
    )
    .unwrap();
    let bad = events
        .by_ref()
        .map(|l| l.unwrap())
        .find(|l| !l.contains(r#""ev":"seeing""#))
        .unwrap();
    assert!(bad.contains(r#""code":"bad_command""#), "{bad}");

    writeln!(stdin, r#"{{"op":"close"}}"#).unwrap();
    assert!(helper.wait().unwrap().success());
}

/// Show first, the camera off; a press; then the camera, and off again.
#[test]
fn a_press_comes_back_and_the_camera_turns_off() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mut helper = Command::new(env!("CARGO_BIN_EXE_logos-qr-helper"))
        .env("LOGOS_QR_HELPER_FAKE", &addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = helper.stdin.take().unwrap();
    let mut events = BufReader::new(helper.stdout.take().unwrap()).lines();
    let (mut device, _) = listener.accept().unwrap();
    device
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut seen = BufReader::new(device.try_clone().unwrap()).lines();
    assert!(events.next().unwrap().unwrap().contains(r#""ev":"ready""#));

    writeln!(
        stdin,
        r#"{{"op":"show","frames":[{{"size":2,"bits":"1001"}}]}}"#
    )
    .unwrap();
    writeln!(stdin, r#"{{"op":"ask","label":"Scan the answer"}}"#).unwrap();
    assert!(seen.next().unwrap().unwrap().contains(r#""show""#));
    // A picture while the camera is off is never read.
    let (w, h, px) = raster("UR:BYTES/HDCXDWINVEZM", 4);
    writeln!(device, "raster {w} {h}").unwrap();
    device.write_all(&px).unwrap();
    writeln!(device, "press").unwrap();
    assert_eq!(events.next().unwrap().unwrap(), r#"{"ev":"pressed"}"#);

    writeln!(stdin, r#"{{"op":"scan"}}"#).unwrap();
    assert_eq!(seen.next().unwrap().unwrap(), r#"{"scan":true}"#);
    let (w, h, px) = raster("UR:BYTES/AEADAOLYTOMWGTKP", 4);
    writeln!(device, "raster {w} {h}").unwrap();
    device.write_all(&px).unwrap();
    let text = loop {
        let line = events.next().unwrap().unwrap();
        if !line.contains(r#""ev":"camera""#) {
            break line;
        }
    };
    assert_eq!(text, r#"{"ev":"text","text":"UR:BYTES/AEADAOLYTOMWGTKP"}"#);

    writeln!(stdin, r#"{{"op":"stop_scan"}}"#).unwrap();
    assert_eq!(seen.next().unwrap().unwrap(), r#"{"scan":false}"#);
    writeln!(stdin, r#"{{"op":"close"}}"#).unwrap();
    assert!(helper.wait().unwrap().success());
}

#[test]
fn exits_when_its_module_goes() {
    let mut helper = Command::new(env!("CARGO_BIN_EXE_logos-qr-helper"))
        .arg("--no-window")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(helper.stdin.take());
    assert!(helper.wait().unwrap().success());
}
