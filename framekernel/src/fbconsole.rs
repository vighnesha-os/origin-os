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
//! Drawing can stop after any pixel and carry on later. Everything the console paints is a
//! rectangle, solid or a glyph, so an unfinished character is a few rectangles and how far the
//! current one has got ([`Painting`]). [`Backlog`] keeps accepted text in order and paints it a
//! given number of pixels at a time. The caller chooses how many, and what happens in between:
//! the kernel uses this to bound the uncached memory it writes in one stretch with interrupts
//! off (`console.rs` gives the bound and the reasoning).
//!
//! Nothing here can panic. A framebuffer whose description is refused gets no console at all;
//! an accepted one's every pixel is inside the visible rows, and a wrong cursor is clamped.
//! The kernel's sink checks each offset again anyway.

use core::sync::atomic::{AtomicU64, Ordering};

use origin_boot_info::{Framebuffer, FramebufferError};

use crate::font;
use crate::linequeue::LineQueue;

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

    /// The rest of text row `row` from `column` on, in the background: how a line ends.
    fn end_of_row(&self, column: u32, row: u32) -> Stroke {
        let top = row * self.row_height();
        Stroke::new(
            column * self.column_width(),
            top,
            self.width,
            top + self.row_height(),
            Ink::Solid(self.background),
        )
    }

    /// All of text row `row`, as the bar.
    fn bar_row(&self, row: u32) -> Stroke {
        let top = row * self.row_height();
        Stroke::new(
            0,
            top,
            self.width,
            top + self.row_height(),
            Ink::Solid(self.bar),
        )
    }

    /// The glyph of `byte` in the cell at `(column, row)`.
    fn glyph_cell(&self, column: u32, row: u32, byte: u8) -> Stroke {
        let (left, top) = (column * self.column_width(), row * self.row_height());
        Stroke::new(
            left,
            top,
            left + self.column_width(),
            top + self.row_height(),
            Ink::Glyph(font::glyph(byte)),
        )
    }

    /// Paints `stroke` from where it had got to, stopping when `budget` runs out. True when
    /// the stroke is finished. Each row is a run: as many of its pixels as the budget allows,
    /// in one tight loop, because this is what every pixel of the console costs.
    fn paint_stroke(
        &self,
        out: &mut impl PixelSink,
        stroke: &mut Stroke,
        budget: &mut u32,
    ) -> bool {
        while stroke.y < stroke.bottom {
            let run = (stroke.right - stroke.x).min(*budget);
            let first = self.offset(stroke.x, stroke.y);
            match stroke.ink {
                Ink::Solid(value) => {
                    for pixel in 0..run as usize {
                        out.write32(first + pixel * 4, value);
                    }
                }
                Ink::Glyph(bitmap) => {
                    // Scale 1 is nearly every display, and a division per pixel would be the
                    // most expensive thing here.
                    let scaled = |d: u32| if self.scale == 1 { d } else { d / self.scale };
                    let cell_row = scaled(stroke.y - stroke.top) as usize;
                    let bits = bitmap.get(cell_row).copied().unwrap_or(0);
                    let start = stroke.x - stroke.left;
                    for pixel in 0..run {
                        let lit = bits & (0x80 >> scaled(start + pixel)) != 0;
                        let value = if lit {
                            self.foreground
                        } else {
                            self.background
                        };
                        out.write32(first + pixel as usize * 4, value);
                    }
                }
            }
            stroke.x += run;
            *budget -= run;
            if stroke.x < stroke.right {
                return false;
            }
            stroke.x = stroke.left;
            stroke.y += 1;
        }
        true
    }

    /// Paints the whole display and moves the cursor to the top-left cell.
    pub fn clear(&self, cursor: &Cursor, out: &mut impl PixelSink) {
        let mut budget = u32::MAX;
        let mut all = Stroke::new(0, 0, self.width, self.height, Ink::Solid(self.background));
        self.paint_stroke(out, &mut all, &mut budget);
        cursor.set(0, 0);
        self.paint_stroke(out, &mut self.bar_row(0), &mut budget);
    }

    /// Starts one byte at the cursor and moves the cursor past it: what is left is painting
    /// the rectangles this returns. `\n` ends a line, `\r` does nothing, printable ASCII draws
    /// its glyph, and anything else draws the replacement glyph. A cell past the last column
    /// wraps to the next row. See the module documentation for the rows.
    fn begin(&self, cursor: &Cursor, byte: u8) -> Painting {
        let (mut column, mut row) = self.clamped(cursor);
        let mut painting = Painting::new();
        match byte {
            b'\n' => {
                painting.add(self.end_of_row(column, row));
                row = (row + 1) % self.rows;
                column = 0;
                painting.add(self.bar_row(row));
            }
            b'\r' => {}
            // UTF-8 continuation bytes: the lead byte already drew the character's cell.
            0x80..=0xBF => {}
            _ => {
                if column == self.columns {
                    // A full row: only the margin to its right is still the bar's.
                    painting.add(self.end_of_row(column, row));
                    row = (row + 1) % self.rows;
                    column = 0;
                    painting.add(self.bar_row(row));
                }
                painting.add(self.glyph_cell(column, row, byte));
                column += 1;
            }
        }
        cursor.set(column, row);
        painting
    }

    /// Paints `painting` from where it had got to, stopping when `budget` runs out. True when
    /// the whole character is drawn.
    fn paint(&self, out: &mut impl PixelSink, painting: &mut Painting, budget: &mut u32) -> bool {
        while painting.next < painting.count {
            let Some(stroke) = painting.strokes.get_mut(painting.next) else {
                break;
            };
            if !self.paint_stroke(out, stroke, budget) {
                return false;
            }
            painting.next += 1;
        }
        true
    }

    /// Where the cursor says the next cell is, made to be on the screen whatever it held.
    fn clamped(&self, cursor: &Cursor) -> (u32, u32) {
        let (column, row) = cursor.get();
        let column = if column > self.columns { 0 } else { column };
        (column, row % self.rows)
    }

    /// Writes `bytes` at the cursor, all of it now.
    pub fn write(&self, cursor: &Cursor, out: &mut impl PixelSink, bytes: &[u8]) {
        let (column, row) = self.clamped(cursor);
        cursor.set(column, row);
        for &byte in bytes {
            let mut painting = self.begin(cursor, byte);
            let mut budget = u32::MAX;
            self.paint(out, &mut painting, &mut budget);
        }
    }
}

/// What a [`Stroke`] paints with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ink {
    /// One pixel value throughout.
    Solid(u32),
    /// A glyph's bitmap, in the foreground and background colors.
    Glyph(&'static [u8; 16]),
}

/// A rectangle to paint and how far that has got: `(x, y)` is the next pixel, and `y` reaches
/// `bottom` when it is finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stroke {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    ink: Ink,
    x: u32,
    y: u32,
}

impl Stroke {
    const fn new(left: u32, top: u32, right: u32, bottom: u32, ink: Ink) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            ink,
            x: left,
            y: top,
        }
    }
}

/// The rest of one character's drawing: at most three rectangles (ending a row, the next row's
/// bar, a glyph).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Painting {
    strokes: [Stroke; 3],
    count: usize,
    next: usize,
}

impl Painting {
    const fn new() -> Self {
        Self {
            strokes: [Stroke::new(0, 0, 0, 0, Ink::Solid(0)); 3],
            count: 0,
            next: 0,
        }
    }

    fn add(&mut self, stroke: Stroke) {
        if let Some(slot) = self.strokes.get_mut(self.count) {
            *slot = stroke;
            self.count += 1;
        }
    }
}

/// What a backlog shows in place of lines it had no room for.
const LOST_NOTICE: &[u8] =
    b"origin-kernel: console: the screen fell behind; some lines are on the UART only\n";

/// Text the console has accepted and the screen has not shown yet, oldest first, and the
/// character being painted. The caller serializes everything; nothing here waits.
///
/// Positions count bytes from the start of time: [`Backlog::push`] returns the position the
/// painting must reach for that text to be fully on the screen, and [`Backlog::painted`] says
/// how far it has got.
#[derive(Debug)]
pub struct Backlog<const N: usize> {
    /// The text not yet started.
    lines: LineQueue<N>,
    /// Bytes ever finished: a byte counts once every pixel of it is written.
    painted: u64,
    /// The character in progress.
    painting: Option<Painting>,
}

impl<const N: usize> Backlog<N> {
    /// An empty backlog.
    pub const fn new() -> Self {
        Self {
            lines: LineQueue::new(LOST_NOTICE),
            painted: 0,
            painting: None,
        }
    }

    /// Whether [`Self::push`] would take `bytes` more bytes now.
    pub const fn has_room(&self, bytes: usize) -> bool {
        self.lines.has_room(bytes)
    }

    /// Accepts `text`, whole or not at all, and returns the position at which it will be
    /// fully on the screen. `None` when there is no room.
    pub fn push(&mut self, text: &[u8]) -> Option<u64> {
        self.lines.push(text)
    }

    /// Records that a line had to be dropped because a caller could not wait for room: the
    /// next line that fits is preceded by a notice saying so.
    pub const fn note_lost(&mut self) {
        self.lines.note_lost();
    }

    /// Paints at most `pixels` more pixels of what is queued, oldest first, starting where the
    /// last call stopped. Returns whether anything is left to paint.
    pub fn paint(
        &mut self,
        screen: &Screen,
        cursor: &Cursor,
        out: &mut impl PixelSink,
        pixels: u32,
    ) -> bool {
        let mut budget = pixels;
        loop {
            if let Some(painting) = self.painting.as_mut() {
                if !screen.paint(out, painting, &mut budget) {
                    return true;
                }
                self.painting = None;
                self.painted += 1;
            }
            match self.lines.pop() {
                Some(byte) => self.painting = Some(screen.begin(cursor, byte)),
                None => return false,
            }
        }
    }

    /// The position the painting has reached (see the type's documentation).
    pub const fn painted(&self) -> u64 {
        self.painted
    }

    /// Whether everything accepted is on the screen.
    pub const fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.painting.is_none()
    }

    /// Forgets everything queued: the screen is gone.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.painting = None;
        self.painted = self.lines.queued();
    }
}

impl<const N: usize> Default for Backlog<N> {
    fn default() -> Self {
        Self::new()
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

    /// A display small enough to compare whole frames.
    fn small() -> Framebuffer {
        let mut f = display();
        f.width = 160;
        f.height = 128;
        f.stride = 160 * 4;
        f.size = u64::from(f.stride) * 128;
        f
    }

    /// Counts the pixels written, and forwards them.
    struct Counting<'a> {
        memory: &'a mut Memory,
        written: u32,
    }

    impl PixelSink for Counting<'_> {
        fn write32(&mut self, offset: usize, value: u32) {
            self.written += 1;
            self.memory.write32(offset, value);
        }
    }

    /// The frame `Screen::write` draws for `text`: the reference every incremental painting
    /// must reach.
    fn written(s: &Screen, f: &Framebuffer, text: &[u8]) -> Memory {
        let mut m = frame(f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        s.write(&cursor, &mut m, text);
        m
    }

    #[test]
    fn painting_in_any_steps_draws_exactly_what_writing_draws() {
        use crate::testing::Rng;
        let f = small();
        let s = Screen::new(&f).unwrap();
        let mut rng = Rng::new(0xC0FF);
        for case in 0..crate::testing::cases(600) {
            let text: Vec<u8> = (0..rng.below(120)).map(|_| rng.byte()).collect();
            // A cursor and a frame in the state the reference draws from.
            let mut m = frame(&f);
            let cursor = Cursor::new();
            s.clear(&cursor, &mut m);
            let mut backlog: Backlog<256> = Backlog::new();
            let end = backlog.push(&text).unwrap();
            assert_eq!(end, text.len() as u64);
            let mut more = true;
            while more {
                // Any step, down to a single pixel, and a step of no pixels at all.
                let step = rng.below(40) as u32;
                let mut counted = Counting {
                    memory: &mut m,
                    written: 0,
                };
                more = backlog.paint(&s, &cursor, &mut counted, step);
                assert!(counted.written <= step, "case {case}: over the budget");
            }
            assert_eq!(backlog.painted(), end, "case {case}");
            assert!(backlog.is_empty());
            assert_eq!(
                m.pixels,
                written(&s, &f, &text).pixels,
                "case {case}: {text:?}"
            );
            let (c, r) = cursor.get();
            assert!(c <= s.columns() && r < s.rows(), "case {case}");
        }
    }

    #[test]
    fn a_section_writes_no_more_pixels_than_it_is_given() {
        let f = display();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        let mut backlog: Backlog<256> = Backlog::new();
        backlog
            .push(b"the line is long enough for many sections\n")
            .unwrap();
        let mut sections = 0;
        loop {
            let mut counted = Counting {
                memory: &mut m,
                written: 0,
            };
            let more = backlog.paint(&s, &cursor, &mut counted, 256);
            assert!(counted.written <= 256);
            sections += 1;
            if !more {
                break;
            }
            assert_eq!(counted.written, 256, "only the last section may be short");
        }
        // 41 glyphs of 8 by 16, the rest of the row, and a whole row of bar: 160 sections.
        let pixels = 41 * 128 + (1280 - 41 * 8) * 16 + 1280 * 16;
        assert_eq!(pixels, 40_960);
        assert_eq!(sections, pixels / 256);
    }

    #[test]
    fn a_line_is_fully_drawn_at_the_position_its_push_returned_and_not_before() {
        let f = small();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        let mut backlog: Backlog<64> = Backlog::new();
        let end_of_a = backlog.push(b"a\n").unwrap();
        let end_of_b = backlog.push(b"b\n").unwrap();
        assert_eq!((end_of_a, end_of_b), (2, 4));
        // One pixel at a time: at the moment the position passes the first line's end, the
        // frame holds that line and nothing of the next.
        while backlog.painted() < end_of_a {
            backlog.paint(&s, &cursor, &mut m, 1);
        }
        assert_eq!(m.pixels, written(&s, &f, b"a\n").pixels);
        while backlog.paint(&s, &cursor, &mut m, 1) {}
        assert_eq!(backlog.painted(), end_of_b);
        assert_eq!(m.pixels, written(&s, &f, b"a\nb\n").pixels);
    }

    #[test]
    fn text_is_taken_whole_or_not_at_all() {
        let f = small();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        let mut backlog: Backlog<8> = Backlog::new();
        assert_eq!(backlog.push(b"12345678"), Some(8));
        assert_eq!(backlog.push(b"9"), None, "full");
        // Refusing nothing changed: the next paint draws the eight bytes.
        while backlog.paint(&s, &cursor, &mut m, u32::MAX) {}
        assert_eq!(backlog.painted(), 8);
        // Drawn, so there is room again, and the ring wraps.
        assert_eq!(backlog.push(b"abcdefgh"), Some(16));
        assert!(!backlog.has_room(1), "full again");
        while backlog.paint(&s, &cursor, &mut m, u32::MAX) {}
        assert_eq!(m.pixels, written(&s, &f, b"12345678abcdefgh").pixels);
    }

    #[test]
    fn a_lost_line_is_owned_up_to_before_the_next_line_that_fits() {
        let f = small();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        let mut backlog: Backlog<256> = Backlog::new();
        backlog.note_lost();
        let end = backlog.push(b"after\n").unwrap();
        assert_eq!(end, (LOST_NOTICE.len() + 6) as u64);
        while backlog.paint(&s, &cursor, &mut m, u32::MAX) {}
        let shown: Vec<u8> = [LOST_NOTICE, b"after\n"].concat();
        assert_eq!(m.pixels, written(&s, &f, &shown).pixels);
        // Said once: the next line has no notice.
        assert_eq!(backlog.push(b"x"), Some(end + 1));
        // When the notice cannot fit, the line waits with it.
        let mut tiny: Backlog<8> = Backlog::new();
        tiny.note_lost();
        assert_eq!(tiny.push(b"line"), None);
    }

    /// Writers pushing lines between painting steps, in any interleaving: the screen ends up
    /// as if the lines had been written whole, in the order they were accepted.
    #[test]
    fn any_interleaving_of_pushes_and_painting_keeps_the_order() {
        use crate::testing::Rng;
        let f = small();
        let s = Screen::new(&f).unwrap();
        let mut rng = Rng::new(0xBEEF);
        for case in 0..crate::testing::cases(400) {
            let mut m = frame(&f);
            let cursor = Cursor::new();
            s.clear(&cursor, &mut m);
            let mut backlog: Backlog<64> = Backlog::new();
            let mut accepted: Vec<u8> = Vec::new();
            for _ in 0..20 {
                let line: Vec<u8> = (0..rng.below(20))
                    .map(|_| b'a' + rng.below(26) as u8)
                    .collect();
                if backlog.push(&line).is_some() {
                    accepted.extend_from_slice(&line);
                }
                for _ in 0..rng.below(4) {
                    backlog.paint(&s, &cursor, &mut m, rng.below(64) as u32);
                }
            }
            while backlog.paint(&s, &cursor, &mut m, 32) {}
            assert_eq!(
                m.pixels,
                written(&s, &f, &accepted).pixels,
                "case {case}: {accepted:?}"
            );
        }
    }

    #[test]
    fn clearing_the_backlog_forgets_the_character_in_progress() {
        let f = small();
        let s = Screen::new(&f).unwrap();
        let mut m = frame(&f);
        let cursor = Cursor::new();
        s.clear(&cursor, &mut m);
        let mut backlog: Backlog<64> = Backlog::new();
        let end = backlog.push(b"gone\n").unwrap();
        assert!(backlog.paint(&s, &cursor, &mut m, 3));
        backlog.clear();
        assert!(backlog.is_empty());
        assert_eq!(backlog.painted(), end);
        assert!(!backlog.paint(&s, &cursor, &mut m, 100));
    }
}
