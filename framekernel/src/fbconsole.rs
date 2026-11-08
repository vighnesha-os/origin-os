//! A text console on the firmware's linear framebuffer (ADR-0010; CLAUDE.md §19,
//! §69).
//!
//! The kernel draws through [`PixelSink`], so the geometry, glyph placement and the handling
//! of every byte are tested on the build host and shared with the boot test, which checks a
//! QEMU screen dump against the same code.
//!
//! The console never scrolls, and it does not read the framebuffer. Each newline moves down a
//! row and repaints only that row as a bar in [`BAR`], and below the last row it wraps to the
//! top: every line costs a fixed amount of pixel writing, whatever the display's size, and
//! reading uncached video memory, which is slow, is not needed. The bar marks where the next
//! line goes, so the screen always reads from just below the bar (oldest) round to just above
//! it (newest).
//!
//! Text is cut into cells: 8 by 16 font cells, scaled by a whole number on large displays so
//! the text stays readable. A byte above 0x7F starts or continues UTF-8; each lead byte draws
//! one replacement glyph and continuation bytes draw nothing, so a line is as many cells wide
//! as it has characters.
//!
//! Nothing here can panic. A framebuffer whose description is refused gets no console at all;
//! an accepted one's every pixel is inside the visible rows, and a wrong cursor is clamped.
//! The kernel's sink checks each offset again anyway.

use core::sync::atomic::{AtomicU64, Ordering};

use origin_boot_info::{Framebuffer, FramebufferError};

use crate::font;

/// Where a console's pixels go: one 32-bit pixel at a byte offset from the first pixel.
pub trait PixelSink {
    /// Writes the pixel at `offset`, a multiple of 4 inside the visible rows.
    fn write32(&mut self, offset: usize, value: u32);
}

/// An RGB color, 8 bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub red: u8,
    /// Green.
    pub green: u8,
    /// Blue.
    pub blue: u8,
}

const fn rgb(red: u8, green: u8, blue: u8) -> Rgb {
    Rgb { red, green, blue }
}

/// Text. Not gray, so a swapped red and blue is visible, to a person and to the boot test.
pub const FOREGROUND: Rgb = rgb(0xD0, 0xE0, 0xF0);
/// Behind text.
pub const BACKGROUND: Rgb = rgb(0x0B, 0x11, 0x1A);
/// The row the next line goes to.
pub const BAR: Rgb = rgb(0x1E, 0x3A, 0x5F);

/// Why a framebuffer cannot be a console. None of these stops a boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenError {
    /// The description itself is wrong.
    Invalid(FramebufferError),
    /// Too few text cells to read anything on it.
    TooSmall,
}

/// The cursor: separate from [`Screen`] and lock-free, so an urgent report can use it without
/// the console lock. Two writers at once may overwrite each other's text, never anything else.
#[derive(Debug)]
pub struct Cursor(AtomicU64);

impl Cursor {
    /// The top-left cell.
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// `(column, row)`.
    fn get(&self) -> (u32, u32) {
        let v = self.0.load(Ordering::Relaxed);
        ((v >> 32) as u32, v as u32)
    }

    fn set(&self, column: u32, row: u32) {
        self.0.store(
            (u64::from(column) << 32) | u64::from(row),
            Ordering::Relaxed,
        );
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

/// A text console over one framebuffer: its geometry and the pixel value of each color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    width: u32,
    height: u32,
    stride: u32,
    scale: u32,
    columns: u32,
    rows: u32,
    foreground: u32,
    background: u32,
    bar: u32,
}

/// The fewest columns and rows a console is any use with.
const MIN_COLUMNS: u32 = 20;
const MIN_ROWS: u32 = 8;

/// The pixel holding `value` in the channel `mask`, scaled to the channel's width.
fn channel(value: u8, mask: u32) -> u32 {
    let shift = mask.trailing_zeros();
    let max = (mask >> shift) as u64;
    (((u64::from(value) * max + 127) / 255) as u32) << shift
}

fn pixel(color: Rgb, f: &Framebuffer) -> u32 {
    channel(color.red, f.red_mask)
        | channel(color.green, f.green_mask)
        | channel(color.blue, f.blue_mask)
}

impl Screen {
    /// The console for `framebuffer`, or why there can be none.
    pub fn new(framebuffer: &Framebuffer) -> Result<Self, ScreenError> {
        framebuffer.check().map_err(ScreenError::Invalid)?;
        // Tall panels get scaled text; two rungs keep 8 by 16 readable up to 5K.
        let scale =
            1 + u32::from(framebuffer.height >= 1440) + u32::from(framebuffer.height >= 2880);
        let columns = framebuffer.width / (font::WIDTH * scale);
        let rows = framebuffer.height / (font::HEIGHT * scale);
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            return Err(ScreenError::TooSmall);
        }
        Ok(Self {
            width: framebuffer.width,
            height: framebuffer.height,
            stride: framebuffer.stride,
            scale,
            columns,
            rows,
            foreground: pixel(FOREGROUND, framebuffer),
            background: pixel(BACKGROUND, framebuffer),
            bar: pixel(BAR, framebuffer),
        })
    }

    /// Text columns.
    pub const fn columns(&self) -> u32 {
        self.columns
    }

    /// Text rows.
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// Display pixels per font pixel.
    pub const fn scale(&self) -> u32 {
        self.scale
    }

    /// The pixel value of the foreground.
    pub const fn foreground(&self) -> u32 {
        self.foreground
    }

    /// The pixel value of the background.
    pub const fn background(&self) -> u32 {
        self.background
    }

    /// The pixel value of the cursor bar.
    pub const fn bar(&self) -> u32 {
        self.bar
    }

    /// Height of a text row in pixels.
    const fn row_height(&self) -> u32 {
        font::HEIGHT * self.scale
    }

    /// Width of a text column in pixels.
    const fn column_width(&self) -> u32 {
        font::WIDTH * self.scale
    }

    /// The byte offset of pixel `(x, y)`.
    fn offset(&self, x: u32, y: u32) -> usize {
        y as usize * self.stride as usize + x as usize * 4
    }

    fn hline(&self, out: &mut impl PixelSink, x0: u32, y: u32, value: u32) {
        for x in x0..self.width {
            out.write32(self.offset(x, y), value);
        }
    }

    /// Paints the part of text row `row` from `from` (a pixel column) to the display's edge.
    fn fill_row(&self, out: &mut impl PixelSink, row: u32, from: u32, value: u32) {
        let top = row * self.row_height();
        for y in top..top + self.row_height() {
            self.hline(out, from, y, value);
        }
    }

    /// Paints `bitmap` into the cell at `(column, row)`, every pixel of it.
    fn draw(&self, out: &mut impl PixelSink, column: u32, row: u32, bitmap: &[u8; 16]) {
        let (left, top) = (column * self.column_width(), row * self.row_height());
        for (index, bits) in (0u32..).zip(bitmap.iter()) {
            for sub_row in 0..self.scale {
                let y = top + index * self.scale + sub_row;
                for bit in 0..font::WIDTH {
                    let value = if bits & (0x80 >> bit) != 0 {
                        self.foreground
                    } else {
                        self.background
                    };
                    for sub_column in 0..self.scale {
                        let x = left + bit * self.scale + sub_column;
                        out.write32(self.offset(x, y), value);
                    }
                }
            }
        }
    }

    /// Paints the whole display and moves the cursor to the top-left cell.
    pub fn clear(&self, cursor: &Cursor, out: &mut impl PixelSink) {
        for y in 0..self.height {
            self.hline(out, 0, y, self.background);
        }
        cursor.set(0, 0);
        self.fill_row(out, 0, 0, self.bar);
    }

    /// Writes `bytes` at the cursor. `\n` ends a line, `\r` does nothing, printable ASCII
    /// draws its glyph, and anything else draws the replacement glyph. A cell past the last
    /// column wraps to the next row. See the module documentation for the rows.
    pub fn write(&self, cursor: &Cursor, out: &mut impl PixelSink, bytes: &[u8]) {
        let (mut column, row) = cursor.get();
        let mut row = row % self.rows;
        if column > self.columns {
            column = 0;
        }
        for &byte in bytes {
            match byte {
                b'\n' => {
                    self.fill_row(out, row, column * self.column_width(), self.background);
                    row = (row + 1) % self.rows;
                    column = 0;
                    self.fill_row(out, row, 0, self.bar);
                }
                b'\r' => {}
                // UTF-8 continuation bytes: the lead byte already drew the character's cell.
                0x80..=0xBF => {}
                _ => {
                    if column == self.columns {
                        // A full row: only the margin to its right is still the bar's.
                        self.fill_row(out, row, column * self.column_width(), self.background);
                        row = (row + 1) % self.rows;
                        column = 0;
                        self.fill_row(out, row, 0, self.bar);
                    }
                    self.draw(out, column, row, font::glyph(byte));
                    column += 1;
                }
            }
        }
        cursor.set(column, row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1280 by 800 display in the blue-green-red byte order of QEMU's VGA.
    fn display() -> Framebuffer {
        Framebuffer {
            base: 0x8000_0000,
            size: 800 * 5120,
            virtual_base: 0xFFFF_E000_0000_0000,
            width: 1280,
            height: 800,
            stride: 5120,
            red_mask: 0x00FF_0000,
            green_mask: 0x0000_FF00,
            blue_mask: 0x0000_00FF,
        }
    }

    /// The pixels of a tiny display, addressed through a stride with padding.
    struct Memory {
        pixels: Vec<u32>,
        stride: usize,
    }

    impl PixelSink for Memory {
        fn write32(&mut self, offset: usize, value: u32) {
            assert_eq!(offset % 4, 0, "unaligned pixel at {offset}");
            let index = offset / 4;
            assert!(
                index < self.pixels.len(),
                "pixel {index} outside the display"
            );
            self.pixels[index] = value;
        }
    }

    impl Memory {
        fn get(&self, x: u32, y: u32) -> u32 {
            self.pixels[y as usize * self.stride / 4 + x as usize]
        }
    }

    fn frame(f: &Framebuffer) -> Memory {
        Memory {
            pixels: vec![0xDEAD_BEEF; (f.visible_bytes() / 4) as usize],
            stride: f.stride as usize,
        }
    }

    #[test]
    fn the_console_fits_the_mode_and_scales_tall_panels() {
        let s = Screen::new(&display()).unwrap();
        assert_eq!((s.columns(), s.rows(), s.scale()), (160, 50, 1));
        for (w, h, cols, rows, scale) in [
            (1920, 1080, 240, 67, 1),
            (1920, 1200, 240, 75, 1),
            (2560, 1440, 160, 45, 2),
            (2880, 1800, 180, 56, 2),
            (3840, 2160, 240, 67, 2),
            (5120, 2880, 213, 60, 3),
            (800, 600, 100, 37, 1),
        ] {
            let mut f = display();
            f.width = w;
            f.height = h;
            f.stride = w * 4;
            f.size = u64::from(f.stride) * u64::from(h);
            let s = Screen::new(&f).unwrap();
            assert_eq!(
                (s.columns(), s.rows(), s.scale()),
                (cols, rows, scale),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn a_description_or_mode_that_cannot_work_is_refused() {
        let mut f = display();
        f.stride = 2;
        assert_eq!(
            Screen::new(&f),
            Err(ScreenError::Invalid(FramebufferError::Stride))
        );
        assert_eq!(
            Screen::new(&Framebuffer::NONE),
            Err(ScreenError::Invalid(FramebufferError::Absent))
        );
        // Too small to be a console.
        let mut f = display();
        f.width = 159;
        f.height = 128;
        f.stride = 159 * 4;
        f.size = u64::from(f.stride) * 128;
        assert_eq!(Screen::new(&f), Err(ScreenError::TooSmall));
        let mut f = display();
        f.width = 160;
        f.height = 127;
        f.stride = 160 * 4;
        f.size = u64::from(f.stride) * 127;
        assert_eq!(Screen::new(&f), Err(ScreenError::TooSmall));
    }

    #[test]
    fn colors_follow_the_channel_masks() {
        let s = Screen::new(&display()).unwrap();
        assert_eq!(s.foreground(), 0x00D0_E0F0);
        assert_eq!(s.background(), 0x000B_111A);
        assert_eq!(s.bar(), 0x001E_3A5F);
        let mut rgb = display();
        rgb.red_mask = 0x0000_00FF;
        rgb.blue_mask = 0x00FF_0000;
        let s = Screen::new(&rgb).unwrap();
        assert_eq!(s.foreground(), 0x00F0_E0D0);
        // A 10-bit channel scales up to its full range.
        assert_eq!(channel(0xFF, 0x3FF0_0000), 0x3FF0_0000);
        assert_eq!(channel(0, 0x3FF0_0000), 0);
        assert_eq!(channel(0x80, 0x0000_03FF), 514);
    }

    #[test]
    fn a_glyph_is_drawn_where_it_belongs_and_nowhere_else() {
        let f = display();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        s.write(&cursor, &mut m, b"Origin");
        // Each pixel of "O" at column 0 and "g" at column 3 is the glyph's or the background.
        for (column, ch) in [(0u32, b'O'), (3, b'g')] {
            let bitmap = font::glyph(ch);
            for y in 0..16 {
                for x in 0..8 {
                    let on = bitmap[y as usize] & (0x80 >> x) != 0;
                    let expected = if on { s.foreground() } else { s.background() };
                    assert_eq!(m.get(column * 8 + x, y), expected, "{ch} {x},{y}");
                }
            }
        }
        // The rest of the cursor row is still the bar; the next row is plain background.
        assert_eq!(m.get(6 * 8, 0), s.bar());
        assert_eq!(m.get(1279, 15), s.bar());
        assert_eq!(m.get(0, 16), s.background());
    }

    #[test]
    fn the_stride_is_honoured_and_padding_is_never_written() {
        let mut f = display();
        f.width = 800;
        f.height = 400;
        f.stride = 1000 * 4; // 200 pixels of padding per row
        f.size = u64::from(f.stride) * 400;
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        s.write(&cursor, &mut m, b"hello\nworld");
        for y in 0..400u32 {
            for x in 800..1000u32 {
                let at = (y * 1000 + x) as usize;
                assert_eq!(m.pixels[at], 0xDEAD_BEEF, "padding pixel {x},{y}");
            }
        }
        assert_eq!(m.get(0, 16 + 3), s.background());
    }

    #[test]
    fn a_newline_moves_the_bar_and_the_screen_wraps() {
        let f = display();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        assert_eq!(m.get(0, 0), s.bar());
        s.write(&cursor, &mut m, b"a\n");
        // Row 0 is ordinary text now, the bar is on row 1.
        assert_eq!(m.get(1279, 0), s.background());
        assert_eq!(m.get(0, 16), s.bar());
        assert_eq!(cursor.get(), (0, 1));
        // 50 rows: the bar goes round to row 0 and paints over the old first line.
        s.write(&cursor, &mut m, &[b'\n'; 49]);
        assert_eq!(cursor.get(), (0, 0));
        assert_eq!(m.get(0, 0), s.bar());
    }

    #[test]
    fn a_line_longer_than_the_screen_wraps_and_a_full_row_takes_only_one_row() {
        let mut f = display();
        f.width = 160;
        f.height = 128;
        f.stride = 160 * 4;
        f.size = u64::from(f.stride) * 128;
        let s = Screen::new(&f).unwrap();
        assert_eq!((s.columns(), s.rows()), (20, 8));
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);

        // Exactly one row of text: the newline follows without an empty row between.
        s.write(&cursor, &mut m, &[b'x'; 20]);
        assert_eq!(
            cursor.get(),
            (20, 0),
            "the wrap waits for another character"
        );
        s.write(&cursor, &mut m, b"\n");
        assert_eq!(cursor.get(), (0, 1));
        // 25 bytes take two rows.
        s.write(&cursor, &mut m, &[b'y'; 25]);
        assert_eq!(cursor.get(), (5, 2));
    }

    #[test]
    fn bytes_are_cells_as_utf8_and_controls_say_so() {
        let f = display();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        // A two-byte character, a four-byte one: one cell each. CR does nothing. TAB is a
        // control character with no glyph, so it shows the replacement.
        s.write(&cursor, &mut m, "§😀\r\t".as_bytes());
        assert_eq!(cursor.get(), (3, 0));
        let replacement = |column: u32| {
            (0..8u32).all(|x| {
                let on = font::REPLACEMENT[5] & (0x80 >> x) != 0;
                let expected = if on { s.foreground() } else { s.background() };
                m.get(column * 8 + x, 5) == expected
            })
        };
        assert!(replacement(0) && replacement(1) && replacement(2));
        // Every pixel is valid for any byte: this also covers 0x00 and 0xFF.
        s.write(&cursor, &mut m, &[0x00, 0x7F, 0xC0, 0xF5, 0xFF]);
        assert_eq!(cursor.get(), (8, 0));
    }

    #[test]
    fn scaled_text_covers_whole_blocks() {
        let mut f = display();
        f.width = 2560;
        f.height = 1440;
        f.stride = 2560 * 4;
        f.size = u64::from(f.stride) * 1440;
        let s = Screen::new(&f).unwrap();
        assert_eq!(s.scale(), 2);
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        s.write(&cursor, &mut m, b"T");
        let t = font::glyph(b'T');
        for y in 0..16 {
            for x in 0..8 {
                let on = t[y as usize] & (0x80 >> x) != 0;
                let expected = if on { s.foreground() } else { s.background() };
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    assert_eq!(m.get(2 * x + dx, 2 * y + dy), expected, "{x},{y}");
                }
            }
        }
    }

    #[test]
    fn a_wrong_cursor_cannot_draw_outside_the_screen() {
        let f = display();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        for (column, row) in [(u32::MAX, u32::MAX), (161, 50), (5, 49), (160, 0)] {
            cursor.set(column, row);
            s.write(&cursor, &mut m, b"bound");
            let (c, r) = cursor.get();
            assert!(c <= s.columns() && r < s.rows(), "{c},{r}");
        }
    }

    /// Hostile geometries and contents: nothing panics and every pixel written is inside the
    /// visible rows and aligned, on every framebuffer the description check accepts.
    #[test]
    fn any_accepted_description_and_any_bytes_stay_inside_the_display() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0xF00D);
        let (mut accepted, mut drawn) = (0u32, 0u32);
        for case in 0..crate::testing::cases(15_000) {
            let mut f = display();
            // Small sizes, so a draw is cheap, with edge values mixed in below.
            f.width = 160 + rng.below(160) as u32;
            f.height = 128 + rng.below(96) as u32;
            f.stride = f.width * 4 + rng.below(16) as u32 * 4;
            f.size = u64::from(f.stride) * u64::from(f.height) + rng.below(64) as u64;
            match rng.below(6) {
                0 => f.width = rng.edge_u32(),
                1 => f.height = rng.edge_u32(),
                2 => f.stride = rng.edge_u32(),
                3 => f.base = rng.edge_u64(),
                4 => f.size = rng.edge_u64(),
                _ => {}
            }
            if rng.below(8) == 0 {
                f.red_mask = rng.edge_u32();
                f.green_mask = rng.edge_u32();
                f.blue_mask = rng.edge_u32();
            }
            let Ok(s) = Screen::new(&f) else {
                continue;
            };
            accepted += 1;
            assert!(f.width >= 160 && f.height >= 128 && f.stride / 4 >= f.width);
            assert!(s.columns() >= 20 && s.rows() >= 8, "case {case}: {s:?}");
            if f.visible_bytes() > 1 << 20 {
                continue;
            }
            drawn += 1;
            let mut m = frame(&f);
            let cursor = Cursor::new();
            if rng.below(32) == 0 {
                s.clear(&cursor, &mut m);
            }
            cursor.set(rng.edge_u32(), rng.edge_u32());
            let bytes: Vec<u8> = (0..rng.below(200)).map(|_| rng.byte()).collect();
            s.write(&cursor, &mut m, &bytes);
            let (c, r) = cursor.get();
            assert!(c <= s.columns() && r < s.rows(), "case {case}");
        }
        assert!(
            accepted > 1000 && drawn > 500,
            "{accepted} accepted, {drawn} drawn"
        );
    }
}
