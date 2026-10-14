//! A frame as terminal text, for `tools/qr-present`: half blocks, two modules a character, black on
//! white whatever the terminal's colours, with the quiet zone drawn white.

use crate::frames::Frame;

const ON: &str = "\x1b[30;107m";
const OFF: &str = "\x1b[0m";

/// The quiet zone: 4 modules where the terminal has room, 2 where it doesn't.
fn quiet(size: usize, cols: usize, rows: usize) -> Option<usize> {
    [4, 2].into_iter().find(|q| fits(size + 2 * q, cols, rows))
}

/// Room for a code `side` modules square, and one status line below it.
pub fn fits(side: usize, cols: usize, rows: usize) -> bool {
    side <= cols && side.div_ceil(2) < rows
}

pub fn frame(f: &Frame, cols: usize, rows: usize, status: &str) -> String {
    let Some(q) = quiet(f.size, cols, rows) else {
        return format!(
            "Make the terminal at least {} columns by {} rows to show this code (it is {cols} by {rows}).\n{status}",
            f.size + 4,
            (f.size + 4).div_ceil(2) + 1
        );
    };
    let side = f.size + 2 * q;
    let dark = |x: usize, y: usize| {
        x >= q && y >= q && x < q + f.size && y < q + f.size && f.dark(x - q, y - q)
    };
    let mut out = String::with_capacity(side * side);
    for row in 0..side.div_ceil(2) {
        out.push_str(ON);
        for x in 0..side {
            let (top, bottom) = (dark(x, 2 * row), 2 * row + 1 < side && dark(x, 2 * row + 1));
            out.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
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
    fn two_modules_a_character() {
        let f = Frame {
            size: 2,
            bits: "1001".into(),
        };
        let text = frame(&f, 80, 24, "ok");
        let lines: Vec<&str> = text.lines().collect();
        // 2 + 2*4 = 10 modules: 5 text rows, then the status line.
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[5], "ok");
        let middle = lines[2].trim_start_matches(ON).trim_end_matches(OFF);
        assert_eq!(middle.chars().count(), 10);
        // Row 2 holds module rows 4 and 5: (1,0) over (0,1) at columns 4 and 5.
        assert_eq!(middle.chars().nth(4), Some('▀'));
        assert_eq!(middle.chars().nth(5), Some('▄'));
    }

    #[test]
    fn a_small_terminal_narrows_the_quiet_zone_then_asks_for_room() {
        let f = checker(21);
        // A quiet zone of 4 puts the first modules on the third text row, of 2 on the second.
        assert!(frame(&f, 29, 16, "").lines().nth(2).unwrap().contains('▀'));
        assert!(frame(&f, 25, 14, "").lines().nth(1).unwrap().contains('▀'));
        assert!(
            frame(&f, 24, 14, "").starts_with("Make the terminal at least 25 columns by 14 rows")
        );
    }
}
