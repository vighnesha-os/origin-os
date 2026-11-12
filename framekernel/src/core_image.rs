//! `origin.core`, as the kernel carries it (ADR-0011 N5; Amendment 2, L2 to L7; task S4-2).
//!
//! `origin.core` is the kernel's only door for a program: the generated system-call stubs and
//! nothing else, one image a release. Until the Loader Service exists (milestone M6) the kernel
//! carries it in its own image, whose signature covers it, as it carries its programs
//! ([`crate::program`]). The build links it once, and `onbf-pack corelib` lays it out as a
//! [`CoreImage`]: its code, each export's name and place, the place of each call's one `syscall`
//! instruction, and the release fingerprint of the definition its stubs were generated from.
//! For the kernel's own target that description is compiled in here. Nothing is parsed at run
//! time, and nothing of the linker's own tables reaches the kernel or a process.
//!
//! [`check`] holds a description to what the kernel relies on, and it runs when the kernel is
//! compiled: a kernel whose `origin.core` was generated from another definition than its own
//! dispatcher, or whose description names a `syscall` where the code has none, does not build.
//!
//! # One set of frames (L2)
//!
//! At boot, before any process exists, [`install`] takes one frame for each page of the image,
//! writes the code into them, and keeps them here. That is the only time they are written: this
//! module keeps the frames to itself and hands out no way to reach them but the mapping of them,
//! read-execute, into a process's space. The framework counts each frame's executable mappings
//! and refuses a writable mapping of a frame that is executable anywhere
//! (`origin_frame::aliases`), so no process can come to write them either.
#![forbid(unsafe_code)]

use origin_syscalls::origin_kernel::kernel::server::{Pins, RELEASE_FINGERPRINT};

/// Bytes in a page.
pub const PAGE: u64 = 4096;
/// The most `origin.core`'s code may map: far above what stubs alone come to (a few hundred
/// bytes today), and small enough that its pages are counted in a `u32` anywhere.
pub const MAX_SPAN: u64 = 1 << 20;
/// The two bytes of a `syscall` instruction (Intel SDM Volume 2, `SYSCALL`; AMD APM Volume 3).
pub const SYSCALL: [u8; 2] = [0x0F, 0x05];

/// One export of `origin.core`: what an import of a program is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreExport {
    /// Its name: `origin_kernel_<method>`.
    pub name: &'static str,
    /// Where its stub starts, from the image's base.
    pub offset: u64,
    /// The version of `origin.kernel` its method arrived in: `(major, minor)` (L5).
    pub since: (u16, u16),
}

/// `origin.core`, as the build laid it out.
#[derive(Clone, Copy, Debug)]
pub struct CoreImage {
    /// The bytes its code maps, a whole number of pages. The image's base is where the first
    /// of them goes in a process.
    pub span: u64,
    /// Its code: the stubs, and the linker's padding between them. The rest of `span` is zero.
    pub code: &'static [u8],
    /// Its exports, strictly ascending by name. An import names one by its place here.
    pub exports: &'static [CoreExport],
    /// Where each call's one `syscall` instruction is, from the image's base (L7).
    pub pins: Pins,
    /// The release fingerprint of the definition the stubs were generated from
    /// (`docs/spec/OIDL.md` §13).
    pub release: [u8; 32],
}

/// Why a description cannot be used. The build never writes one; [`check`] makes sure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// The span is zero, not whole pages, or over [`MAX_SPAN`]; or the code is empty or longer
    /// than the span.
    Span,
    /// An export is outside the code, or the exports are not strictly ascending by name.
    Export,
    /// A call's pin is not the place of a `syscall` instruction in the code, or two calls
    /// share one.
    Pin,
    /// The stubs were generated from another definition than this kernel's dispatcher.
    Release,
}

/// Whether `a` sorts strictly before `b`, byte by byte.
const fn before(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut i = 0;
    while i < a.len() && i < b.len() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
        i += 1;
    }
    a.len() < b.len()
}

/// Whether `image` keeps every rule the kernel relies on: a span of whole pages up to
/// [`MAX_SPAN`] that holds all of the code; exports strictly ascending by name, each inside the
/// code; every call's pin at a `syscall` instruction of the code, no two calls at one; and the
/// release fingerprint of this kernel's own dispatcher.
///
/// A `const fn`, so that the kernel's build runs it on the description it compiles in.
///
/// # Errors
///
/// The first rule broken, in the order above.
pub const fn check(image: &CoreImage) -> Result<(), CoreError> {
    let code = image.code;
    if image.span == 0
        || !image.span.is_multiple_of(PAGE)
        || image.span > MAX_SPAN
        || code.is_empty()
        || code.len() as u64 > image.span
    {
        return Err(CoreError::Span);
    }
    let mut i = 0;
    while i < image.exports.len() {
        let export = &image.exports[i];
        if export.offset >= code.len() as u64 {
            return Err(CoreError::Export);
        }
        if i > 0 && !before(image.exports[i - 1].name, export.name) {
            return Err(CoreError::Export);
        }
        i += 1;
    }
    let pins = image.pins.each();
    let mut i = 0;
    while i < pins.len() {
        let at = pins[i].1;
        // Both bytes inside the code. `at` is below its length here, so the sum cannot wrap.
        if at >= code.len() as u64 || at + 1 >= code.len() as u64 {
            return Err(CoreError::Pin);
        }
        let at = at as usize;
        if code[at] != SYSCALL[0] || code[at + 1] != SYSCALL[1] {
            return Err(CoreError::Pin);
        }
        let mut j = 0;
        while j < i {
            if pins[j].1 == pins[i].1 {
                return Err(CoreError::Pin);
            }
            j += 1;
        }
        i += 1;
    }
    let mut i = 0;
    while i < image.release.len() {
        if image.release[i] != RELEASE_FINGERPRINT[i] {
            return Err(CoreError::Release);
        }
        i += 1;
    }
    Ok(())
}

// The description `onbf-pack corelib` wrote for this build (`cargo xtask` names its directory in
// ORIGIN_CARRIED_PROGRAMS, with the carried programs' descriptions).
#[cfg(target_os = "none")]
include!(concat!(
    env!(
        "ORIGIN_CARRIED_PROGRAMS",
        "build the kernel with `cargo xtask`, which links origin.core and names its description's directory in ORIGIN_CARRIED_PROGRAMS"
    ),
    "/origin_core.rs"
));

// The kernel is compiled only with an `origin.core` that passes: a description from another
// definition's stubs, or one whose pins are not its code's `syscall` instructions, stops the
// build here.
#[cfg(target_os = "none")]
const _: () = assert!(
    check(&ORIGIN_CORE).is_ok(),
    "the description of origin.core this kernel carries does not pass core_image::check"
);

/// `origin.core`, as this kernel carries it.
#[cfg(target_os = "none")]
#[must_use]
pub(crate) fn image() -> &'static CoreImage {
    &ORIGIN_CORE
}

#[cfg(target_os = "none")]
pub(crate) use shared::{InstallError, install, installed};

/// The one set of frames, and the only code that touches them.
#[cfg(target_os = "none")]
mod shared {
    use alloc::vec::Vec;

    use origin_frame::once::SetOnce;
    use origin_frame::userspace::SharedFrame;

    use super::{ORIGIN_CORE, PAGE};

    /// `origin.core`'s frames, one a page, in order: made by [`install`], and never written
    /// after it returns.
    static FRAMES: SetOnce<Vec<SharedFrame>> = SetOnce::new();

    /// Why `origin.core` could not be put into its frames.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum InstallError {
        /// No frame, or no kernel memory for their record.
        NoMemory,
        /// It is already installed: the frames are written once.
        Twice,
    }

    /// Takes one frame for each page of `origin.core` and writes its code into them: once, at
    /// boot, before any process exists and so before any mapping of them does. Returns how
    /// many frames it took.
    ///
    /// # Errors
    ///
    /// [`InstallError::NoMemory`], with every frame it took given back; [`InstallError::Twice`]
    /// on a second call, which changes nothing.
    pub(crate) fn install() -> Result<usize, InstallError> {
        if FRAMES.get().is_some() {
            return Err(InstallError::Twice);
        }
        let pages = (ORIGIN_CORE.span / PAGE) as usize;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(pages)
            .map_err(|_| InstallError::NoMemory)?;
        // A fresh frame is zero, as the span past the code must be.
        for chunk in 0..pages {
            let frame = SharedFrame::new().ok_or(InstallError::NoMemory)?;
            let page = ORIGIN_CORE
                .code
                .chunks(PAGE as usize)
                .nth(chunk)
                .unwrap_or(&[]);
            for (word, bytes) in page.chunks(8).enumerate() {
                let mut value = [0u8; 8];
                if let Some(low) = value.get_mut(..bytes.len()) {
                    low.copy_from_slice(bytes);
                }
                if !frame.write_u64(word * 8, u64::from_le_bytes(value)) {
                    return Err(InstallError::NoMemory);
                }
            }
            frames.push(frame);
        }
        FRAMES.set(frames).map(|()| pages).map_err(|_| InstallError::Twice)
    }

    /// How many frames hold `origin.core`, once it is installed.
    #[must_use]
    pub(crate) fn installed() -> Option<usize> {
        FRAMES.get().map(Vec::len)
    }
}

#[cfg(test)]
mod tests;
