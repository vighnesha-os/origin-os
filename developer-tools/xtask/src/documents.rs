//! `cargo xtask check`'s gate on documents in the repository (CLAUDE.md §17, decision item 107).
//!
//! The repository is public. A specification whose terms forbid redistribution must not be in
//! it, and a commit that held one stays in the branch's history after the file is deleted. On
//! 2026-11-10 a vendor's specification was one `git push` from being published that way.
//!
//! The rule: no tracked file is a document or an archive, unless it is on [`ALLOWED`].
//!
//! - A **document** is a file made for reading, not for building: PDF, PostScript, DjVu, an
//!   e-book, an office file, rich text, compiled help, a saved web page. The repository's own
//!   writing is Markdown and plain text, which are neither.
//! - An **archive** is refused with them because the gate cannot see what is inside one.
//! - A file is judged by its name and by its first bytes, so a PDF renamed `notes.txt` is still
//!   a PDF.
//! - The branch's **history** is judged by name: a document added by one commit and deleted by a
//!   later one is still published when the branch is pushed. The remedy there is to drop the
//!   commit, not to delete the file again.
//!
//! What it does not see: a scanned page saved as an image, text copied out of a document, and
//! anything done to avoid it on purpose. Local copies of specifications belong in
//! `specifications/`, which `.gitignore` keeps out.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use crate::{Result, collect_files, repo_root};

/// The tracked files the rule would refuse, by exact path, and why each is kept. An entry that
/// no longer names a refused file fails the gate, so the list cannot go stale. There is no way to
/// allow a directory: a PDF put beside the test vectors is refused like any other.
const ALLOWED: &[(&str, &str)] = &[(
    "framekernel/assets/initramfs.tar",
    "the initial RAM file system the kernel embeds and its boot proof unpacks: three text files written for it",
)];

/// How much of a file is read: past the place a tar archive keeps its signature.
const HEAD: u64 = 512;

/// What kind of refused file `path` is by its name, if it is one.
fn named(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (_, extension) = name.rsplit_once('.')?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "pdf" => "a PDF",
        "ps" | "eps" | "dvi" => "a PostScript or DVI document",
        "djvu" | "djv" => "a DjVu document",
        "epub" | "mobi" | "azw" | "azw3" => "an e-book",
        // Not `dot`: that is also a Graphviz drawing, which is source. An old Word template of
        // that name is still caught by its first bytes.
        "doc" | "docx" | "docm" | "dotx" | "xls" | "xlsx" | "xlsm" | "ppt" | "pptx" | "pps"
        | "ppsx" | "odt" | "ods" | "odp" | "odg" | "pages" | "numbers" | "xps" | "oxps" => {
            "an office document"
        }
        "rtf" => "a rich-text document",
        "chm" => "a compiled help file",
        "htm" | "html" | "xhtml" | "mht" | "mhtml" => "a saved web page",
        "zip" | "7z" | "rar" | "tar" | "tgz" | "gz" | "bz2" | "tbz" | "tbz2" | "xz" | "txz"
        | "zst" | "tzst" => "an archive",
        _ => return None,
    })
}

/// What kind of refused file starts with the bytes `head`, if it is one.
fn sniffed(head: &[u8]) -> Option<&'static str> {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"%PDF-", "a PDF"),
        (b"%!PS", "a PostScript document"),
        (b"AT&TFORM", "a DjVu document"),
        // The container of the older office formats.
        (
            &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
            "an office document",
        ),
        (b"{\\rtf", "a rich-text document"),
        (b"ITSF", "a compiled help file"),
        // ZIP: also what the newer office formats and EPUB are inside.
        (b"PK\x03\x04", "a ZIP archive or an office document"),
        (b"PK\x05\x06", "a ZIP archive"),
        (b"PK\x07\x08", "a ZIP archive"),
        (&[0x1F, 0x8B], "a gzip archive"),
        (b"BZh", "a bzip2 archive"),
        (&[0xFD, b'7', b'z', b'X', b'Z', 0x00], "an xz archive"),
        (&[0x28, 0xB5, 0x2F, 0xFD], "a Zstandard archive"),
        (&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C], "a 7z archive"),
        (b"Rar!\x1A\x07", "a RAR archive"),
    ];
    if let Some(&(_, what)) = SIGNATURES.iter().find(|(magic, _)| head.starts_with(magic)) {
        return Some(what);
    }
    if head.get(257..262) == Some(b"ustar") {
        return Some("a tar archive");
    }
    // A web page: after a byte-order mark and white space, `<!DOCTYPE html` or `<html`.
    let text = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    let text = text.trim_ascii_start();
    let opens = |tag: &[u8]| {
        text.get(..tag.len())
            .is_some_and(|t| t.eq_ignore_ascii_case(tag))
    };
    (opens(b"<!doctype html") || opens(b"<html")).then_some("a saved web page")
}

/// One tracked file as the gate sees it.
#[derive(Debug)]
struct Tracked {
    path: String,
    /// Its first bytes, or `None` if the working tree has no such file (deleted and not yet
    /// staged) or it is a symbolic link: it is then judged by its name alone.
    head: Option<Vec<u8>>,
}

/// A path some commit of the branch added.
#[derive(Debug)]
struct Added {
    path: String,
    commit: String,
}

/// What a run looked at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Seen {
    files: usize,
    /// Tracked files judged by name alone.
    by_name: usize,
    /// Paths in the branch's history that are no longer tracked.
    gone: usize,
}

/// Everything wrong: a refused file that is tracked, one that is only in history, and an
/// allowlist entry nothing needs.
fn problems(
    tracked: &[Tracked],
    history: &[Added],
    allowed: &[(&str, &str)],
) -> (Vec<String>, Seen) {
    let mut problems = Vec::new();
    let mut seen = Seen::default();
    let mut needed = BTreeSet::new();
    let is_allowed = |path: &str| allowed.iter().any(|&(a, _)| a == path);
    let present: BTreeSet<&str> = tracked.iter().map(|t| t.path.as_str()).collect();
    for file in tracked {
        seen.files += 1;
        seen.by_name += usize::from(file.head.is_none());
        let by_name = named(&file.path);
        let by_content = file.head.as_deref().and_then(sniffed);
        let what = match (by_name, by_content) {
            (None, None) => continue,
            (Some(what), _) => format!("{what} by its name"),
            (None, Some(what)) => format!("{what} by its first bytes"),
        };
        if is_allowed(&file.path) {
            needed.insert(file.path.as_str());
        } else {
            problems.push(format!("{}: {what}, and tracked", file.path));
        }
    }
    let mut reported = BTreeSet::new();
    for added in history {
        if present.contains(added.path.as_str()) {
            continue;
        }
        if reported.insert(added.path.as_str()) {
            seen.gone += 1;
        } else {
            continue;
        }
        let Some(what) = named(&added.path) else {
            continue;
        };
        if is_allowed(&added.path) {
            needed.insert(added.path.as_str());
        } else {
            let commit = added.commit.get(..12).unwrap_or(&added.commit);
            problems.push(format!(
                "{}: {what} by its name, no longer tracked but added by commit {commit}, which this branch still holds: deleting the file again does not take it back; drop that commit",
                added.path
            ));
        }
    }
    for &(path, _) in allowed {
        if !needed.contains(path) {
            problems.push(format!(
                "{path} is on the allowlist and is not a tracked document or archive: take it off"
            ));
        }
    }
    (problems, seen)
}

/// The gate: see the module documentation.
pub(crate) fn gate() -> Result {
    let root = repo_root();
    let (found, seen, listing) = scan(&root, ALLOWED)?;
    if seen.files == 0 {
        return Err("document gate: no file was read: nothing was checked".into());
    }
    if !found.is_empty() {
        return Err(format!(
            "documents and archives stay out of the repository (CLAUDE.md §17, decision item 107). Keep a specification in specifications/, which is ignored; a file this tree really needs goes on the allowlist in developer-tools/xtask/src/documents.rs with its reason:\n  {}",
            found.join("\n  ")
        ));
    }
    let by_name = if seen.by_name == 0 {
        String::new()
    } else {
        format!(
            ", {} of them by name alone (missing from the working tree, or a link)",
            seen.by_name
        )
    };
    match listing {
        Listing::Git => println!(
            "xtask: document gate: none of the {} tracked files is a document or an archive outside the allowlist of {}{by_name}; nor is any of the {} paths this branch's history added and no longer has",
            seen.files,
            ALLOWED.len(),
            seen.gone
        ),
        Listing::Tree => println!(
            "xtask: document gate: not a git checkout, so every file in the tree was read instead of the tracked ones: none of the {} is a document or an archive outside the allowlist of {}",
            seen.files,
            ALLOWED.len()
        ),
    }
    Ok(())
}

/// Where the list of files came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    /// git's index, and the history of `HEAD`.
    Git,
    /// A walk of the directory: `root` is not the top of a git checkout.
    Tree,
}

/// Reads the files under `root` and judges them.
fn scan(root: &Path, allowed: &[(&str, &str)]) -> Result<(Vec<String>, Seen, Listing)> {
    let (paths, history, from) = match tracked_paths(root)? {
        Some(paths) => (paths, added_in_history(root)?, Listing::Git),
        None => (tree_paths(root)?, Vec::new(), Listing::Tree),
    };
    let mut tracked = Vec::with_capacity(paths.len());
    for path in paths {
        let head = head_of(&root.join(&path))?;
        tracked.push(Tracked { path, head });
    }
    let (found, seen) = problems(&tracked, &history, allowed);
    Ok((found, seen, from))
}

/// The first bytes of the file at `path`; `None` if there is no such file or it is a link.
fn head_of(path: &Path) -> Result<Option<Vec<u8>>> {
    let unreadable = |e: std::io::Error| format!("document gate: {}: {e}", path.display());
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e)),
    };
    if meta.file_type().is_symlink() {
        // git tracks where a link points, not what is there.
        return Ok(None);
    }
    let mut head = Vec::new();
    fs::File::open(path)
        .map_err(unreadable)?
        .take(HEAD)
        .read_to_end(&mut head)
        .map_err(unreadable)?;
    Ok(Some(head))
}

/// `git` in `root`, with `args`: its output if it ran and succeeded.
fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| out.stdout)
}

/// The paths git tracks under `root`, or `None` if `root` is not the top of a git checkout (a
/// plain copy of the tree, say, or a directory inside some other repository).
fn tracked_paths(root: &Path) -> Result<Option<Vec<String>>> {
    let top = git(root, &["rev-parse", "--show-toplevel"])
        .map(|out| String::from_utf8_lossy(&out).trim().to_string());
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !top.is_some_and(|top| same(Path::new(&top), root)) {
        return Ok(None);
    }
    let listed = git(root, &["ls-files", "-z"])
        .ok_or("document gate: `git ls-files` failed in a git checkout")?;
    Ok(Some(nul_separated(&listed).collect()))
}

/// Every path a commit reachable from `HEAD` added, with that commit. Merges are compared with
/// each parent, so a file that only a merge brought in is found too.
fn added_in_history(root: &Path) -> Result<Vec<Added>> {
    if git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_none() {
        // A repository with no commit yet has no history to read.
        return Ok(Vec::new());
    }
    let log = git(
        root,
        &[
            "log",
            "-m",
            "--no-renames",
            "--diff-filter=A",
            "--name-only",
            "--format=%x01%H",
            "-z",
            "HEAD",
        ],
    )
    .ok_or("document gate: `git log` failed in a git checkout")?;
    let mut added = Vec::new();
    let mut commit = String::new();
    for piece in nul_separated(&log) {
        let piece = piece.trim_start_matches('\n');
        if let Some(id) = piece.strip_prefix('\u{1}') {
            commit = id.to_string();
        } else if !piece.is_empty() {
            added.push(Added {
                path: piece.to_string(),
                commit: commit.clone(),
            });
        }
    }
    Ok(added)
}

/// The pieces of git's NUL-separated output that are not empty.
fn nul_separated(output: &[u8]) -> impl Iterator<Item = String> + '_ {
    output
        .split(|&b| b == 0)
        .filter(|piece| !piece.is_empty())
        .map(|piece| String::from_utf8_lossy(piece).into_owned())
}

/// Every file under `root`, for a tree that is not a git checkout: all but hidden directories,
/// `target/` and the ignored `specifications/`.
fn tree_paths(root: &Path) -> Result<Vec<String>> {
    let local = root.join("specifications");
    let mut files = Vec::new();
    collect_files(root, &|dir| dir == local, &|_| true, &mut files)
        .map_err(|e| format!("document gate: {e}"))?;
    Ok(files
        .iter()
        .filter_map(|file| file.strip_prefix(root).ok())
        .map(|file| file.to_string_lossy().into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target_dir;
    use std::path::PathBuf;

    fn file(path: &str, head: &[u8]) -> Tracked {
        Tracked {
            path: path.to_string(),
            head: Some(head.to_vec()),
        }
    }

    fn found(tracked: &[Tracked], history: &[Added], allowed: &[(&str, &str)]) -> Vec<String> {
        problems(tracked, history, allowed).0
    }

    /// The first bytes of a tar archive: a name, then `ustar` at byte 257.
    fn tar_head() -> Vec<u8> {
        let mut head = vec![0; 512];
        head[..9].copy_from_slice(b"etc/motd\0");
        head[257..262].copy_from_slice(b"ustar");
        head
    }

    #[test]
    fn a_document_is_caught_by_its_name() {
        for (path, what) in [
            ("specifications/D51397-019-vt-directed-io-spec.pdf", "a PDF"),
            ("docs/Spec.PDF", "a PDF"),
            ("a/b/figure.eps", "a PostScript or DVI document"),
            ("scan.djvu", "a DjVu document"),
            ("book.epub", "an e-book"),
            ("notes.docx", "an office document"),
            ("sheet.xls", "an office document"),
            ("deck.pptx", "an office document"),
            ("letter.odt", "an office document"),
            ("memo.rtf", "a rich-text document"),
            ("api.chm", "a compiled help file"),
            ("docs/uefi-2.11.html", "a saved web page"),
            ("specs.zip", "an archive"),
            ("vectors.tar.gz", "an archive"),
            ("usb4.7z", "an archive"),
        ] {
            let found = found(&[file(path, b"plain text")], &[], &[]);
            assert_eq!(found.len(), 1, "{path}: {found:?}");
            assert!(
                found[0].contains(&format!("{path}: {what} by its name, and tracked")),
                "{found:?}"
            );
        }
    }

    #[test]
    fn a_document_under_another_name_is_caught_by_its_first_bytes() {
        let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        ole.extend_from_slice(&[0; 32]);
        for (head, what) in [
            (b"%PDF-1.6\n%\xE2\xE3\xCF\xD3".to_vec(), "a PDF"),
            (b"%!PS-Adobe-3.0".to_vec(), "a PostScript document"),
            (b"AT&TFORM\0\0\0\0DJVM".to_vec(), "a DjVu document"),
            (ole, "an office document"),
            (b"{\\rtf1\\ansi".to_vec(), "a rich-text document"),
            (b"ITSF\x03\0\0\0".to_vec(), "a compiled help file"),
            (
                b"PK\x03\x04\x14\0".to_vec(),
                "a ZIP archive or an office document",
            ),
            (vec![0x1F, 0x8B, 8, 0], "a gzip archive"),
            (b"BZh91AY&SY".to_vec(), "a bzip2 archive"),
            (vec![0xFD, b'7', b'z', b'X', b'Z', 0, 0], "an xz archive"),
            (vec![0x28, 0xB5, 0x2F, 0xFD, 0], "a Zstandard archive"),
            (vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C], "a 7z archive"),
            (b"Rar!\x1A\x07\0".to_vec(), "a RAR archive"),
            (tar_head(), "a tar archive"),
            (
                b"\xEF\xBB\xBF\n <!DocType HTML>".to_vec(),
                "a saved web page",
            ),
            (b"<html lang=\"en\">".to_vec(), "a saved web page"),
        ] {
            // The names of files this repository really tracks.
            for path in [
                "docs/research/notes.md",
                "security/crypto/tests/vectors/acvp/sha2-256.txt",
                "LICENSE-MIT",
                "data.bin",
            ] {
                let found = found(&[file(path, &head)], &[], &[]);
                assert_eq!(found.len(), 1, "{path} as {what}: {found:?}");
                assert!(
                    found[0].contains(&format!("{path}: {what} by its first bytes, and tracked")),
                    "{found:?}"
                );
            }
        }
    }

    #[test]
    fn source_text_and_keys_are_none_of_these() {
        let tracked = [
            file("framekernel/src/kernel.rs", b"//! The boot flow.\n"),
            file("docs/STATUS.md", b"# Status\n"),
            file("Cargo.toml", b"[workspace]\n"),
            file(".gitignore", b"target\n"),
            file("LICENSE-MIT", b"Permission is hereby granted"),
            file(
                "security/crypto/tests/vectors/acvp/sha2-256.txt",
                b"# NIST ACVP\n",
            ),
            file(
                "boot/uefi/trust/TEST-ONLY-origin-kernel.mldsa87",
                &[0x30, 0x82, 0x0A],
            ),
            file(
                "sdk/interfaces/origin.kernel.oidl",
                b"interface origin.kernel",
            ),
            file("developer-tools/toolchain/verify.sh", b"#!/bin/sh\n"),
            // A Graphviz drawing is source, whatever Word once called its templates.
            file("docs/layers.dot", b"digraph layers {\n"),
            // Text that mentions a signature without starting with it.
            file(
                "docs/a.md",
                b"A PDF starts with %PDF- and a page with <html>.\n",
            ),
            file("empty", b""),
        ];
        let (found, seen) = problems(&tracked, &[], &[]);
        assert_eq!(found, [""; 0]);
        assert_eq!(seen.files, tracked.len());
    }

    #[test]
    fn the_allowlist_is_exact_and_cannot_go_stale() {
        let tar = file("framekernel/assets/initramfs.tar", &tar_head());
        let allowed = [(
            "framekernel/assets/initramfs.tar",
            "the kernel's embedded file system",
        )];
        assert_eq!(found(std::slice::from_ref(&tar), &[], &allowed), [""; 0]);
        // Off the list, the same file is refused.
        let refused = found(std::slice::from_ref(&tar), &[], &[]);
        assert_eq!(refused.len(), 1, "{refused:?}");
        // A directory cannot be allowed: a PDF beside an allowed file is refused.
        let beside = file("framekernel/assets/manual.pdf", b"%PDF-1.7");
        let both = found(&[tar, beside], &[], &allowed);
        assert_eq!(both.len(), 1, "{both:?}");
        assert!(
            both[0].starts_with("framekernel/assets/manual.pdf: a PDF"),
            "{both:?}"
        );
        // An entry that names nothing refused, or an ordinary file, fails.
        for tracked in [
            vec![],
            vec![file("framekernel/assets/initramfs.tar", b"not an archive")],
        ] {
            let allowed = [("framekernel/assets/initramfs.txt", "left behind")];
            let stale = found(&tracked, &[], &allowed);
            assert!(
                stale.iter().any(|p| p.contains(
                    "initramfs.txt is on the allowlist and is not a tracked document or archive"
                )),
                "{stale:?}"
            );
        }
    }

    #[test]
    fn a_document_deleted_by_a_later_commit_is_still_caught() {
        let history = [
            Added {
                path: "specifications/D51397-019-vt-directed-io-spec.pdf".into(),
                commit: "5b6ab78aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            },
            // The same path through a merge, compared with each parent.
            Added {
                path: "specifications/D51397-019-vt-directed-io-spec.pdf".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            Added {
                path: "docs/old-notes.md".into(),
                commit: "1111111111111111111111111111111111111111".into(),
            },
            Added {
                path: "README.md".into(),
                commit: "2222222222222222222222222222222222222222".into(),
            },
        ];
        let tracked = [file("README.md", b"# Origin OS\n")];
        let (found, seen) = problems(&tracked, &history, &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("vt-directed-io-spec.pdf: a PDF by its name, no longer tracked but added by commit 5b6ab78aaaaa"), "{found:?}");
        assert!(found[0].contains("drop that commit"), "{found:?}");
        // Two paths are gone from the tree; the one still tracked is judged as a file.
        assert_eq!(seen.gone, 2);
        // An allowed path in history is allowed, and keeps its entry needed.
        let allowed = [(
            "specifications/D51397-019-vt-directed-io-spec.pdf",
            "a control",
        )];
        assert_eq!(problems(&tracked, &history, &allowed).0, [""; 0]);
    }

    #[test]
    fn a_tracked_file_missing_from_the_tree_is_judged_by_its_name() {
        let missing = |path: &str| Tracked {
            path: path.to_string(),
            head: None,
        };
        let (found, seen) = problems(&[missing("gone.pdf"), missing("gone.rs")], &[], &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("gone.pdf: a PDF by its name"),
            "{found:?}"
        );
        assert_eq!(seen.by_name, 2);
    }

    /// The real tree passes, and the gate reads what git tracks.
    #[test]
    fn the_real_tree_passes() {
        let (found, seen, _) = scan(&repo_root(), ALLOWED).expect("the scan");
        assert_eq!(found, [""; 0]);
        assert!(seen.files > 300, "only {} files read", seen.files);
        // Each entry of the allowlist is a real file with a reason.
        for &(path, reason) in ALLOWED {
            assert!(repo_root().join(path).is_file(), "{path}");
            assert!(!reason.is_empty(), "{path}");
        }
    }

    /// A new directory for one test, under the target directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = target_dir()
            .join("xtask")
            .join("document-gate-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=xtask test",
                "-c",
                "user.email=xtask@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The whole path, in a real repository made for the test: a PDF staged, committed, deleted
    /// by a second commit, and then the first commit dropped.
    #[test]
    fn a_pdf_in_a_real_repository_is_caught_staged_committed_and_deleted() {
        let dir = scratch("repository");
        run_git(&dir, &["init", "--quiet"]);
        fs::write(dir.join("README.md"), "# A repository\n").unwrap();
        run_git(&dir, &["add", "README.md"]);
        run_git(&dir, &["commit", "--quiet", "-m", "first"]);
        let (found, seen, from) = scan(&dir, &[]).expect("the scan");
        assert_eq!(found, [""; 0]);
        assert_eq!((seen.files, from), (1, Listing::Git));

        // Untracked, a PDF is nobody's business. Staged, it is tracked.
        fs::create_dir(dir.join("specifications")).unwrap();
        let pdf = dir.join("specifications").join("vendor spec.pdf");
        fs::write(&pdf, b"%PDF-1.6\n").unwrap();
        assert_eq!(scan(&dir, &[]).expect("the scan").0, [""; 0]);
        run_git(&dir, &["add", "specifications/vendor spec.pdf"]);
        let staged = scan(&dir, &[]).expect("the scan").0;
        assert_eq!(staged.len(), 1, "{staged:?}");
        assert!(
            staged[0].starts_with("specifications/vendor spec.pdf: a PDF by its name, and tracked"),
            "{staged:?}"
        );

        // Committed, then deleted by a later commit: still in the branch.
        run_git(&dir, &["commit", "--quiet", "-m", "a specification"]);
        run_git(&dir, &["rm", "--quiet", "specifications/vendor spec.pdf"]);
        run_git(&dir, &["commit", "--quiet", "-m", "remove it"]);
        let deleted = scan(&dir, &[]).expect("the scan").0;
        assert_eq!(deleted.len(), 1, "{deleted:?}");
        assert!(
            deleted[0].contains("no longer tracked but added by commit"),
            "{deleted:?}"
        );

        // A renamed copy is caught by what it holds.
        fs::write(dir.join("notes.txt"), b"%PDF-1.6\n").unwrap();
        run_git(&dir, &["add", "notes.txt"]);
        let renamed = scan(&dir, &[]).expect("the scan").0;
        assert!(
            renamed
                .iter()
                .any(|p| p.starts_with("notes.txt: a PDF by its first bytes")),
            "{renamed:?}"
        );
        run_git(&dir, &["rm", "--quiet", "--cached", "notes.txt"]);

        // The remedy: the branch without the commits that held it.
        run_git(&dir, &["reset", "--quiet", "--hard", "HEAD~2"]);
        assert_eq!(scan(&dir, &[]).expect("the scan").0, [""; 0]);
    }

    /// A copy of a tree that is not a git checkout: every file in it is read.
    #[test]
    fn a_tree_outside_git_is_walked() {
        let dir = scratch("plain-copy");
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::create_dir_all(dir.join("specifications")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("docs/a.md"), "# Notes\n").unwrap();
        fs::write(dir.join("docs/b.pdf"), b"%PDF-1.6\n").unwrap();
        // The ignored directory for local copies, and build output, are not the tree.
        fs::write(dir.join("specifications/c.pdf"), b"%PDF-1.6\n").unwrap();
        fs::write(dir.join("target/d.zip"), b"PK\x03\x04").unwrap();
        // The scratch directory is inside this repository's checkout, but is not its top.
        let (found, seen, from) = scan(&dir, &[]).expect("the scan");
        assert_eq!(from, Listing::Tree);
        assert_eq!(seen.files, 2);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("docs/b.pdf: a PDF by its name"),
            "{found:?}"
        );
    }
}
