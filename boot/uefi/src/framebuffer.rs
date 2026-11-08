//! Turning the firmware's graphics mode into the boot-info framebuffer (ADR-0010 E1).
//!
//! The kernel never sees UEFI structures (ADR-0002 B4), so the firmware's pixel formats become
//! channel masks here. The mode is whatever the firmware is already displaying: the loader
//! changes no mode, because a mode change can fail or blank the screen and the current mode is
//! known to be visible.
//!
//! A mode that is unusable is not a failed boot. The loader says why and hands the kernel
//! [`Framebuffer::NONE`], and the kernel carries on with its serial console.
//!
//! References: UEFI 2.11 §12.9 `EFI_GRAPHICS_OUTPUT_PROTOCOL` (the pixel format definitions
//! and `PixelsPerScanLine`), confirmed against EDK II's `GraphicsOutput.h`.

use origin_boot_info::{Framebuffer, FramebufferError};

use crate::protocols::{GraphicsModeInformation, pixel_format};

/// Why a graphics mode cannot be handed to the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unusable {
    /// The firmware gave no mode to look at.
    NoMode,
    /// `PixelBltOnly`: the mode has no framebuffer the kernel could draw on.
    BltOnly,
    /// A pixel format value this loader does not know.
    Format(u32),
    /// A `PixelBitMask` mode whose pixels are not 32 bits wide.
    PixelSize,
    /// `PixelsPerScanLine` times four does not fit in the stride.
    Stride,
    /// What the check in `origin_boot_info` found wrong with the result.
    Invalid(FramebufferError),
}

/// The masks of a mode. `None` when the format has no framebuffer.
fn channel_masks(info: &GraphicsModeInformation) -> Result<(u32, u32, u32), Unusable> {
    match info.pixel_format {
        // Byte zero is the channel that comes first in the name, and a pixel is read as a
        // little-endian 32-bit value.
        pixel_format::RGB => Ok((0x0000_00FF, 0x0000_FF00, 0x00FF_0000)),
        pixel_format::BGR => Ok((0x00FF_0000, 0x0000_FF00, 0x0000_00FF)),
        pixel_format::BIT_MASK => {
            let used = info.red_mask | info.green_mask | info.blue_mask | info.reserved_mask;
            // The highest bit in use decides how wide a pixel is. The kernel only draws
            // 32-bit pixels, and masks of 24 bits or fewer describe something narrower.
            if used >> 24 == 0 {
                return Err(Unusable::PixelSize);
            }
            Ok((info.red_mask, info.green_mask, info.blue_mask))
        }
        pixel_format::BLT_ONLY => Err(Unusable::BltOnly),
        other => Err(Unusable::Format(other)),
    }
}

/// The boot-info description of the mode `info`, whose framebuffer is at `base`.
///
/// `virtual_base` is left 0: the page tables are built after this, from the result.
pub fn describe(
    info: &GraphicsModeInformation,
    base: u64,
    size: u64,
) -> Result<Framebuffer, Unusable> {
    let (red_mask, green_mask, blue_mask) = channel_masks(info)?;
    let framebuffer = Framebuffer {
        base,
        size,
        virtual_base: 0,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        stride: info
            .pixels_per_scan_line
            .checked_mul(4)
            .ok_or(Unusable::Stride)?,
        red_mask,
        green_mask,
        blue_mask,
    };
    framebuffer.check().map_err(Unusable::Invalid)?;
    Ok(framebuffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgr() -> GraphicsModeInformation {
        GraphicsModeInformation {
            version: 0,
            horizontal_resolution: 1280,
            vertical_resolution: 800,
            pixel_format: pixel_format::BGR,
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
            reserved_mask: 0,
            pixels_per_scan_line: 1280,
        }
    }

    #[test]
    fn the_two_byte_formats_become_masks() {
        let f = describe(&bgr(), 0x8000_0000, 0x40_0000).unwrap();
        assert_eq!(
            f,
            Framebuffer {
                base: 0x8000_0000,
                size: 0x40_0000,
                virtual_base: 0,
                width: 1280,
                height: 800,
                stride: 5120,
                red_mask: 0x00FF_0000,
                green_mask: 0x0000_FF00,
                blue_mask: 0x0000_00FF,
            }
        );
        let mut info = bgr();
        info.pixel_format = pixel_format::RGB;
        let f = describe(&info, 0x8000_0000, 0x40_0000).unwrap();
        assert_eq!(
            (f.red_mask, f.green_mask, f.blue_mask),
            (0xFF, 0xFF00, 0xFF_0000)
        );
    }

    #[test]
    fn a_wider_scan_line_keeps_its_padding() {
        let mut info = bgr();
        info.pixels_per_scan_line = 1366;
        info.horizontal_resolution = 1360;
        let f = describe(&info, 0x8000_0000, 0x80_0000).unwrap();
        assert_eq!(f.stride, 1366 * 4);
        assert_eq!(f.width, 1360);
    }

    #[test]
    fn a_bit_mask_mode_keeps_its_masks_when_it_is_32_bit() {
        let mut info = bgr();
        info.pixel_format = pixel_format::BIT_MASK;
        info.red_mask = 0x3FF0_0000;
        info.green_mask = 0x000F_FC00;
        info.blue_mask = 0x0000_03FF;
        info.reserved_mask = 0xC000_0000;
        let f = describe(&info, 0x8000_0000, 0x40_0000).unwrap();
        assert_eq!(
            (f.red_mask, f.green_mask, f.blue_mask),
            (0x3FF0_0000, 0x000F_FC00, 0x0000_03FF)
        );

        // 8:8:8 with no reserved bits: 24-bit pixels, which the kernel does not draw.
        info.red_mask = 0x00FF_0000;
        info.green_mask = 0x0000_FF00;
        info.blue_mask = 0x0000_00FF;
        info.reserved_mask = 0;
        assert_eq!(
            describe(&info, 0x8000_0000, 0x40_0000),
            Err(Unusable::PixelSize)
        );
    }

    #[test]
    fn modes_with_no_usable_framebuffer_are_named_not_fatal() {
        let mut info = bgr();
        info.pixel_format = pixel_format::BLT_ONLY;
        assert_eq!(describe(&info, 0, 0), Err(Unusable::BltOnly));
        info.pixel_format = 4;
        assert_eq!(describe(&info, 0, 0), Err(Unusable::Format(4)));
        // A base of zero, or a size smaller than the visible rows.
        assert_eq!(
            describe(&bgr(), 0, 0x40_0000),
            Err(Unusable::Invalid(FramebufferError::Absent))
        );
        assert_eq!(
            describe(&bgr(), 0x8000_0000, 4096),
            Err(Unusable::Invalid(FramebufferError::Size))
        );
        let mut huge = bgr();
        huge.pixels_per_scan_line = u32::MAX;
        assert_eq!(
            describe(&huge, 0x8000_0000, 0x40_0000),
            Err(Unusable::Stride)
        );
    }
}
