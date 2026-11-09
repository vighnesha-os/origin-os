//! Reading a QEMU screen dump and checking it against what the kernel's framebuffer console
//! must have drawn (ADR-0010).
//!
//! `screendump` writes the guest's display as a binary PPM. The kernel's output is known
//! from its serial log, so what the screen shows is known too. [`check_frame`] computes the
//! rows the console's rules give for that log, and reads the dump back cell by cell: every
//! cell must be one glyph, exactly, in the console's foreground and background colors, and
//! every row must say what the log says it should.
//!
//! This check is written from the console's specification, not with its code: the rows are
//! computed here by [`model_rows`] (the wrap rule, the lazy wrap, the rule for bytes that are
//! not printable), and the colors are the literal values the specification names, because a
//! bug in the kernel's drawing code must not be able to vouch for itself. The font is the one
//! piece shared with the kernel, since a font bug would show in both: `framekernel::font`'s
//! own tests, and looking at the picture, cover it.
//!
//! The console never scrolls, so the row each line lands on depends on how many rows every
//! earlier line took. One line lost or doubled anywhere in the boot moves everything after
//! it, and the comparison fails.

use std::collections::HashMap;

use framekernel::font;

type Result<T = ()> = std::result::Result<T, String>;

/// The colors the console draws in, as the specification and `fbconsole` give them, with red
/// in the high byte, so they compare directly with a screen dump's `0xRRGGBB` pixels.
const FOREGROUND: u32 = 0xD0E0F0;
const BACKGROUND: u32 = 0x0B111A;
const BAR: u32 = 0x1E3A5F;

/// A screen dump: 24-bit pixels, row after row.
pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    rgb: Vec<u8>,
}

impl Image {
    /// The pixel at `(x, y)`, as `0xRRGGBB`.
    fn pixel(&self, x: u32, y: u32) -> u32 {
        let at = (y as usize * self.width as usize + x as usize) * 3;
        match self.rgb.get(at..at + 3) {
            Some([r, g, b]) => u32::from(*r) << 16 | u32::from(*g) << 8 | u32::from(*b),
            _ => 0,
        }
    }
}

/// Reads the `P6` binary PPM `qemu`'s `screendump` writes.
pub(crate) fn parse_ppm(data: &[u8]) -> Result<Image> {
    let mut rest = data;
    let mut token = || -> Option<&[u8]> {
        let start = rest.iter().position(|b| !b.is_ascii_whitespace())?;
        rest = rest.get(start..)?;
        let end = rest
            .iter()
            .position(|b| b.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        rest = tail;
        Some(word)
    };
    let number =
        |word: Option<&[u8]>| -> Option<u32> { std::str::from_utf8(word?).ok()?.parse().ok() };
    if token() != Some(b"P6") {
        return Err("not a binary PPM".into());
    }
    let (width, height, depth) = (number(token()), number(token()), number(token()));
    let (Some(width), Some(height), Some(255)) = (width, height, depth) else {
        return Err("a PPM with an unexpected header".into());
    };
    // One whitespace byte separates the header from the pixels.
    let pixels = rest.get(1..).ok_or("a PPM with no pixels")?;
    let expected = width as usize * height as usize * 3;
    if pixels.len() != expected {
        return Err(format!(
            "a PPM of {width}x{height} with {} bytes of pixels, not {expected}",
            pixels.len()
        ));
    }
    Ok(Image {
        width,
        height,
        rgb: pixels.to_vec(),
    })
}

/// The text cells of a `width` by `height` display, by the console's rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Grid {
    columns: u32,
    rows: u32,
    scale: u32,
}

impl Grid {
    fn new(width: u32, height: u32) -> Result<Self> {
        let scale = if height >= 2880 {
            3
        } else if height >= 1440 {
            2
        } else {
            1
        };
        let (columns, rows) = (width / (8 * scale), height / (16 * scale));
        if columns < 20 || rows < 8 {
            return Err(format!("a {width}x{height} display has no console"));
        }
        Ok(Self {
            columns,
            rows,
            scale,
        })
    }
}

/// What the screen must show.
struct Model {
    /// Text of each row; each character is one cell, and a cell the font has no glyph for is
    /// the replacement mark, `\u{25A1}`.
    rows: Vec<String>,
    /// The row the next line goes to: painted as the bar.
    cursor: usize,
}

/// The rows of a `width` by `height` screen after the console has written `text`, computed
/// from the console's rules (ADR-0010 E3): the first row is row 0; `\n` moves to the next row
/// and blanks it (wrapping at the bottom); a printable byte takes a cell and wraps lazily, so
/// a full row followed by `\n` takes one row; `\r` and UTF-8 continuation bytes take no
/// cell; anything else takes a cell with the replacement glyph.
fn model_rows(width: u32, height: u32, text: &str) -> Result<Model> {
    let grid = Grid::new(width, height)?;
    let (columns, rows) = (grid.columns as usize, grid.rows as usize);
    let mut cells = vec![vec![' '; columns]; rows];
    let (mut row, mut column) = (0usize, 0usize);
    for &byte in text.as_bytes() {
        match byte {
            b'\n' => {
                row = (row + 1) % rows;
                column = 0;
                cells[row].fill(' ');
            }
            b'\r' | 0x80..=0xBF => {}
            _ => {
                if column == columns {
                    row = (row + 1) % rows;
                    column = 0;
                    cells[row].fill(' ');
                }
                cells[row][column] = if (font::FIRST..=font::LAST).contains(&byte) {
                    char::from(byte)
                } else {
                    '\u{25A1}'
                };
                column += 1;
            }
        }
    }
    Ok(Model {
        rows: cells
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect(),
        cursor: row,
    })
}

/// The screen read back.
struct Decoded {
    /// The rows, top to bottom. A cell that is not exactly one glyph in the console's colors
    /// is `\u{FFFD}`.
    rows: Vec<String>,
    /// The row painted as the bar, if any is.
    bar: Option<usize>,
}

/// Reads a frame back into text and checks its margins are the background.
fn decode(image: &Image) -> Result<Decoded> {
    let grid = Grid::new(image.width, image.height)?;
    let (width, height) = (image.width, image.height);
    let (cell_w, cell_h) = (font::WIDTH * grid.scale, font::HEIGHT * grid.scale);
    let mut by_bits: HashMap<[u8; 16], char> = (font::FIRST..=font::LAST)
        .map(|b| (*font::glyph(b), char::from(b)))
        .collect();
    by_bits.insert(font::REPLACEMENT, '\u{25A1}');

    let mut decoded = Decoded {
        rows: Vec::new(),
        bar: None,
    };
    for row in 0..grid.rows {
        let top = row * cell_h;
        let is_bar = (top..top + cell_h).all(|y| (0..width).all(|x| image.pixel(x, y) == BAR));
        if is_bar {
            if decoded.bar.is_some() {
                return Err(format!("two rows are painted as the bar (row {row} too)"));
            }
            decoded.bar = Some(row as usize);
            decoded.rows.push(String::new());
            continue;
        }
        let mut text = String::new();
        for column in 0..grid.columns {
            let left = column * cell_w;
            let mut bits = [0u8; 16];
            let mut exact = true;
            for (y, line) in (0u32..).zip(bits.iter_mut()) {
                for x in 0..font::WIDTH {
                    let mut seen = None;
                    // Every display pixel of the font pixel: scale by scale.
                    for sy in 0..grid.scale {
                        for sx in 0..grid.scale {
                            let value =
                                image.pixel(left + x * grid.scale + sx, top + y * grid.scale + sy);
                            exact &= seen.is_none_or(|v| v == value);
                            seen = Some(value);
                        }
                    }
                    match seen {
                        Some(FOREGROUND) => *line |= 0x80 >> x,
                        Some(BACKGROUND) => {}
                        _ => exact = false,
                    }
                }
            }
            text.push(if exact {
                by_bits.get(&bits).copied().unwrap_or('\u{FFFD}')
            } else {
                '\u{FFFD}'
            });
        }
        // Right of the last whole column, a text row is background.
        let margin_right = (top..top + cell_h)
            .all(|y| (grid.columns * cell_w..width).all(|x| image.pixel(x, y) == BACKGROUND));
        if !margin_right {
            text.push('\u{FFFD}');
        }
        decoded.rows.push(text.trim_end().to_string());
    }
    // Below the last whole row, the display is background.
    let bottom = grid.rows * cell_h;
    if !(bottom..height).all(|y| (0..width).all(|x| image.pixel(x, y) == BACKGROUND)) {
        return Err(format!(
            "the rows below row {} are not background",
            grid.rows
        ));
    }
    Ok(decoded)
}

/// The text of a screen dump, oldest row first: from just after the cursor bar round to just
/// before it, without the bar itself or empty rows.
pub(crate) fn screen_text(image: &Image) -> Result<Vec<String>> {
    let decoded = decode(image)?;
    let bar = decoded.bar.ok_or("the screen dump has no cursor bar")?;
    let (newer, older) = decoded.rows.split_at(bar);
    Ok(older
        .iter()
        .skip(1)
        .chain(newer)
        .filter(|row| !row.is_empty())
        .cloned()
        .collect())
}

/// Checks that `image` shows what the console must draw for `text`: every row the text says,
/// the bar on the cursor's row.
pub(crate) fn check_frame(image: &Image, text: &str) -> Result {
    let model = model_rows(image.width, image.height, text)?;
    let decoded = decode(image)?;
    if decoded.bar != Some(model.cursor) {
        return Err(format!(
            "the cursor bar is on row {:?}, and the serial log puts it on row {}",
            decoded.bar, model.cursor
        ));
    }
    let wrong = model
        .rows
        .iter()
        .zip(&decoded.rows)
        .enumerate()
        .find(|(row, (expected, shown))| *row != model.cursor && expected != shown);
    if let Some((row, (expected, shown))) = wrong {
        return Err(format!(
            "the screen differs from the serial log at text row {row}:\n  on the screen: {shown:?}\n  expected:      {expected:?}",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use framekernel::fbconsole::{self, Cursor, PixelSink, Screen};
    use origin_boot_info::Framebuffer;

    const W: u32 = 1280;
    const H: u32 = 800;

    /// The kernel's own drawing, into memory, in the 0xRRGGBB layout of a screen dump.
    struct Memory {
        pixels: Vec<u32>,
    }

    impl PixelSink for Memory {
        fn write32(&mut self, offset: usize, value: u32) {
            if let Some(slot) = self.pixels.get_mut(offset / 4) {
                *slot = value;
            }
        }
    }

    fn framebuffer(width: u32, height: u32) -> Framebuffer {
        Framebuffer {
            base: 0x1000,
            size: u64::from(width) * 4 * u64::from(height),
            virtual_base: 0,
            width,
            height,
            stride: width * 4,
            red_mask: 0x00FF_0000,
            green_mask: 0x0000_FF00,
            blue_mask: 0x0000_00FF,
        }
    }

    /// The picture the kernel's console draws after `text` on a `width` by `height` display.
    fn drawn(width: u32, height: u32, text: &str) -> Vec<u32> {
        let f = framebuffer(width, height);
        let screen = Screen::new(&f).unwrap();
        let mut memory = Memory {
            pixels: vec![0; width as usize * height as usize],
        };
        let cursor = Cursor::new();
        screen.clear(&cursor, &mut memory);
        screen.write(&cursor, &mut memory, text.as_bytes());
        memory.pixels
    }

    fn ppm(width: u32, height: u32, pixels: &[u32]) -> Vec<u8> {
        let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
        for p in pixels {
            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        out
    }

    fn image(width: u32, height: u32, text: &str) -> Image {
        parse_ppm(&ppm(width, height, &drawn(width, height, text))).unwrap()
    }

    fn logged() -> String {
        let mut text = String::from("origin-kernel: console: framebuffer 1280x800\n");
        for n in 0..120 {
            text += &format!("origin-kernel: line {n} of the boot\n");
        }
        text += "origin-kernel: M1 entry complete; halting\n";
        text
    }

    fn from_text(text: &str) -> Image {
        image(W, H, text)
    }

    /// Whatever the kernel draws and what the model says must be the same picture, on a
    /// log with every byte class and on a display with margins: the check is only worth
    /// something if the model is right. A model bug fails here, a kernel bug in the boot.
    #[test]
    fn the_kernels_drawing_and_the_model_agree() {
        let text = logged();
        assert_eq!(check_frame(&from_text(&text), &text), Ok(()));
        // Long lines wrap, a full row then a newline takes one row, the screen wraps, and
        // bytes the font has no glyph for are one cell each.
        let mixed: String = (0..10)
            .map(|n| format!("{}\n", "x".repeat(150 + n * 3)))
            .chain(["a tab\there and a § sign and a four-byte 😀\n".to_string()])
            .collect();
        let long: String = (0..4).map(|_| mixed.clone()).collect();
        assert_eq!(check_frame(&from_text(&long), &long), Ok(()));
        // A 1024 by 700 display: one column and part of a row left over, and the bar and
        // the background in the margins.
        let margins = image(1020, 700, &long);
        assert_eq!(check_frame(&margins, &long), Ok(()));
        // A scaled display: every font pixel is a 2 by 2 block.
        let scaled = image(2560, 1440, &logged());
        assert_eq!(check_frame(&scaled, &logged()), Ok(()));
    }

    /// The literal colors here are the specification's; the console must keep them.
    #[test]
    fn the_console_really_draws_in_the_specified_colors() {
        let value = |c: fbconsole::Rgb| {
            u32::from(c.red) << 16 | u32::from(c.green) << 8 | u32::from(c.blue)
        };
        assert_eq!(value(fbconsole::FOREGROUND), FOREGROUND);
        assert_eq!(value(fbconsole::BACKGROUND), BACKGROUND);
        assert_eq!(value(fbconsole::BAR), BAR);
    }

    /// The negative controls the boot test relies on: each way the kernel could get the
    /// screen wrong is caught.
    #[test]
    fn every_wrong_picture_fails() {
        let text = logged();
        let good = drawn(W, H, &text);
        let check = |pixels: &[u32]| check_frame(&parse_ppm(&ppm(W, H, pixels)).unwrap(), &text);
        assert_eq!(check(&good), Ok(()));

        // A line lost anywhere in the boot moves every later row.
        let lost = text.replacen("origin-kernel: line 7 of the boot\n", "", 1);
        assert!(check(&drawn(W, H, &lost)).is_err());

        // The console never ran: the screen still shows what the firmware left there.
        assert!(check(&vec![0; good.len()]).is_err());

        // Red and blue swapped: the colors are not the specification's.
        let swapped: Vec<u32> = good
            .iter()
            .map(|p| (p & 0xFF) << 16 | (p & 0xFF00) | (p >> 16) & 0xFF)
            .collect();
        assert!(check(&swapped).is_err());

        // A glyph drawn a pixel to the side in one cell (a wrong column offset or stride
        // would show up the same way, in every cell).
        let mut shifted = good.clone();
        let row = (3 * 16 + 5) * W as usize;
        shifted.copy_within(row..row + W as usize - 1, row + 1);
        assert!(check(&shifted).is_err());

        // One pixel of background turned another color, in the middle of a glyph's cell.
        let mut speck = good.clone();
        speck[(10 * W) as usize + 20] = 0x00FF_0000;
        assert!(check(&speck).is_err());

        // A second bar, and a bar on the wrong row.
        let mut extra = good.clone();
        extra[(4 * 16 * W) as usize..(5 * 16 * W) as usize].fill(BAR);
        assert!(check(&extra).is_err());

        // The error says which row, in words.
        let mut changed = text.clone();
        changed.insert(changed.len() - 1, '!');
        let err = check_frame(&from_text(&changed), &text).unwrap_err();
        assert!(err.contains("halting!"), "{err}");
        assert!(err.contains("expected:"), "{err}");
    }

    #[test]
    fn a_screen_decodes_to_its_lines_oldest_first() {
        let text = logged();
        let rows = screen_text(&from_text(&text)).unwrap();
        // 50 rows, 122 lines logged, one row is the bar: the last 49 lines.
        assert_eq!(rows.len(), 49);
        assert_eq!(
            rows.first().map(String::as_str),
            Some("origin-kernel: line 72 of the boot")
        );
        assert_eq!(
            rows.last().map(String::as_str),
            Some("origin-kernel: M1 entry complete; halting")
        );
    }

    #[test]
    fn a_ppm_is_read_exactly() {
        let image = parse_ppm(b"P6\n2 1\n255\n\x01\x02\x03\xF0\xE0\xD0").unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixel(0, 0), 0x010203);
        assert_eq!(image.pixel(1, 0), 0xF0E0D0);
        // Comments are not something QEMU writes, and a short file is one still being written.
        assert!(parse_ppm(b"P5\n2 1\n255\n\x01\x02").is_err());
        assert!(parse_ppm(b"P6\n2 1\n255\n\x01\x02\x03").is_err());
        assert!(parse_ppm(b"P6\n2 1\n65535\n").is_err());
    }

    #[test]
    fn a_frame_that_is_not_the_consoles_is_not_accepted_as_text() {
        // Noise cells decode to the replacement mark, not to a guess.
        let mut noisy = drawn(W, H, "hello\n");
        noisy[(6 * W) as usize + 3] = 0x0012_3456;
        let image = parse_ppm(&ppm(W, H, &noisy)).unwrap();
        let rows = screen_text(&image).unwrap();
        assert!(rows.iter().any(|r| r.contains('\u{FFFD}')), "{rows:?}");
    }
}
