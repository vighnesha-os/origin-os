//! `cargo xtask check`'s gate on reproducible builds (CLAUDE.md §75, §60 and final rule 28).
//!
//! Every image in [`IMAGES`] is built twice from nothing, in two target directories whose
//! names and lengths differ, and every build in the second starts [`APART`] after the last
//! build in the first ended. The two files of each image must be the same bytes.
//!
//! That shows an image does not depend on when it was built, on what Cargo had built before
//! it, or on where its target directory is. It does not show that another machine gives the
//! same bytes: both builds have one toolchain, installed in one place, on one host
//! (`docs/research/2026-11-10-loader-reproducible-build.md` says what is known to differ).
//!
//! The loader was not reproducible until its link arguments made it so (`.cargo/config.toml`):
//! the linker wrote the time of the link into the PE header, and an identifier that changed
//! with every link beside it. A time stamp that differs is named in the report, because that
//! is what comes back if those arguments stop reaching the linker.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::{Loader, Result, build_kernel_in, build_loader_in, read_file, target_dir};

/// An image the gate builds twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Image {
    /// The loader with one of the boots' features, or with none: what a release builds.
    Loader(Option<Loader>),
    /// The Framekernel ELF, which `onbf-pack` turns into the KRN image.
    Kernel,
}

/// The loader each way it is built, and the kernel.
const IMAGES: &[Image] = &[
    Image::Loader(None),
    Image::Loader(Some(Loader::Verifying)),
    Image::Loader(Some(Loader::Developer)),
    Image::Kernel,
];

/// The two target directories, under `target/xtask/reproducible/`. The names differ in length
/// too: a path that reached an image would then move everything after it as well.
const DIRECTORIES: [&str; 2] = ["a", "another-target-directory"];

/// How long after the first directory's last build the second directory's first one starts.
/// A PE time stamp counts seconds, so two links within one second would agree by chance.
const APART: Duration = Duration::from_secs(2);

/// Differing bytes with fewer equal bytes than this between them are reported as one place.
const RUN_GAP: usize = 4;
/// How many places a report lists, and how many bytes of each.
const REPORTED_RUNS: usize = 8;
const REPORTED_BYTES: usize = 16;

impl Image {
    /// What the image is, for messages.
    fn name(self) -> String {
        match self {
            Self::Loader(None) => "the loader as a release builds it (no feature)".into(),
            Self::Loader(Some(loader)) => format!("the loader with `{}`", loader.feature()),
            Self::Kernel => "the kernel ELF".into(),
        }
    }

    /// A file name for the copies kept when the two builds differ.
    fn file_name(self) -> String {
        match self {
            Self::Loader(None) => "loader".into(),
            Self::Loader(Some(loader)) => format!("loader-{}", loader.feature()),
            Self::Kernel => "kernel".into(),
        }
    }

    /// Builds the image into `target` and returns the file's path.
    fn build(self, target: &Path) -> Result<PathBuf> {
        match self {
            Self::Loader(loader) => build_loader_in(loader.map(Loader::feature), Some(target)),
            Self::Kernel => build_kernel_in(Some(target)),
        }
    }
}

/// Builds every image twice and fails at the first whose two files differ.
pub(crate) fn gate() -> Result {
    let work = target_dir().join("xtask").join("reproducible");
    let [first_dir, second_dir] = DIRECTORIES.map(|name| work.join(name));
    // From nothing: in each directory Cargo compiles and links everything again.
    for dir in [&first_dir, &second_dir] {
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    // The loader's builds all write one path in a directory, so each image is read as soon as
    // it is built.
    let mut first = Vec::new();
    for &image in IMAGES {
        first.push(read_file(&image.build(&first_dir)?)?);
    }
    thread::sleep(APART);
    for (&image, first) in IMAGES.iter().zip(&first) {
        let second = read_file(&image.build(&second_dir)?)?;
        if let Some(report) = difference(first, &second) {
            // Both are kept for whoever investigates: the loader's next build in a directory
            // writes over this one.
            let kept = [(first, "first"), (&second, "second")].map(|(bytes, which)| {
                let path = work.join(format!("{}.{which}", image.file_name()));
                match fs::write(&path, bytes) {
                    Ok(()) => format!("  the {which} build's file is kept as {}", path.display()),
                    Err(e) => format!("  the {which} build's file could not be kept: {e}"),
                }
            });
            return Err(format!(
                "{} is not reproducible (CLAUDE.md §75): two builds of the same source give different files\n{report}\n{}",
                image.name(),
                kept.join("\n"),
            ));
        }
        println!(
            "xtask: reproducible-build gate: {}: {} bytes, the same from two builds (SHA-256 {})",
            image.name(),
            first.len(),
            hex(&origin_crypto::Sha256::digest(first), "")
        );
    }
    Ok(())
}

/// What differs between two builds of an image, as a report, or `None` if they are the same
/// bytes. An empty file is a difference: a gate that compared nothing has shown nothing.
fn difference(first: &[u8], second: &[u8]) -> Option<String> {
    if first.is_empty() || second.is_empty() {
        return Some(format!(
            "  the first is {} bytes and the second {}: an empty file is not an image",
            first.len(),
            second.len()
        ));
    }
    let differs = |at: usize| first.get(at) != second.get(at);
    let shared = first.len().min(second.len());
    let runs = differing_runs(first, second);
    if runs.is_empty() && first.len() == second.len() {
        return None;
    }
    let mut report = Vec::new();
    if first.len() != second.len() {
        report.push(format!(
            "  the first is {} bytes and the second {}",
            first.len(),
            second.len()
        ));
    }
    report.push(format!(
        "  {} of the first {shared} bytes differ, in {} place(s); for each, its offset, its length, and its bytes in the first build | in the second",
        (0..shared).filter(|&at| differs(at)).count(),
        runs.len()
    ));
    for run in runs.iter().take(REPORTED_RUNS) {
        let shown = run.start..run.end.min(run.start + REPORTED_BYTES);
        let more = if shown.end < run.end { " ..." } else { "" };
        report.push(format!(
            "  {:#08x}, {} byte(s): {}{more} | {}{more}",
            run.start,
            run.len(),
            hex(first.get(shown.clone()).unwrap_or_default(), " "),
            hex(second.get(shown).unwrap_or_default(), " "),
        ));
    }
    if runs.len() > REPORTED_RUNS {
        report.push(format!(
            "  and {} more place(s)",
            runs.len() - REPORTED_RUNS
        ));
    }
    if let Some(stamp) = pe_time_stamp(first)
        && stamp.clone().any(differs)
    {
        report.push(format!(
            "  bytes {:#x} to {:#x} are the PE header's time stamp, which `/TIMESTAMP:0` sets to zero (`.cargo/config.toml`). If it differs, the loader's link arguments did not reach the linker: RUSTFLAGS in the environment replaces the list they are in",
            stamp.start,
            stamp.end - 1
        ));
    }
    Some(report.join("\n"))
}

/// The places where the two differ, within the length they share: each a range that starts
/// and ends on a differing byte. Differences with fewer than [`RUN_GAP`] equal bytes between
/// them are one place.
fn differing_runs(first: &[u8], second: &[u8]) -> Vec<Range<usize>> {
    let mut runs: Vec<Range<usize>> = Vec::new();
    for (at, _) in first
        .iter()
        .zip(second)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
    {
        match runs.last_mut() {
            Some(run) if at - run.end < RUN_GAP => run.end = at + 1,
            _ => runs.push(at..at + 1),
        }
    }
    runs
}

/// Where a PE image's header keeps its time stamp: the four bytes that start eight after the
/// `PE\0\0` signature, whose offset is the word at 0x3C (Microsoft's PE format specification,
/// "MS-DOS Stub", "Signature" and "COFF File Header"). `None` for a file that is not a PE image.
fn pe_time_stamp(image: &[u8]) -> Option<Range<usize>> {
    if image.get(..2)? != b"MZ" {
        return None;
    }
    let word: [u8; 4] = image.get(0x3C..0x40)?.try_into().ok()?;
    let signature = usize::try_from(u32::from_le_bytes(word)).ok()?;
    if image.get(signature..signature.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let start = signature.checked_add(8)?;
    let end = start.checked_add(4)?;
    (end <= image.len()).then_some(start..end)
}

/// `bytes` in hexadecimal, two digits each, with `between` between them.
fn hex(bytes: &[u8], between: &str) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(between)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with the start of a PE image: the stub's pointer, the signature, and a COFF
    /// header whose time stamp is `stamp`, then `rest`.
    fn pe_image(stamp: u32, rest: &[u8]) -> Vec<u8> {
        let mut image = vec![0u8; 0x40];
        image[..2].copy_from_slice(b"MZ");
        image[0x3C..0x40].copy_from_slice(&0x78u32.to_le_bytes());
        image.resize(0x78, 0x90);
        image.extend_from_slice(b"PE\0\0");
        image.extend_from_slice(&[0x64, 0x86, 0x05, 0x00]);
        image.extend_from_slice(&stamp.to_le_bytes());
        image.extend_from_slice(rest);
        image
    }

    #[test]
    fn the_same_bytes_are_no_difference() {
        let image = pe_image(0, &[7; 300]);
        assert_eq!(difference(&image, &image.clone()), None);
        assert_eq!(difference(&[1], &[1]), None);
    }

    #[test]
    fn one_changed_byte_is_found_with_its_offset_and_both_values() {
        let first = vec![0xAAu8; 4096];
        for at in [0, 1, 2047, 4095] {
            let mut second = first.clone();
            second[at] = 0x55;
            let report = difference(&first, &second).expect("a planted difference");
            assert!(
                report.contains("1 of the first 4096 bytes differ, in 1 place(s)"),
                "{report}"
            );
            assert!(
                report.contains(&format!("{at:#08x}, 1 byte(s): aa")),
                "{report}"
            );
            assert!(report.contains("aa | 55"), "{report}");
            // Told the other way round, it is the same difference.
            assert!(difference(&second, &first).is_some());
        }
    }

    #[test]
    fn a_different_length_is_a_difference_even_if_every_shared_byte_agrees() {
        let first = vec![3u8; 100];
        let longer = vec![3u8; 101];
        for (a, b) in [(&first, &longer), (&longer, &first)] {
            let report = difference(a, b).expect("the lengths differ");
            assert!(
                report.contains("0 of the first 100 bytes differ"),
                "{report}"
            );
            assert!(
                report.contains(&format!(
                    "the first is {} bytes and the second {}",
                    a.len(),
                    b.len()
                )),
                "{report}"
            );
        }
    }

    #[test]
    fn an_empty_image_is_never_the_same_as_anything() {
        for (a, b) in [(&[][..], &[][..]), (&[][..], &[1][..]), (&[1][..], &[][..])] {
            let report = difference(a, b).expect("an empty image");
            assert!(report.contains("an empty file is not an image"), "{report}");
        }
    }

    #[test]
    fn differences_close_together_are_one_place_and_far_apart_are_two() {
        let first = vec![0u8; 64];
        let mut second = first.clone();
        // Two bytes of one field, and a third just inside the gap.
        second[10] = 1;
        second[11] = 1;
        second[11 + RUN_GAP] = 1;
        // One past the gap from there: a place of its own.
        second[11 + RUN_GAP + RUN_GAP + 1] = 1;
        assert_eq!(
            differing_runs(&first, &second),
            vec![10..11 + RUN_GAP + 1, 20..21]
        );
        let report = difference(&first, &second).expect("planted");
        assert!(
            report.contains("4 of the first 64 bytes differ, in 2 place(s)"),
            "{report}"
        );
    }

    #[test]
    fn a_long_report_is_cut_short_and_says_so() {
        let first = vec![0u8; 4096];
        let mut second = first.clone();
        // More places than a report lists, and one longer than it shows.
        for place in 0..REPORTED_RUNS + 3 {
            second[100 * place + 50] = 0xFF;
        }
        for byte in &mut second[2000..2000 + REPORTED_BYTES + 5] {
            *byte = 0xEE;
        }
        let report = difference(&first, &second).expect("planted");
        assert!(report.contains("and 4 more place(s)"), "{report}");
        assert_eq!(report.matches("byte(s): ").count(), REPORTED_RUNS);
        let mut second = first.clone();
        for byte in &mut second[2000..2000 + REPORTED_BYTES + 5] {
            *byte = 0xEE;
        }
        let report = difference(&first, &second).expect("planted");
        assert!(report.contains("21 byte(s): "), "{report}");
        assert_eq!(report.matches(" ...").count(), 2, "{report}");
        assert_eq!(report.matches("ee").count(), REPORTED_BYTES, "{report}");
    }

    #[test]
    fn a_pe_time_stamp_that_differs_is_named() {
        // What the loader's two builds looked like before its link arguments: a second apart.
        let first = pe_image(0x6AF3_1402, &[9; 64]);
        let second = pe_image(0x6AF3_1408, &[9; 64]);
        assert_eq!(pe_time_stamp(&first), Some(0x80..0x84));
        let report = difference(&first, &second).expect("the stamps differ");
        assert!(report.contains("0x000080, 1 byte(s): 02 | 08"), "{report}");
        assert!(
            report.contains("bytes 0x80 to 0x83 are the PE header's time stamp"),
            "{report}"
        );
        assert!(report.contains("RUSTFLAGS"), "{report}");
    }

    #[test]
    fn a_difference_elsewhere_in_a_pe_image_is_not_called_its_time_stamp() {
        let first = pe_image(0, &[9; 64]);
        let mut second = first.clone();
        // The byte before the stamp, the byte after it, and one far away.
        for at in [0x7F, 0x84, 0xA0] {
            second[at] ^= 0x01;
            let report = difference(&first, &second).expect("planted");
            assert!(!report.contains("time stamp"), "{at:#x}: {report}");
            second[at] ^= 0x01;
        }
    }

    #[test]
    fn only_a_pe_image_has_a_pe_time_stamp() {
        let image = pe_image(1, &[0; 16]);
        assert_eq!(pe_time_stamp(&image), Some(0x80..0x84));
        // An ELF file, an empty file, a stub with no signature where it points, a pointer
        // past the end, and a header cut off inside the stamp.
        assert_eq!(pe_time_stamp(b"\x7fELF\x02\x01\x01\x00"), None);
        assert_eq!(pe_time_stamp(&[]), None);
        let mut wrong = image.clone();
        wrong[0x78] = b'N';
        assert_eq!(pe_time_stamp(&wrong), None);
        let mut far = image.clone();
        far[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(pe_time_stamp(&far), None);
        assert_eq!(pe_time_stamp(&image[..0x82]), None);
        assert_eq!(pe_time_stamp(&image[..0x84]), Some(0x80..0x84));
    }

    #[test]
    fn the_gate_builds_every_loader_the_boots_use_and_the_kernel() {
        for image in [
            Image::Loader(None),
            Image::Loader(Some(Loader::Verifying)),
            Image::Loader(Some(Loader::Developer)),
            Image::Kernel,
        ] {
            assert!(IMAGES.contains(&image), "{image:?}");
        }
        // No two images share a name or a kept file.
        for (i, a) in IMAGES.iter().enumerate() {
            for b in &IMAGES[i + 1..] {
                assert_ne!(a.name(), b.name());
                assert_ne!(a.file_name(), b.file_name());
            }
        }
    }

    #[test]
    fn the_two_directories_differ_in_name_and_in_length() {
        assert_ne!(DIRECTORIES[0], DIRECTORIES[1]);
        assert_ne!(DIRECTORIES[0].len(), DIRECTORIES[1].len());
        // A PE time stamp counts whole seconds.
        assert!(APART >= Duration::from_secs(2));
    }
}
