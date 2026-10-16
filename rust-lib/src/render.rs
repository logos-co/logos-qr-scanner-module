//! A frame as terminal text, for `tools/qr-present`: black on white whatever the terminal's colours,
//! with the quiet zone drawn white.
//!
//! Each module is two spaces painted in its colour, one text row tall: about square in any terminal,
//! and drawn by no font. Half blocks (`▀`) doubled the density, but Terminal on macOS draws them well
//! short of half a cell, and the Keycard Shell couldn't read those codes.

use crate::frames::Frame;

// 256-colour black and white: fixed, unlike the 16 colours a theme redefines.
const LIGHT: &str = "\x1b[48;5;231m";
const DARK: &str = "\x1b[48;5;16m";
const OFF: &str = "\x1b[0m";

/// The quiet zone: 4 modules where the terminal has room, 2 where it doesn't.
fn quiet(size: usize, cols: usize, rows: usize) -> Option<usize> {
    [4, 2].into_iter().find(|q| fits(size + 2 * q, cols, rows))
}

/// Room for a code `side` modules square, two columns a module, and a status of up to two lines.
pub fn fits(side: usize, cols: usize, rows: usize) -> bool {
    2 * side <= cols && side + 2 <= rows
}

pub fn frame(f: &Frame, cols: usize, rows: usize, status: &str) -> String {
    let Some(q) = quiet(f.size, cols, rows) else {
        return format!(
            "Make the terminal at least {} columns by {} rows to show this code (it is {cols} by {rows}).\n{status}",
            2 * (f.size + 4),
            f.size + 6
        );
    };
    let side = f.size + 2 * q;
    let dark = |x: usize, y: usize| {
        x >= q && y >= q && x < q + f.size && y < q + f.size && f.dark(x - q, y - q)
    };
    let mut out = String::with_capacity(side * side * 4);
    for y in 0..side {
        // The colour is set only where it changes.
        let mut pen = None;
        for x in 0..side {
            let d = dark(x, y);
            if pen != Some(d) {
                out.push_str(if d { DARK } else { LIGHT });
                pen = Some(d);
            }
            out.push_str("  ");
        }
        out.push_str(OFF);
        out.push('\n');
    }
    out.push_str(status);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's modules as `#` dark and `.` light, its colours read back.
    fn plain(line: &str) -> String {
        let mut out = String::new();
        let mut dark = false;
        let mut rest = line;
        while !rest.is_empty() {
            if let Some(code) = rest.strip_prefix("\x1b[") {
                let end = code.find('m').unwrap();
                match &code[..end] {
                    "48;5;16" => dark = true,
                    "48;5;231" => dark = false,
                    _ => {}
                }
                rest = &code[end + 1..];
                continue;
            }
            let cell = rest.get(..2).expect("two spaces a module");
            assert_eq!(cell, "  ");
            out.push(if dark { '#' } else { '.' });
            rest = &rest[2..];
        }
        out
    }

    fn checker(n: usize) -> Frame {
        let bits = (0..n * n)
            .map(|i| {
                if (i / n + i % n).is_multiple_of(2) {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        Frame { size: n, bits }
    }

    #[test]
    fn a_module_is_two_painted_spaces_on_one_row() {
        let f = Frame {
            size: 2,
            bits: "1001".into(),
        };
        let text = frame(&f, 80, 24, "ok");
        let lines: Vec<&str> = text.lines().collect();
        // 2 + 2*4 = 10 modules: 10 rows, then the status line.
        assert_eq!(lines.len(), 11);
        assert_eq!(lines[10], "ok");
        assert_eq!(plain(lines[0]), "..........");
        assert_eq!(plain(lines[4]), "....#.....");
        assert_eq!(plain(lines[5]), ".....#....");
        // Painted, never drawn: no glyph a font could size.
        assert!(!text.contains('▀') && !text.contains('█'));
    }

    #[test]
    fn a_small_terminal_narrows_the_quiet_zone_then_asks_for_room() {
        let f = checker(21);
        // 21 + 2*4 = 29 modules: 58 columns and 31 rows; with a quiet zone of 2, 50 and 27.
        assert_eq!(
            plain(frame(&f, 58, 31, "").lines().nth(4).unwrap()).len(),
            29
        );
        assert_eq!(
            plain(frame(&f, 50, 27, "").lines().nth(2).unwrap()).len(),
            25
        );
        assert!(
            frame(&f, 49, 27, "").starts_with("Make the terminal at least 50 columns by 27 rows")
        );
    }
}
