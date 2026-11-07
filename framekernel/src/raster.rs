//! A 2D software rasterizer (CLAUDE.md §50 Origin UI, §52 the compositor's software
//! fallback): fills, blits and alpha blends over a 32-bit pixel surface. Pure, no `unsafe`,
//! bounds-checked and clipped, tested on the host; the compositor will draw with it, to a
//! framebuffer or an off-screen surface.
//!
//! Pixels are `0xAARRGGBB` (straight alpha). The surface is a borrowed row-major buffer
//! with a stride, so a view into a larger framebuffer works. Every operation clips to the
//! surface, so out-of-bounds coordinates are safe and simply draw nothing where they fall
//! outside.

/// A packed `0xAARRGGBB` pixel. Helpers keep the channel layout in one place.
pub mod color {
    pub const fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
        ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
        argb(0xFF, r, g, b)
    }
    pub const BLACK: u32 = rgb(0, 0, 0);
    pub const WHITE: u32 = rgb(0xFF, 0xFF, 0xFF);
}

/// A rectangle. A zero or negative size is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// The rectangle clipped to `0..width, 0..height`, as inclusive-exclusive pixel bounds
    /// `(x0, y0, x1, y1)`, or `None` if nothing is left.
    fn clip(&self, width: i32, height: i32) -> Option<(i32, i32, i32, i32)> {
        let x0 = self.x.max(0);
        let y0 = self.y.max(0);
        let x1 = self.x.saturating_add(self.w).min(width);
        let y1 = self.y.saturating_add(self.h).min(height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }
}

/// A borrowed 32-bit pixel surface: `width` × `height`, each row `stride` pixels apart
/// (`stride >= width`).
#[derive(Debug)]
pub struct Surface<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
    stride: i32,
}

impl<'a> Surface<'a> {
    /// Wraps `pixels` as a `width` × `height` surface with the given row stride, or `None`
    /// if the dimensions are not positive or the buffer is too small.
    pub fn new(pixels: &'a mut [u32], width: i32, height: i32, stride: i32) -> Option<Self> {
        if width <= 0 || height <= 0 || stride < width {
            return None;
        }
        let needed = (stride as usize).checked_mul(height as usize)?;
        if pixels.len() < needed {
            return None;
        }
        Some(Self {
            pixels,
            width,
            height,
            stride,
        })
    }

    pub const fn width(&self) -> i32 {
        self.width
    }
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// The pixel at `(x, y)`, or `None` if outside.
    pub fn get(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize) * (self.stride as usize) + (x as usize);
        self.pixels.get(i).copied()
    }

    fn set(&mut self, x: i32, y: i32, px: u32) {
        let i = (y as usize) * (self.stride as usize) + (x as usize);
        if let Some(slot) = self.pixels.get_mut(i) {
            *slot = px;
        }
    }

    /// Fills the whole surface with `color`.
    pub fn clear(&mut self, color: u32) {
        self.fill_rect(Rect::new(0, 0, self.width, self.height), color);
    }

    /// Fills `rect` (clipped) with the opaque `color`.
    pub fn fill_rect(&mut self, rect: Rect, color: u32) {
        if let Some((x0, y0, x1, y1)) = rect.clip(self.width, self.height) {
            for y in y0..y1 {
                for x in x0..x1 {
                    self.set(x, y, color);
                }
            }
        }
    }

    /// Alpha-blends `color` over `rect` (clipped): `src` over `dst`, straight alpha.
    pub fn blend_rect(&mut self, rect: Rect, color: u32) {
        if let Some((x0, y0, x1, y1)) = rect.clip(self.width, self.height) {
            for y in y0..y1 {
                for x in x0..x1 {
                    let under = self.get(x, y).unwrap_or(0);
                    self.set(x, y, blend(color, under));
                }
            }
        }
    }

    /// A one-pixel outline of `rect` (clipped) in `color`.
    pub fn stroke_rect(&mut self, rect: Rect, color: u32) {
        self.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1), color);
        self.fill_rect(Rect::new(rect.x, rect.y + rect.h - 1, rect.w, 1), color);
        self.fill_rect(Rect::new(rect.x, rect.y, 1, rect.h), color);
        self.fill_rect(Rect::new(rect.x + rect.w - 1, rect.y, 1, rect.h), color);
    }

    /// Copies `src` (a `sw` × `sh` buffer, row-major, tightly packed) to `(dx, dy)`,
    /// clipped, opaque. A source pixel outside `src` is skipped.
    pub fn blit(&mut self, src: &[u32], sw: i32, sh: i32, dx: i32, dy: i32) {
        let Some((x0, y0, x1, y1)) = Rect::new(dx, dy, sw, sh).clip(self.width, self.height) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let (sx, sy) = (x - dx, y - dy);
                if sx >= 0 && sy >= 0 && sx < sw && sy < sh {
                    let i = (sy as usize) * (sw as usize) + (sx as usize);
                    if let Some(&px) = src.get(i) {
                        self.set(x, y, px);
                    }
                }
            }
        }
    }
}

/// `src` over `dst` with straight alpha (Porter-Duff "over"): the result is opaque when
/// `dst` is. Rounding is `(a*s + (255-a)*d + 127) / 255` per channel.
pub fn blend(src: u32, dst: u32) -> u32 {
    let a = (src >> 24) & 0xFF;
    let chan = |shift: u32| {
        let s = (src >> shift) & 0xFF;
        let d = (dst >> shift) & 0xFF;
        let v = (a * s + (255 - a) * d + 127) / 255;
        v & 0xFF
    };
    let out_a = {
        let d = (dst >> 24) & 0xFF;
        (a + (255 - a) * d / 255) & 0xFF
    };
    (out_a << 24) | (chan(16) << 16) | (chan(8) << 8) | chan(0)
}

/// A fixed demonstration scene, drawn by both the host test and the kernel so their output
/// can be compared bit for bit. It exercises clear, fills (including off the edges),
/// stroke, a translucent blend and a blit.
pub fn demo_scene(s: &mut Surface<'_>) {
    s.clear(color::rgb(0x10, 0x10, 0x18));
    // Three overlapping opaque rectangles, two running off the edges.
    s.fill_rect(Rect::new(-10, -10, 40, 40), color::rgb(0xC0, 0x30, 0x30));
    s.fill_rect(Rect::new(20, 10, 40, 30), color::rgb(0x30, 0xC0, 0x30));
    s.fill_rect(
        Rect::new(s.width() - 20, s.height() - 20, 40, 40),
        color::rgb(0x30, 0x30, 0xC0),
    );
    // A one-pixel frame inset by two pixels.
    s.stroke_rect(Rect::new(2, 2, s.width() - 4, s.height() - 4), color::WHITE);
    // A half-transparent white wash over a central band.
    s.blend_rect(
        Rect::new(0, s.height() / 2 - 4, s.width(), 8),
        color::argb(0x80, 0xFF, 0xFF, 0xFF),
    );
    // A small checker sprite blitted near the top-left.
    let mut sprite = [0u32; 8 * 8];
    for (i, px) in sprite.iter_mut().enumerate() {
        let (x, y) = (i % 8, i / 8);
        *px = if (x + y) % 2 == 0 {
            color::rgb(0xFF, 0xD0, 0x00)
        } else {
            color::rgb(0x00, 0x00, 0x00)
        };
    }
    s.blit(&sprite, 8, 8, 6, 6);
}

/// The CRC-32 of [`demo_scene`] on a 160×48 surface (proven by the host test, checked by
/// the kernel, so the two renderers agree bit for bit).
pub const DEMO_WIDTH: i32 = 160;
pub const DEMO_HEIGHT: i32 = 48;
pub const DEMO_CRC: u32 = 0x912d_f877;

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    #[test]
    fn fills_and_clipping() {
        let mut buf = vec![0u32; 10 * 10];
        let mut s = Surface::new(&mut buf, 10, 10, 10).unwrap();
        s.clear(color::BLACK);
        s.fill_rect(Rect::new(2, 2, 3, 3), color::WHITE);
        assert_eq!(s.get(2, 2), Some(color::WHITE));
        assert_eq!(s.get(4, 4), Some(color::WHITE));
        assert_eq!(s.get(5, 5), Some(color::BLACK), "exclusive edge");
        assert_eq!(s.get(1, 1), Some(color::BLACK));
        // A rect entirely off the surface draws nothing and does not panic.
        s.fill_rect(Rect::new(-50, -50, 10, 10), color::WHITE);
        s.fill_rect(Rect::new(100, 100, 10, 10), color::WHITE);
        // A rect straddling the corner clips.
        s.fill_rect(Rect::new(-2, -2, 4, 4), color::rgb(1, 2, 3));
        assert_eq!(s.get(0, 0), Some(color::rgb(1, 2, 3)));
        assert_eq!(s.get(2, 0), Some(color::BLACK));
        assert_eq!(s.get(10, 0), None, "outside");
    }

    #[test]
    fn blend_math() {
        // Opaque source replaces the destination; zero alpha leaves it.
        assert_eq!(
            blend(color::rgb(10, 20, 30), color::rgb(1, 2, 3)),
            color::rgb(10, 20, 30)
        );
        assert_eq!(
            blend(color::argb(0, 9, 9, 9), color::rgb(1, 2, 3)),
            color::rgb(1, 2, 3)
        );
        // 50% white over black is ~128 grey, and the result is opaque.
        let g = blend(color::argb(0x80, 0xFF, 0xFF, 0xFF), color::BLACK);
        assert_eq!(g >> 24, 0xFF);
        assert_eq!(g & 0xFF, 128);
        assert_eq!((g >> 8) & 0xFF, 128);
    }

    #[test]
    fn blit_with_clipping() {
        let mut buf = vec![color::BLACK; 6 * 6];
        let mut s = Surface::new(&mut buf, 6, 6, 6).unwrap();
        let src: std::vec::Vec<u32> = (0..9).map(|i| color::rgb(i as u8, 0, 0)).collect();
        s.blit(&src, 3, 3, 4, 4); // bottom-right corner, clips to a 2x2 region
        assert_eq!(s.get(4, 4), Some(color::rgb(0, 0, 0)));
        assert_eq!(s.get(5, 5), Some(color::rgb(4, 0, 0)));
        assert_eq!(s.get(5, 4), Some(color::rgb(1, 0, 0)));
        // A fully off-surface blit is a no-op.
        s.blit(&src, 3, 3, -5, -5);
    }

    #[test]
    fn surface_construction_rejects_bad_dimensions() {
        let mut buf = vec![0u32; 4];
        assert!(Surface::new(&mut buf, 0, 1, 1).is_none());
        assert!(Surface::new(&mut buf, 3, 1, 2).is_none(), "stride < width");
        assert!(
            Surface::new(&mut buf, 2, 3, 2).is_none(),
            "buffer too small"
        );
        assert!(Surface::new(&mut buf, 2, 2, 2).is_some());
    }

    #[test]
    fn the_demo_scene_is_deterministic_and_matches_the_published_crc() {
        let mut buf = vec![0u32; (DEMO_WIDTH * DEMO_HEIGHT) as usize];
        let mut s = Surface::new(&mut buf, DEMO_WIDTH, DEMO_HEIGHT, DEMO_WIDTH).unwrap();
        demo_scene(&mut s);
        // Rendering again yields identical bytes.
        let mut buf2 = vec![0u32; (DEMO_WIDTH * DEMO_HEIGHT) as usize];
        let mut s2 = Surface::new(&mut buf2, DEMO_WIDTH, DEMO_HEIGHT, DEMO_WIDTH).unwrap();
        demo_scene(&mut s2);
        assert_eq!(buf, buf2);
        // The CRC the kernel will check against.
        let bytes: std::vec::Vec<u8> = buf.iter().flat_map(|p| p.to_le_bytes()).collect();
        let crc = crate::gpt::crc32(&bytes);
        assert_eq!(crc, EXPECTED, "update DEMO_CRC to {crc:#010x}");
        assert_eq!(DEMO_CRC, EXPECTED, "DEMO_CRC must match the proven CRC");
    }

    /// The CRC proven here; `DEMO_CRC` mirrors it for the kernel to check against.
    const EXPECTED: u32 = 0x912d_f877;
}
