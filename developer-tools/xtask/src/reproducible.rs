//! `cargo xtask check`'s gate on reproducible builds (CLAUDE.md §75, §60 and final rule 28).
//!
//! Every image in [`IMAGES`] is built twice from nothing, in two target directories whose
//! names and lengths differ, and every build in the second starts [`APART`] after the last
//! build in the first ended. The two files of each image must be the same bytes.
//!
//! That shows an image does not depend on when it was built, on what Cargo had built before
//! it, or on where its target directory is. Each file is then searched for the paths of the
//! machine that built it: the toolchain's directory, Cargo's home and the checkout
//! ([`toolchain::host_paths`]). None may be there.
//!
//! The programs the kernel carries are images of their own here. Each is built in both
//! directories, and its linker output and every file `onbf-pack carried` writes for it are
//! compared and searched like any other. Each directory's kernel carries that directory's
//! programs, and the gate fails unless the kernel's image holds the bytes of every segment it
//! read: what was checked is then what the kernel carries.
//!
//! A copy of every file the gate read, from each of the two builds, is left under
//! `target/xtask/reproducible/files/` ([`KEPT`]). CI's `check` job keeps that directory with
//! the run, so a file whose digest differs from another machine's can be compared with it
//! byte by byte.
//!
//! None of this shows that another machine gives the same bytes: both builds have one
//! toolchain on one host. `docs/research/2026-11-11-toolchain-path-in-images.md` says what
//! was tried with a second copy of the toolchain and what CI printed, and
//! `docs/research/2026-11-10-loader-reproducible-build.md` what made the loader differ.
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

use crate::ring3::{self, CarriedKind};
use crate::{Loader, Result, build_kernel_in, build_loader_in, read_file, target_dir, toolchain};

/// What the gate builds twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Image {
    /// The loader with one of the boots' features, or with none: what a release builds.
    Loader(Option<Loader>),
    /// Every program the kernel carries: each one's linker output, and each file
    /// `onbf-pack carried` writes for it.
    Programs,
    /// The Framekernel ELF, which `onbf-pack` turns into the KRN image.
    Kernel,
}

/// The loader each way it is built, the programs the kernel carries, and the kernel. The
/// programs come before the kernel: a fault in a program is then reported at the program.
const IMAGES: &[Image] = &[
    Image::Loader(None),
    Image::Loader(Some(Loader::Verifying)),
    Image::Loader(Some(Loader::Developer)),
    Image::Programs,
    Image::Kernel,
];

/// One file a build wrote, read as soon as it was built.
#[derive(Debug)]
struct Built {
    /// What the file is, for messages.
    name: String,
    /// A file name for the copies kept when the two builds differ.
    file_name: String,
    bytes: Vec<u8>,
    /// Whether the kernel's image must hold these bytes as they are: a carried program's
    /// segment.
    in_kernel: bool,
}

/// The two target directories, under `target/xtask/reproducible/`. The names differ in length
/// too: a path that reached an image would then move everything after it as well.
const DIRECTORIES: [&str; 2] = ["a", "another-target-directory"];

/// Where a copy of every file the gate read is left, under `target/xtask/reproducible/`: one
/// directory for each build, named in [`BUILDS`]. The loader's three builds write one path in
/// a target directory, so only a copy keeps all three. `.github/workflows/ci.yml` names this
/// directory.
const KEPT: &str = "files";
/// The two builds, in the order of [`DIRECTORIES`].
const BUILDS: [&str; 2] = ["first", "second"];

/// How long after the first directory's last build the second directory's first one starts.
/// A PE time stamp counts seconds, so two links within one second would agree by chance.
const APART: Duration = Duration::from_secs(2);

/// Differing bytes with fewer equal bytes than this between them are reported as one place.
const RUN_GAP: usize = 4;
/// How many places a report lists, and how many bytes of each.
const REPORTED_RUNS: usize = 8;
const REPORTED_BYTES: usize = 16;

impl Image {
    /// Builds the image into `target` and reads every file it is made of. The loader's builds
    /// all write one path in a directory, so nothing is left to be read later.
    fn build(self, target: &Path) -> Result<Vec<Built>> {
        let one = |name: String, file_name: String, path: PathBuf| -> Result<Vec<Built>> {
            Ok(vec![Built {
                name,
                file_name,
                bytes: read_file(&path)?,
                in_kernel: false,
            }])
        };
        match self {
            Self::Loader(loader) => {
                let path = build_loader_in(loader.map(Loader::feature), Some(target))?;
                match loader {
                    None => one(
                        "the loader as a release builds it (no feature)".into(),
                        "loader".into(),
                        path,
                    ),
                    Some(loader) => one(
                        format!("the loader with `{}`", loader.feature()),
                        format!("loader-{}", loader.feature()),
                        path,
                    ),
                }
            }
            Self::Programs => {
                ring3::programs(Some(target))?;
                let mut built = Vec::new();
                for file in ring3::carried_files(Some(target))? {
                    let what = match file.kind {
                        CarriedKind::Linked => "as the linker wrote it".to_string(),
                        CarriedKind::Segment | CarriedKind::Description => {
                            format!("in `{}`", file.file)
                        }
                    };
                    built.push(Built {
                        name: format!("the carried program `{}` {what}", file.program),
                        file_name: format!("carried-{}-{}", file.program, file.file),
                        bytes: read_file(&file.path)?,
                        in_kernel: file.kind == CarriedKind::Segment,
                    });
                }
                Ok(built)
            }
            Self::Kernel => one(
                "the kernel ELF".into(),
                "kernel".into(),
                build_kernel_in(Some(target))?,
            ),
        }
    }
}

/// How much of an image's text a report shows where a host path was found.
const REPORTED_TEXT: usize = 200;

/// Builds every image twice and fails at the first file whose two builds differ, or that
/// holds a path of this machine, or at a kernel that does not hold what the gate read of the
/// programs it carries.
pub(crate) fn gate() -> Result {
    let (host_paths, set_aside) = toolchain::host_paths()?;
    for path in &set_aside {
        println!(
            "xtask: reproducible-build gate: not looked for in the images: {}, `{}` (not UTF-8, or too short to tell from an image's own text)",
            path.what, path.path
        );
    }
    let work = target_dir().join("xtask").join("reproducible");
    let [first_dir, second_dir] = DIRECTORIES.map(|name| work.join(name));
    // From nothing: in each directory Cargo compiles and links everything again.
    // The kept copies go too: none may be left from an earlier tree.
    let kept = work.join(KEPT);
    for dir in [&first_dir, &second_dir, &kept] {
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    let mut first_builds = Vec::new();
    for &image in IMAGES {
        let built = image.build(&first_dir)?;
        keep(&kept, BUILDS[0], &built)?;
        first_builds.push(built);
    }
    thread::sleep(APART);
    for (&image, first) in IMAGES.iter().zip(&first_builds) {
        let second = image.build(&second_dir)?;
        // Before the comparison: a file that differs is then there from both builds.
        keep(&kept, BUILDS[1], &second)?;
        let names = |built: &[Built]| -> Vec<String> {
            built.iter().map(|file| file.file_name.clone()).collect()
        };
        if names(first) != names(&second) {
            return Err(format!(
                "the two builds did not write the same files (CLAUDE.md §75): the first wrote {:?} and the second {:?}",
                names(first),
                names(&second)
            ));
        }
        for (first, second) in first.iter().zip(&second) {
            check_pair(first, second, &kept, &host_paths)?;
        }
        if image == Image::Kernel {
            let segments: Vec<&Built> = first_builds
                .iter()
                .flatten()
                .filter(|file| file.in_kernel)
                .collect();
            for kernel in first {
                if let Some(missing) = segment_not_in(&kernel.bytes, &segments) {
                    return Err(format!(
                        "{} does not hold the bytes of {} ({} bytes) (CLAUDE.md §75): the gate read a file of a carried program that is not what the kernel carries. If the kernel now holds its programs another way, `ring3::carried_files` and this check must follow it",
                        kernel.name,
                        missing.name,
                        missing.bytes.len()
                    ));
                }
                println!(
                    "xtask: reproducible-build gate: {} holds the bytes of each of the {} carried segments the gate read",
                    kernel.name,
                    segments.len()
                );
            }
        }
    }
    println!(
        "xtask: reproducible-build gate: a copy of each of the {} files it read, from each build, is in {}",
        first_builds.iter().map(Vec::len).sum::<usize>(),
        kept.display()
    );
    Ok(())
}

/// Leaves a copy of each of `built`, the files of one image from the build named `build`,
/// in `kept`.
fn keep(kept: &Path, build: &str, built: &[Built]) -> Result {
    let dir = kept.join(build);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for file in built {
        let path = dir.join(&file.file_name);
        fs::write(&path, &file.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Fails if the two builds of one file differ, or if it holds a path of this machine; else
/// prints its size and SHA-256.
fn check_pair(
    first: &Built,
    second: &Built,
    kept: &Path,
    host_paths: &[toolchain::HostPath],
) -> Result {
    if let Some(report) = difference(&first.bytes, &second.bytes) {
        // Where [`keep`] left both, for whoever investigates: the next build in a target
        // directory may write over its file.
        let copies = [(first, BUILDS[0]), (second, BUILDS[1])].map(|(built, build)| {
            format!(
                "  the {build} build's file is kept as {}",
                kept.join(build).join(&built.file_name).display()
            )
        });
        return Err(format!(
            "{} is not reproducible (CLAUDE.md §75): two builds of the same source give different files\n{report}\n{}",
            first.name,
            copies.join("\n"),
        ));
    }
    if let Some((path, at)) = toolchain::host_path_in(&first.bytes, host_paths) {
        return Err(format!(
            "{} holds a path of the machine that built it (CLAUDE.md §75, §60): {}, `{}`, at offset {at:#x}\n  the text there: `{}`\n  `xtask` remaps the toolchain's source directory in every image build (`toolchain::image_config`, `toolchain::userspace_config`). A path that still reaches an image needs a remap of its own, or must not be compiled in",
            first.name,
            path.what,
            path.path,
            text_at(&first.bytes, at),
        ));
    }
    println!(
        "xtask: reproducible-build gate: {}: {} bytes, the same from two builds and free of this machine's paths (SHA-256 {})",
        first.name,
        first.bytes.len(),
        hex(&origin_crypto::Sha256::digest(&first.bytes), "")
    );
    Ok(())
}

/// The first of `segments` whose bytes `kernel` does not hold. An empty segment is held by
/// any image.
fn segment_not_in<'a>(kernel: &[u8], segments: &[&'a Built]) -> Option<&'a Built> {
    segments.iter().copied().find(|segment| {
        let bytes = segment.bytes.as_slice();
        !bytes.is_empty() && !kernel.windows(bytes.len()).any(|window| window == bytes)
    })
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
            "  bytes {:#x} to {:#x} are the PE header's time stamp, which `/TIMESTAMP:0` sets to zero (`.cargo/config.toml`). If it differs, the loader's link arguments did not reach the linker",
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

/// The printable text of `image` from `at`, up to [`REPORTED_TEXT`] bytes: the whole of a
/// path that starts there.
fn text_at(image: &[u8], at: usize) -> String {
    image
        .get(at..)
        .unwrap_or_default()
        .iter()
        .take(REPORTED_TEXT)
        .take_while(|byte| byte.is_ascii_graphic() || **byte == b' ')
        .map(|&byte| char::from(byte))
        .collect()
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
        assert!(report.contains("did not reach the linker"), "{report}");
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
    fn the_text_at_an_offset_stops_at_the_first_byte_that_is_not_text() {
        let mut image = vec![0u8; 64];
        image[10..30].copy_from_slice(b"/Users/x/a file.rs\0Z");
        assert_eq!(text_at(&image, 10), "/Users/x/a file.rs");
        assert_eq!(text_at(&image, 0), "");
        assert_eq!(text_at(&image, 64), "");
        assert_eq!(text_at(&image, 1000), "");
        let long = vec![b'a'; 3 * REPORTED_TEXT];
        assert_eq!(text_at(&long, 5).len(), REPORTED_TEXT);
    }

    #[test]
    fn the_gate_builds_every_loader_the_boots_use_the_programs_and_the_kernel() {
        for image in [
            Image::Loader(None),
            Image::Loader(Some(Loader::Verifying)),
            Image::Loader(Some(Loader::Developer)),
            Image::Programs,
            Image::Kernel,
        ] {
            assert!(IMAGES.contains(&image), "{image:?}");
        }
        // The programs are built and read before the kernel that carries them, and no image
        // is listed twice.
        let at = |image: Image| IMAGES.iter().position(|listed| *listed == image);
        assert!(at(Image::Programs) < at(Image::Kernel));
        for (i, image) in IMAGES.iter().enumerate() {
            assert!(!IMAGES[i + 1..].contains(image), "{image:?}");
        }
    }

    /// A file a build wrote, for the checks that take one.
    fn built(name: &str, bytes: &[u8], in_kernel: bool) -> Built {
        Built {
            name: name.to_string(),
            file_name: name.replace(' ', "-"),
            bytes: bytes.to_vec(),
            in_kernel,
        }
    }

    #[test]
    fn a_segment_the_kernel_does_not_hold_is_found() {
        let mut kernel = vec![0x90u8; 4096];
        kernel[100..108].copy_from_slice(b"SEGMENT0");
        kernel[4088..4096].copy_from_slice(b"SEGMENT1");
        let held = [
            built("segment 0", b"SEGMENT0", true),
            built("segment 1, at the image's end", b"SEGMENT1", true),
            built("an empty segment", b"", true),
        ];
        let all: Vec<&Built> = held.iter().collect();
        assert!(segment_not_in(&kernel, &all).is_none());
        // One byte of a segment changed in the kernel: it no longer holds that segment.
        kernel[104] ^= 0x01;
        let missing = segment_not_in(&kernel, &all).expect("a planted difference");
        assert_eq!(missing.name, "segment 0");
        kernel[104] ^= 0x01;
        // A segment longer than the kernel, and one the kernel holds only the start of.
        let longer = built("longer than the image", &vec![0x90u8; 5000], true);
        assert_eq!(
            segment_not_in(&kernel, &[&longer]).map(|s| s.name.as_str()),
            Some("longer than the image")
        );
        let cut = built("cut short", b"SEGMENT1 and more", true);
        assert_eq!(
            segment_not_in(&kernel, &[&held[1], &cut]).map(|s| s.name.as_str()),
            Some("cut short")
        );
        assert!(segment_not_in(&[], &[&held[2]]).is_none());
        assert!(segment_not_in(&[], &[&held[0]]).is_some());
    }

    #[test]
    fn a_file_that_differs_or_holds_a_host_path_fails_and_says_which() {
        let work = target_dir().join("xtask").join("reproducible-tests");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).expect("a scratch directory");
        let paths = vec![toolchain::HostPath {
            what: "the toolchain's directory",
            path: "/Users/someone/.rustup/toolchains/pinned".into(),
        }];
        let clean = built("the carried program `p` in `p.0.bin`", &[7u8; 64], true);
        assert_eq!(check_pair(&clean, &clean, &work, &paths), Ok(()));
        // The second build differs in one byte. As in the gate, each build's file is kept
        // before the two are compared.
        let mut other = built("the carried program `p` in `p.0.bin`", &[7u8; 64], true);
        other.bytes[9] = 8;
        keep(&work, BUILDS[0], std::slice::from_ref(&clean)).expect("the first is kept");
        keep(&work, BUILDS[1], std::slice::from_ref(&other)).expect("the second is kept");
        let error = check_pair(&clean, &other, &work, &paths).expect_err("a planted difference");
        assert!(
            error.starts_with("the carried program `p` in `p.0.bin` is not reproducible"),
            "{error}"
        );
        assert!(error.contains("0x000009, 1 byte(s): 07 | 08"), "{error}");
        // The report names the two copies, and each holds its own build's bytes.
        for (build, bytes) in [(BUILDS[0], &clean.bytes), (BUILDS[1], &other.bytes)] {
            let copy = work.join(build).join(&clean.file_name);
            assert!(error.contains(&copy.display().to_string()), "{error}");
            assert_eq!(&fs::read(&copy).expect("the copy is there"), bytes);
        }
        // Both builds agree, and hold the toolchain's path.
        let mut bytes = vec![0u8; 256];
        let location = b"/Users/someone/.rustup/toolchains/pinned/lib/rustlib/src/rust/library/core/src/slice/index.rs";
        bytes[40..40 + location.len()].copy_from_slice(location);
        let leaking = built("the carried program `p` in `p.1.bin`", &bytes, true);
        let error = check_pair(&leaking, &leaking, &work, &paths).expect_err("a planted path");
        assert!(
            error.starts_with(
                "the carried program `p` in `p.1.bin` holds a path of the machine that built it"
            ),
            "{error}"
        );
        assert!(error.contains("at offset 0x28"), "{error}");
        assert!(error.contains("core/src/slice/index.rs`"), "{error}");
        fs::remove_dir_all(&work).expect("the scratch directory goes");
    }

    #[test]
    fn every_file_of_a_build_is_kept_under_its_own_name() {
        let kept = target_dir().join("xtask").join("reproducible-tests-kept");
        let _ = fs::remove_dir_all(&kept);
        // The loader's three builds write one path; their copies are three files.
        let files = [
            Built {
                file_name: "loader".into(),
                ..built("the loader", b"one", false)
            },
            Built {
                file_name: "loader-test-trust-store".into(),
                ..built("the loader with a feature", b"two", false)
            },
        ];
        keep(&kept, BUILDS[0], &files).expect("kept");
        keep(&kept, BUILDS[1], &files[..1]).expect("kept");
        assert_eq!(fs::read(kept.join("first/loader")).expect("there"), b"one");
        assert_eq!(
            fs::read(kept.join("first/loader-test-trust-store")).expect("there"),
            b"two"
        );
        assert_eq!(fs::read(kept.join("second/loader")).expect("there"), b"one");
        assert!(!kept.join("second/loader-test-trust-store").exists());
        // The workflow names the directory by this path.
        assert_eq!(KEPT, "files");
        assert_ne!(BUILDS[0], BUILDS[1]);
        fs::remove_dir_all(&kept).expect("the scratch directory goes");
    }

    #[test]
    fn the_two_directories_differ_in_name_and_in_length() {
        assert_ne!(DIRECTORIES[0], DIRECTORIES[1]);
        assert_ne!(DIRECTORIES[0].len(), DIRECTORIES[1].len());
        // A PE time stamp counts whole seconds.
        assert!(APART >= Duration::from_secs(2));
    }
}
